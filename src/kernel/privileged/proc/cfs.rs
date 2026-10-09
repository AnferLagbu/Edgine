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

/// CFS 运行队列 —— 以 (vruntime, pid) 为键的有序树 + 本核 vruntime 时基下界。
///
/// ## 单时基模型 (DECISION-099)
///
/// 队列只有**一个** vruntime 时间轴, 其原点即 `min_vruntime` (下称 floor)。
/// 三条不变式从构造上封死历史上造成"多秒级进程饿死"的三条背离路径:
///
/// - **IC1 单一时基** —— 任何任务的可比较 vruntime 均落在
///   `[floor - CFS_MIN_GRANULARITY, +∞)` 内: 入队落点由 [`Self::enqueue`] 钳制,
///   并由调用方**回写** `Process::cfs_vruntime`。旧实现只钳树键而不回写任务自身
///   vruntime, 使"运行中的任务"与"树上的任务"分属两个时间域, 可无界发散
///   (实测 `curr vr=45` 对 `floor=5152`, 抢占差值被饱和成 0 ⇒ 该核永不重调度)。
/// - **IC2 结构即真相** —— 可运行性判据一律取自 `tree`, **不**并行维护计数
///   器。旧 `nr_running` 与树一旦背离 (实测 `nr=108 / tree=0`), `is_empty()` 与
///   抢占判据同时失真, 树上的任务变成"隐形饥饿"。
/// - **IC3 下界单调** —— floor 只由 tick 随运行任务的 vruntime 经
///   [`Self::advance_floor`] 推进, **只增不减**; 绝不"重设为树最小键"(旧
///   `sync_min_vruntime`: 被钳到 floor 的唤醒者会反过来推高 floor, 形成棘轮),
///   也不整树折叠 (旧 `boost_all_vruntime`: 抹掉相对次序)。
///
/// `total_weight` 是"可运行集 (树上的 + 正在本核运行的) 权重和", 由
/// [`Self::enqueue`]/[`Self::dequeue`] 依调用方传入的记账凭证精确加减一次;
/// 树内移动 (`pick_next`/`requeue`) 不动账。记账凭证由 `Process::cfs_on_rq`
/// 承载 (语义同 Linux 的 `on_rq`: 处于可运行记账中, 上核运行期间仍为 true)。
pub struct CfsRunQueue {
    pub tree: BTreeMap<(u64, Pid), ()>,
    /// 本核 vruntime 下界 (IC3: 单调不减)。字段刻意私有 —— 只允许经
    /// [`Self::floor`] 读、[`Self::advance_floor`] 写, 防止调用点重新引入
    /// "重设"语义。
    min_vruntime: AtomicU64,
    pub total_weight: AtomicU64,
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
        }
    }

    /// 本核 vruntime 下界 (IC3)。
    #[inline]
    pub fn floor(&self) -> u64 {
        self.min_vruntime.load(Ordering::Acquire)
    }

    /// 把下界推进到 `vr` (只增不减)。由调度器 tick 随运行任务的 vruntime 调用
    /// —— 这是本队列唯一的 floor 写入路径, 也是 CFS 天然的反饥饿机制
    /// (取代旧 `boost_all_vruntime` 周期折叠)。
    #[inline]
    pub fn advance_floor(&self, vr: u64) {
        self.min_vruntime.fetch_max(vr, Ordering::AcqRel);
    }

    /// 树上最小 vruntime (即下一次 [`Self::pick_next`] 的候选) —— 抢占判据的
    /// 比较对象 (IC1: 与运行任务的 vr 同域, 差值有意义)。
    #[inline]
    pub fn leftmost_vruntime(&self) -> Option<u64> {
        self.tree.first_key_value().map(|(&(vr, _), ())| vr)
    }

    /// 树上的可运行任务数 (不含正在本核运行的任务; IC2 唯一真相)。
    #[inline]
    pub fn len(&self) -> usize {
        self.tree.len()
    }

    /// IC1 时基对齐: 把任意 vruntime 收进本队列的单一时间域
    /// `[floor - MIN_GRANULARITY, +∞)`。
    ///
    /// 低于下限的陈旧值 (睡久了的任务 / 从未在队列落过点的 boot 任务) 被抬到该
    /// 下限 —— 它们因此略微领先队列 (交互性), 但领先量有界, 不会倒过来饿死 CPU
    /// 密集任务; 高于下限的值原样保留 (已消耗较多 CPU 的任务应排在后面)。
    ///
    /// 调度器 tick 也对运行中的任务调用本函数 —— 那是 IC1 的兜底: 经
    /// `keep_current` 回退上核的任务 (如 boot 期 init) 可能从未在队列落过点。
    #[inline]
    pub fn placement(&self, vruntime: u64) -> u64 {
        vruntime.max(self.floor().saturating_sub(MIN_GRANULARITY_TICKS))
    }

    /// 入队 (**幂等**): 把 `pid` 放到 IC1 钳制后的位置, 返回该位置。
    ///
    /// `accounted` = 该任务的权重是否已在 `total_weight` 中 (调用方以
    /// `Process::cfs_on_rq` 承载)。已在队者重复入队只是**移动**其节点 (唤醒重投 /
    /// 迁移回源), 不二次记账 —— "每睡眠周期净漏 +1"那类记账泄漏由此构造性消灭。
    ///
    /// 调用方**必须**把返回值回写 `Process::cfs_vruntime`, 否则 IC1 不成立。
    pub fn enqueue(&mut self, pid: Pid, vruntime: u64, weight: u64, accounted: bool) -> u64 {
        self.remove_pid(pid);
        let placed = self.placement(vruntime);
        self.tree.insert((placed, pid), ());
        if !accounted {
            self.total_weight.fetch_add(weight, Ordering::Release);
        }
        placed
    }

    /// 把 `pid` 从树上摘除, 返回其原 vruntime; **不动** `total_weight`。
    ///
    /// 键含 vruntime 而调用方只知 pid, 故按 pid 线性扫描: 任务上限 255, 且
    /// 仅发生在入队去重/阻塞/迁移路径 (非 tick 热路), 优于另存一份 pid→键索引
    /// (那本身就是又一处会背离的记账)。
    fn remove_pid(&mut self, pid: Pid) -> Option<u64> {
        let key = self.tree.keys().find(|&&(_, p)| p == pid).copied();
        match key {
            Some((vr, _)) => {
                self.tree.remove(&(vr, pid));
                Some(vr)
            }
            None => None,
        }
    }

    /// 出队: 让 `pid` 离开可运行集 (阻塞 / 退出 / 被拣出但不可调度)。
    ///
    /// `accounted` 同 [`Self::enqueue`]; 未记账者幂等返回 (不二次扣权重)。
    /// 返回该 pid 当时是否在树上。
    pub fn dequeue(&mut self, pid: Pid, weight: u64, accounted: bool) -> bool {
        let existed = self.remove_pid(pid).is_some();
        if accounted {
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
        }
        existed
    }

    /// 拣出最小 vruntime 者。不动记账也不动 floor: 被拣者随即上核运行,
    /// 仍在可运行集中 (语义同 Linux `deactivate_task` 之外的"当前运行任务")。
    pub fn pick_next(&mut self) -> Option<(Pid, u64)> {
        let (&(vruntime, pid), ()) = self.tree.first_key_value()?;
        self.tree.remove(&(vruntime, pid));
        Some((pid, vruntime))
    }

    /// 放回被切走的运行任务: 位置即其自身 vruntime (IC1 已保证它不低于
    /// `floor - MIN_GRANULARITY`), 不动记账。
    pub fn requeue(&mut self, pid: Pid, vruntime: u64) {
        self.tree.insert((vruntime, pid), ());
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
        // IC2: 结构即真相 —— 绝不按并行计数器判定 (旧 `nr_running` 失真是
        // 本核任务隐形饥饿的直接根因).
        self.tree.is_empty()
    }

    pub fn steal_highest_vruntime(&mut self) -> Option<(Pid, u64)> {
        let (&(vruntime, pid), ()) = self.tree.last_key_value()?;
        self.tree.remove(&(vruntime, pid));
        Some((pid, vruntime))
    }
}

// ============================================================================
// Deadline 运行队列 (EDF)
// ============================================================================

/// Deadline 运行队列 — 以 (绝对截止期, pid) 为键, 并跟踪总利用率。
///
/// 可运行性判据与 CFS 同款取结构真相 (IC2: 树即真相, 不并行维护计数器)。
pub struct DlRunQueue {
    pub tree: BTreeMap<(u64, Pid), ()>,
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
            total_utilization: 0,
        }
    }

    pub fn enqueue(&mut self, pid: Pid, deadline_abs: u64, util_pct: u64) -> bool {
        if self.total_utilization.saturating_add(util_pct) > DL_MAX_UTILIZATION_PCT {
            return false;
        }
        self.tree.insert((deadline_abs, pid), ());
        self.total_utilization += util_pct;
        true
    }

    pub fn dequeue(&mut self, pid: Pid, deadline_abs: u64, util_pct: u64) {
        if self.tree.remove(&(deadline_abs, pid)).is_some() {
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
        self.tree.is_empty()
    }

    /// 树上的截止期任务数 (结构真相; IC2)。
    pub fn len(&self) -> usize {
        self.tree.len()
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
    // 让该进程 vruntime 以 1024 倍速度暴涨, 而上核任务的 vr 又直接推进本核
    // `min_vruntime` 下界 (IC3), 等于把同核其他任务的抢占差值无限拉大 ⇒ 集体
    // 饥饿 (历史缺陷根因之一, 与 `init_kernel_process_fields` 漏写 `cfs_weight` 叠加)。
    let w = if weight == 0 { NICE0_WEIGHT } else { weight };
    (NICE0_WEIGHT / w).max(1)
}

/// 判定当前任务是否应被树上最小 vruntime 者抢占。
///
/// 两个入参**必须来自同一时基域** (即都已经 [`CfsRunQueue::placement`] 对齐):
/// `curr_vruntime` 是运行任务欠账, `next_vruntime` 是队列左端。差值超过按权重
/// 缩小的最小粒度即让出。若调用方传入跨域的值, `saturating_sub` 会把负差压成
/// 0 ⇒ 永假 ⇒ 该核永不请求重调度 (S-1 进程饥饿的直接机理)。
#[inline]
pub fn cfs_should_preempt(curr_vruntime: u64, next_vruntime: u64, weight: u64) -> bool {
    let threshold = if weight == 0 {
        MIN_GRANULARITY_TICKS.saturating_mul(NICE0_WEIGHT)
    } else {
        MIN_GRANULARITY_TICKS.saturating_mul(NICE0_WEIGHT) / weight
    };
    curr_vruntime.saturating_sub(next_vruntime) > threshold
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

    /// 6. CfsRunQueue: 入队/拣出/出队 (IC2: 判据一律取自树)
    #[test]
    fn test_sched_cfs_basic_ops() {
        let mut q = CfsRunQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.enqueue(1, 100, 1024, false), 100);
        assert_eq!(q.enqueue(2, 150, 1024, false), 150);
        assert_eq!(q.enqueue(3, 200, 1024, false), 200);
        assert_eq!(q.len(), 3);
        assert_eq!(q.total_weight.load(Ordering::Acquire), 3 * 1024);
        // pick_next: 最小 vruntime 是 100 (PID 1)
        let (pid, vr) = q.pick_next().unwrap();
        assert_eq!(pid, 1);
        assert_eq!(vr, 100);
        // 拣出即离开树 (len 随之下降), 但不动记账: 被拣者随即上核运行,
        // 仍在可运行集中 (IC1: 与队列同域).
        assert_eq!(q.len(), 2);
        assert_eq!(q.total_weight.load(Ordering::Acquire), 3 * 1024);
        // 被切走者经 requeue 原位放回, 同样不动账.
        q.requeue(1, 100);
        assert_eq!(q.len(), 3);
        assert_eq!(q.total_weight.load(Ordering::Acquire), 3 * 1024);
        // dequeue 撤账: 离开可运行集 (阻塞/退出), 且幂等.
        assert!(q.dequeue(1, 1024, true));
        assert!(!q.dequeue(1, 1024, false));
        assert_eq!(q.total_weight.load(Ordering::Acquire), 2 * 1024);
        assert_eq!(q.len(), 2);
        assert_eq!(q.leftmost_vruntime(), Some(150));
    }

    /// 6b. IC2 回归: 可运行性判据不得与树背离 (旧 `nr_running` 计数器失真是
    /// 本核任务隐形饥饿的直接根因).
    #[test]
    fn test_sched_cfs_structural_truth_no_counter_drift() {
        let mut q = CfsRunQueue::new();
        // 历史场景: 任务反复 睡眠→唤醒→入队→拣出. 旧模型每循环净漏 +1
        // (nr=108 而 tree=0), is_empty() 因此长期为真 false, 抢占判据失真.
        for i in 0..100u32 {
            let pid = 10 + i;
            q.enqueue(pid, 0, 1024, false);
            assert!(!q.is_empty());
            let (picked, _vr) = q.pick_next().unwrap();
            assert_eq!(picked, pid);
            // 该任务上核跑完后阻塞离开: 拣出已使它离树, 故此处只撤账
            // (返回 false = 树上无此节点), 权重恰一次性扣回.
            assert!(!q.dequeue(picked, 1024, true));
        }
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert_eq!(q.total_weight.load(Ordering::Acquire), 0);
    }

    /// 6c. IC1 回归: 入队幂等 —— 重复唤醒/迁移回源只移动节点, 不重复记账.
    #[test]
    fn test_sched_cfs_enqueue_is_idempotent() {
        let mut q = CfsRunQueue::new();
        q.enqueue(7, 100, 1024, false);
        // 已在记账中 (accounted=true) 的重复入队 = 移动.
        q.enqueue(7, 200, 1024, true);
        q.enqueue(7, 300, 1024, true);
        assert_eq!(q.len(), 1);
        assert_eq!(q.total_weight.load(Ordering::Acquire), 1024);
        assert_eq!(q.leftmost_vruntime(), Some(300));
    }

    /// 7. CfsRunQueue: calc_time_slice (权重比例)
    #[test]
    fn test_sched_cfs_time_slice() {
        let q = CfsRunQueue::new();
        // total=0 → 返回 MIN_GRANULARITY (避免除零)
        assert_eq!(q.calc_time_slice(1024), MIN_GRANULARITY_TICKS);
        // weight=0 → MIN_GRANULARITY (避免除零)
        let mut q = CfsRunQueue::new();
        q.enqueue(1, 0, 1024, false);
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

    /// 8. IC1 + IC3: 落点钳制与下界单调 (S-1 饿死根的直接回归)
    ///
    /// 回归: 旧 `sync_min_vruntime` 把 floor "重设为树最小键" —— 被钳到 floor 的
    /// 唤醒者反过来推高 floor, 形成棘轮; 而运行任务的 `cfs_vruntime` 从不与
    /// floor 对齐, 两者差值最终被 `saturating_sub` 压成 0 ⇒ 本核永不请求重调度.
    #[test]
    fn test_sched_cfs_placement_and_monotonic_floor() {
        let mut q = CfsRunQueue::new();
        // IC3: floor 只增不减 —— 往低推进必须是 no-op.
        q.advance_floor(1000);
        q.advance_floor(500);
        assert_eq!(q.floor(), 1000);
        // IC1: 远低于 floor 的 vr (睡久了 / boot 期从未落点的任务) 被抬到
        // floor - MIN_GRANULARITY, 而不是原样入队.
        assert_eq!(q.placement(45), 1000 - MIN_GRANULARITY_TICKS);
        let placed = q.enqueue(10, 45, NICE0_WEIGHT, false);
        assert_eq!(placed, 1000 - MIN_GRANULARITY_TICKS);
        // 高于 floor 的值原样保留 (已消耗较多 CPU 的任务应排在后面).
        assert_eq!(q.placement(6000), 6000);
        // 唤醒者落在队列最前 → 下一个拣出点即选中它 (有界延迟).
        assert_eq!(q.leftmost_vruntime(), Some(placed));
        assert_eq!(q.pick_next().unwrap().0, 10);
    }

    /// 8b. 抢占判据必须在同一时基内比较 (旧模型下差值恒 0).
    #[test]
    fn test_sched_cfs_preempt_uses_shared_time_base() {
        let mut q = CfsRunQueue::new();
        q.advance_floor(5152);
        // 旧模型的致命情形: 运行任务 vr=45, 队列 floor=5152 (两个时基域).
        // 新模型由 tick 经 placement 先把运行任务抬进 floor 邻域, 差值即有意义.
        let reconciled = q.placement(45);
        let wakee = q.enqueue(10, 0, NICE0_WEIGHT, false);
        assert_eq!(wakee, reconciled);
        assert!(
            !cfs_should_preempt(reconciled, wakee, NICE0_WEIGHT),
            "刚对齐时差值 0, 不该抢占"
        );
        // 运行任务多跑 MIN_GRANULARITY+1 个 tick 后必须触发抢占 (旧模型永假).
        assert!(
            cfs_should_preempt(reconciled + MIN_GRANULARITY_TICKS + 1, wakee, NICE0_WEIGHT),
            "同域比较下欠账超过最小粒度即应让出"
        );
        assert!(
            !cfs_should_preempt(45, 5152, NICE0_WEIGHT),
            "跨域差值被 saturating_sub 压成 0 —— 这正是必须经 placement 对齐的理由"
        );
    }

    /// 8c. 端到端时基仿真: S-1 故障场景 (CPU 密集任务在核上长跑, 交互任务在
    /// floor 已推进很远之后才唤醒) 下, 唤醒者必须在**有界 tick** 内被拣出.
    ///
    /// 与 `scheduler.rs` 的实际路径同构: tick 内 `placement` 对齐 → 累加 delta →
    /// `advance_floor` → 与 `leftmost_vruntime()` 同域比较 → 抢占时 `requeue` 原位
    /// 放回 + `pick_next` 拣新者. 旧模型在此场景下差值被 `saturating_sub` 压成 0
    /// ⇒ 该核永不请求重调度 ⇒ 唤醒者永久饿死 (实测 4~10 s, 跨过宿主 5 s 超时).
    #[test]
    fn test_sched_cfs_wakeup_bounded_latency_under_tick_loop() {
        let mut q = CfsRunQueue::new();
        // 后台 CPU 密集任务入队后立即上核运行 (树中不保留, 但仍在记账中).
        q.enqueue(1, 0, NICE0_WEIGHT, false);
        let (running, vr) = q.pick_next().unwrap();
        assert_eq!(running, 1);
        let mut curr_vr = vr;
        let mut wake_tick = 0u64;
        let mut picked_wakee = false;

        for tick in 1..=2000u64 {
            // tick 路径: 运行任务时基累加 + 推进队列下界 (IC1 + IC3)
            let base = q.placement(curr_vr);
            curr_vr = base.saturating_add(calc_vruntime_delta(NICE0_WEIGHT));
            q.advance_floor(curr_vr);

            if tick == 1000 {
                // 交互任务唤醒: 自身 vr 仍停留在很久以前 (旧模型的两个时基域)
                let placed = q.enqueue(2, 0, NICE0_WEIGHT, false);
                assert!(
                    placed >= q.floor().saturating_sub(MIN_GRANULARITY_TICKS),
                    "IC1: 唤醒落点必须收进 floor 邻域"
                );
                assert_eq!(q.leftmost_vruntime(), Some(placed));
            }

            // 抢占判定 (IC1: 两者同域)
            let Some(next_vr) = q.leftmost_vruntime() else {
                continue;
            };
            if cfs_should_preempt(curr_vr, next_vr, NICE0_WEIGHT) {
                wake_tick = tick;
                let bg_vr = curr_vr;
                // 切走者原位放回, 唤醒者上核
                q.requeue(1, bg_vr);
                let (picked, _) = q.pick_next().unwrap();
                assert_eq!(picked, 2, "唤醒者必须被拣出 (反饥饿)");
                picked_wakee = true;
                // 交互任务跑一个 tick 后主动阻塞: 它正在核上 (已不在树上),
                // 故出队只撤账 —— 返回 false 是结构真相的体现 (IC2).
                assert!(
                    !q.dequeue(2, NICE0_WEIGHT, true),
                    "核上任务出树不入队, 只撤账"
                );
                // 后台任务重新成为运行者 (仍在记账中, 移动节点不动账)
                q.enqueue(1, bg_vr, NICE0_WEIGHT, true);
                let (r, v) = q.pick_next().unwrap();
                assert_eq!(r, 1);
                assert_eq!(v, bg_vr, "回放的后台任务必须回到原时基位置 (IC3 不倒退)");
                break;
            }
        }

        assert!(picked_wakee, "2000 tick 内唤醒者从未被拣出 = S-1 饿死重现");
        assert!(
            wake_tick <= 1000 + MIN_GRANULARITY_TICKS + 2,
            "唤醒到抢占的延迟必须有界 (实得 tick={wake_tick})"
        );
        // 记账守恒: 只剩后台任务一个可运行主体
        assert_eq!(q.total_weight.load(Ordering::Acquire), NICE0_WEIGHT);
        // IC3: floor 只增不减, 且不小于运行任务的时基
        assert!(q.floor() >= 1000);
        assert_eq!(q.len(), 0, "树上只剩正在运行的任务 (已拣出) 则为空");
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
        // 加入 4 个进程, 不同 vruntime + weight (均不低于 floor - MIN_GRANULARITY,
        // 不发生入队侧钳制, 以便校验按 vruntime 排序).
        q.enqueue(10, 100, 1024, false);
        q.enqueue(20, 150, 2048, false); // 更高权重
        q.enqueue(30, 250, 1024, false);
        q.enqueue(40, 200, 1024, false);
        assert_eq!(q.total_weight.load(Ordering::Acquire), 1024 * 3 + 2048);
        // 按 vruntime 顺序调度
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 10); // vruntime=100
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 20); // vruntime=150
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 40); // vruntime=200
        let (pid, _) = q.pick_next().unwrap();
        assert_eq!(pid, 30); // vruntime=250
        // IC2: 队列空以树为准 —— 拣完即为空, 不残留任何平行计数.
        assert!(q.pick_next().is_none());
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }
}
