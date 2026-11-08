// SPDX-License-Identifier: MPL-2.0
//! E-03 EEVDF 队列机制对拍: 纯函数 / 树账+运行账 / 合格前缀选择 / 迁移守恒.
//!
//! 口径来源: `docs/plan/eevdf-scheduler.md` E-03 与 E-01 冻结的
//! `host-tests/src/eevdf_model.rs`. 本文件只测队列**行为**与参考模型的一致性;
//! 生产接线的源码形状由 `eevdf_single_time_base_test.rs` /
//! `smp_rq_migration_test.rs` 的既有契约覆盖.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

use edgine::kernel::privileged::config::{
    EEVDF_MIN_GRANULARITY, EEVDF_NICE0_WEIGHT, EEVDF_TARGET_LATENCY, EEVDF_VIRT_ONE,
};
use edgine::kernel::privileged::proc::eevdf::{
    EevdfRunQueue, EevdfTaskView, MIN_GRANULARITY_VIRT, QueuedTask, TARGET_LATENCY_VIRT, VIRT_ONE,
    calc_vruntime_delta_virt, clamp_requested_slice, default_slice_real, effective_slice_real,
    is_eligible, nice_to_weight, slice_virt_of, virtual_deadline, wake_vruntime,
    weighted_avg_vruntime,
};
use edgine::kernel::privileged::proc::process::Process;
use edgine_host_tests::eevdf_model::{
    MAX_TASKS, MIN_GRANULARITY, MIN_GRANULARITY_VIRT as MODEL_MIN_GRANULARITY_VIRT, Model,
    NICE0_WEIGHT, TARGET_LATENCY, TARGET_LATENCY_VIRT as MODEL_TARGET_LATENCY_VIRT,
    VIRT_ONE as MODEL_VIRT_ONE, clamp_requested_slice as model_clamp_requested_slice,
    default_slice_real as model_default_slice_real, delta_vruntime,
    is_eligible as model_is_eligible, slice_virt_of as model_slice_virt_of,
    weighted_avg_vruntime as model_weighted_avg_vruntime,
};

/// 测试用 pid 探针上限: 覆盖本文件全部用例 (最大 255).
const MAX_PID_PROBE: u32 = 300;

fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

fn random_nice(state: &mut u64) -> i8 {
    let index = u8::try_from(lcg_next(state) % 40).expect("nice 索引必在 u8 内");
    i8::try_from(i32::from(index) - 20).expect("nice 必在 -20..=19")
}

/// 从公开账目暴力重算, 逐项对账 (IE2: 每个在账任务恰计一次).
fn reconcile(queue: &EevdfRunQueue) {
    let mut tree_pids = BTreeSet::new();
    let mut tree_weight = 0u64;
    let mut tree_sum = 0u128;
    for &(vruntime, pid) in &queue.tree {
        assert!(tree_pids.insert(pid), "树内 pid {pid} 出现重复节点");
        let task = queue
            .tree_task(pid)
            .unwrap_or_else(|| panic!("树节点 pid {pid} 无账目"));
        assert!(queue.is_on_tree(pid));
        assert_eq!(task.vruntime, vruntime, "pid {pid} 树键与账目落点背离");
        tree_weight = tree_weight.saturating_add(task.weight);
        tree_sum = tree_sum.saturating_add(u128::from(task.weight) * u128::from(vruntime));
    }
    assert_eq!(queue.len(), tree_pids.len());
    assert_eq!(queue.tree_weight(), tree_weight);
    assert_eq!(queue.tree_sum_w_vr(), tree_sum);

    let mut running_weight = 0u64;
    let mut running_sum = 0u128;
    let mut running_count = 0usize;
    for pid in 0..MAX_PID_PROBE {
        let Some(task) = queue.running_task(pid) else {
            continue;
        };
        assert!(queue.is_running(pid));
        assert!(!tree_pids.contains(&pid), "pid {pid} 同时挂在树账与运行账");
        assert_eq!(task.pid, pid);
        running_weight = running_weight.saturating_add(task.weight);
        running_sum =
            running_sum.saturating_add(u128::from(task.weight) * u128::from(task.vruntime));
        running_count += 1;
    }
    assert_eq!(queue.running_count(), running_count);
    assert_eq!(queue.running_weight(), running_weight);
    assert_eq!(queue.running_sum_w_vr(), running_sum);

    let total_weight = tree_weight.saturating_add(running_weight);
    let total_sum = tree_sum.saturating_add(running_sum);
    assert_eq!(queue.runnable_weight(), total_weight);
    assert_eq!(queue.sum_w_vr(), total_sum);
    assert_eq!(queue.total_weight.load(Ordering::Acquire), total_weight);
    assert_eq!(
        queue.avg_vruntime(),
        weighted_avg_vruntime(total_weight, total_sum)
    );
}

/// 纯函数逐项对拍 E-01 冻结模型 (含 40 档 nice 与随机边界).
#[test]
fn pure_functions_match_frozen_reference_model() {
    assert_eq!(VIRT_ONE, MODEL_VIRT_ONE);
    assert_eq!(VIRT_ONE, EEVDF_VIRT_ONE);
    assert_eq!(VIRT_ONE, 1 << 16);
    assert_eq!(TARGET_LATENCY_VIRT, MODEL_TARGET_LATENCY_VIRT);
    assert_eq!(MIN_GRANULARITY_VIRT, MODEL_MIN_GRANULARITY_VIRT);
    assert_eq!(TARGET_LATENCY, EEVDF_TARGET_LATENCY);
    assert_eq!(MIN_GRANULARITY, EEVDF_MIN_GRANULARITY);
    assert_eq!(NICE0_WEIGHT, EEVDF_NICE0_WEIGHT);

    let mut state = 0xdead_beef_cafe_1234u64;
    for nice in -20..=19 {
        let weight = nice_to_weight(nice);
        assert_eq!(
            calc_vruntime_delta_virt(weight),
            delta_vruntime(weight),
            "nice {nice} 的 Q16.16 Δvr 与模型背离"
        );
        for _ in 0..20 {
            let weight_incl = if lcg_next(&mut state).trailing_zeros() >= 3 {
                0
            } else {
                weight.saturating_add(lcg_next(&mut state) % 100_000)
            };
            assert_eq!(
                default_slice_real(weight, weight_incl),
                model_default_slice_real(weight, weight_incl)
            );
            let requested = lcg_next(&mut state) % 200;
            assert_eq!(
                clamp_requested_slice(requested),
                model_clamp_requested_slice(requested)
            );
            let slice_real = clamp_requested_slice(requested);
            let slice_virt = slice_virt_of(slice_real, weight);
            assert_eq!(slice_virt, model_slice_virt_of(slice_real, weight));
            let vruntime = lcg_next(&mut state) % 1_000_000;
            assert_eq!(
                virtual_deadline(vruntime, slice_virt),
                vruntime.saturating_add(slice_virt)
            );
            let avg = lcg_next(&mut state) % 1_000_000;
            assert_eq!(
                wake_vruntime(vruntime, avg, slice_virt),
                vruntime.max(avg.saturating_sub(slice_virt))
            );
            let total_weight = weight.saturating_add(lcg_next(&mut state) % 100_000);
            let sum_w_vr = u128::from(lcg_next(&mut state) % 1_000_000) * u128::from(total_weight);
            assert_eq!(
                weighted_avg_vruntime(total_weight, sum_w_vr),
                model_weighted_avg_vruntime(total_weight, sum_w_vr)
            );
            assert_eq!(
                is_eligible(vruntime, total_weight, sum_w_vr),
                model_is_eligible(vruntime, total_weight, sum_w_vr)
            );
        }
        let weight_incl = weight.saturating_add(1024);
        assert_eq!(
            effective_slice_real(weight, weight_incl, None),
            model_default_slice_real(weight, weight_incl)
        );
        assert_eq!(
            effective_slice_real(weight, weight_incl, Some(1)),
            MIN_GRANULARITY
        );
        assert_eq!(
            effective_slice_real(weight, weight_incl, Some(1000)),
            TARGET_LATENCY
        );
    }
    assert_eq!(calc_vruntime_delta_virt(0), delta_vruntime(0));
    assert_eq!(calc_vruntime_delta_virt(u64::MAX), delta_vruntime(u64::MAX));
}

/// 随机操作序列下账目必须与暴力重算逐项一致 (权重守恒 / 无重复节点 / 两账互斥).
#[test]
fn queue_ledgers_match_bruteforce_under_random_operations() {
    let mut queue = EevdfRunQueue::new();
    let mut state = 0x5eed_1234_abcd_9876u64;
    for _ in 0..2000 {
        let pid = u32::try_from(lcg_next(&mut state) % 24).expect("pid 必在 u32 内");
        let weight = nice_to_weight(random_nice(&mut state));
        let vruntime = lcg_next(&mut state) % 100_000;
        let accounted = lcg_next(&mut state) & 1 == 0;
        match lcg_next(&mut state) % 6 {
            0 | 1 => {
                queue.enqueue(pid, vruntime, weight, accounted);
            }
            2 => {
                queue.dequeue(pid, lcg_next(&mut state) % 200_000, accounted);
            }
            3 => {
                let _ = queue.pick_eevdf(None, |_| None);
            }
            4 => {
                let _ = queue.reweight(pid, weight);
            }
            _ => {
                let base = queue.running_task(pid).map_or(0, |task| task.vruntime);
                let next = base.saturating_add(lcg_next(&mut state) % 500);
                let advanced = queue.advance_running_vruntime(pid, next);
                assert_eq!(advanced, queue.is_running(pid));
            }
        }
        queue.advance_floor(lcg_next(&mut state) % 100_000);
        reconcile(&queue);
    }
}

fn build_model_case(state: &mut u64, tasks: u32) -> (Model, EevdfRunQueue, BTreeMap<u32, u64>) {
    let mut model = Model::new();
    let mut queue = EevdfRunQueue::new();
    let mut deadlines = BTreeMap::new();
    for index in 0..tasks {
        let pid = index + 1;
        let nice = random_nice(state);
        let raw_vr = lcg_next(state) % 10_000;
        let requested = 8 + lcg_next(state) % 53;
        assert!(model.add_task(pid, nice, raw_vr));
        assert!(model.set_requested_slice(pid, Some(requested)));
        let _ = model.wake(0, pid);
        let task = *model.task(pid).expect("已登记任务");
        queue.enqueue(pid, task.vr, task.weight, false);
        deadlines.insert(pid, task.vd);
    }
    (model, queue, deadlines)
}

/// 无 current 时, `pick_eevdf` 的合格前缀与 `(vd, pid)` 选择必须与模型逐例一致.
#[test]
fn selection_matches_reference_model_without_current() {
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..500 {
        let tasks = 1 + u32::try_from(lcg_next(&mut state) % 12).expect("任务数必在 u32 内");
        let (model, mut queue, deadlines) = build_model_case(&mut state, tasks);
        let expected = model.pick_winner(0).expect("非空可运行集必有胜者");
        let pick = queue
            .pick_eevdf(None, |pid| deadlines.get(&pid).copied())
            .expect("非空可运行集必有胜者");
        assert_eq!(pick.pid, expected, "选择结果与参考模型背离");
        assert!(pick.from_tree);
        let task = *model.task(pick.pid).expect("胜者必已登记");
        assert_eq!(pick.vruntime, task.vr);
        assert_eq!(pick.vdeadline, task.vd);
    }
}

fn build_current_case(
    state: &mut u64,
    tasks: u32,
) -> (Model, EevdfRunQueue, BTreeMap<u32, u64>, EevdfTaskView) {
    const CURRENT_PID: u32 = 100;
    let mut model = Model::new();
    let mut queue = EevdfRunQueue::new();
    let mut deadlines = BTreeMap::new();

    let raw_vr = lcg_next(state) % 10_000;
    let requested = 8 + lcg_next(state) % 53;
    assert!(model.add_task(CURRENT_PID, random_nice(state), raw_vr));
    assert!(model.set_requested_slice(CURRENT_PID, Some(requested)));
    let _ = model.wake(0, CURRENT_PID);
    let current_task = *model.task(CURRENT_PID).expect("current 必已登记");
    assert_eq!(model.schedule(0), Some(CURRENT_PID));
    queue.enqueue(CURRENT_PID, current_task.vr, current_task.weight, false);
    let pick = queue
        .pick_eevdf(None, |_| None)
        .expect("current 入树后必被拣出");
    assert_eq!((pick.pid, pick.vruntime), (CURRENT_PID, current_task.vr));
    deadlines.insert(CURRENT_PID, current_task.vd);

    for index in 0..tasks {
        let pid = index + 1;
        let raw_vr = lcg_next(state) % 10_000;
        let requested = 8 + lcg_next(state) % 53;
        assert!(model.add_task(pid, random_nice(state), raw_vr));
        assert!(model.set_requested_slice(pid, Some(requested)));
        let _ = model.wake(0, pid);
        let task = *model.task(pid).expect("已登记任务");
        queue.enqueue(pid, task.vr, task.weight, false);
        deadlines.insert(pid, task.vd);
    }

    let current = *model.task(CURRENT_PID).expect("current 必已登记");
    let view = EevdfTaskView {
        pid: CURRENT_PID,
        vruntime: current.vr,
        vdeadline: current.vd,
        weight: current.weight,
    };
    (model, queue, deadlines, view)
}

/// current-aware 选择: 合格 current 与树候选同台 `(vd, pid)` 比较.
#[test]
fn selection_matches_reference_model_with_current() {
    let mut state = 0x0fed_cba9_8765_4321u64;
    for _ in 0..300 {
        let tasks = 1 + u32::try_from(lcg_next(&mut state) % 5).expect("任务数必在 u32 内");
        let (model, mut queue, deadlines, current) = build_current_case(&mut state, tasks);
        let expected = model.pick_winner(0).expect("非空可运行集必有胜者");
        let pick = queue
            .pick_eevdf(Some(current), |pid| deadlines.get(&pid).copied())
            .expect("非空可运行集必有胜者");
        assert_eq!(pick.pid, expected, "current-aware 选择与参考模型背离");
        assert_eq!(pick.from_tree, pick.pid != current.pid);
    }
}

/// 跨核迁移: 源撤账 + 目的记账, `W`/`S` 两核之和守恒且落点不变 (IE5).
#[test]
fn migration_conserves_weight_and_weighted_vruntime() {
    let mut source = EevdfRunQueue::new();
    let mut destination = EevdfRunQueue::new();
    let mut state = 0xabcd_1234_5678_9ef0u64;
    let mut moved = Vec::new();
    for index in 0..32u32 {
        let pid = index + 1;
        let weight = nice_to_weight(random_nice(&mut state));
        let vruntime = lcg_next(&mut state) % 50_000;
        source.enqueue(pid, vruntime, weight, false);
        if lcg_next(&mut state) & 1 == 0 {
            moved.push((pid, vruntime, weight));
        }
    }
    let total_weight = source.runnable_weight();
    let total_sum = source.sum_w_vr();
    for &(pid, vruntime, weight) in &moved {
        assert!(source.dequeue(pid, weight, true));
        let placed = destination.enqueue(pid, vruntime, weight, false);
        assert_eq!(placed, vruntime, "迁移不得改写落点 (IE5)");
        assert_eq!(
            destination.tree_task(pid),
            Some(QueuedTask {
                pid,
                vruntime,
                weight
            })
        );
    }
    assert_eq!(
        source
            .runnable_weight()
            .saturating_add(destination.runnable_weight()),
        total_weight
    );
    assert_eq!(
        source.sum_w_vr().saturating_add(destination.sum_w_vr()),
        total_sum
    );
    reconcile(&source);
    reconcile(&destination);
}

/// 255 任务边界: 树 -> 运行账的移动不改变 W/S, 全部撤账后归零.
#[test]
fn max_tasks_boundary_preserves_ledgers() {
    let mut queue = EevdfRunQueue::new();
    let count = u32::try_from(MAX_TASKS).expect("MAX_TASKS 必在 u32 内");
    let mut expected_weight = 0u64;
    for index in 0..count {
        let pid = index + 1;
        let nice_index = u8::try_from(index % 40).expect("nice 索引必在 u8 内");
        let nice = i8::try_from(i32::from(nice_index) - 20).expect("nice 必在 -20..=19");
        let weight = nice_to_weight(nice);
        expected_weight = expected_weight.saturating_add(weight);
        queue.enqueue(pid, u64::from(index) * 1024, weight, false);
    }
    assert_eq!(u32::try_from(queue.len()).expect("树深必在 u32 内"), count);
    assert_eq!(queue.runnable_weight(), expected_weight);
    reconcile(&queue);

    let mut picked = BTreeSet::new();
    for _ in 0..count {
        let pid = queue.pick_eevdf(None, |_| None).expect("仍有树内任务").pid;
        assert!(picked.insert(pid), "拣出 pid {pid} 重复");
    }
    assert!(queue.tree.is_empty());
    assert_eq!(
        u32::try_from(queue.running_count()).expect("运行账必在 u32 内"),
        count
    );
    assert_eq!(queue.runnable_weight(), expected_weight, "树->运行不得改 W");
    reconcile(&queue);

    for pid in picked {
        assert!(!queue.dequeue(pid, 0, true), "运行账成员出树 -> false");
    }
    assert_eq!(queue.runnable_weight(), 0);
    assert_eq!(queue.total_weight.load(Ordering::Acquire), 0);
    reconcile(&queue);
}

/// 陈旧凭证不得让派生权重漂移: 扣减量取结构记账, 重复出队幂等.
#[test]
fn stale_credentials_cannot_drift_derived_weight() {
    let mut queue = EevdfRunQueue::new();
    queue.enqueue(5, 100, 1024, false);
    queue.enqueue(5, 200, 2048, true);
    assert_eq!(queue.runnable_weight(), 2048, "重复入队不得叠加权重");
    assert!(queue.dequeue(5, u64::MAX, true));
    assert_eq!(queue.runnable_weight(), 0, "扣减量必须取结构记账");
    assert_eq!(queue.total_weight.load(Ordering::Acquire), 0);
    assert!(
        !queue.dequeue(5, u64::MAX, true),
        "陈旧凭证重复出队必须 no-op"
    );
    assert_eq!(queue.total_weight.load(Ordering::Acquire), 0);
}

/// `Process::new` 的 EEVDF 字段必须是"未建账"初态, 不得继承任何陈旧切片.
#[test]
fn process_new_has_unbuilt_eevdf_state() {
    let process = Process::new(0xEE03, "eevdf-queue-test", None);
    assert_eq!(process.eevdf_vdeadline.load(Ordering::Acquire), 0);
    assert_eq!(process.eevdf_slice_real.load(Ordering::Acquire), 0);
    assert_eq!(process.eevdf_slice_virt.load(Ordering::Acquire), 0);
    assert_eq!(process.eevdf_slice_left.load(Ordering::Acquire), 0);
    assert_eq!(process.eevdf_requested_slice.load(Ordering::Acquire), 0);
    assert_eq!(
        process.eevdf_weight.load(Ordering::Acquire),
        EEVDF_NICE0_WEIGHT
    );
    assert_eq!(process.eevdf_vruntime.load(Ordering::Acquire), 0);
    assert!(!process.eevdf_on_rq.load(Ordering::Acquire));
}
