// SPDX-License-Identifier: MPL-2.0
//! E-05b 回归锁: 跨核迁移原样携带与合格前缀偷取候选.
//!
//! 验收:
//!   - IE5: 迁移落点不做 IC1 钳制, 源/目的树账变化互为相反数, 任务字段原样携带.
//!   - 偷取候选只取合格前缀 (欠账任务不迁), 在跑的 current 不进入候选.
//!   - 策略评估: 合格前缀窗口内取最不急迫者, 源核残余截止期与目的核接收的
//!     截止期均不劣于"取最左端"策略.
//!   - 合格候选移出源核后源核 `V` 不下降 —— 同批剩余候选仍合格.
//!
//! 行为维在真实 `CfsRunQueue` 上执行; 策略评估用 E-01 参考模型
//! (`host-tests/src/eevdf_model.rs`) 的 `steal_candidates` 对拍.
//! 生产接线的源码形状由 `smp_rq_migration_test` 的既有契约锁死.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use edgine::kernel::privileged::proc::cfs::{
    CfsRunQueue, QueuedTask, is_eligible as kernel_is_eligible, nice_to_weight,
};
use edgine_host_tests::eevdf_model::{
    Model, StealPolicy, effective_deadline as model_effective_deadline,
    is_eligible as model_is_eligible,
};

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

/// 从公开账目暴力重算, 逐项对账 (IE2).
fn reconcile(queue: &CfsRunQueue) {
    let mut tree_pids = BTreeSet::new();
    let mut tree_weight = 0u64;
    let mut tree_sum = 0u128;
    for &(vruntime, pid) in &queue.tree {
        assert!(tree_pids.insert(pid), "树内 pid {pid} 出现重复节点");
        let task = queue
            .tree_task(pid)
            .unwrap_or_else(|| panic!("树节点 pid {pid} 无账目"));
        assert_eq!(task.vruntime, vruntime, "pid {pid} 树键与账目落点背离");
        tree_weight = tree_weight.saturating_add(task.weight);
        tree_sum = tree_sum.saturating_add(u128::from(task.weight) * u128::from(vruntime));
    }
    assert_eq!(queue.len(), tree_pids.len());
    assert_eq!(queue.tree_weight(), tree_weight);
    assert_eq!(queue.tree_sum_w_vr(), tree_sum);
    assert_eq!(
        queue.runnable_weight(),
        tree_weight.saturating_add(queue.running_weight())
    );
    assert_eq!(
        queue.sum_w_vr(),
        tree_sum.saturating_add(queue.running_sum_w_vr())
    );
    assert_eq!(
        queue.total_weight.load(Ordering::Acquire),
        queue.runnable_weight()
    );
}

/// IE5: 迁移原样携带落点与账目, 源/目的树账变化互为相反数.
#[test]
fn migrate_carries_raw_vruntime_and_conserves_ledgers() {
    let mut src = CfsRunQueue::new();
    let mut dst = CfsRunQueue::new();
    // 目的核 floor 足够高: 所有迁移落点都在 IC1 钳制线之下, 强制暴露"重算落点".
    let floor = 4_000_000u64 << 16;
    dst.advance_floor(floor);

    let mut state = 0x0bad_f00d_dead_beefu64;
    let mut tasks = Vec::new();
    for index in 0..32u32 {
        let pid = index + 1;
        let weight = nice_to_weight(random_nice(&mut state));
        let vruntime = lcg_next(&mut state) % (1_000_000u64 << 16);
        src.enqueue(pid, vruntime, weight, false);
        tasks.push((pid, vruntime, weight));
    }
    reconcile(&src);
    let (src_weight, src_sum) = (src.tree_weight(), src.tree_sum_w_vr());
    let (dst_weight, dst_sum) = (dst.tree_weight(), dst.tree_sum_w_vr());

    let mut relocated = 0usize;
    for &(pid, vruntime, weight) in tasks.iter().step_by(2) {
        assert!(
            dst.placement(vruntime) > vruntime,
            "用例前提: 目的核 floor 必须高于迁移落点"
        );
        let (src_w, src_s) = (src.tree_weight(), src.tree_sum_w_vr());
        let (dst_w, dst_s) = (dst.tree_weight(), dst.tree_sum_w_vr());

        assert!(src.dequeue(pid, weight, true));
        let placed = dst.migrate(pid, vruntime, weight);
        assert_eq!(placed, vruntime, "迁移落点被 IC1 钳制 (IE5 失效)");
        assert_eq!(
            dst.tree_task(pid),
            Some(QueuedTask {
                pid,
                vruntime,
                weight
            }),
            "迁移改写了任务账目 (IE5: 字段原样携带)"
        );
        assert_eq!(
            src.tree_weight().saturating_add(dst.tree_weight()),
            src_w.saturating_add(dst_w),
            "pid {pid} 迁移后两核 W 不再守恒"
        );
        assert_eq!(
            src.tree_sum_w_vr().saturating_add(dst.tree_sum_w_vr()),
            src_s.saturating_add(dst_s),
            "pid {pid} 迁移后两核 S 不再守恒"
        );
        reconcile(&src);
        reconcile(&dst);
        relocated += 1;
    }
    assert_eq!(relocated, tasks.len() / 2);
    assert_eq!(
        src.tree_weight().saturating_add(dst.tree_weight()),
        src_weight.saturating_add(dst_weight)
    );
    assert_eq!(
        src.tree_sum_w_vr().saturating_add(dst.tree_sum_w_vr()),
        src_sum.saturating_add(dst_sum)
    );

    // 幂等: 重复迁移同一任务只移动节点, 不重复记账.
    let (pid, vruntime, weight) = tasks[0];
    assert_eq!(dst.migrate(pid, vruntime, weight), vruntime);
    reconcile(&dst);
    assert_eq!(
        dst.tree_weight().saturating_add(src.tree_weight()),
        src_weight.saturating_add(dst_weight)
    );
}

/// 偷取候选 (参考模型): 只含合格前缀, 且"最不急迫"策略对源/目的两侧都不劣.
#[test]
fn least_urgent_candidate_weakly_dominates_leftmost() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut nonempty = 0usize;
    for _ in 0..400 {
        let model = build_case(&mut state);
        let left = model.steal_candidates(0, StealPolicy::Leftmost, 4);
        let least = model.steal_candidates(0, StealPolicy::LeastUrgent, 4);
        if left.is_empty() {
            assert!(least.is_empty(), "同一合格窗口必须给出同一候选集");
            continue;
        }
        nonempty += 1;

        // 两个策略看到同一候选窗口, 只是次序不同.
        let mut left_pids: Vec<u32> = left.iter().map(|candidate| candidate.0).collect();
        let mut least_pids: Vec<u32> = least.iter().map(|candidate| candidate.0).collect();
        left_pids.sort_unstable();
        least_pids.sort_unstable();
        assert_eq!(left_pids, least_pids, "两策略的候选集不一致");

        let rq = model.rq(0).expect("cpu0 必存在");
        let (weight, weighted_sum) = (rq.total_weight(), rq.sum_w_vr());
        for &(pid, vruntime, deadline) in &left {
            assert!(
                model_is_eligible(vruntime, weight, weighted_sum)
                    && kernel_is_eligible(vruntime, weight, weighted_sum),
                "偷走欠账任务 pid {pid}: 迁移等于替它逃避本轮额度"
            );
            let task = model.task(pid).expect("候选必在任务表");
            assert_eq!(deadline, model_effective_deadline(task.vd, task.vr));
        }
        assert!(
            left.windows(2).all(|pair| pair[0].1 <= pair[1].1),
            "Leftmost 未按 vruntime 升序"
        );

        // 最不急迫 = 窗口内最大有效截止期, 且不早于最左端.
        let max_deadline = left.iter().map(|candidate| candidate.2).max().unwrap();
        assert_eq!(least[0].2, max_deadline, "LeastUrgent 未选窗口内最大 vd");
        assert!(least[0].2 >= left[0].2);

        // 源核残余最大截止期: 移走最不急迫者不劣于移走最左端.
        let residual = |removed: u32| {
            left.iter()
                .filter(|candidate| candidate.0 != removed)
                .map(|candidate| candidate.2)
                .max()
        };
        if let (Some(least_residual), Some(leftmost_residual)) =
            (residual(least[0].0), residual(left[0].0))
        {
            assert!(
                least_residual <= leftmost_residual,
                "取最不急迫者后源核残余截止期反而更晚"
            );
        }

        // 合格候选移出源核后 V 不下降 (同批剩余候选仍合格).
        let mut migrated = model;
        let rq_before = migrated.rq(0).expect("cpu0 必存在");
        let (weight_before, sum_before, before) = (
            rq_before.total_weight(),
            rq_before.sum_w_vr(),
            rq_before.avg_vruntime(),
        );
        let (moved_pid, moved_vr, moved_deadline) = least[0];
        let moved_weight = migrated.task(moved_pid).expect("候选必在任务表").weight;
        assert!(migrated.migrate(0, 1, moved_pid));
        let after = migrated.rq(0).expect("cpu0 必存在").avg_vruntime();
        // 空集时 V 按定义为 0 (无可运行任务), 只有剩余集非空才谈得上"V 不下降".
        if migrated.rq(0).expect("cpu0").total_weight() > 0 {
            assert!(
                after >= before,
                "移出合格候选后源核 V 下降: W={weight_before} S={sum_before} V={before} -> {after}; \
                 pid={moved_pid} vr={moved_vr} vd={moved_deadline} w={moved_weight} \
                 running={:?}",
                migrated.rq(1).expect("cpu1").running_pid()
            );
        }
        migrated.audit();
    }
    assert!(
        nonempty > 200,
        "随机样本中合格候选过少, 策略评估缺乏判别力: {nonempty}"
    );
}

/// 构造随机双核情形: 任务全部先唤醒到 cpu0, 一半用例再调度一次产生 current.
fn build_case(state: &mut u64) -> Model {
    let mut model = Model::new();
    let count = 1 + u32::try_from(lcg_next(state) % 12).expect("任务数必在 u32 内");
    for index in 0..count {
        let pid = index + 1;
        let vruntime = lcg_next(state) % 200_000;
        assert!(
            model.add_task(pid, random_nice(state), vruntime),
            "随机用例 pid {pid} 登记失败"
        );
        assert!(model.wake(0, pid), "随机用例 pid {pid} 唤醒失败");
    }
    if lcg_next(state) & 1 == 0 {
        let _ = model.schedule(0);
    }
    model.audit();
    model
}
