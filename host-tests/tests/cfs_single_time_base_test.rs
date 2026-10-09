// SPDX-License-Identifier: MPL-2.0
//! DECISION-099 (S-1 根治) 回归: CFS 单时基 + 结构真相 + 幂等记账.
//!
//! 缺陷 (取证结论见 `docs/report/` 与 `docs/plan/net-e2e-tcp-echo-flakiness.md`):
//! `CfsRunQueue` 旧实现同时维护"队列 `min_vruntime`"与"任务自身 `cfs_vruntime`"
//! 两个时间域, 且入队只钳树键不回写任务; 运行任务的 vruntime 与队列下界无界发散
//! (实测 `curr vr=45 / floor=5152`), `cfs_should_preempt` 的 `saturating_sub` 把
//! 负差压成 0 ⇒ 本核永不置 `need_reschedule` ⇒ 树上的 Ready 任务饿死 4~10 s
//! (跨过宿主 5 s recv 超时, 即 e2e 抖动). 并行计数器 `nr_running` 又与树背离
//! (实测 `nr=108 / tree=0`), 使 `is_empty()` 与抢占判据同时失真.
//!
//! 本测试锁定五条不变式的**接线契约** (队列语义本身由 `cfs.rs` 内联单测覆盖):
//!
//! - IC1 单一时基 —— 入队落点必须回写 `Process::cfs_vruntime`
//! - IC2 结构即真相 —— 不得存在并行可运行计数器
//! - IC3 下界单调 —— floor 只有 tick 一条 `fetch_max` 写路径, 且字段私有
//! - IC4 可运行集 = 树 —— 阻塞 / 不可重排 / 迁移都要正确撤账
//! - IC5 相对次序不被抹掉 —— 周期性整树折叠 boost 必须已删除

use std::fs;
use std::path::PathBuf;

const CFS: &str = "src/kernel/privileged/proc/cfs.rs";
const SCHED: &str = "src/kernel/privileged/proc/scheduler.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免注释里的历史名词被误判为代码
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 源码片段匹配统一忽略空白: rustfmt 会把长链式调用折行, 契约关心的是调用序列
/// 本身, 不是排版.
fn has(haystack: &str, needle: &str) -> bool {
    let norm = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    norm(haystack).contains(&norm(needle))
}

/// 取 `sig` 所在函数的体 (含签名行), 按花括号配对截取
fn fn_body(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let bytes: Vec<char> = src[start..].chars().collect();
    let mut depth = 0usize;
    let mut seen_open = false;
    for (i, c) in bytes.iter().enumerate() {
        match c {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    return bytes[..=i].iter().collect();
                }
            }
            _ => {}
        }
    }
    panic!("函数体花括号不配对: {sig}");
}

/// IC2: 并行可运行计数器不得复活 (队列 / 调度器两侧都不得出现).
#[test]
fn no_parallel_runnable_counter() {
    let cfs = code_only(&read_src(CFS));
    let sched = code_only(&read_src(SCHED));
    assert!(
        !cfs.contains("nr_running"),
        "IC2: cfs.rs 不得保留 nr_running 计数器"
    );
    assert!(
        !sched.contains("nr_running"),
        "IC2: scheduler.rs 不得读取并行计数器 (与树背离即隐形饥饿)"
    );
}

/// IC3 + IC5: floor 只有单调推进一条写路径, 折叠 / 重设原语必须已删除.
#[test]
fn floor_has_only_monotonic_writer() {
    let cfs = code_only(&read_src(CFS));
    for gone in [
        "fn boost_all_vruntime",
        "fn sync_min_vruntime",
        "fn update_curr",
    ] {
        assert!(
            !has(&cfs, gone),
            "IC3/IC5: 旧原语 {gone} 不得回归 (重设 floor 形成棘轮, 折叠整树抹掉相对次序)"
        );
    }
    // 唯一写路径: advance_floor 的 fetch_max (只增不减)
    assert!(
        has(&cfs, "self.min_vruntime.fetch_max(vr"),
        "IC3: floor 必须只经 advance_floor 的 fetch_max 推进"
    );
    // 字段私有 —— 否则调用点可重新引入"重设"语义
    assert!(
        !cfs.contains("pub min_vruntime"),
        "IC3: min_vruntime 字段必须私有, 只经 floor()/advance_floor() 暴露"
    );

    let sched = code_only(&read_src(SCHED));
    assert!(
        !sched.contains(".min_vruntime"),
        "IC3: scheduler.rs 不得直接触碰 min_vruntime 字段"
    );
}

/// IC1: 入队落点必须回写任务自身 vruntime (只钳树键不回写正是 S-1 根因).
#[test]
fn enqueue_placement_is_written_back_to_task() {
    let sched = read_src(SCHED);
    let body = code_only(&fn_body(&sched, "fn cfs_enqueue_to"));
    assert!(
        has(&body, ".enqueue(pid, vr, weight, accounted)"),
        "IC1: 入队必须把记账凭证 accounted 透传到队列"
    );
    assert!(
        has(&body, "p.cfs_vruntime.store(placed"),
        "IC1: 队列返回的落点必须回写 Process::cfs_vruntime"
    );
    assert!(
        has(&body, "p.cfs_on_rq.store(true"),
        "IC4: 入队必须置记账凭证"
    );
}

/// IC4: 阻塞与"切走且不可重排"都必须撤账, 且撤账幂等.
#[test]
fn blocking_and_dead_prev_deactivate_accounting() {
    let sched = read_src(SCHED);
    let block_body = code_only(&fn_body(&sched, "pub fn block"));
    assert!(
        has(&block_body, "self.cfs_deactivate(pid)"),
        "IC4: 阻塞即离开可运行集 —— block() 必须撤账"
    );
    let schedule_body = code_only(&fn_body(&sched, "pub fn schedule"));
    assert!(
        has(&schedule_body, "self.cfs_deactivate(current_pid)"),
        "IC4: prev 不可重排 (Blocked/Zombie) 时必须撤账, 否则 total_weight 单调虚增"
    );
    let deactivate = code_only(&fn_body(&sched, "fn cfs_deactivate"));
    assert!(
        has(&deactivate, "if !accounted"),
        "撤账必须以 cfs_on_rq 为凭证做到幂等 (block() 与 schedule() 覆盖同一任务)"
    );
    // 被切走且仍可重排者: 原位放回, 不动账 (始终在可运行集中)
    assert!(
        has(&schedule_body, ".requeue(current_pid, vr)"),
        "IC1: 可重排的 prev 必须原位 requeue (旧 update_curr 语义已删)"
    );
}

/// IC2/IC4: pick_cfs_task 拣到不可调度节点必须摘除, 不得暂存放回.
#[test]
fn pick_prunes_unschedulable_nodes() {
    let sched = read_src(SCHED);
    let body = code_only(&fn_body(&sched, "fn pick_cfs_task"));
    assert!(
        has(&body, "cfs_rq.dequeue(pid, weight, on_rq)"),
        "不可调度节点必须撤账摘除 (留在树上会长期占据最小键位并扭曲 is_empty())"
    );
    assert!(
        !body.contains("skipped"),
        "旧实现的 skipped 暂存放回 = 不可调度节点永久占据树左端 (本核饥饿路径)"
    );
}

/// IC1 + IC3: tick 必须推进 floor 并在同一时基内做抢占判定.
#[test]
fn tick_advances_floor_and_compares_in_one_domain() {
    let sched = code_only(&read_src(SCHED));
    assert!(
        has(&sched, "cfs_rq.advance_floor(vr)"),
        "IC3: 反饥饿机制只能是 tick 随运行任务推进 floor"
    );
    assert!(
        has(&sched, "cfs_rq.leftmost_vruntime()"),
        "IC1: 抢占比较对象必须取自树左端 (与运行任务同域)"
    );
    assert!(
        has(&sched, "cfs_rq.placement(p.cfs_vruntime.load"),
        "IC1: 从未在队列落过点的运行任务必须先对齐到时基域"
    );
    assert!(
        has(&sched, "vr > cfs_rq.floor() + TARGET_LATENCY_TICKS"),
        "IC1: 让出判据同样必须以 floor 为同域基准"
    );
}

/// IC4: 跨核迁移必须把记账随任务一起搬走, 投递统一走 cfs_enqueue_to.
#[test]
fn load_balance_transfers_accounting() {
    let sched = read_src(SCHED);
    let body = code_only(&fn_body(&sched, "pub fn load_balance"));
    assert!(
        has(&body, "src_rq.dequeue(pid, weight, true)"),
        "IC4: 偷取必须从源核 total_weight 撤账 (否则两核权重和发散, 均衡判定失真)"
    );
    assert!(
        has(&body, "self.cfs_enqueue_to(pid, this_cpu)"),
        "IC1/IC4: 投递必须经统一入队入口 (落点钳制 + 回写 + 记账)"
    );
    assert!(
        has(&body, "self.cfs_enqueue_to(pid, busiest_cpu)"),
        "亲和性不符时退回源核同样要走统一入口"
    );
    assert!(
        !body.contains("dst_rq"),
        "不得长期持有目标核 rq 锁后再走会取同一把锁的入队路径"
    );
}

/// 记账 API 签名契约: accounted 凭证不可省略.
#[test]
fn queue_api_requires_accounting_credential() {
    let cfs = read_src(CFS);
    assert!(
        has(
            &cfs,
            "pub fn enqueue(&mut self, pid: Pid, vruntime: u64, weight: u64, accounted: bool) -> u64"
        ),
        "enqueue 必须收 accounted 并返回落点 (调用方据此回写 IC1)"
    );
    assert!(
        has(
            &cfs,
            "pub fn dequeue(&mut self, pid: Pid, weight: u64, accounted: bool) -> bool"
        ),
        "dequeue 必须收 accounted 以保证撤账幂等"
    );
    let pick = fn_body(&cfs, "pub fn pick_next(&mut self)");
    assert!(
        !pick.contains("total_weight"),
        "pick_next 只做树内移动, 不得动记账 (被拣者随即上核, 仍属可运行集)"
    );
}

/// 五条不变式必须有内核侧内联单测 (no_std + host 双跑, `make test-kernel-host`).
#[test]
fn cfs_invariant_unit_tests_present() {
    let cfs = read_src(CFS);
    for name in [
        "test_sched_cfs_structural_truth_no_counter_drift",
        "test_sched_cfs_enqueue_is_idempotent",
        "test_sched_cfs_placement_and_monotonic_floor",
        "test_sched_cfs_preempt_uses_shared_time_base",
        "test_sched_cfs_wakeup_bounded_latency_under_tick_loop",
    ] {
        assert!(
            cfs.contains(name),
            "DECISION-099 不变式缺少内联单测: {name}"
        );
    }
}
