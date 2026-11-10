// SPDX-License-Identifier: MPL-2.0
//! E-08 线程维度语义对齐回归锁.
//!
//! 口径来源: `docs/plan/eevdf-scheduler.md` E-08 与多线程工程 D7
//! (`Process` = 每线程 TCB, `ThreadGroup` 只承载组身份/生命周期/共享资源).
//!
//! 本文件锁定:
//!
//! - `nice`/`eevdf_weight`/`eevdf_vruntime`/`eevdf_*` 全部落在 TCB (`Process`),
//!   组对象不持有任何调度字段; 调度热路径 (`pick`/`tick`/`wake`) 与退出路径
//!   都不按 `tgid` 聚合或选人;
//! - 同组兄弟线程的 EEVDF 状态相互独立: 一方的 tick 推进 / 重权 / 迁移 /
//!   退出不改写另一方的 vr、权重、切片与截止期;
//! - 进程级捷径 (children/wait、fd 关闭、cwd、信号投递) 不读写调度字段.
//!
//! 队列公式与三分入队语义已由 `eevdf_queue_mechanism_test` /
//! `eevdf_tick_accounting_test` / `eevdf_migration_test` 锁定; 本文件只补
//! 「两个调度实体属于同一线程组」这一维度, 不重复实现公式对拍.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use edgine::kernel::privileged::proc::eevdf::{
    EevdfRunQueue, NICE0_WEIGHT, VIRT_ONE, calc_vruntime_delta_virt, nice_to_weight,
    virtual_deadline,
};
use edgine::kernel::privileged::proc::{Process, ThreadGroup};

const PROCESS_SRC: &str = "src/kernel/privileged/proc/process.rs";
const THREAD_GROUP_SRC: &str = "src/kernel/privileged/proc/thread_group.rs";
const SCHEDULER_SRC: &str = "src/kernel/privileged/proc/scheduler.rs";
const SIGNAL_SRC: &str = "src/kernel/privileged/proc/signal.rs";
const FS_CONTEXT_SRC: &str = "src/kernel/privileged/proc/fs_context.rs";
const WAIT4_SRC: &str = "src/kernel/functions/proc/wait4.rs";
const OPEN_SRC: &str = "src/kernel/functions/fs/open.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免注释里的历史名词被误判为代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 源码片段匹配统一忽略空白 (rustfmt 折行不影响契约).
fn has(haystack: &str, needle: &str) -> bool {
    let norm = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    norm(haystack).contains(&norm(needle))
}

/// 取 `sig` 所在函数的体 (含签名行), 按花括号配对截取.
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
    panic!("函数体未闭合: {sig}");
}

/// 按给定权重/虚拟时间写满一个 TCB 的本轮 EEVDF 状态, 返回 `(vr, slice_virt, vd)`.
fn seed_schedule(proc: &Process, weight: u64, vr: u64, slice_real: u64) -> (u64, u64, u64) {
    let slice_virt = calc_vruntime_delta_virt(weight).saturating_mul(slice_real);
    let vd = virtual_deadline(vr, slice_virt);
    proc.eevdf_weight.store(weight, Ordering::Release);
    proc.eevdf_vruntime.store(vr, Ordering::Release);
    proc.eevdf_slice_real.store(slice_real, Ordering::Release);
    proc.eevdf_slice_virt.store(slice_virt, Ordering::Release);
    proc.eevdf_slice_left.store(slice_real, Ordering::Release);
    proc.eevdf_vdeadline.store(vd, Ordering::Release);
    (vr, slice_virt, vd)
}

/// 读取 TCB 的 EEVDF 状态快照: `(weight, vr, slice_real, slice_virt, slice_left, vd)`.
fn snapshot(proc: &Process) -> (u64, u64, u64, u64, u64, u64) {
    (
        proc.eevdf_weight.load(Ordering::Acquire),
        proc.eevdf_vruntime.load(Ordering::Acquire),
        proc.eevdf_slice_real.load(Ordering::Acquire),
        proc.eevdf_slice_virt.load(Ordering::Acquire),
        proc.eevdf_slice_left.load(Ordering::Acquire),
        proc.eevdf_vdeadline.load(Ordering::Acquire),
    )
}

#[test]
fn scheduling_state_is_owned_by_the_tcb() {
    let process = code_only(&read_src(PROCESS_SRC));
    for field in [
        "pub nice: AtomicU32,",
        "pub eevdf_vruntime: AtomicU64,",
        "pub eevdf_weight: AtomicU64,",
        "pub eevdf_vdeadline: AtomicU64,",
        "pub eevdf_slice_real: AtomicU64,",
        "pub eevdf_slice_virt: AtomicU64,",
        "pub eevdf_slice_left: AtomicU64,",
        "pub eevdf_requested_slice: AtomicU64,",
    ] {
        assert!(
            has(&process, field),
            "E-08/D7: 每线程 TCB 必须承载调度字段 {field}"
        );
    }

    let group = code_only(&read_src(THREAD_GROUP_SRC));
    for token in [
        "eevdf_",
        "pub nice",
        "sched_policy",
        "rt_priority",
        "dl_runtime",
    ] {
        assert!(
            !has(&group, token),
            "E-08/D7: ThreadGroup 不得持有调度字段 {token} (调度实体是每线程 TCB)"
        );
    }
}

#[test]
fn selection_and_tick_are_keyed_by_tid_not_tgid() {
    let sched = code_only(&read_src(SCHEDULER_SRC));

    let pick = fn_body(&sched, "fn pick_eevdf_task(");
    assert!(
        has(&pick, "eevdf_rq.pick_eevdf("),
        "E-08: EEVDF 选路必须走 pick_eevdf"
    );
    for token in ["tgid", "thread_group", "group()"] {
        assert!(
            !has(&pick, token),
            "E-08: pick_eevdf_task 不得按组身份选人 (出现 {token})"
        );
    }

    let tick = fn_body(&sched, "fn tick_current_eevdf(");
    assert!(
        has(&tick, "PROCESS_TABLE.with_process(current_pid"),
        "E-08: tick 必须只读写 current 线程自己的 TCB"
    );
    assert!(
        has(&tick, "advance_running_vruntime(current_pid"),
        "E-08: tick 运行账增量必须以 current 的 TID 为键"
    );
    for token in ["tgid", "thread_group", "group()"] {
        assert!(!has(&tick, token), "E-08: tick 不得按组聚合 (出现 {token})");
    }

    let wake = fn_body(&sched, "fn wake_preempts(");
    assert!(
        has(&wake, "p.eevdf_vdeadline.load"),
        "E-08: 唤醒抢占必须比较目标线程自己的 vd"
    );
    assert!(
        has(&wake, "p.eevdf_vruntime.load"),
        "E-08: 唤醒抢占必须比较 current 自己的 vr"
    );
    assert!(!has(&wake, "tgid"), "E-08: 唤醒抢占不得以 tgid 判定资格");

    let current = fn_body(&sched, "pub fn current() -> Option<Pid>");
    assert!(
        has(&current, "per_cpu().current.load"),
        "E-08: current 必须是每核当前 TCB 的 PID"
    );
    assert!(!has(&current, "tgid"), "E-08: current 不得回落到 tgid");
}

#[test]
fn exit_path_touches_only_the_exiting_thread() {
    let sched = code_only(&read_src(SCHEDULER_SRC));
    let exit = fn_body(&sched, "pub fn exit(&self, exit_code: u32)");

    assert!(
        has(&exit, "group.leave(pid)"),
        "E-08: 退出必须只离开调用线程的组成员身份"
    );
    assert!(
        has(&exit, "group.mark_exited(code)"),
        "E-08: 组退出状态只在最后成员离开时置位"
    );
    assert!(
        has(&exit, "proc.mark_zombie()"),
        "E-08: 退出只把调用线程置 Zombie (R-4: 经不可拒绝终态写点)"
    );
    for token in ["eevdf_rq", "eevdf_vruntime", "eevdf_weight", "eevdf_"] {
        assert!(
            !has(&exit, token),
            "E-08: 线程退出不得改写任何调度账目 (出现 {token})"
        );
    }
}

#[test]
fn process_level_shortcuts_carry_no_scheduling_state() {
    for (label, path) in [
        ("信号投递", SIGNAL_SRC),
        ("wait4", WAIT4_SRC),
        ("FS 上下文", FS_CONTEXT_SRC),
        ("fd 关闭", OPEN_SRC),
    ] {
        let code = code_only(&read_src(path));
        for token in ["eevdf_vruntime", "eevdf_weight", "eevdf_"] {
            assert!(
                !has(&code, token),
                "E-08: {label} 路径不得读写调度字段 {token}"
            );
        }
    }
}

#[test]
fn sibling_threads_keep_independent_eevdf_state() {
    let leader = Process::new(0xE801, "e08-leader", None);
    let mut sibling = Process::new(0xE802, "e08-sibling", None);
    assert!(
        sibling.join_thread_group(Arc::clone(leader.group())),
        "E-08: 兄弟线程必须能加入同一线程组"
    );
    assert_eq!(sibling.tgid(), leader.pid.0, "同组线程共享 tgid");
    assert_ne!(leader.pid.0, sibling.pid.0, "同组线程的 TID 必须互异");

    let weight_a = nice_to_weight(-5);
    let weight_b = nice_to_weight(10);
    let leader_round = seed_schedule(&leader, weight_a, 5 * VIRT_ONE, 4);
    let sibling_round = seed_schedule(&sibling, weight_b, 9 * VIRT_ONE, 7);

    assert_eq!(
        snapshot(&leader),
        (
            weight_a,
            leader_round.0,
            4,
            leader_round.1,
            4,
            leader_round.2
        ),
        "leader 的本轮状态必须自洽"
    );
    assert_eq!(
        snapshot(&sibling),
        (
            weight_b,
            sibling_round.0,
            7,
            sibling_round.1,
            7,
            sibling_round.2
        ),
        "兄弟线程的本轮状态必须独立建立, 不得被 leader 覆盖"
    );
    assert_ne!(
        snapshot(&leader),
        snapshot(&sibling),
        "不同 nice/vr 的兄弟线程必须呈现不同调度状态"
    );
    assert_ne!(
        calc_vruntime_delta_virt(weight_a),
        calc_vruntime_delta_virt(weight_b),
        "Q16.16 增量必须随线程各自权重变化"
    );
}

#[test]
fn run_queue_accounts_sibling_threads_as_separate_entities() {
    let pid_a = 0xE811u32;
    let pid_b = 0xE812u32;
    let weight_a = nice_to_weight(-10);
    let weight_b = nice_to_weight(10);
    let mut rq = EevdfRunQueue::new();

    let vr_a = rq.enqueue(pid_a, 100 * VIRT_ONE, weight_a, false);
    let vr_b = rq.enqueue(pid_b, 200 * VIRT_ONE, weight_b, false);
    assert_eq!(
        rq.tree_task(pid_b).map(|task| task.vruntime),
        Some(vr_b),
        "兄弟线程必须各自持有树节点"
    );

    let picked = rq
        .pick_eevdf(None, |_| None)
        .expect("E-08: 非空可运行集必须选出任务");
    assert_eq!(picked.pid, pid_a, "合格前缀内应选最早截止的线程");
    assert!(picked.from_tree, "胜者必须由树侧移入运行账");
    assert_eq!(
        rq.tree_task(pid_b).map(|task| task.vruntime),
        Some(vr_b),
        "拣出一个线程不得改写兄弟线程的树节点"
    );

    let delta = calc_vruntime_delta_virt(weight_a);
    assert!(
        rq.advance_running_vruntime(pid_a, vr_a.saturating_add(delta)),
        "运行账增量必须命中正在运行的线程"
    );
    assert_eq!(
        rq.tree_task(pid_b).map(|task| task.vruntime),
        Some(vr_b),
        "推进一个线程的 vr 不得带动兄弟线程"
    );

    assert!(
        rq.reweight(pid_b, weight_b.saturating_add(1)),
        "兄弟线程的权重变更必须在树账内命中"
    );
    assert_eq!(
        rq.running_task(pid_a).map(|task| task.weight),
        Some(weight_a),
        "重权兄弟线程不得改写正在运行线程的权重"
    );
    assert!(
        !rq.dequeue(pid_a, weight_a, true),
        "运行账任务出队不回报树上移除"
    );
    assert!(
        rq.tree_task(pid_b).is_some(),
        "一个线程退出可运行集不得摘除兄弟线程"
    );
}

#[test]
fn migration_carries_only_the_moving_thread() {
    let pid_a = 0xE821u32;
    let pid_b = 0xE822u32;
    let weight_a = nice_to_weight(-10);
    let weight_b = nice_to_weight(0);
    let mut source = EevdfRunQueue::new();
    let mut destination = EevdfRunQueue::new();

    let vr_a = source.enqueue(pid_a, 80 * VIRT_ONE, weight_a, false);
    let vr_b = source.enqueue(pid_b, 120 * VIRT_ONE, weight_b, false);
    assert!(source.dequeue(pid_a, weight_a, true), "迁移前先撤源树账");

    let placed = destination.migrate(pid_a, vr_a, weight_a);
    assert_eq!(placed, vr_a, "迁移必须原样携带 vr (IE5)");
    assert!(
        destination.tree_task(pid_b).is_none(),
        "迁移不得把同组兄弟线程一并带走"
    );
    assert_eq!(
        source.tree_task(pid_b).map(|task| task.vruntime),
        Some(vr_b),
        "源核上的兄弟线程必须原样保留"
    );
    assert!(
        destination.tree_task(pid_a).is_some(),
        "被迁移线程必须落在目的核树账"
    );
}

#[test]
fn non_last_thread_exit_leaves_sibling_schedule_state_intact() {
    let leader = Process::new(0xE831, "e08-leader", None);
    let mut sibling = Process::new(0xE832, "e08-sibling", None);
    assert!(sibling.join_thread_group(Arc::clone(leader.group())));
    let group = Arc::clone(leader.group());

    let sibling_weight = nice_to_weight(5);
    let (vr, slice_virt, vd) = seed_schedule(&sibling, sibling_weight, 42 * VIRT_ONE, 6);
    let before = snapshot(&sibling);

    assert!(
        !group.leave(leader.pid.0),
        "E-08: 组内仍有成员时不得判定为最后退出"
    );
    assert!(!group.has_exited(), "非最后成员退出不得置组退出态");
    assert_eq!(
        snapshot(&sibling),
        before,
        "兄弟线程退出不得改写仍在运行线程的调度状态"
    );
    assert_eq!(group.members(), vec![sibling.pid.0]);

    assert!(group.leave(sibling.pid.0), "最后一个成员离开必须回报组已空");
    group.mark_exited(7);
    assert!(group.has_exited(), "最后成员退出后组进入退出态");
    assert_eq!(group.exit_code(), 7, "组退出码必须是首个有效值");
    assert_eq!(
        snapshot(&sibling),
        (sibling_weight, vr, 6, slice_virt, 6, vd),
        "组生命周期迁移不得把调度状态搬到组对象上"
    );
}

#[test]
fn thread_group_defaults_do_not_seed_scheduling_state() {
    let group = ThreadGroup::new_leader(0xE841);
    assert_eq!(group.members(), vec![0xE841]);
    assert!(!group.has_exited());
    assert_eq!(group.member_count(), 1);
    assert_eq!(NICE0_WEIGHT, 1024, "nice 0 的基准权重必须保持单源");
}
