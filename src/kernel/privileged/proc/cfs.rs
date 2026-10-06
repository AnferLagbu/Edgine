#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! CFS (Completely Fair Scheduler) — 调度策略 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 策略代码 (权重表 + vruntime 计算 + 时间片计算 + CFS/DL 运行队列) 于
//! T1-1 (2026-06-16) 迁至 `functions::proc::sched_policy`, 本文件仅
//! re-export。按统一判据反转：CfsRunQueue/DlRunQueue 是 privileged scheduler
//! 的运行队列机制状态, 权重/vruntime/抢占判定全部操作队列内部字段, 无法
//! 脱离机制状态独立存在 (同 DECISION-M sys_pm_dispatch 判据: 算法本质是
//! 机制状态操作) — 迁回。依赖闭包仅 privileged。
//!
//! functions 侧改 `pub use crate::privileged::proc::cfs::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::proc::Pid;

// ============================================================================
// CFS Constants
// ============================================================================

// DECISION-O ②: CFS_* 权威归 privileged/config/sched (机制常量), functions 侧
// 纯 re-export — cfs 依赖收敛单向 (本文件零 functions 引用)
pub use crate::privileged::config::{
    CFS_BOOST_INTERVAL as CFS_BOOST_INTERVAL_TICKS,
    CFS_DL_MAX_UTILIZATION_PCT as DL_MAX_UTILIZATION_PCT, CFS_DL_MIN_PERIOD as DL_MIN_PERIOD_TICKS,
    CFS_DL_MIN_RUNTIME as DL_MIN_RUNTIME_TICKS, CFS_MIN_GRANULARITY as MIN_GRANULARITY_TICKS,
    CFS_NICE0_WEIGHT as NICE0_WEIGHT, CFS_TARGET_LATENCY as TARGET_LATENCY_TICKS,
};

pub const LOAD_BALANCE_THRESHOLD: u64 = 1024;

// ============================================================================
// NICE → 权重映射
// ============================================================================

pub const NICE_TO_WEIGHT: [u64; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

/// 将 nice 值 (-20..=19) 映射为其 CFS 调度权重; 越界值收敛到端点。
#[inline]
pub fn nice_to_weight(nice: i8) -> u64 {
    let clamped = nice.clamp(-20, 19);
    let idx = (clamped + 20) as usize;
    NICE_TO_WEIGHT[idx]
}

/// 将 CFS 权重反查为最接近的 nice 值 (-20..=19)。
#[inline]
pub fn weight_to_nice(weight: u64) -> i8 {
    if weight >= NICE_TO_WEIGHT[0] {
        return -20;
    }
    if weight <= NICE_TO_WEIGHT[39] {
        return 19;
    }
    let mut best = 0i8;
    let mut best_diff = u64::MAX;
    for (i, &w) in NICE_TO_WEIGHT.iter().enumerate() {
        let diff = w.abs_diff(weight);
        if diff < best_diff {
            best_diff = diff;
            best = (i as i32 - 20) as i8;
        }
    }
    best
}

// ============================================================================
// Deadline 调度 (EDF + CBS)
// ============================================================================

/// Deadline (EDF + CBS) 调度参数 — 运行时、相对截止期与周期。
#[derive(Debug, Clone, Copy)]
pub struct DeadlineParams {
    pub runtime: u64,
    pub deadline: u64,
    pub period: u64,
}

impl DeadlineParams {
    pub const fn new() -> Self {
        Self {
            runtime: 0,
            deadline: 0,
            period: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.runtime >= DL_MIN_RUNTIME_TICKS
            && self.deadline >= self.runtime
            && self.period >= self.deadline
            && self.period >= DL_MIN_PERIOD_TICKS
    }

    pub fn utilization_pct(&self) -> u64 {
        if self.period == 0 {
            return 0;
        }
        (self.runtime * 100) / self.period
    }
}

// ============================================================================
// CFS Run Queue
// ============================================================================

/// CFS 运行队列 — 以 (vruntime, pid) 为键的有序树, 并跟踪最小 vruntime 与总权重。
pub struct CfsRunQueue {
    pub tree: BTreeMap<(u64, Pid), ()>,
    pub min_vruntime: AtomicU64,
    pub total_weight: AtomicU64,
    pub nr_running: u32,
}

impl CfsRunQueue {
    #[expect(
        clippy::zero_sized_map_values,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn new() -> Self {
        Self {
            tree: BTreeMap::new(),
            min_vruntime: AtomicU64::new(0),
            total_weight: AtomicU64::new(0),
            nr_running: 0,
        }
    }

    pub fn enqueue(&mut self, pid: Pid, vruntime: u64, weight: u64) {
        let min_vr = self.min_vruntime.load(Ordering::Acquire);
        let start_vr = vruntime.max(min_vr);

        self.tree.insert((start_vr, pid), ());
        self.total_weight.fetch_add(weight, Ordering::Release);
        self.nr_running += 1;
        // 维持不变式 min_vruntime == 队列最小 vruntime:
        // 旧实现只在移除路径 (dequeue/pick_next) 推进 min_vruntime, 入队侧不推进
        // 会使 min_vruntime 长期停在初值 0, 后续入队者的"提升到 min_vruntime"
        // 形同虚设 (新线程可越过已在队的低 vruntime 线程) — 2026-09-24 UT-06 实测修复.
        self.sync_min_vruntime();
    }

    pub fn dequeue(&mut self, pid: Pid, vruntime: u64, weight: u64) -> bool {
        let mut prev = self.total_weight.load(Ordering::Acquire);
        loop {
            let new = prev.saturating_sub(weight);
            match self.total_weight.compare_exchange_weak(
                prev,
                new,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
        self.nr_running = self.nr_running.saturating_sub(1);
        if self.tree.remove(&(vruntime, pid)).is_some() {
            self.sync_min_vruntime();
            true
        } else {
            false
        }
    }

    pub fn pick_next(&mut self) -> Option<(Pid, u64)> {
        let (&(vruntime, pid), ()) = self.tree.first_key_value()?;
        self.tree.remove(&(vruntime, pid));
        self.sync_min_vruntime();
        Some((pid, vruntime))
    }

    pub fn update_curr(&mut self, pid: Pid, new_vruntime: u64) {
        let min_vr = self.min_vruntime.load(Ordering::Acquire);
        let start_vr = new_vruntime.max(min_vr);
        self.tree.insert((start_vr, pid), ());
    }

    pub fn calc_time_slice(&self, weight: u64) -> u64 {
        let total_w = self.total_weight.load(Ordering::Acquire);
        if total_w == 0 || weight == 0 {
            return MIN_GRANULARITY_TICKS;
        }
        let slice = TARGET_LATENCY_TICKS.saturating_mul(weight) / total_w;
        slice.max(MIN_GRANULARITY_TICKS)
    }

    pub fn get_weighted_load(&self) -> u64 {
        self.total_weight.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.nr_running == 0
    }

    pub fn boost_all_vruntime(&mut self) {
        if self.tree.is_empty() {
            return;
        }

        let min_vr = self.tree.first_key_value().map_or(0, |(&(vr, _), ())| vr);

        let entries: alloc::vec::Vec<(Pid, u64)> =
            self.tree.keys().map(|&(vr, pid)| (pid, vr)).collect();

        self.tree.clear();
        for (pid, _old_vr) in entries {
            self.tree.insert((min_vr, pid), ());
        }

        self.min_vruntime.store(min_vr, Ordering::Release);
    }

    fn sync_min_vruntime(&mut self) {
        if let Some((&(min_vr, _), ())) = self.tree.first_key_value() {
            self.min_vruntime.store(min_vr, Ordering::Release);
        }
    }

    pub fn steal_highest_vruntime(&mut self) -> Option<(Pid, u64)> {
        let (&(vruntime, pid), ()) = self.tree.last_key_value()?;
        self.tree.remove(&(vruntime, pid));
        self.sync_min_vruntime();
        Some((pid, vruntime))
    }
}

// ============================================================================
// Deadline 运行队列 (EDF)
// ============================================================================

/// Deadline 运行队列 — 以 (绝对截止期, pid) 为键, 并跟踪总利用率。
pub struct DlRunQueue {
    pub tree: BTreeMap<(u64, Pid), ()>,
    pub nr_running: u32,
    pub total_utilization: u64,
}

impl DlRunQueue {
    #[expect(
        clippy::zero_sized_map_values,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn new() -> Self {
        Self {
            tree: BTreeMap::new(),
            nr_running: 0,
            total_utilization: 0,
        }
    }

    pub fn enqueue(&mut self, pid: Pid, deadline_abs: u64, util_pct: u64) -> bool {
        if self.total_utilization.saturating_add(util_pct) > DL_MAX_UTILIZATION_PCT {
            return false;
        }
        self.tree.insert((deadline_abs, pid), ());
        self.nr_running += 1;
        self.total_utilization += util_pct;
        true
    }

    pub fn dequeue(&mut self, pid: Pid, deadline_abs: u64, util_pct: u64) {
        if self.tree.remove(&(deadline_abs, pid)).is_some() {
            self.nr_running = self.nr_running.saturating_sub(1);
            self.total_utilization = self.total_utilization.saturating_sub(util_pct);
        }
    }

    pub fn pick_next(&mut self) -> Option<(Pid, u64)> {
        let (&(dl_abs, pid), ()) = self.tree.first_key_value()?;
        self.tree.remove(&(dl_abs, pid));
        Some((pid, dl_abs))
    }

    pub fn reinsert(&mut self, pid: Pid, dl_abs: u64) {
        self.tree.insert((dl_abs, pid), ());
    }

    pub fn is_empty(&self) -> bool {
        self.nr_running == 0
    }

    pub fn get_load(&self) -> u32 {
        self.nr_running
    }
}

// ============================================================================
// Tick 计数辅助
// ============================================================================

/// 计算单 tick 的 vruntime 增量 (权重越大增量越小; 权重为 0 时退化为 nice0 权重)。
#[inline]
pub fn calc_vruntime_delta(weight: u64) -> u64 {
    // 权重 0 是非法态 (合法权重恒 ≥ `nice_to_weight(19)`=15). 此处按 `NICE0_WEIGHT`
    // "退化权重" 处理, 故增量取 1。**不可**直接返回 `NICE0_WEIGHT` 作为增量: 那会
    // 让该进程 vruntime 以 1024 倍速度暴涨, 经入队/更新的 `start_vr = max(vr, min_vr)`
    // 把所在核 `min_vruntime` 钉在高位, 使同核其他任务永不满足抢占/让出条件而集体
    // 饥饿 (历史缺陷根因之一, 与 `init_kernel_process_fields` 漏写 `cfs_weight` 叠加)。
    let w = if weight == 0 { NICE0_WEIGHT } else { weight };
    (NICE0_WEIGHT / w).max(1)
}

/// 判定当前线程是否应被抢占 — 其 vruntime 超出最小 vruntime 的差值超过按权重缩放的时间片阈值。
#[inline]
pub fn cfs_should_preempt(curr_vruntime: u64, min_vruntime: u64, weight: u64) -> bool {
    let threshold = if weight == 0 {
        MIN_GRANULARITY_TICKS.saturating_mul(NICE0_WEIGHT)
    } else {
        MIN_GRANULARITY_TICKS.saturating_mul(NICE0_WEIGHT) / weight
    };
    curr_vruntime.saturating_sub(min_vruntime) > threshold
}

// ============================================================================
// 调度策略实现 — functions 层策略主体
// ============================================================================

use crate::privileged::proc::sched_trait::SchedDecision;
use crate::privileged::proc::types::ThreadPriority;

/// 默认调度策略 — functions 层安全实现
///
/// 策略决策 (优先级选择、boost 触发、时间片计算) 全部在此.
/// privileged 层的 `SchedulerEx` 仅保留 `RunQueue` 操作和上下文切换机制.
///
/// ## 设计
///
/// - 从高到低优先级扫描
/// - 可通过替换此 struct 自定义调度行为
/// - 在 `functions::proc::init()` 中通过 `register_sched_policy()` 注册
pub struct DefaultPolicy;

impl SchedDecision for DefaultPolicy {
    fn pick_next_priority(&self, queue_lengths: [u32; 5]) -> Option<usize> {
        // 从高到低优先级扫描
        for prio in (0..5).rev() {
            if queue_lengths[prio] > 0 {
                return Some(prio);
            }
        }
        None
    }

    fn should_boost(&self, tick_count: u64, last_boost: u64) -> bool {
        tick_count.saturating_sub(last_boost) >= CFS_BOOST_INTERVAL_TICKS
    }

    fn boost_target(&self) -> ThreadPriority {
        ThreadPriority::High
    }

    fn time_slice_for(&self, priority: ThreadPriority) -> u32 {
        use crate::privileged::config::{
            SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM,
            SCHED_LEVEL_3_QUANTUM,
        };
        match priority {
            ThreadPriority::Realtime => SCHED_LEVEL_0_QUANTUM,
            ThreadPriority::High => SCHED_LEVEL_1_QUANTUM,
            ThreadPriority::Normal => SCHED_LEVEL_2_QUANTUM,
            ThreadPriority::Low => SCHED_LEVEL_3_QUANTUM,
            // DECISION-072: u32::MAX = "永不过期"语义; 仅当无其他优先级任务时被调度
            // (调度器 FIFO 行为); 其他任务唤醒后会抢占, 无死循环风险.
            ThreadPriority::Idle => u32::MAX,
        }
    }

    fn should_reschedule(&self, time_slice_remaining: u32) -> bool {
        time_slice_remaining <= 1
    }
}

/// 注册调度策略到 privileged
///
/// 由 `functions::proc::init()` 调用. 只能注册一次.
///
/// # Errors
///
/// 当调度策略已被注册时返回 `Err(())`.
pub fn register_default_policy() -> Result<(), ()> {
    static POLICY: DefaultPolicy = DefaultPolicy;
    crate::privileged::proc::register_sched_decision(&POLICY).map_err(|_| ())
}

// ============================================================================
// 单元测试 — 调度策略契约
// ============================================================================
//
// 覆盖:
// - nice_to_weight / weight_to_nice: NICE 双向转换 (含 -20..19 边界 + clamp)
// - mlfq_level_to_nice: 层级 → nice
// - DeadlineParams 校验: is_valid + utilization_pct
// - CfsRunQueue: enqueue/dequeue/pick_next + 时间片计算
// - DefaultPolicy 调度: time_slice_for + should_reschedule

#[cfg(test)]
mod tests {
    use super::*;
    // DECISION-J 归属反转: SCHED_LEVEL_* 权威定义已迁回 privileged::config,
    // functions::config 仅 re-export 兼容层; privileged 侧测试不得反向引用 functions.
    use crate::privileged::config::{
        SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM, SCHED_LEVEL_3_QUANTUM,
    };

    /// 1. nice_to_weight: -20..19 全范围
    #[test]
    fn test_sched_nice_to_weight() {
        // nice=-20 → 88761 (NICE_TO_WEIGHT[0])
        assert_eq!(nice_to_weight(-20), 88761);
        // nice=0 → 1024 (NICE_TO_WEIGHT[20])
        assert_eq!(nice_to_weight(0), 1024);
        // nice=19 → 15 (NICE_TO_WEIGHT[39])
        assert_eq!(nice_to_weight(19), 15);
        // 越界 clamp: -100 → -20
        assert_eq!(nice_to_weight(-100), 88761);
        // 越界 clamp: 100 → 19
        assert_eq!(nice_to_weight(100), 15);
        // 边界: i8 最小/最大
        assert_eq!(nice_to_weight(i8::MIN), 88761);
        assert_eq!(nice_to_weight(i8::MAX), 15);
    }

    /// 2. weight_to_nice: 反向转换
    #[test]
    fn test_sched_weight_to_nice() {
        // 88761 → -20 (NICE_TO_WEIGHT[0])
        assert_eq!(weight_to_nice(88761), -20);
        // 1024 → 0 (精确)
        assert_eq!(weight_to_nice(1024), 0);
        // 15 → 19
        assert_eq!(weight_to_nice(15), 19);
        // 越界: >= 88761 → -20
        assert_eq!(weight_to_nice(100000), -20);
        assert_eq!(weight_to_nice(u64::MAX), -20);
        // 越界: <= 15 → 19
        assert_eq!(weight_to_nice(10), 19);
        assert_eq!(weight_to_nice(0), 19);
        // 近似匹配: 找最接近
        let nice = weight_to_nice(5000);
        // 5000 在 NICE_TO_WEIGHT 中无精确匹配, 应返回最近 nice
        assert!(nice >= -20 && nice <= 19);
    }

    /// 3. DeadlineParams: is_valid 边界
    #[test]
    fn test_sched_deadline_is_valid() {
        // 默认: 全 0 → invalid
        assert!(!DeadlineParams::new().is_valid());
        // runtime < MIN → invalid
        // DL_MIN_RUNTIME_TICKS == 1, 故"小于 MIN"的样本取 0 (原用例取 1 即等于
        // MIN, 断言必然不成立; 2026-09-24 UT-06 实测修正).
        let mut p = DeadlineParams {
            runtime: 0,
            deadline: 100,
            period: 100,
        };
        assert!(!p.is_valid());
        // runtime >= MIN, deadline >= runtime, period >= deadline, period >= MIN_PERIOD → valid
        p.runtime = DL_MIN_RUNTIME_TICKS;
        assert!(p.is_valid());
        // period 小于 MIN_PERIOD → 无效
        p.period = 1;
        assert!(!p.is_valid());
        // period 小于 deadline → 无效
        p.period = 50;
        p.deadline = 100;
        assert!(!p.is_valid());
    }

    /// 5. DeadlineParams: utilization_pct 利用率
    #[test]
    fn test_sched_deadline_utilization() {
        assert_eq!(DeadlineParams::new().utilization_pct(), 0); // period=0 → 0
        let p = DeadlineParams {
            runtime: 50,
            deadline: 100,
            period: 100,
        };
        assert_eq!(p.utilization_pct(), 50); // 50/100 * 100 = 50%
        let p = DeadlineParams {
            runtime: 100,
            deadline: 100,
            period: 100,
        };
        assert_eq!(p.utilization_pct(), 100); // 100%
    }

    /// 6. CfsRunQueue: 入队/选下一个/出队
    #[test]
    fn test_sched_cfs_basic_ops() {
        let mut q = CfsRunQueue::new();
        assert!(q.is_empty());
        // enqueue 3 个进程 (入队侧的 vruntime 提升见 enqueue 内 min_vruntime 同步)
        q.enqueue(1, 100, 1024);
        q.enqueue(2, 150, 1024);
        q.enqueue(3, 200, 1024);
        assert_eq!(q.nr_running, 3);
        // pick_next: 最小 vruntime 是 100 (PID 1)
        let (pid, vr) = q.pick_next().unwrap();
        assert_eq!(pid, 1);
        assert_eq!(vr, 100);
        // pick_next 只从红黑树取出, 不递减 nr_running (与 update_curr 对称,
        // 配额账由调用方负责; 2026-09-24 UT-06 实测修正原断言).
        assert_eq!(q.nr_running, 3);
        // pick_next: 下一个是 150 (PID 2)
        let (pid, vr) = q.pick_next().unwrap();
        assert_eq!(pid, 2);
        assert_eq!(vr, 150);
        // pick_next: 最后一个是 200 (PID 3)
        let (pid, vr) = q.pick_next().unwrap();
        assert_eq!(pid, 3);
        assert_eq!(vr, 200);
        // 空队列
        assert!(q.pick_next().is_none());
    }

    /// 7. CfsRunQueue: calc_time_slice (权重比例)
    #[test]
    fn test_sched_cfs_time_slice() {
        let q = CfsRunQueue::new();
        // total=0 → 返回 MIN_GRANULARITY (避免除零)
        assert_eq!(q.calc_time_slice(1024), MIN_GRANULARITY_TICKS);
        // weight=0 → MIN_GRANULARITY (避免除零)
        let mut q = CfsRunQueue::new();
        q.enqueue(1, 0, 1024);
        assert_eq!(q.calc_time_slice(0), MIN_GRANULARITY_TICKS);
        // 单进程 (total=1024, weight=1024) → TARGET_LATENCY
        let q = CfsRunQueue::new();
        // 直接设置 total_weight
        q.total_weight.store(1024, Ordering::Release);
        assert_eq!(q.calc_time_slice(1024), TARGET_LATENCY_TICKS);
        // 2 进程 (total=2048, weight=1024) → TARGET/2
        let q = CfsRunQueue::new();
        q.total_weight.store(2048, Ordering::Release);
        assert_eq!(q.calc_time_slice(1024), TARGET_LATENCY_TICKS / 2);
    }

    /// 7b. calc_vruntime_delta: 权重 0 (非法态) 必须退化为增量 1, 而非 NICE0_WEIGHT.
    ///
    /// 回归: 历史实现 `if weight == 0 { return NICE0_WEIGHT; }` 让权重 0 的进程
    /// vruntime 以 1024 倍速度增长, 经 `start_vr = max(vr, min_vr)` 把所在核的
    /// `min_vruntime` 钉在高位, 使同核任务永不满足抢占条件而集体饥饿.
    #[test]
    fn test_sched_calc_vruntime_delta_zero_weight() {
        // 权重 0 → 退化权重 NICE0_WEIGHT → 增量 1 (绝不可为 1024)
        assert_eq!(calc_vruntime_delta(0), 1);
        // nice=0 权重 (1024) → 增量 1
        assert_eq!(calc_vruntime_delta(NICE0_WEIGHT), 1);
        // nice=19 最低合法权重 (15) → 增量 1024/15 = 68
        assert_eq!(calc_vruntime_delta(nice_to_weight(19)), NICE0_WEIGHT / 15);
        // nice=-20 最高权重 (88761) → 整除为 0, 钳到下限 1
        assert_eq!(calc_vruntime_delta(nice_to_weight(-20)), 1);
    }

    /// 8. CfsRunQueue: enqueue 时 vruntime 自动对齐到 min_vruntime
    #[test]
    fn test_sched_cfs_min_vruntime_alignment() {
        let mut q = CfsRunQueue::new();
        // 先 enqueue 1 个, vruntime=100
        q.enqueue(1, 100, 1024);
        // min_vruntime = 100
        assert_eq!(q.min_vruntime.load(Ordering::Acquire), 100);
        // 再 enqueue 1 个, vruntime=50 (小于 min)
        q.enqueue(2, 50, 1024);
        // 应被提升到 100 (max(50, 100))
        assert_eq!(q.min_vruntime.load(Ordering::Acquire), 100);
        // 实际入队位置: (100, 2)
        let (pid, vr) = q.pick_next().unwrap();
        assert_eq!(pid, 1);
        assert_eq!(vr, 100);
        let (pid, vr) = q.pick_next().unwrap();
        assert_eq!(pid, 2);
        assert_eq!(vr, 100);
    }

    /// 9. DefaultPolicy: `time_slice_for` (4 级优先级)
    #[test]
    fn test_sched_default_time_slice() {
        let p = DefaultPolicy;
        assert_eq!(
            p.time_slice_for(ThreadPriority::Realtime),
            SCHED_LEVEL_0_QUANTUM
        );
        assert_eq!(
            p.time_slice_for(ThreadPriority::High),
            SCHED_LEVEL_1_QUANTUM
        );
        assert_eq!(
            p.time_slice_for(ThreadPriority::Normal),
            SCHED_LEVEL_2_QUANTUM
        );
        assert_eq!(p.time_slice_for(ThreadPriority::Low), SCHED_LEVEL_3_QUANTUM);
        assert_eq!(p.time_slice_for(ThreadPriority::Idle), u32::MAX);
    }

    /// 10. DefaultPolicy: 是否需要重新调度
    #[test]
    fn test_sched_default_should_reschedule() {
        let p = DefaultPolicy;
        // 剩余时间片 > 1 → 不重调度
        assert!(!p.should_reschedule(10));
        assert!(!p.should_reschedule(2));
        // 剩余时间片 <= 1 → 应重调度
        assert!(p.should_reschedule(1));
        assert!(p.should_reschedule(0));
    }

    /// 11. integration: 完整调度循环
    #[test]
    fn test_sched_cfs_full_cycle() {
        let mut q = CfsRunQueue::new();
        // 加入 4 个进程, 不同 vruntime + weight (均不低于当前 min_vruntime,
        // 不发生入队侧提升, 以便校验按 vruntime 排序; 2026-09-24 UT-06 实测修正).
        q.enqueue(10, 100, 1024);
        q.enqueue(20, 150, 2048); // 更高权重
        q.enqueue(30, 250, 1024);
        q.enqueue(40, 200, 1024);
        // 按 vruntime 顺序调度
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 10); // vruntime=100
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 20); // vruntime=150
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 40); // vruntime=200
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 30); // vruntime=250
        // 队列空以红黑树为准 (is_empty 基于 nr_running, pick_next 不递减该计数)
        assert!(q.pick_next().is_none());
        assert_eq!(q.nr_running, 4);
    }
}
