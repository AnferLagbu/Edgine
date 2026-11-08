// SPDX-License-Identifier: MPL-2.0
//! K-07b 批3 调度观测面与多线程交叉验证.
//!
//! 判据来源: `host-tests/benches/baseline.json` 顶层 `sched_criteria`
//! (机器可读基线; 性能指标腿 `host-tests/tests/perf_metric_test.rs` 只读 `results`, 互不干扰).
//!
//! 验收点:
//! - IE1 具名用例: 账目/V/lag/切片全在 Q16.16 单时基上, 参考模型对账通过;
//! - IE4 具名用例: 连续 tick/schedule 下本核 floor 只增不减;
//! - 唤醒信用上界 / 稳态 lag 上界 / 唤醒周转次数 / 公平性份额差 均满足基线判据;
//! - `EevdfRunQueue::observation` 守恒自检与计数器语义 (K-07b 批3 内核观测面);
//! - 接线锁: syscall 464 单源 + dispatch 分发 + 用户态触发 + 门禁脚本断言.

use std::path::Path;

use edgine::kernel::privileged::proc::eevdf::{EevdfObsCounters, EevdfRunQueue};
use edgine::kernel::privileged::syscall::types as ktypes;
use edgine_abi::sched as abi;
use edgine_host_tests::eevdf_model::{
    Model, TARGET_LATENCY, VIRT_ONE, delta_vruntime, weighted_avg_vruntime,
};

/// `baseline.json` 中承载调度公平性/时延判据的顶层键.
const CRITERIA_KEY: &str = "sched_criteria";

/// 调度基线与门禁共用的判据集合.
#[derive(Debug, Clone, Copy)]
struct SchedCriteria {
    /// 稳态下每个在账任务 `|lag| <= slice_virt * bound / 1000`.
    lag_bound_permille: u64,
    /// Wake 睡眠信用 `V_before - placed <= slice_virt * bound / 1000`.
    wake_credit_bound_permille: u64,
    /// 新唤醒任务被拣出所需的 `schedule()` 次数上限.
    wake_turnaround_schedules: u64,
    /// 公平性统计窗口 (tick).
    fairness_window_ticks: u64,
    /// 等权任务窗口内 CPU 份额极差占均值的千分比上限.
    fairness_spread_permille: u64,
}

fn read_baseline() -> serde_json::Value {
    let path = Path::new("benches/baseline.json");
    let text = std::fs::read_to_string(path).expect("baseline.json 必须存在");
    serde_json::from_str(&text).expect("baseline.json 必须是合法 JSON")
}

fn load_criteria() -> SchedCriteria {
    let baseline = read_baseline();
    let c = baseline
        .get(CRITERIA_KEY)
        .expect("baseline.json 缺 sched_criteria (K-07b 批3 判据)");
    let field = |name: &str| {
        c.get(name)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("sched_criteria.{name} 必须是 u64"))
    };
    SchedCriteria {
        lag_bound_permille: field("lag_bound_permille"),
        wake_credit_bound_permille: field("wake_credit_bound_permille"),
        wake_turnaround_schedules: field("wake_turnaround_schedules"),
        fairness_window_ticks: field("fairness_window_ticks"),
        fairness_spread_permille: field("fairness_spread_permille"),
    }
}

fn idx(pid: u32) -> usize {
    usize::try_from(pid).expect("测试 pid 必须放入 usize")
}

// ── IE1 / IE4 具名用例 ────────────────────────────────────────────────────

/// IE1 同域: `vr`/`vd`/`slice_virt`/`floor` 全在 Q16.16 单一时基, 账目守恒.
#[test]
fn ie1_single_domain_and_ledgers_conserve() {
    let mut model = Model::new();
    assert!(model.add_task(1, 0, 0));
    assert!(model.add_task(2, -5, 0));
    assert!(model.wake(0, 1));
    assert!(model.wake(0, 2));
    for _ in 0..240 {
        model.tick(0);
        model.schedule(0);
    }
    model.audit();

    let rq = model.rq(0).expect("cpu 0 必须存在");
    assert_eq!(
        rq.avg_vruntime(),
        weighted_avg_vruntime(rq.total_weight(), rq.sum_w_vr()),
        "IE1: V 必须由同一账目域重算"
    );
    for pid in [1u32, 2] {
        let task = model.task(pid).expect("任务必须存在");
        assert_eq!(
            task.slice_virt,
            delta_vruntime(task.weight).saturating_mul(task.slice_real),
            "IE1: pid {pid} 的 slice_virt 不在 Q16.16 同域"
        );
        assert_eq!(
            task.vd,
            task.vr
                .saturating_add(delta_vruntime(task.weight).saturating_mul(task.slice_left)),
            "IE1: pid {pid} 的 vd 与 vr/切片不同域"
        );
    }
}

/// IE4 floor 单调: tick + schedule 连续推进下本核 floor 只增不减, 且确实前推.
#[test]
fn ie4_floor_monotonic_under_ticks_and_schedules() {
    let mut model = Model::new();
    assert!(model.add_task(1, 0, 0));
    assert!(model.add_task(2, 5, 0));
    assert!(model.wake(0, 1));
    assert!(model.wake(0, 2));

    let mut floor = model.floor(0).expect("cpu 0 必须存在");
    for round in 0..600 {
        model.tick(0);
        model.schedule(0);
        let now = model.floor(0).expect("cpu 0 必须存在");
        assert!(
            now >= floor,
            "IE4: floor 在 round {round} 回退 {floor} -> {now}"
        );
        floor = now;
    }
    assert!(floor > 0, "IE4: 跑满窗口后 floor 必须前推");
    model.audit();
}

// ── 基线判据: 时延 / lag / 公平性 ─────────────────────────────────────────

/// IE3 睡眠信用上界: Wake 落点不低于 `V_before - slice_virt`, 且满足基线千分比.
#[test]
fn wake_credit_bounded_by_baseline_criteria() {
    let criteria = load_criteria();
    let mut model = Model::new();
    assert!(model.add_task(1, 0, 0));
    assert!(model.wake(0, 1));
    for _ in 0..300 {
        model.tick(0);
        model.schedule(0);
    }
    let v_before = model.rq(0).expect("cpu 0 必须存在").avg_vruntime();

    assert!(model.add_task(2, 0, 0));
    assert!(model.wake(0, 2));
    let task = model.task(2).expect("任务必须存在");
    let credit = v_before.saturating_sub(task.vr);
    let bound = task
        .slice_virt
        .saturating_mul(criteria.wake_credit_bound_permille)
        / 1000;
    assert!(
        credit <= bound,
        "唤醒信用越界: credit={credit} > bound={bound} (slice_virt={})",
        task.slice_virt
    );
}

/// 稳态 lag 上界: 三个等权任务长时间轮转, 每个在账任务的 `|lag|` 不超过判据.
#[test]
fn steady_state_lag_bounded_by_baseline_criteria() {
    let criteria = load_criteria();
    let mut model = Model::new();
    for pid in 1..=3u32 {
        assert!(model.add_task(pid, 0, 0));
        assert!(model.wake(0, pid));
    }

    let mut worst_permille = 0u64;
    for _ in 0..600 {
        model.tick(0);
        model.schedule(0);
        let rq = model.rq(0).expect("cpu 0 必须存在");
        let avg = i128::from(rq.avg_vruntime());
        for pid in 1..=3u32 {
            if model.location(pid).is_none() {
                continue;
            }
            let task = model.task(pid).expect("任务必须存在");
            let lag = avg - i128::from(task.vr);
            let ratio = lag.unsigned_abs() * 1000 / u128::from(task.slice_virt.max(1));
            let ratio = u64::try_from(ratio).unwrap_or(u64::MAX);
            worst_permille = worst_permille.max(ratio);
        }
    }
    assert!(
        worst_permille <= criteria.lag_bound_permille,
        "稳态 lag 越界: {worst_permille}‰ > {}‰",
        criteria.lag_bound_permille
    );
    model.audit();
}

/// 唤醒周转: 深度睡眠任务唤醒后须在判据给的 `schedule()` 次数内被拣出.
#[test]
fn wake_turnaround_within_baseline_criteria() {
    let criteria = load_criteria();
    let mut model = Model::new();
    for pid in 1..=2u32 {
        assert!(model.add_task(pid, 0, 0));
        assert!(model.wake(0, pid));
    }
    for _ in 0..300 {
        model.tick(0);
        model.schedule(0);
    }

    assert!(model.add_task(9, 0, 0));
    assert!(model.wake(0, 9));
    let mut schedules = 0u64;
    loop {
        schedules += 1;
        model.tick(0);
        if model.schedule(0) == Some(9) {
            break;
        }
        assert!(
            schedules < criteria.wake_turnaround_schedules,
            "唤醒周转超限: 已 {schedules} 次调度仍未拣出 pid 9"
        );
    }
    assert!(
        schedules <= criteria.wake_turnaround_schedules,
        "唤醒周转 {schedules} 次 > 判据 {}",
        criteria.wake_turnaround_schedules
    );
}

/// 公平性窗口: 等权任务在窗口内的 CPU 份额极差 (占均值千分比) 不超过判据.
#[test]
fn fairness_spread_within_baseline_criteria() {
    let criteria = load_criteria();
    let mut model = Model::new();
    for pid in 1..=3u32 {
        assert!(model.add_task(pid, 0, 0));
        assert!(model.wake(0, pid));
    }
    // 预热一个目标延迟, 跳过首轮建账切片不齐的瞬态.
    for _ in 0..TARGET_LATENCY {
        model.tick(0);
        model.schedule(0);
    }

    let mut counts = [0u64; 4];
    for _ in 0..criteria.fairness_window_ticks {
        let running = model
            .rq(0)
            .expect("cpu 0 必须存在")
            .running_pid()
            .expect("窗口内必须有运行任务");
        counts[idx(running)] += 1;
        model.tick(0);
        model.schedule(0);
    }

    let shares = &counts[1..=3];
    let max = shares.iter().copied().max().expect("非空窗口");
    let min = shares.iter().copied().min().expect("非空窗口");
    let total: u64 = shares.iter().sum();
    let average = (total / 3).max(1);
    let spread_permille = (max - min).saturating_mul(1000) / average;
    assert!(
        spread_permille <= criteria.fairness_spread_permille,
        "公平性份额差 {spread_permille}‰ > {}‰ (max={max} min={min} avg={average})",
        criteria.fairness_spread_permille
    );
    model.audit();
}

// ── 内核观测面: EevdfRunQueue::observation ─────────────────────────────────

/// 空队列快照: 零账目, 守恒成立, lag 归一为 0.
#[test]
fn eevdf_observation_empty_queue_is_conserved() {
    let queue = EevdfRunQueue::new();
    let snapshot = queue.observation();
    assert!(snapshot.conserve_ok, "空队列必须通过守恒自检");
    assert_eq!(snapshot.total_weight, 0);
    assert_eq!(snapshot.avg_vruntime, 0);
    assert_eq!(snapshot.tree_len, 0);
    assert_eq!(snapshot.running_len, 0);
    assert_eq!(snapshot.lag_min, 0);
    assert_eq!(snapshot.lag_max, 0);
}

/// 入队 → 拣出改变账目分解, 但 `W`/`S`/`V` 与守恒结论始终一致.
#[test]
fn eevdf_observation_tracks_tree_and_running_ledgers() {
    let mut queue = EevdfRunQueue::new();
    let base = 10 * VIRT_ONE;
    let placed_a = queue.enqueue(1, base, 1024, false);
    let placed_b = queue.enqueue(2, base.saturating_add(5 * VIRT_ONE), 1024, false);

    let tree_only = queue.observation();
    assert!(tree_only.conserve_ok);
    assert_eq!(tree_only.tree_len, 2);
    assert_eq!(tree_only.running_len, 0);
    assert_eq!(tree_only.total_weight, 2048);
    let expected_avg = (placed_a.saturating_add(placed_b)) / 2;
    assert_eq!(tree_only.avg_vruntime, expected_avg);
    assert!(tree_only.lag_min <= tree_only.lag_max);

    let pick = queue
        .pick_eevdf(None, |_| None)
        .expect("非空队列必须拣出任务");
    assert!(pick.from_tree);
    let after_pick = queue.observation();
    assert!(after_pick.conserve_ok, "拣出后账目必须仍守恒");
    assert_eq!(after_pick.tree_len, 1);
    assert_eq!(after_pick.running_len, 1);
    assert_eq!(after_pick.total_weight, tree_only.total_weight);
    assert_eq!(after_pick.avg_vruntime, tree_only.avg_vruntime);
}

/// 观测计数器: 唤醒 / 切片命中 / 等待时延样本的平均与峰值语义.
#[test]
fn eevdf_observation_counters_accumulate() {
    let mut queue = EevdfRunQueue::new();
    queue.note_wakeup();
    queue.note_wakeup();
    queue.note_slice_hit();
    queue.note_wait(1_000);
    queue.note_wait(3_000);

    let counters = queue.obs_counters();
    assert_eq!(
        counters,
        EevdfObsCounters {
            wakeups: 2,
            slice_hits: 1,
            wait_samples: 2,
            wait_ns_sum: 4_000,
            wait_ns_max: 3_000,
        }
    );
    assert_eq!(counters.wait_ns_avg(), 2_000);
    assert_eq!(queue.observation().counters, counters);
}

// ── 接线锁: syscall 464 全链路 ───────────────────────────────────────────

/// ABI 单源: 观测 syscall 编号 464 由 `edgine-abi` 定义, 内核出口同值.
#[test]
fn sched_snapshot_syscall_number_is_single_sourced() {
    assert_eq!(abi::SYS_SGEG_SCHED_SNAPSHOT, 464);
    assert_eq!(ktypes::SYS_SGEG_SCHED_SNAPSHOT, 464);
    assert_eq!(
        ktypes::SYS_SGEG_SCHED_SNAPSHOT,
        abi::SYS_SGEG_SCHED_SNAPSHOT,
        "内核出口必须复用 edgine-abi 单源"
    );
}

/// 源码接线锁: dispatch / functions / userlib / init / 门禁脚本均已接线.
#[test]
fn sched_snapshot_plumbing_is_wired_end_to_end() {
    let read = |path: &str| {
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读取 {path} 失败: {error}"))
    };

    let dispatch = read("../src/kernel/functions/syscall/dispatch.rs");
    assert!(
        dispatch.contains("SYS_SGEG_SCHED_SNAPSHOT =>"),
        "dispatch_proc_sched 缺 SYS_SGEG_SCHED_SNAPSHOT 分发臂"
    );
    assert!(
        dispatch.contains("sched_snapshot_syscall()"),
        "dispatch 未调用 sched_snapshot_syscall"
    );

    let functions = read("../src/kernel/functions/proc/sched.rs");
    assert!(
        functions.contains("pub fn sched_snapshot_syscall()"),
        "functions 层缺 sched_snapshot_syscall"
    );
    assert!(
        functions.contains("Scheduler::sched_snapshot_dump()"),
        "functions 层必须委托 privileged 观测导出"
    );

    let userlib = read("../src/user/lib/src/sys.rs");
    assert!(
        userlib.contains("pub fn sched_snapshot()"),
        "userlib 缺 sched_snapshot 包装"
    );
    assert!(
        userlib.contains("edgine_abi::sched::SYS_SGEG_SCHED_SNAPSHOT"),
        "userlib 编号必须取自 edgine-abi 单源"
    );

    let init = read("../src/user/init/src/main.rs");
    assert!(
        init.contains("sched_snapshot()"),
        "init 未在运行期触发观测导出"
    );

    for script in [
        "../scripts/qemu_boot_test.sh",
        "../scripts/thread_stress_test.sh",
        "../scripts/initramfs_boot_test.sh",
    ] {
        let text = read(script);
        assert!(text.contains("SNAP END"), "{script} 缺观测导出里程碑断言");
        assert!(text.contains("conserve=ok"), "{script} 缺账目守恒断言");
    }
}
