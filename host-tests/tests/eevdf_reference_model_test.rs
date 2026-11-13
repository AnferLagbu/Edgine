// SPDX-License-Identifier: MPL-2.0
//! E-01 EEVDF 参考模型对拍: Q16.16 口径 + 账目对账 + 三段入队语义.
//!
//! 追踪: `docs/plan/archive/eevdf-scheduler.md` E-01 (§3.3 公式 / §3.8 开放议题).
//!
//! 验收点:
//! - 常量与内核单源一致; nice -20..19 全 40 档 `Δvr` 随权重严格递减 (不再全为 1)
//! - 切片上下限、请求切片夹取、`slice_virt = Δvr * slice_real` 精确对齐
//! - `W/S/V`、合格判定与暴力重算一致; 非空可运行集必有合格任务
//! - IE3 睡眠信用有界; 负 lag 只来源于欠账
//! - IE5 迁移守恒 (字段不变, 两核树账互为相反数)
//! - IE6 Wake/Migrate/Requeue 三分语义; 切片到期恰好在 `vr` 推进 `slice_virt` 时
//! - 255 任务边界、全体欠账 + 连续唤醒下的服务推进、随机操作序列对账

use edgine::kernel::privileged::config::{
    EEVDF_MIN_GRANULARITY, EEVDF_NICE0_WEIGHT, EEVDF_TARGET_LATENCY,
};
use edgine::kernel::privileged::proc::eevdf::NICE_TO_WEIGHT as KERNEL_NICE_TO_WEIGHT;
use edgine_host_tests::eevdf_model::{
    Location, MAX_TASKS, MIN_GRANULARITY, MIN_GRANULARITY_VIRT, Model, NICE_TO_WEIGHT,
    NICE0_WEIGHT, TARGET_LATENCY, TARGET_LATENCY_VIRT, VIRT_ONE, clamp_requested_slice,
    default_slice_real, delta_vruntime, is_eligible, nice_to_weight, slice_virt_of,
    weighted_avg_vruntime,
};

fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

fn idx(pid: u32) -> usize {
    usize::try_from(pid).expect("测试 pid 必须能放入 usize")
}

/// 常量与内核单源一致; Q16.16 换算口径固定.
#[test]
fn frozen_constants_match_kernel_single_source() {
    assert_eq!(VIRT_ONE, 1 << 16);
    assert_eq!(TARGET_LATENCY_VIRT, TARGET_LATENCY << 16);
    assert_eq!(MIN_GRANULARITY_VIRT, MIN_GRANULARITY << 16);
    assert_eq!(TARGET_LATENCY, EEVDF_TARGET_LATENCY);
    assert_eq!(MIN_GRANULARITY, EEVDF_MIN_GRANULARITY);
    assert_eq!(NICE0_WEIGHT, EEVDF_NICE0_WEIGHT);
    assert_eq!(NICE_TO_WEIGHT, KERNEL_NICE_TO_WEIGHT);
    assert_eq!(NICE_TO_WEIGHT[0], 88_761);
    assert_eq!(NICE_TO_WEIGHT[20], 1_024);
    assert_eq!(NICE_TO_WEIGHT[39], 15);
    assert_eq!(MAX_TASKS, 255);
}

/// Q16.16 专项: 40 档 `Δvr` 随权重严格递减, 且不再退化为 1.
#[test]
fn delta_vruntime_strictly_decreasing_over_40_nice_levels() {
    let mut previous = u64::MAX;
    for nice in (-20..=19).rev() {
        let weight = nice_to_weight(nice);
        let delta = delta_vruntime(weight);
        assert_eq!(
            delta,
            (NICE0_WEIGHT << 16) / weight,
            "nice {nice} 的 Δvr 必须按 floor((NICE0<<16)/w) 计算"
        );
        assert!(delta > 1, "nice {nice} 的 Δvr 退化为 1");
        assert!(
            delta < previous,
            "nice {nice} 的 Δvr 未随权重增加而严格递减"
        );
        previous = delta;
    }
    assert_eq!(delta_vruntime(nice_to_weight(0)), VIRT_ONE);
    assert_eq!(delta_vruntime(nice_to_weight(-20)), 756);
    assert_eq!(delta_vruntime(nice_to_weight(19)), 4_473_924);
    assert_eq!(delta_vruntime(0), VIRT_ONE, "w=0 按 NICE0 退化");
    assert_eq!(delta_vruntime(u64::MAX), 1, "商为 0 时下限为 1");
}

/// 默认真实切片始终落在 `[MIN_GRANULARITY, TARGET_LATENCY]`; 请求切片统一夹取.
#[test]
fn slice_bounds_and_requested_clamp_table() {
    for nice in -20..=19 {
        let weight = nice_to_weight(nice);
        let solo = default_slice_real(weight, weight);
        assert_eq!(
            solo, TARGET_LATENCY,
            "单任务应获得完整目标延迟 (nice {nice})"
        );
        for others in [0u64, 1, 7, 63, 254] {
            let weight_incl = weight.saturating_add(NICE0_WEIGHT.saturating_mul(others));
            let real = default_slice_real(weight, weight_incl);
            assert!(
                (MIN_GRANULARITY..=TARGET_LATENCY).contains(&real),
                "nice {nice} / 其他 {others} 的切片越界: {real}"
            );
            assert_eq!(
                slice_virt_of(real, weight),
                delta_vruntime(weight) * real,
                "slice_virt 必须 = Δvr * slice_real"
            );
        }
    }
    assert_eq!(default_slice_real(0, NICE0_WEIGHT), MIN_GRANULARITY);
    assert_eq!(default_slice_real(NICE0_WEIGHT, 0), MIN_GRANULARITY);
    assert_eq!(
        default_slice_real(88_761, NICE0_WEIGHT),
        TARGET_LATENCY,
        "非法 W_inc < w 由上限夹取"
    );

    assert_eq!(clamp_requested_slice(0), MIN_GRANULARITY);
    assert_eq!(clamp_requested_slice(1), MIN_GRANULARITY);
    assert_eq!(clamp_requested_slice(7), MIN_GRANULARITY);
    assert_eq!(clamp_requested_slice(8), MIN_GRANULARITY);
    assert_eq!(clamp_requested_slice(9), 9);
    assert_eq!(clamp_requested_slice(59), 59);
    assert_eq!(clamp_requested_slice(60), TARGET_LATENCY);
    assert_eq!(clamp_requested_slice(61), TARGET_LATENCY);
    assert_eq!(clamp_requested_slice(100), TARGET_LATENCY);
    assert_eq!(clamp_requested_slice(u64::MAX), TARGET_LATENCY);
}

/// IE3: Wake 信用不超过一个 `slice_virt`; 负 lag 只来自 `vr_raw > V_before`.
#[test]
fn wake_credit_bounded_and_negative_lag_only_for_debt() {
    let mut model = Model::new();
    for (pid, nice, vr) in [
        (1u32, -20i8, 50_000u64),
        (2, -10, 120_000),
        (3, 0, 200_000),
        (4, 10, 300_000),
    ] {
        assert!(model.add_task(pid, nice, vr));
        assert!(model.wake(0, pid));
    }
    assert!(model.add_task(100, 0, 0));
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut positive = 0usize;
    let mut negative = 0usize;
    for _ in 0..2_000 {
        let raw = lcg_next(&mut state) % 5_000_000;
        assert!(model.set_raw_vruntime(100, raw));
        let rq = model.rq(0).expect("cpu0 必须存在");
        let weight_before = rq.total_weight();
        let sum_before = rq.sum_w_vr();
        let avg_before = weighted_avg_vruntime(weight_before, sum_before);
        let slice_real = default_slice_real(NICE0_WEIGHT, weight_before + NICE0_WEIGHT);
        let slice_virt = slice_virt_of(slice_real, NICE0_WEIGHT);
        let placed = raw.max(avg_before.saturating_sub(slice_virt));
        assert!(model.wake(0, 100));
        let task = model.task(100).expect("任务必须存在");
        assert_eq!(task.vr, placed);
        assert_eq!(task.slice_real, slice_real);
        assert_eq!(task.slice_left, slice_real);
        assert_eq!(task.vd, placed + slice_virt);
        let lag = i128::from(avg_before) - i128::from(placed);
        assert!(
            lag <= i128::from(slice_virt),
            "睡眠信用超过一个 slice_virt: lag={lag} virt={slice_virt}"
        );
        if raw <= avg_before {
            assert!(lag >= 0, "raw={raw} <= V={avg_before} 时不得为欠账");
            positive += 1;
        } else {
            assert!(lag < 0, "raw={raw} > V={avg_before} 时必须保留欠账");
            negative += 1;
        }
        model.audit();
        assert!(model.block(100));
        model.audit();
    }
    assert!(
        positive > 0 && negative > 0,
        "样本必须同时覆盖信用与欠账两侧"
    );
}

/// IE6: 切片到期恰好发生在 `vr` 推进一个 `slice_virt` 时, 并请求重调度.
#[test]
fn slice_expiry_is_exact_and_requests_resched() {
    for (nice, expected_virt) in [(-20i8, 45_360u64), (0, 3_932_160), (19, 268_435_440)] {
        let mut model = Model::new();
        assert!(model.add_task(1, nice, 0));
        assert!(model.wake(0, 1));
        assert_eq!(model.schedule(0), Some(1));
        assert!(!model.needs_resched(0));
        let task = model.task(1).expect("任务必须存在");
        assert_eq!(task.slice_real, TARGET_LATENCY, "单任务切片 = 目标延迟");
        assert_eq!(task.slice_virt, expected_virt);
        assert_eq!(task.vd, expected_virt);
        let delta = delta_vruntime(task.weight);
        for tick in 1..=TARGET_LATENCY {
            let outcome = model.tick(0).expect("调度中的 cpu0 必须有 current");
            assert_eq!(outcome.delta, delta);
            assert_eq!(outcome.expired, tick == TARGET_LATENCY);
            model.audit();
            if tick < TARGET_LATENCY {
                assert!(!model.needs_resched(0));
                assert_eq!(model.rq(0).expect("cpu0").running_pid(), Some(1));
            }
        }
        assert!(model.needs_resched(0));
        let rq = model.rq(0).expect("cpu0");
        assert_eq!(rq.running_pid(), None, "到期后先按 Requeue 语义重投树");
        assert!(rq.contains_pid(1));
        let task = model.task(1).expect("任务必须存在");
        assert_eq!(task.vr, expected_virt, "到期时 vr 必须恰好推进 slice_virt");
        assert_eq!(task.slice_left, TARGET_LATENCY);
        assert_eq!(task.vd, expected_virt + expected_virt);
        assert_eq!(model.floor(0), Some(expected_virt));
        assert_eq!(model.schedule(0), Some(1));
        assert!(!model.needs_resched(0));
        model.audit();
    }
}

/// D8③: 请求切片只在下次建账 (Wake/Requeue) 生效, 且跨 Requeue 保持.
#[test]
fn requested_slice_applies_at_next_refill_only() {
    let mut model = Model::new();
    assert!(model.add_task(1, 0, 0));
    assert!(model.set_requested_slice(1, Some(4)));
    assert!(model.wake(0, 1));
    assert_eq!(model.schedule(0), Some(1));
    let task = model.task(1).expect("任务必须存在");
    assert_eq!(task.slice_real, MIN_GRANULARITY, "请求 4 夹取到最小粒度");
    assert_eq!(task.slice_left, MIN_GRANULARITY);
    assert_eq!(task.slice_virt, VIRT_ONE * MIN_GRANULARITY);

    // 第一次到期后仍按请求值 (8) 补充.
    for _ in 0..MIN_GRANULARITY {
        model.tick(0);
    }
    assert!(model.needs_resched(0));
    let task = model.task(1).expect("任务必须存在");
    assert_eq!(task.slice_real, MIN_GRANULARITY);
    assert_eq!(task.slice_left, MIN_GRANULARITY);
    model.schedule(0);

    // 运行中改请求不得打断当前切片.
    assert!(model.set_requested_slice(1, Some(30)));
    let task = model.task(1).expect("任务必须存在");
    assert_eq!(
        task.slice_real, MIN_GRANULARITY,
        "改请求不得立即改写当前切片"
    );
    assert_eq!(task.slice_left, MIN_GRANULARITY);
    model.audit();
    for _ in 0..MIN_GRANULARITY {
        model.tick(0);
    }
    model.audit();
    let task = model.task(1).expect("任务必须存在");
    assert_eq!(task.slice_real, 30, "下次 Requeue 才采用请求值");
    assert_eq!(task.slice_left, 30);
    assert_eq!(task.slice_virt, VIRT_ONE * 30);

    // 清除请求后回到默认值 (单任务 = 目标延迟).
    assert!(model.set_requested_slice(1, None));
    model.schedule(0);
    for _ in 0..30 {
        model.tick(0);
    }
    let task = model.task(1).expect("任务必须存在");
    assert_eq!(task.slice_real, TARGET_LATENCY);
    assert_eq!(task.slice_left, TARGET_LATENCY);
    model.audit();
}

/// IE5: Migrate 原样携带 `vr/切片/vd`, 源/目的树账变化互为相反数.
#[test]
fn migrate_preserves_payload_and_conserves_ledgers() {
    let mut model = Model::new();
    for (pid, nice, vr) in [
        (1u32, -20i8, 10_000u64),
        (2, -10, 80_000),
        (3, 0, 150_000),
        (4, 10, 220_000),
    ] {
        assert!(model.add_task(pid, nice, vr));
        assert!(model.wake(0, pid));
    }
    assert!(model.schedule(0).is_some());
    model.audit();
    let running = model.rq(0).expect("cpu0").running_pid();
    let tree_pid = model
        .rq(0)
        .expect("cpu0")
        .tree_keys()
        .map(|(_, pid)| pid)
        .find(|pid| Some(*pid) != running)
        .expect("树上必须有可迁移任务");
    let before = *model.task(tree_pid).expect("任务必须存在");
    let (source_weight_before, source_sum_before) = {
        let source = model.rq(0).expect("cpu0");
        (source.total_weight(), source.sum_w_vr())
    };
    let (dest_weight_before, dest_sum_before) = {
        let dest = model.rq(1).expect("cpu1");
        (dest.total_weight(), dest.sum_w_vr())
    };

    assert!(model.migrate(0, 1, tree_pid));
    let after = *model.task(tree_pid).expect("任务必须存在");
    assert_eq!(after.vr, before.vr);
    assert_eq!(after.vd, before.vd);
    assert_eq!(after.slice_real, before.slice_real);
    assert_eq!(after.slice_virt, before.slice_virt);
    assert_eq!(after.slice_left, before.slice_left);
    assert_eq!(model.location(tree_pid), Some(Location::Tree(1)));
    let (source_weight_after, source_sum_after) = {
        let source = model.rq(0).expect("cpu0");
        (source.total_weight(), source.sum_w_vr())
    };
    let (dest_weight_after, dest_sum_after) = {
        let dest = model.rq(1).expect("cpu1");
        (dest.total_weight(), dest.sum_w_vr())
    };
    let weight = before.weight;
    assert_eq!(source_weight_before - source_weight_after, weight);
    assert_eq!(dest_weight_after - dest_weight_before, weight);
    assert_eq!(
        source_sum_before - source_sum_after,
        u128::from(weight) * u128::from(before.vr)
    );
    assert_eq!(
        dest_sum_after - dest_sum_before,
        u128::from(weight) * u128::from(before.vr)
    );
    assert_eq!(
        (source_weight_before + dest_weight_before),
        (source_weight_after + dest_weight_after),
        "全局 W 守恒"
    );
    assert_eq!(
        (source_sum_before + dest_sum_before),
        (source_sum_after + dest_sum_after),
        "全局 S 守恒"
    );
    model.audit();

    assert!(!model.migrate(0, 1, tree_pid), "任务不在源队列时必须失败");
    assert!(!model.migrate(1, 1, tree_pid), "同核迁移必须失败");
    if let Some(pid) = running {
        assert!(!model.migrate(0, 1, pid), "运行任务不得作为偷取候选");
    }
    assert!(model.migrate(1, 0, tree_pid));
    assert_eq!(model.location(tree_pid), Some(Location::Tree(0)));
    model.audit();
}

/// 同权重任务的 `(vd, pid)` 选择: 同切片下严格按截止期轮转, 240 tick 各 120.
#[test]
fn equal_weight_selection_round_robins_by_deadline() {
    let mut model = Model::new();
    for pid in 1..=2u32 {
        assert!(model.add_task(pid, 0, 0));
        assert!(
            model.set_requested_slice(pid, Some(30)),
            "固定切片使 vd 可比"
        );
        assert!(model.wake(0, pid));
    }
    model.audit();
    assert_eq!(model.pick_winner(0), Some(1), "同 vd 时 pid 小者先");
    let mut counts = [0usize; 3];
    for _ in 0..240 {
        if model.needs_resched(0) || model.rq(0).expect("cpu0").running_pid().is_none() {
            assert!(model.schedule(0).is_some());
        }
        let running = model.rq(0).expect("cpu0").running_pid().expect("不空闲");
        counts[idx(running)] += 1;
        model.tick(0);
        model.audit();
    }
    assert_eq!(counts[1], 120);
    assert_eq!(counts[2], 120);
}

/// 高权重 CPU-bound 任务的长期份额贴近权重比 (88761/89785 ≈ 98.9%).
#[test]
fn high_weight_task_share_bounded_by_slice_cap() {
    let mut model = Model::new();
    assert!(model.add_task(10, -20, 0));
    assert!(model.add_task(11, 0, 0));
    assert!(model.wake(0, 10));
    assert!(model.wake(0, 11));
    model.audit();
    let mut high = 0u64;
    let mut low = 0u64;
    for _ in 0..6_700 {
        if model.needs_resched(0) || model.rq(0).expect("cpu0").running_pid().is_none() {
            assert!(model.schedule(0).is_some());
        }
        match model.rq(0).expect("cpu0").running_pid().expect("不空闲") {
            10 => high += 1,
            11 => low += 1,
            other => panic!("未知 pid {other}"),
        }
        model.tick(0);
    }
    model.audit();
    assert_eq!(high + low, 6_700);
    let share_pm = high * 1_000 / (high + low);
    assert!(
        (970..=995).contains(&share_pm),
        "高权重份额越界: {share_pm}‰ (high={high}, low={low})"
    );
    assert!(
        low >= 50,
        "低权重任务必须获得与其权重成比例的份额: low={low}"
    );
}

/// 高权重短切片任务频繁唤醒: 每周期最多一个请求切片, 不能按权重垄断 CPU.
#[test]
fn high_weight_frequent_waker_cannot_bank_credit() {
    let mut model = Model::new();
    assert!(model.add_task(20, -20, 0));
    assert!(model.add_task(21, 0, 0));
    assert!(model.set_requested_slice(20, Some(MIN_GRANULARITY)));
    assert!(model.wake(0, 21));
    assert_eq!(model.schedule(0), Some(21));
    let period = 10u64;
    let total_ticks = 5_000u64;
    let mut high = 0u64;
    let mut low = 0u64;
    let mut high_budget = 0u64;
    for tick in 0..total_ticks {
        if tick % period == 0 {
            if model.location(20).is_some() {
                assert!(model.block(20));
            }
            assert!(model.set_raw_vruntime(20, 0));
            assert!(model.wake(0, 20));
            high_budget = 0;
        }
        if model.needs_resched(0) || model.rq(0).expect("cpu0").running_pid().is_none() {
            assert!(model.schedule(0).is_some());
        }
        let running = model.rq(0).expect("cpu0").running_pid().expect("不空闲");
        if running == 20 {
            high += 1;
            high_budget += 1;
        } else if running == 21 {
            low += 1;
        } else {
            panic!("未知 pid {running}");
        }
        model.tick(0);
        if running == 20 && high_budget == MIN_GRANULARITY {
            assert!(model.block(20), "本周期预算耗尽后必须睡眠");
        }
        model.audit();
    }
    assert_eq!(high + low, total_ticks);
    let share_pm = high * 1_000 / (high + low);
    assert!(
        (750..=850).contains(&share_pm),
        "频繁唤醒者的 CPU 上界失控: {share_pm}‰ (high={high}, low={low})"
    );
    assert!(low >= 750, "低权重任务不得被频繁唤醒者饿死: low={low}");
}

/// 255 任务边界: 账目/合格判定/选择在最大权重和下不溢出.
#[test]
fn capacity_boundary_255_tasks() {
    let mut model = Model::new();
    for raw_pid in 1..=u32::try_from(MAX_TASKS).expect("上限可放入 u32") {
        assert!(model.add_task(raw_pid, -20, u64::from(raw_pid) * 100_000));
    }
    assert!(!model.add_task(256, 0, 0), "超过 MAX_TASKS 必须拒绝登记");
    for raw_pid in 1..=u32::try_from(MAX_TASKS).expect("上限可放入 u32") {
        assert!(model.wake(0, raw_pid));
    }
    model.audit();
    let rq = model.rq(0).expect("cpu0");
    let expected_w = 255 * 88_761;
    assert_eq!(rq.total_weight(), expected_w);
    let mut expected_s = 0u128;
    let mut min_vr = u64::MAX;
    for pid in model.task_pids() {
        let task = model.task(pid).expect("任务必须存在");
        expected_s += u128::from(task.weight) * u128::from(task.vr);
        min_vr = min_vr.min(task.vr);
    }
    assert_eq!(rq.sum_w_vr(), expected_s);
    assert_eq!(
        rq.avg_vruntime(),
        weighted_avg_vruntime(expected_w, expected_s)
    );
    assert!(
        is_eligible(min_vr, rq.total_weight(), rq.sum_w_vr()),
        "非空可运行集的最小 vr 任务必须合格"
    );
    let cross = u128::from(u64::MAX) * u128::from(rq.total_weight());
    assert!(cross > u128::from(u64::MAX), "u128 交叉乘法必须有充足余量");
    assert!(model.schedule(0).is_some());
    for _ in 0..100 {
        model.tick(0);
        model.audit();
    }
}

/// 数学事实: 全体 lag < 0 不可达; 选择结果等于合格集合的最小 `(vd, pid)`.
#[test]
fn eligible_set_never_empty_and_min_vr_eligible() {
    let mut state = 0x0bad_cafe_dead_beefu64;
    for _ in 0..200 {
        let mut model = Model::new();
        let count = 1 + usize::try_from(lcg_next(&mut state) % 12).expect("样本可放入 usize");
        for offset in 0..count {
            let pid = u32::try_from(offset + 1).expect("测试 pid 可放入 u32");
            let nice = i8::try_from(lcg_next(&mut state) % 40).expect("nice 档位可放入 i8") - 20;
            let vr = lcg_next(&mut state) % 50_000_000;
            assert!(model.add_task(pid, nice, vr));
            assert!(model.wake(0, pid));
        }
        model.audit();
        let rq = model.rq(0).expect("cpu0");
        let total = rq.total_weight();
        let sum = rq.sum_w_vr();
        let mut min_vr = u64::MAX;
        let mut eligible = 0usize;
        let mut best: Option<(u64, u32)> = None;
        for pid in model.task_pids() {
            if model.location(pid).is_none() {
                continue;
            }
            let task = model.task(pid).expect("任务必须存在");
            min_vr = min_vr.min(task.vr);
            if is_eligible(task.vr, total, sum) {
                eligible += 1;
                let candidate = (task.vd, pid);
                if best.is_none_or(|current| candidate < current) {
                    best = Some(candidate);
                }
            }
        }
        assert!(eligible >= 1, "非空可运行集必须存在合格任务");
        assert!(is_eligible(min_vr, total, sum), "最小 vr 任务必须合格");
        assert_eq!(model.pick_winner(0), best.map(|(_, pid)| pid));
    }
}

/// 全体树任务欠账 + 连续唤醒: 重任务追平后所有任务都获得服务.
#[test]
fn adversarial_backlog_with_continuous_wakes_makes_progress() {
    let mut model = Model::new();
    assert!(model.add_task(1, -20, 0));
    for pid in 2..=4u32 {
        assert!(model.add_task(pid, 0, 100_000));
    }
    for pid in 1..=4u32 {
        assert!(model.wake(0, pid));
    }
    assert_eq!(model.schedule(0), Some(1), "重任务的 lag 最大, 应先运行");
    assert!(model.add_task(5, 0, 100_000));
    let mut counts = [0u64; 6];
    let mut first_service = [None; 6];
    for tick in 0..5_000u64 {
        if tick % 7 == 0 {
            if model.location(5).is_some() {
                assert!(model.block(5));
            }
            assert!(model.set_raw_vruntime(5, 100_000));
            model.wake(0, 5);
        }
        if model.needs_resched(0) || model.rq(0).expect("cpu0").running_pid().is_none() {
            assert!(model.schedule(0).is_some());
        }
        let running = model.rq(0).expect("cpu0").running_pid().expect("不空闲");
        counts[idx(running)] += 1;
        first_service[idx(running)].get_or_insert(tick);
        model.tick(0);
        if running == 5 {
            assert!(model.block(5), "探针每次唤醒只运行 1 tick");
        }
        if tick % 100 == 0 {
            model.audit();
        }
    }
    model.audit();
    assert_eq!(counts.iter().sum::<u64>(), 5_000);
    for pid in 1..=4u32 {
        assert!(counts[idx(pid)] > 0, "基础任务 pid {pid} 未被服务");
        assert!(
            first_service[idx(pid)].is_some_and(|tick| tick < 1_000),
            "基础任务 pid {pid} 首次服务延迟超过 1000 tick"
        );
    }
    assert!(counts[idx(5)] > 0, "连续唤醒探针未被服务");
}

/// 随机操作序列 (wake/block/tick/migrate/请求切片) 后与暴力重算对账.
#[test]
fn random_operation_sequence_reconciles_with_brute_force() {
    let mut model = Model::new();
    let mut state = 0xfeed_face_0123_4567u64;
    for pid in 1..=8u32 {
        let nice = i8::try_from(lcg_next(&mut state) % 40).expect("nice 档位可放入 i8") - 20;
        let vr = lcg_next(&mut state) % 2_000_000;
        assert!(model.add_task(pid, nice, vr));
    }
    for _ in 0..5_000 {
        let op = lcg_next(&mut state) % 100;
        let pid = 1 + u32::try_from(lcg_next(&mut state) % 8).expect("pid 可放入 u32");
        match op {
            0..=34 => {
                let cpu = usize::try_from(lcg_next(&mut state) % 2).expect("核号可放入 usize");
                if model.location(pid).is_none() {
                    model.wake(cpu, pid);
                }
            }
            35..=54 => {
                if model.location(pid).is_some() {
                    assert!(model.block(pid));
                }
            }
            55..=84 => {
                let cpu = usize::try_from(lcg_next(&mut state) % 2).expect("核号可放入 usize");
                let _ = model.tick(cpu);
                if model.needs_resched(cpu) {
                    let _ = model.schedule(cpu);
                }
            }
            85..=94 => {
                let from = usize::try_from(lcg_next(&mut state) % 2).expect("核号可放入 usize");
                let to = 1 - from;
                let candidate = model
                    .rq(from)
                    .and_then(|rq| rq.tree_keys().next())
                    .map(|(_, tree_pid)| tree_pid);
                if let Some(tree_pid) = candidate {
                    assert!(model.migrate(from, to, tree_pid));
                }
            }
            _ => {
                let requested = lcg_next(&mut state) % 100;
                let value = if requested == 0 {
                    None
                } else {
                    Some(requested)
                };
                model.set_requested_slice(pid, value);
            }
        }
        for cpu in 0..2 {
            if model.needs_resched(cpu) {
                let _ = model.schedule(cpu);
            }
        }
        model.audit();
    }
}
