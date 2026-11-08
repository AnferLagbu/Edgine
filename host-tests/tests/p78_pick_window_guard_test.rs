// SPDX-License-Identifier: MPL-2.0
// P-78 回归锁: 拣中窗口 (pick window) 内不得允许跨核搬动已拣中的任务.
//
// # 缺陷机制 (q_diag3 第 1 轮直接取证)
//
// `pick_eevdf_task` 把胜者从树搬进**源核**的 running 账 (`eevdf_on_rq` 仍为
// true, `eevdf_on_cpu` 仍为 false), 随即放掉 rq 锁; `schedule()` 稍后才写
// `Running` / `eevdf_on_cpu=true` / `per_cpu.current`. 这中间的窗口里:
//   - 投送屏障看不见它 (`on_cpu` 还是 false);
//   - 在核核查看不见它 (源/目的 `current` 都还不是它);
//   - 跨核偷取看不见它 (候选只取树前缀, 它已离树).
// 但**归属认领 + 跨核搬动**看得见它: `place_locked` 只按 `prev_cpu` 归属与
// `on_rq` 快照摘源投目的. 一次并发投送 (实测为 tick 的无主任务恢复) 就能把它
// 从源核 running 账搬进目的核树 —— 源核照旧切到它, 目的核再从树拣一次, 两核
// 交叠同一块 16 KiB 进程内核栈. 实测两核 RSP 均落在 init 的
// `0x7ca4000..0x7ca8000` 内, CPU1 `current_pid=5`, CPU0 以 pid=5 执行
// `vec=6` 非法指令后 HLT.
//
// # 修复形态 (= 本测试的断言对象)
//
// 1. `place_locked` 在任何记账动作之前用 `src.is_running(pid)` 拒收 ——
//    running 账是"已拣中"的唯一凭证;
// 2. 该判据排在投送屏障与在核核查**之前**: 那两个判据在拣中窗口里都是盲的;
// 3. 跨核搬动仍只有 `src_rq.dequeue` 一处 (单入口), 判据因而覆盖全部搬运者
//    (tick 无主恢复 / 唤醒投送 / prev 落地 / 负载均衡);
// 4. `requeue_prev` 的状态推进不得把**归属仍在别核**的 Running 任务打回
//    `Ready` —— 搬动已被判据 1 拒收, 若状态仍被改写, 该任务随后的 `block()`
//    就是非法的 `Ready→Blocked`, 阻塞被吞.
//
// 行为面另行锁住判据所依赖的记账事实: `pick_eevdf` 的树内胜者拣出后**只在**
// running 账里, 对树与偷取候选都不可见.
//
// # 断言方式
//
// 跨核竞态无法在主机侧构造 (无真 SMP 内核上下文), 故窗口形状用源码文本契约
// 锁死 (剥注释 + 去空白; 不受 rustfmt 换行影响); 记账事实用真 `EevdfRunQueue`
// 行为测. 文本谓词的判别力由 [`pick_window_negative_controls`] 用合成回退样本
// 反向验证.

use edgine::kernel::privileged::proc::eevdf::{EevdfRunQueue, EevdfTaskView};
use std::fs;

const SCHED: &str = "../src/kernel/privileged/proc/scheduler.rs";

// ============================================================================
// 文本读取与规范化 (与 smp_rq_migration_test 同口径)
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 剥除注释行: 修复把被禁形态写进了注释, 不剥会误判.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && !l.starts_with("/*") && !l.starts_with('*'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 在测试模块声明处截断, 使内核内联用例不污染契约.
fn truncate_test_module(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#[cfg(test)]") || line.starts_with("#[cfg(all(test") {
            let next = lines.get(i + 1).unwrap_or(&"");
            if next.starts_with("mod ") || next.starts_with("pub mod ") {
                return lines[..i].join("\n");
            }
        }
    }
    src.to_string()
}

fn squash(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

fn prod_squashed(path: &str) -> String {
    squash(&truncate_test_module(&strip_comments(&read(path))))
}

/// 取 `sig` 函数体 (花括号配对; 输入须已剔注释并 squash).
fn fn_body<'a>(code: &'a str, sig: &str) -> Option<&'a str> {
    let at = code.find(sig)?;
    let open = code[at..].find('{')? + at;
    let mut depth = 0usize;
    for (i, ch) in code[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&code[open..=(open + i)]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `place_locked` 的拣中拒收谓词: 判据存在, 且排在任何记账动作之前.
fn pick_window_guard(body: &str) -> bool {
    let Some(gate) = body.find("is_running(pid)){returnPlacementOutcome::FAILED;}") else {
        return false;
    };
    let Some(barrier) = body.find("Self::place_barrier(") else {
        return false;
    };
    let Some(dequeue) = body.find("src_rq.dequeue(") else {
        return false;
    };
    let Some(enqueue) = body.find("dst.enqueue(") else {
        return false;
    };
    gate < barrier && gate < dequeue && gate < enqueue
}

/// `requeue_prev` 的外来运行态谓词: 门判据在 Ready 写入之前, 且门内不写状态.
fn foreign_running_gate(body: &str) -> bool {
    let Some(gate) = body.find("letforeign_running=state==ProcessState::Running") else {
        return false;
    };
    let Some(ready) = body.find("p.set_state_safe(ProcessState::Ready)") else {
        return false;
    };
    gate < ready
        && body.contains(
            "letfailed=ifforeign_running{false}else{p.set_state_safe(ProcessState::Ready).is_err()};",
        )
}

// ============================================================================
// 形状层: 判据 1/2 (拣中即拒收, 且在盲判据之前)
// ============================================================================

#[test]
fn place_locked_rejects_picked_task_before_any_ledger_action() {
    let sched = prod_squashed(SCHED);
    let body = fn_body(&sched, "fnplace_locked(").expect("place_locked 不存在");
    assert!(
        pick_window_guard(body),
        "place_locked 必须在投送屏障与摘源投目的之前拒收 running 账里的任务: \
         拣中到 schedule() 写 on_cpu/current 之间, 屏障与在核核查都是盲的"
    );
    assert!(
        body.contains(
            "ifsrc.as_ref().is_some_and(|rq|rq.is_running(pid)){returnPlacementOutcome::FAILED;}"
        ),
        "拒收必须按**源队列** running 账判定 (目的队列在册属已投送, 不是拣中)"
    );
    assert_eq!(
        body.matches("src_rq.dequeue(").count(),
        1,
        "跨核搬动必须只有一处摘源: 判据只要覆盖它, 就覆盖全部搬运者"
    );
}

#[test]
fn every_mover_funnels_through_the_guarded_entry() {
    let sched = prod_squashed(SCHED);
    // 全文件只有三处 place_locked 调用, 且都在 eevdf_place_on_inner 体内.
    assert_eq!(
        sched.matches("Self::place_locked(").count(),
        3,
        "place_locked 新增调用点会绕过单入口口径, 必须重新核对锁序与认领前提"
    );
    let inner = fn_body(&sched, "fneevdf_place_on_inner(").expect("eevdf_place_on_inner 不存在");
    assert_eq!(
        inner.matches("Self::place_locked(").count(),
        3,
        "三个 place_locked 调用 (同核 / 升序 / 降序双持) 必须全部落在单入口体内"
    );
    // tick 恢复腿只走公开入口 —— 不直呼 _inner, 因而不享有 owner_landing 豁免.
    let recover =
        fn_body(&sched, "fntick_recover_orphans(){").expect("tick_recover_orphans 不存在");
    assert!(
        recover.contains("Self::eevdf_place_on(pid,dst_cpu,PlacementKind::Requeue)"),
        "无主任务恢复必须走 eevdf_place_on (owner_landing=false), 继承拣中判据"
    );
    assert!(
        !recover.contains("eevdf_place_on_inner"),
        "恢复腿不得直呼 _inner: owner_landing 豁免只给 schedule() 的 prev 收尾"
    );
}

// ============================================================================
// 形状层: 判据 4 (prev 收尾不得改写别核在跑任务的状态)
// ============================================================================

#[test]
fn requeue_prev_never_readies_foreign_running_task() {
    let sched = prod_squashed(SCHED);
    let body = fn_body(&sched, "fnrequeue_prev(").expect("requeue_prev 不存在");
    assert!(
        foreign_running_gate(body),
        "requeue_prev 必须对\"Running 且归属在别核\"的任务跳过 Ready 写入: \
         搬动已被 place_locked 拒收, 状态推进归那个核"
    );
    assert!(
        body.contains("owner!=NO_RQ_CPU&&owner!=this_cpu"),
        "外来运行态必须同时排除无归属与已归本核: 前者是 create/boot 形态, 后者是正常 prev"
    );
    assert_eq!(
        body.matches("set_state_safe(ProcessState::Ready)").count(),
        1,
        "正常 prev 的 Ready 写入必须保留 (不得为绕过门禁而删除整条推进)"
    );
}

// ============================================================================
// 行为层: 判据所依赖的记账事实 (拣出 = 离树 + 进运行账)
// ============================================================================

#[test]
fn picked_task_leaves_tree_and_lives_only_in_running_ledger() {
    let mut rq = EevdfRunQueue::new();
    rq.enqueue(21, 30, 100, false);
    assert!(rq.is_on_tree(21) && !rq.is_running(21), "入队只落树账");

    let pick = rq
        .pick_eevdf(None, |_| None)
        .expect("树上唯一任务必然被拣出");
    assert_eq!(pick.pid, 21);
    assert!(
        pick.from_tree,
        "树内胜者必须走 activate_tree_task (from_tree=true)"
    );
    assert!(
        rq.is_running(21),
        "拣出后必须进 running 账: 这是\"已拣中\"的唯一凭证, 也是 place_locked 判据的读面"
    );
    assert!(!rq.is_on_tree(21), "拣出后必须离树");
    assert_eq!(rq.len(), 0);
    assert_eq!(rq.running_count(), 1);
    assert_eq!(rq.tree_weight(), 0, "拣出 = 同一笔账从树账移到运行账");
    assert_eq!(rq.running_weight(), 100);

    // 目的核能再次拣到它, 前提只能是有人把它搬回某棵树 —— 这正是被
    // place_locked 拒收的跨核搬动 (src.dequeue + dst.enqueue). 单看本队列,
    // 拣中任务对树视图与偷取候选都不可见.
    let mut out = [0u32; 4];
    assert_eq!(
        rq.eligible_pids(&mut out),
        0,
        "已拣中任务不得进入跨核偷取候选"
    );
    let view = EevdfTaskView {
        pid: 21,
        vruntime: pick.vruntime,
        vdeadline: pick.vdeadline,
        weight: 100,
    };
    let again = rq
        .pick_eevdf(Some(view), |_| None)
        .expect("运行账在册的 current 只回报不重拣");
    assert_eq!(again.pid, 21);
    assert!(!again.from_tree, "运行账里的任务不得再次从树拣出");
}

// ============================================================================
// 负控制: 谓词必须在回归真的发生时亮起
// ============================================================================

#[test]
fn pick_window_negative_controls() {
    type Fixture<'a> = (&'a str, &'a str, for<'b> fn(&'b str) -> bool);
    let fixtures: &[Fixture<'_>] = &[
        (
            "旧形态: 拣中后仍允许跨核搬动",
            "ifgone{returnPlacementOutcome::FAILED;}ifletSome(outcome)=\
             Self::place_barrier(pid,on_cpu,owner_landing,&plan){returnoutcome;}\
             letsrc_rq=src_rq;src_rq.dequeue(pid,weight,on_rq);\
             dst.enqueue(pid,plan.vruntime,weight,false);",
            pick_window_guard,
        ),
        (
            "补丁式: 判据排在屏障/摘源之后",
            "ifletSome(outcome)=Self::place_barrier(pid,on_cpu,owner_landing,&plan){\
             returnoutcome;}letsrc_rq=src_rq;src_rq.dequeue(pid,weight,on_rq);\
             ifsrc.as_ref().is_some_and(|rq|rq.is_running(pid)){\
             returnPlacementOutcome::FAILED;}dst.enqueue(pid,plan.vruntime,weight,false);",
            pick_window_guard,
        ),
        (
            "旧形态: prev 收尾无条件写 Ready",
            "letfailed=p.set_state_safe(ProcessState::Ready).is_err();",
            foreign_running_gate,
        ),
        (
            "门禁存在但不排在本核在跑判据之后 (仍改写外来运行态)",
            "letfailed=p.set_state_safe(ProcessState::Ready).is_err();\
             letforeign_running=state==ProcessState::Running&&owner!=NO_RQ_CPU\
             &&owner!=this_cpu;",
            foreign_running_gate,
        ),
    ];
    for (name, fixture, predicate) in fixtures {
        let squashed = squash(&strip_comments(fixture));
        assert!(
            !predicate(&squashed),
            "负控制失效: 合成回退样本 {name:?} 未被识别 —— 对应门禁已形同虚设"
        );
    }
}
