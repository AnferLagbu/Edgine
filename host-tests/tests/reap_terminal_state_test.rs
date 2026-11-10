// SPDX-License-Identifier: MPL-2.0
// R-4 回归锁: 退出必须落不可拒绝的终态, 且退出中的 TCB 不得再被拣选/重排.
//
// # 缺陷机制 (aarch64 `[THREAD] FAIL: reap-timeout` 的根因链)
//
// `Scheduler::exit` 旧实现以 `set_state_safe(Zombie)` 落终态, 但状态机白名单
// 只允许 Running/Blocked→Zombie. 任务若以 Ready (已投送、尚未上核) 或 Frozen
// 进入退出路径, 写入被拒绝且 `let _` 静默丢弃, TCB 仍留在可运行集:
//   1. 退出链末尾的 `schedule()` 经 `keep_current` (Running/Ready 即保留) 选中
//      自己, 提前返回 —— exit 返回用户态, 本该结束的进程继续执行用户代码;
//   2. 此后每次被切走都经 `prev_requeue` 重新入树 (`[REDISPATCH] via=requeue
//      state=Ready gexit=1`), 该 TCB 永不落 Zombie, 线程组等不齐, `wait4`
//      5 s 预算耗尽 ⇒ `[THREAD] FAIL: reap-timeout`.
//
// # 修复形态 (= 本测试的断言对象)
//
// 1. `Process::mark_zombie` 是退出专用终态写点: 任意存活态 → Zombie, Zombie
//    幂等, `Terminated` (已回收) 保持原状; `Scheduler::exit` 只经它落终态;
// 2. 全部拣选面排除 `exiting` 任务: EEVDF 树选择与 current 视图、DL、RT、
//    `keep_current`、跨核偷取、tick「Ready 隐形」恢复网;
// 3. 全部投送面拒绝 `exiting` 任务: `place_locked` 的 gone 判据、`reap_off_cpu`
//    的补投支;
// 4. `schedule()` 的 prev 收尾对 `exiting` 一律不重排, 改走 `eevdf_deactivate`.
//
// # 断言方式
//
// 行为层: `mark_zombie` 的全状态迁移与 `set_state_safe` 的拒绝面可在主机侧
// 直接构造真 `Process`; tick 恢复判据 `ready_orphan_due` 是公开纯判据.
// 拣选/投送内部函数依赖 per-CPU 运行队列与在核上下文, 主机侧无法构造, 故用
// 源码文本契约锁死 (剥注释 + 去空白), 判别力由合成回退样本反向验证.

use edgine::kernel::privileged::proc::process::Process;
use edgine::kernel::privileged::proc::{ProcessState, Scheduler};
use std::fs;
use std::sync::atomic::Ordering;

const SCHED: &str = "../src/kernel/privileged/proc/scheduler.rs";

// ============================================================================
// 文本读取与规范化 (与 p78_pick_window_guard_test 同口径)
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 剥除注释行: 修复说明里会引用被禁形态, 不剥会误判.
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

// ============================================================================
// 行为层: 终态写点与状态机拒绝面
// ============================================================================

#[test]
fn mark_zombie_forces_terminal_from_every_alive_state() {
    for (pid, from) in (410u32..).zip([
        ProcessState::Created,
        ProcessState::Ready,
        ProcessState::Running,
        ProcessState::Blocked,
        ProcessState::Frozen,
        ProcessState::Zombie,
    ]) {
        let proc = Process::new(pid, "reap-terminal-test", None);
        proc.state.store(from as u32, Ordering::SeqCst);
        proc.mark_zombie();
        assert_eq!(
            proc.get_state(),
            ProcessState::Zombie,
            "{from:?} 进入退出路径必须落 Zombie: 状态机拒绝会把 TCB 留在可运行集"
        );
        proc.mark_zombie();
        assert_eq!(
            proc.get_state(),
            ProcessState::Zombie,
            "重复落终态必须幂等 ({from:?} 入口)"
        );
    }
}

#[test]
fn mark_zombie_preserves_already_reaped_terminated() {
    let proc = Process::new(420, "reap-terminal-test", None);
    proc.state
        .store(ProcessState::Terminated as u32, Ordering::SeqCst);
    proc.mark_zombie();
    assert_eq!(
        proc.get_state(),
        ProcessState::Terminated,
        "Terminated 是已回收终态, 迟到的退出写者不得把它打回僵尸"
    );
}

#[test]
fn state_machine_rejects_zombie_from_ready_and_frozen() {
    let proc = Process::new(430, "reap-terminal-test", None);
    for from in [
        ProcessState::Created,
        ProcessState::Ready,
        ProcessState::Frozen,
    ] {
        proc.state.store(from as u32, Ordering::SeqCst);
        assert!(
            proc.set_state_safe(ProcessState::Zombie).is_err(),
            "{from:?}→Zombie 必须被白名单拒绝 (这正是旧 exit 静默吞错的入口)"
        );
        assert_eq!(
            proc.get_state(),
            from,
            "拒绝时必须保持原状态: 旧 exit 的 `let _` 正是把失败静默丢弃"
        );
    }
    for from in [ProcessState::Running, ProcessState::Blocked] {
        proc.state.store(from as u32, Ordering::SeqCst);
        assert!(
            proc.set_state_safe(ProcessState::Zombie).is_ok(),
            "{from:?}→Zombie 是原白名单内的合法迁移"
        );
        assert_eq!(proc.get_state(), ProcessState::Zombie);
    }
}

#[test]
fn ready_orphan_due_excludes_exiting_task() {
    let proc = Process::new(440, "reap-terminal-test", None);
    proc.state
        .store(ProcessState::Ready as u32, Ordering::SeqCst);
    proc.eevdf_placed_once.store(true, Ordering::SeqCst);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        Some(440),
        "前置: 曾在队 + Ready + 不在队/不在核 + Normal 是恢复网的正样本"
    );
    proc.exiting.store(true, Ordering::SeqCst);
    assert_eq!(
        Scheduler::ready_orphan_due(&proc, 0),
        None,
        "退出中的任务不得由 tick 恢复网补投: 补投等于把退出重新钉进可运行集"
    );
}

// ============================================================================
// 形状层: 终态写点 (判据 1)
// ============================================================================

/// `Scheduler::exit` 必须走不可拒绝的 `mark_zombie`, 且全文件不得再出现状态机
/// 的 Zombie 写入 (它会静默拒绝 Ready/Frozen).
fn exit_uses_forced_terminal(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "pubfnexit(&self,exit_code:u32)") else {
        return false;
    };
    body.contains("proc.mark_zombie();") && !sched.contains("set_state_safe(ProcessState::Zombie)")
}

// ============================================================================
// 形状层: 拣选面排除 (判据 2)
// ============================================================================

/// EEVDF 树内选择: 不可调度判据必须同时排除 Zombie 与 exiting.
fn eevdf_pick_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fnpick_eevdf_task()") else {
        return false;
    };
    body.contains(
        "state!=ProcessState::Blocked&&state!=ProcessState::Zombie\
         &&!p.exiting.load(Ordering::Acquire)&&p.get_sched_policy()==SchedPolicy::Normal",
    )
}

/// current 的 EEVDF 视图: 退出中的 current 不得成为选择胜者 (否则
/// `pick.from_tree == false` 让 schedule 直接回报 current).
fn eevdf_current_view_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fneevdf_current_view(") else {
        return false;
    };
    body.contains("||p.exiting.load(Ordering::Acquire){returnNone;}")
}

/// DL 拣选: 退出中的 deadline 任务不得上核.
fn deadline_pick_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fnpick_deadline_task()") else {
        return false;
    };
    body.contains(
        "p.get_state()!=ProcessState::Zombie&&!p.exiting.load(Ordering::Acquire)\
         &&p.get_sched_policy()==SchedPolicy::Deadline",
    )
}

/// RT 拣选: 退出中的实时任务不得上核.
fn rt_pick_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fnpick_next_task(") else {
        return false;
    };
    body.contains("p.get_state()!=ProcessState::Zombie&&!p.exiting.load(Ordering::Acquire)")
}

/// idle 兜底的 keep-current: 退出中的 current 不得被原地保留 (它就是 exit 的
/// 最后一次 schedule 直接返回用户态的那条路).
fn keep_current_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fnpick_next_task(") else {
        return false;
    };
    body.contains(
        "(s==ProcessState::Running||s==ProcessState::Ready)&&!p.exiting.load(Ordering::Acquire)",
    )
}

/// tick「Ready 隐形」恢复网: 退出中的任务不属恢复对象.
fn orphan_recovery_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "pubfnready_orphan_due(") else {
        return false;
    };
    body.contains("||process.exiting.load(Ordering::Acquire)||!process.eevdf_placed_once")
}

/// 跨核偷取: 退出中的候选撤账摘除, 不得搬进目的核.
fn steal_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fnsteal_locked(") else {
        return false;
    };
    body.contains("let(state,exiting,policy,vr,weight,on_rq,on_cpu,vd)=PROCESS_TABLE")
        && body.contains("state!=ProcessState::Terminated&&!exiting&&policy==SchedPolicy::Normal")
}

// ============================================================================
// 形状层: 投送/重排面拒绝 (判据 3/4)
// ============================================================================

/// `place_locked` 的 gone 判据: 退出中与 Zombie/Terminated 同价.
fn placement_rejects_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fnplace_locked(") else {
        return false;
    };
    body.contains("p.exiting.load(Ordering::Acquire),p.eevdf_vruntime.load(Ordering::Acquire)")
        && body.contains("letgone=exiting||state==ProcessState::Zombie")
}

/// `reap_off_cpu` 的补投支: 退出中的 prev 不得回填.
fn reap_repopulation_excludes_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "fnreap_off_cpu(") else {
        return false;
    };
    body.contains("&&!was_exiting&&per_cpu.idle.load(Ordering::SeqCst)!=prev")
}

/// `schedule()` 的 prev 收尾: exiting 一律不重排, 改走撤账.
fn schedule_never_requeues_exiting(sched: &str) -> bool {
    let Some(body) = fn_body(sched, "pubfnschedule()->Option<Pid>") else {
        return false;
    };
    body.contains("&&(prev_state_code==1||prev_state_code==2)&&!prev_exiting")
        && body.contains("prev_state_code==3||prev_state_code==4||prev_exiting")
}

// ============================================================================
// 形状层断言
// ============================================================================

#[test]
fn exit_terminates_through_the_forced_write_point() {
    let sched = prod_squashed(SCHED);
    assert!(
        exit_uses_forced_terminal(&sched),
        "Scheduler::exit 必须经 mark_zombie 落终态, 且全文件不得再有 \
         set_state_safe(Zombie): 白名单拒绝 Ready/Frozen 会被 `let _` 静默吞掉"
    );
}

#[test]
fn every_pick_site_excludes_exiting_tasks() {
    let sched = prod_squashed(SCHED);
    assert!(
        eevdf_pick_excludes_exiting(&sched),
        "pick_eevdf_task 的可调度判据必须排除 exiting"
    );
    assert!(
        eevdf_current_view_excludes_exiting(&sched),
        "eevdf_current_view 必须排除 exiting: current 胜出时不走树账, 直接回报"
    );
    assert!(
        deadline_pick_excludes_exiting(&sched),
        "pick_deadline_task 的 alive 判据必须排除 exiting"
    );
    assert!(
        rt_pick_excludes_exiting(&sched),
        "RT 拣选的 alive 判据必须排除 exiting"
    );
    assert!(
        keep_current_excludes_exiting(&sched),
        "keep_current 必须排除 exiting: 否则 exit 末尾的 schedule 原地返回用户态"
    );
    assert!(
        orphan_recovery_excludes_exiting(&sched),
        "tick 恢复网必须排除 exiting: 补投等于把退出重新钉进可运行集"
    );
    assert!(
        steal_excludes_exiting(&sched),
        "跨核偷取必须排除 exiting 候选并撤账摘除"
    );
}

#[test]
fn every_placement_site_rejects_exiting_tasks() {
    let sched = prod_squashed(SCHED);
    assert!(
        placement_rejects_exiting(&sched),
        "place_locked 的 gone 判据必须纳入 exiting"
    );
    assert!(
        reap_repopulation_excludes_exiting(&sched),
        "reap_off_cpu 的补投支必须排除 was_exiting"
    );
    assert!(
        schedule_never_requeues_exiting(&sched),
        "schedule 的 prev 收尾必须对 exiting 关闭重排并改走 eevdf_deactivate"
    );
}

// ============================================================================
// 负控制: 谓词必须在回归真的发生时亮起
// ============================================================================

#[test]
fn terminal_state_negative_controls() {
    type Fixture<'a> = (&'a str, &'a str, for<'b> fn(&'b str) -> bool);
    let fixtures: &[Fixture<'_>] = &[
        (
            "旧形态: exit 走状态机落 Zombie (Ready/Frozen 被静默拒绝)",
            "pubfnexit(&self,exit_code:u32){let_=proc.set_state_safe(ProcessState::Zombie);}",
            exit_uses_forced_terminal,
        ),
        (
            "旧形态: EEVDF 可调度判据不含 exiting",
            "fnpick_eevdf_task()->Option<Pid>{letstate=p.get_state();\
             (state!=ProcessState::Blocked&&state!=ProcessState::Zombie\
             &&p.get_sched_policy()==SchedPolicy::Normal,);}",
            eevdf_pick_excludes_exiting,
        ),
        (
            "旧形态: current 视图不含 exiting",
            "fneevdf_current_view(rq:&EevdfRunQueue)->Option<EevdfTaskView>{\
             ifp.get_sched_policy()!=SchedPolicy::Normal\
             ||(state!=ProcessState::Running&&state!=ProcessState::Ready){returnNone;}}",
            eevdf_current_view_excludes_exiting,
        ),
        (
            "旧形态: DL alive 判据不含 exiting",
            "fnpick_deadline_task()->Option<Pid>{p.get_state()!=ProcessState::Zombie\
             &&p.get_sched_policy()==SchedPolicy::Deadline}",
            deadline_pick_excludes_exiting,
        ),
        (
            "旧形态: RT alive 判据不含 exiting",
            "fnpick_next_task(){with_process(rt_pid,|p|p.get_state()!=ProcessState::Zombie)}",
            rt_pick_excludes_exiting,
        ),
        (
            "旧形态: keep_current 接受 Ready 的退出任务",
            "fnpick_next_task(){with_process(current_pid,|p|{lets=p.get_state();\
             (s==ProcessState::Running||s==ProcessState::Ready)})}",
            keep_current_excludes_exiting,
        ),
        (
            "旧形态: tick 恢复网不排除 exiting",
            "pubfnready_orphan_due(process:&Process,current_pid:Pid)->Option<Pid>{\
             ifprocess.get_state()!=ProcessState::Ready||!process.eevdf_placed_once.load()\
             {returnNone;}}",
            orphan_recovery_excludes_exiting,
        ),
        (
            "旧形态: 偷取可调度判据不含 exiting",
            "fnsteal_locked(){let(state,policy,vr,weight,on_rq,on_cpu,vd)=PROCESS_TABLE;\
             letschedulable=state!=ProcessState::Zombie&&policy==SchedPolicy::Normal;}",
            steal_excludes_exiting,
        ),
        (
            "旧形态: place_locked 的 gone 只看 Zombie/Terminated",
            "fnplace_locked(){let(state,vr)=snap;letgone=state==ProcessState::Zombie\
             ||state==ProcessState::Terminated;ifgone{returnPlacementOutcome::FAILED;}}",
            placement_rejects_exiting,
        ),
        (
            "旧形态: reap_off_cpu 补投不排除 was_exiting",
            "fnreap_off_cpu(per_cpu:&PerCpuSched){ifstate==ProcessState::Ready&&!on_rq\
             &&policy==SchedPolicy::Normal&&per_cpu.idle.load(Ordering::SeqCst)!=prev{\
             Self::eevdf_place_on(prev,0,PlacementKind::Requeue);}}",
            reap_repopulation_excludes_exiting,
        ),
        (
            "旧形态: prev 重排只看状态码",
            "pubfnschedule()->Option<Pid>{letprev_requeue=prev_ptr.is_some()\
             &&(prev_state_code==1||prev_state_code==2);\
             ifprev_ptr.is_some()&&(prev_state_code==3||prev_state_code==4){deactivate();}}",
            schedule_never_requeues_exiting,
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
