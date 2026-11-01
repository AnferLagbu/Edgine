#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! CFS (Completely Fair Scheduler) — 调度策略 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 策略代码 (权重表 + vruntime 计算 + 时间片计算 + CFS/DL 运行队列) 于
//! T1-1 (2026-06-16) 迁至 `functions::proc::sched_policy`, 本文件仅
//! re-export。按统一判据反转：CfsRunQueue/DlRunQueue 是 privileged scheduler
//! 的运行队列机制状态, 权重/vruntime/抢占判定全部操作队列内部字段, 无法
//! 脱离机制状态独立存在 (同 DECISION-M `sys_pm_dispatch` 判据: 算法本质是
//! 机制状态操作) — 迁回。依赖闭包仅 privileged。
//!
//! functions 侧改 `pub use crate::privileged::proc::cfs::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。

use alloc::collections::{BTreeMap, BTreeSet};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::proc::Pid;

// ============================================================================
// CFS Constants
// ============================================================================

// DECISION-O ②: CFS_* 权威归 privileged/config/sched (机制常量), functions 侧
// 纯 re-export — cfs 依赖收敛单向 (本文件零 functions 引用)
pub use crate::privileged::config::{
    CFS_DL_MAX_UTILIZATION_PCT as DL_MAX_UTILIZATION_PCT, CFS_DL_MIN_PERIOD as DL_MIN_PERIOD_TICKS,
    CFS_DL_MIN_RUNTIME as DL_MIN_RUNTIME_TICKS, CFS_MIN_GRANULARITY as MIN_GRANULARITY_TICKS,
    CFS_NICE0_WEIGHT as NICE0_WEIGHT, CFS_TARGET_LATENCY as TARGET_LATENCY_TICKS,
    CFS_VIRT_ONE as VIRT_ONE, CFS_VIRT_SHIFT as VIRT_SHIFT,
};

/// 负载均衡触发阈值 (权重差)
pub const LOAD_BALANCE_THRESHOLD: u64 = 1024;

/// 目标延迟的 Q16.16 表示 (EEVDF 虚拟时间域).
pub const TARGET_LATENCY_VIRT: u64 = TARGET_LATENCY_TICKS << VIRT_SHIFT;

/// 最小粒度的 Q16.16 表示 (EEVDF 虚拟时间域).
pub const MIN_GRANULARITY_VIRT: u64 = MIN_GRANULARITY_TICKS << VIRT_SHIFT;

// ============================================================================
// NICE → 权重映射
// ============================================================================

/// nice 值 (-20..=19) 到 CFS 权重的查表 (索引 = nice + 20)
pub const NICE_TO_WEIGHT: [u64; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

/// 将 nice 值 (-20..=19) 映射为其 CFS 调度权重; 越界值收敛到端点。
#[inline]
#[must_use]
pub fn nice_to_weight(nice: i8) -> u64 {
    let clamped = nice.clamp(-20, 19);
    let idx = crate::privileged::glue::usize_from_i8(clamped + 20);
    NICE_TO_WEIGHT[idx]
}

/// 将 CFS 权重反查为最接近的 nice 值 (-20..=19)。
#[inline]
#[must_use]
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
            best = crate::privileged::glue::low_i8_from_i32(
                crate::privileged::glue::low_i32_from_usize(i) - 20,
            );
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
    /// 每周期运行预算 (tick)
    pub runtime: u64,
    /// 相对截止期 (tick)
    pub deadline: u64,
    /// 调度周期 (tick)
    pub period: u64,
}

impl Default for DeadlineParams {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadlineParams {
    /// 构造全零参数 (初始为无效状态)
    #[must_use]
    pub const fn new() -> Self {
        Self {
            runtime: 0,
            deadline: 0,
            period: 0,
        }
    }

    /// 校验参数是否满足 EDF + CBS 约束
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.runtime >= DL_MIN_RUNTIME_TICKS
            && self.deadline >= self.runtime
            && self.period >= self.deadline
            && self.period >= DL_MIN_PERIOD_TICKS
    }

    /// 计算利用率百分比 (`runtime / period * 100`)
    #[must_use]
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

/// 队列内任务的记账副本 (权重 + 落点)。
///
/// 与 `tree` 键 / 运行账分开保存: 键只承载排序所需的 `(vruntime, pid)`，权重与
/// 落点由账目持有。出队时按**账目**中的权重扣减，调用方传入的陈旧权重不参与运算
/// (排队期间 nice 变更不会让 `W` 漂移)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedTask {
    /// 任务号。
    pub pid: Pid,
    /// 记账落点 (vruntime; 单位随调用方口径，同一队列内必须同域)。
    pub vruntime: u64,
    /// 记账权重。
    pub weight: u64,
}

/// EEVDF 选择所需的当前运行任务视图。
///
/// `current` 通常已在队列运行账中 (W/S 已含它), 此时本视图只用于提供最新的
/// vruntime / 截止期; 若它**不在**任何账目中 (boot 期经 `keep_current` 上核),
/// [`CfsRunQueue::pick_eevdf`] 会把这份权重临时并入 `W`/`S`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EevdfTaskView {
    /// 任务号。
    pub pid: Pid,
    /// 当前虚拟运行时间 (与队列同域)。
    pub vruntime: u64,
    /// 本轮虚拟截止期。
    pub vdeadline: u64,
    /// 调度权重。
    pub weight: u64,
}

/// EEVDF 选择结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EevdfPick {
    /// 被选中的任务号。
    pub pid: Pid,
    /// 该任务的虚拟运行时间。
    pub vruntime: u64,
    /// 该任务的虚拟截止期。
    pub vdeadline: u64,
    /// `true` = 胜者来自树账, 已被移入运行账 (调用方须处理旧 current);
    /// `false` = current 胜出, 账目未动。
    pub from_tree: bool,
}

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
/// ## EEVDF 账目分解 (E-03)
///
/// 可运行集拆成两份**互斥**账目:
///
/// - **树账**: 在 [`Self::tree`] 上等待的任务; `tree_weight` / `tree_sum_w_vr`;
/// - **运行账**: 已被 [`Self::pick_next`] / [`Self::pick_eevdf`] 拣出、正在本核
///   运行的任务; `running_weight` / `running_sum_w_vr`。
///
/// 由此得到 EEVDF 的两个对账量: `W = tree_weight + running_weight`,
/// `S = tree_sum_w_vr + running_sum_w_vr`, `V = floor(S / W)`。
///
/// `total_weight` 只是 `W` 的对外缓存: 每次结构变更后由 `sync_total_weight` 从
/// 账目重算, **不再**由调用方按传入权重增减。旧模型按调用方权重扣账, 排队期间
/// nice 变更或陈旧凭证都会让 `W` 漂移; 结构记账使这一点在构造上不可能。
///
/// IE2 (账目完备) 由三类写路径共同维持: 入队只落树账, 拣出把同一笔账从树移到
/// 运行账, 出队从两账之一摘除。`enqueue` / `dequeue` 先按 pid 摘除任意既有记账
/// 再动作, 因此重复调用幂等, 且同一任务不会同时出现在两账里。
pub struct CfsRunQueue {
    /// 可运行任务树, 键 = (vruntime, pid) (IC2 唯一真相)。
    ///
    /// 只读视图: 所有写路径必须经本 impl, 否则结构与账目立即背离。
    pub tree: BTreeSet<(u64, Pid)>,
    /// 本核 vruntime 下界 (IC3: 单调不减)。字段刻意私有 —— 只允许经
    /// [`Self::floor`] 读、[`Self::advance_floor`] 写, 防止调用点重新引入
    /// "重设"语义。
    min_vruntime: AtomicU64,
    /// 可运行集权重和 `W` 的对外缓存 (由账目派生, 只读)。
    pub total_weight: AtomicU64,
    /// 树账: pid → 记账副本。
    tree_ledger: BTreeMap<Pid, QueuedTask>,
    /// 树账权重和。
    tree_weight: u64,
    /// 树账加权虚拟时间 `Σ w * vr`。
    tree_sum_w_vr: u128,
    /// 运行账: 已被拣出、正在本核运行的任务。
    running_ledger: BTreeMap<Pid, QueuedTask>,
    /// 运行账权重和。
    running_weight: u64,
    /// 运行账加权虚拟时间 `Σ w * vr`。
    running_sum_w_vr: u128,
}

impl Default for CfsRunQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl CfsRunQueue {
    /// 构造空的 CFS 运行队列
    #[must_use]
    pub fn new() -> Self {
        Self {
            tree: BTreeSet::new(),
            min_vruntime: AtomicU64::new(0),
            total_weight: AtomicU64::new(0),
            tree_ledger: BTreeMap::new(),
            tree_weight: 0,
            tree_sum_w_vr: 0,
            running_ledger: BTreeMap::new(),
            running_weight: 0,
            running_sum_w_vr: 0,
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

    /// 树上最小 vruntime (即下一次 [`Self::pick_next`] 的候选)。
    ///
    /// E-04 起生产 tick / 选择路径改由切片与 [`Self::pick_eevdf`] 判定, 本访问器
    /// 保留为队列结构断言与诊断入口 (不承载抢占判据)。
    #[inline]
    pub fn leftmost_vruntime(&self) -> Option<u64> {
        self.tree.first().map(|&(vr, _)| vr)
    }

    /// 树上的可运行任务数 (不含正在本核运行的任务; IC2 唯一真相)。
    #[inline]
    pub fn len(&self) -> usize {
        self.tree.len()
    }

    /// 运行账中的任务数 (正在本核运行, 已离树但仍在可运行集)。
    #[inline]
    pub fn running_count(&self) -> usize {
        self.running_ledger.len()
    }

    /// `pid` 当前是否在树账中。
    #[inline]
    pub fn is_on_tree(&self, pid: Pid) -> bool {
        self.tree_ledger.contains_key(&pid)
    }

    /// `pid` 当前是否在运行账中。
    #[inline]
    pub fn is_running(&self, pid: Pid) -> bool {
        self.running_ledger.contains_key(&pid)
    }

    /// 读取 `pid` 的树账副本 (落点 + 记账权重)。
    #[inline]
    pub fn tree_task(&self, pid: Pid) -> Option<QueuedTask> {
        self.tree_ledger.get(&pid).copied()
    }

    /// 读取 `pid` 的运行账副本 (落点 + 记账权重)。
    #[inline]
    pub fn running_task(&self, pid: Pid) -> Option<QueuedTask> {
        self.running_ledger.get(&pid).copied()
    }

    /// 树账权重和。
    #[inline]
    pub fn tree_weight(&self) -> u64 {
        self.tree_weight
    }

    /// 树账加权虚拟时间 `Σ w * vr`。
    #[inline]
    pub fn tree_sum_w_vr(&self) -> u128 {
        self.tree_sum_w_vr
    }

    /// 运行账权重和。
    #[inline]
    pub fn running_weight(&self) -> u64 {
        self.running_weight
    }

    /// 运行账加权虚拟时间 `Σ w * vr`。
    #[inline]
    pub fn running_sum_w_vr(&self) -> u128 {
        self.running_sum_w_vr
    }

    /// 可运行集加权虚拟时间 `S = 树账 + 运行账`。
    #[inline]
    pub fn sum_w_vr(&self) -> u128 {
        self.tree_sum_w_vr.saturating_add(self.running_sum_w_vr)
    }

    /// 可运行集权重和 `W = 树账 + 运行账` (EEVDF 的派生真值)。
    #[inline]
    pub fn runnable_weight(&self) -> u64 {
        self.tree_weight.saturating_add(self.running_weight)
    }

    /// 本核加权平均虚拟时间 `V = floor(S / W)`; `W == 0` 时为 0。
    #[inline]
    pub fn avg_vruntime(&self) -> u64 {
        weighted_avg_vruntime(self.runnable_weight(), self.sum_w_vr())
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
    ///
    /// E-04 起本队列全链路使用 Q16.16 虚拟时间 (E-01 冻结口径), 钳制量取
    /// [`MIN_GRANULARITY_VIRT`] —— 与 `floor` / 任务 `vr` 同域。
    #[inline]
    pub fn placement(&self, vruntime: u64) -> u64 {
        vruntime.max(self.floor().saturating_sub(MIN_GRANULARITY_VIRT))
    }

    /// 入队 (**幂等**): 把 `pid` 放到 IC1 钳制后的位置, 返回该位置。
    ///
    /// 记账以**结构真相**为准: 无论 `accounted` 为何, 先按 pid 摘除树账与运行账中
    /// 的任意既有记录再重投。于是重复唤醒 / 迁移回源只是移动节点, 天然幂等;
    /// "凭证为 true 但历史记账已漏"的状态也会被这次重投修复。
    ///
    /// `accounted` 是调用方持有的 `Process::cfs_on_rq` 凭证, 保留在签名里使调用点
    /// 的语义显式 (真正的陈旧凭证判定在 [`Self::dequeue`])。
    ///
    /// 调用方**必须**把返回值回写 `Process::cfs_vruntime`, 否则 IC1 不成立。
    pub fn enqueue(&mut self, pid: Pid, vruntime: u64, weight: u64, accounted: bool) -> u64 {
        let _ = accounted;
        let _ = self.remove_tree_task(pid);
        let _ = self.remove_running_task(pid);
        let placed = self.placement(vruntime);
        self.record_tree(QueuedTask {
            pid,
            vruntime: placed,
            weight,
        });
        self.sync_total_weight();
        placed
    }

    /// 出队: 让 `pid` 离开可运行集 (阻塞 / 退出 / 被拣出但不可调度)。
    ///
    /// 扣减量一律取**账目中的权重**; `weight` 参数可能在排队期间因 nice 变更而过期,
    /// 用过期值扣账正是旧模型 `total_weight` 漂移的来源。返回该 pid 当时是否在树上。
    ///
    /// 幂等: 结构上已无可摘除的记录时不二次扣账。`accounted == true` 却查无此人
    /// 即陈旧凭证 —— 仍按结构真相 no-op, 只把派生缓存重新对齐一次。
    pub fn dequeue(&mut self, pid: Pid, weight: u64, accounted: bool) -> bool {
        let _ = weight;
        let was_on_tree = self.remove_tree_task(pid).is_some();
        let was_running = self.remove_running_task(pid).is_some();
        if was_on_tree || was_running {
            self.sync_total_weight();
            return was_on_tree;
        }
        if accounted {
            self.sync_total_weight();
        }
        false
    }

    /// 在 rq 锁内变更排队任务的权重 (E-02 的 `set_nice` 接线入口)。
    ///
    /// 只改账目权重, 不改落点与树键 —— 相对次序由 vruntime 决定, 权重只影响后续
    /// 的 `Δvr` / 切片与 `V`。`pid` 不在可运行集中时返回 `false` (不产生幽灵账)。
    pub fn reweight(&mut self, pid: Pid, weight: u64) -> bool {
        if let Some(old) = self.tree_ledger.get(&pid).copied() {
            self.tree_weight = self
                .tree_weight
                .saturating_sub(old.weight)
                .saturating_add(weight);
            self.tree_sum_w_vr = self
                .tree_sum_w_vr
                .saturating_sub(u128::from(old.weight) * u128::from(old.vruntime))
                .saturating_add(u128::from(weight) * u128::from(old.vruntime));
            self.tree_ledger.insert(pid, QueuedTask { weight, ..old });
            self.sync_total_weight();
            return true;
        }
        if let Some(old) = self.running_ledger.get(&pid).copied() {
            self.running_weight = self
                .running_weight
                .saturating_sub(old.weight)
                .saturating_add(weight);
            self.running_sum_w_vr = self
                .running_sum_w_vr
                .saturating_sub(u128::from(old.weight) * u128::from(old.vruntime))
                .saturating_add(u128::from(weight) * u128::from(old.vruntime));
            self.running_ledger
                .insert(pid, QueuedTask { weight, ..old });
            self.sync_total_weight();
            return true;
        }
        false
    }

    /// 推进运行账中任务的 vruntime (tick 记账入口, E-04 接线)。
    ///
    /// 只更新运行账的加权和, `W` 不变。vruntime 回退返回 `false` 且不改任何账目:
    /// 运行任务的虚拟时间只增不减。
    pub fn advance_running_vruntime(&mut self, pid: Pid, vruntime: u64) -> bool {
        let Some(old) = self.running_ledger.get(&pid).copied() else {
            return false;
        };
        if vruntime < old.vruntime {
            return false;
        }
        if vruntime == old.vruntime {
            return true;
        }
        self.running_sum_w_vr = self
            .running_sum_w_vr
            .saturating_sub(u128::from(old.weight) * u128::from(old.vruntime))
            .saturating_add(u128::from(old.weight) * u128::from(vruntime));
        self.running_ledger
            .insert(pid, QueuedTask { vruntime, ..old });
        true
    }

    /// 拣出最小 vruntime 者。把同一笔账从树账**移入**运行账 (W/S 不变),
    /// 语义同 Linux 的"当前运行任务仍属可运行集"。
    pub fn pick_next(&mut self) -> Option<(Pid, u64)> {
        let pid = self.tree.first()?.1;
        let task = self.activate_tree_task(pid)?;
        Some((task.pid, task.vruntime))
    }

    /// EEVDF 选择 (D7: 单树合格前缀 + 线性 `(vd, pid)` 选择)。
    ///
    /// 选择规则 (与 `host-tests/src/eevdf_model.rs` 冻结口径逐项一致):
    ///
    /// 1. 由本队列账目算 `W` / `S` / `V = floor(S / W)`。`current` 若**未**在本队列
    ///    记账 (树账与运行账都没有它), 其 `weight` 与 `weight * vruntime` 临时并入
    ///    `W` / `S` —— 覆盖 boot 期经 `keep_current` 上核、从未入队的任务。
    /// 2. 合格集合 = `vr <= V` 的树内前缀 + 合格的 `current`。
    /// 3. 取 `(vdeadline, pid)` 最小者; 树内任务的截止期由 `vdeadline_of` 提供,
    ///    缺省 (未建账) 按 `vd = vruntime` 参与选择。
    /// 4. 无合格者 (账目完备时不可达) 回退: `current.vr <= 树左端` 保持 current,
    ///    否则拣树左端。
    ///
    /// 树内胜者会被移入运行账 (`from_tree == true`); `current` 胜出时只回报, 不动
    /// 账目 (`from_tree == false`)。调用方在 `from_tree == true` 时必须自行把旧
    /// `current` 出队或重投 —— 本函数不知道调用方以何种策略处理被切走者。
    pub fn pick_eevdf(
        &mut self,
        current: Option<EevdfTaskView>,
        mut vdeadline_of: impl FnMut(Pid) -> Option<u64>,
    ) -> Option<EevdfPick> {
        let mut weight = self.runnable_weight();
        let mut weighted_sum = self.sum_w_vr();
        if let Some(view) = current
            && !self.tree_ledger.contains_key(&view.pid)
            && !self.running_ledger.contains_key(&view.pid)
        {
            weight = weight.saturating_add(view.weight);
            weighted_sum =
                weighted_sum.saturating_add(u128::from(view.weight) * u128::from(view.vruntime));
        }
        let avg = weighted_avg_vruntime(weight, weighted_sum);

        // 合格树前缀: vr <= V 即 vr*W <= S (V 是 floor(S/W), 前缀判定无需再乘).
        let mut best: Option<(u64, Pid)> = None;
        if weight != 0 {
            for &(vruntime, pid) in self.tree.range(..=(avg, Pid::MAX)) {
                let vdeadline = vdeadline_of(pid).unwrap_or(vruntime);
                let candidate = (vdeadline, pid);
                if best.is_none_or(|winner| candidate < winner) {
                    best = Some(candidate);
                }
            }
        }
        if let Some(view) = current
            && !self.tree_ledger.contains_key(&view.pid)
            && is_eligible(view.vruntime, weight, weighted_sum)
        {
            let candidate = (view.vdeadline, view.pid);
            if best.is_none_or(|winner| candidate < winner) {
                best = Some(candidate);
            }
        }

        if let Some((vdeadline, pid)) = best {
            if let Some(view) = current
                && view.pid == pid
            {
                return Some(EevdfPick {
                    pid,
                    vruntime: view.vruntime,
                    vdeadline,
                    from_tree: false,
                });
            }
            let task = self.activate_tree_task(pid)?;
            return Some(EevdfPick {
                pid,
                vruntime: task.vruntime,
                vdeadline,
                from_tree: true,
            });
        }

        // 防御性回退: 账目完备时合格集合非空 (min(vr) <= V), 故本支只承接空集
        // 与非法权重 (w == 0) 两种退化状态.
        let Some((leftmost_vr, leftmost_pid)) = self.tree.first().copied() else {
            return current.map(|view| EevdfPick {
                pid: view.pid,
                vruntime: view.vruntime,
                vdeadline: view.vdeadline,
                from_tree: false,
            });
        };
        if let Some(view) = current
            && view.vruntime <= leftmost_vr
        {
            return Some(EevdfPick {
                pid: view.pid,
                vruntime: view.vruntime,
                vdeadline: view.vdeadline,
                from_tree: false,
            });
        }
        let vdeadline = vdeadline_of(leftmost_pid).unwrap_or(leftmost_vr);
        let task = self.activate_tree_task(leftmost_pid)?;
        Some(EevdfPick {
            pid: leftmost_pid,
            vruntime: task.vruntime,
            vdeadline,
            from_tree: true,
        })
    }

    /// 按 vruntime 升序把至多 `out.len()` 个**在树上的** pid 写入 `out`, 返回个数。
    ///
    /// 不改动树也不动记账 —— 跨核偷取需要"先定候选、逐个认领归属、再在**同一段
    /// 持锁期**里搬移", 而 [`Self::pick_next`] 一拣就离树, 无法表达"认领失败就放过
    /// 它"。调用方必须已持有本队列的锁 (候选在锁内才保持有效)。
    pub fn leftmost_pids(&self, out: &mut [Pid]) -> usize {
        let mut n = 0usize;
        for key in &self.tree {
            if n >= out.len() {
                break;
            }
            out[n] = key.1;
            n += 1;
        }
        n
    }

    /// 读取可运行集权重和 (负载均衡判据; 由账目派生)。
    pub fn get_weighted_load(&self) -> u64 {
        self.runnable_weight()
    }

    /// 树上是否无任务 (IC2: 结构即真相)
    pub fn is_empty(&self) -> bool {
        // IC2: 结构即真相 —— 绝不按并行计数器判定 (旧 `nr_running` 失真是
        // 本核任务隐形饥饿的直接根因).
        self.tree.is_empty()
    }

    /// 把 `W` 缓存重新对齐到账目真值。
    fn sync_total_weight(&self) {
        self.total_weight.store(
            self.tree_weight.saturating_add(self.running_weight),
            Ordering::Release,
        );
    }

    /// 把一笔账记入树账 (调用方保证该 pid 不在树账/运行账中)。
    fn record_tree(&mut self, task: QueuedTask) {
        self.tree.insert((task.vruntime, task.pid));
        self.tree_weight = self.tree_weight.saturating_add(task.weight);
        self.tree_sum_w_vr = self
            .tree_sum_w_vr
            .saturating_add(u128::from(task.weight) * u128::from(task.vruntime));
        self.tree_ledger.insert(task.pid, task);
    }

    /// 把一笔账记入运行账 (调用方保证该 pid 不在运行账中)。
    fn record_running(&mut self, task: QueuedTask) {
        self.running_weight = self.running_weight.saturating_add(task.weight);
        self.running_sum_w_vr = self
            .running_sum_w_vr
            .saturating_add(u128::from(task.weight) * u128::from(task.vruntime));
        self.running_ledger.insert(task.pid, task);
    }

    /// 按 pid 摘除树账 (节点 + 权重 + 加权和), 返回原账目副本。
    fn remove_tree_task(&mut self, pid: Pid) -> Option<QueuedTask> {
        let task = self.tree_ledger.remove(&pid)?;
        self.tree.remove(&(task.vruntime, pid));
        self.tree_weight = self.tree_weight.saturating_sub(task.weight);
        self.tree_sum_w_vr = self
            .tree_sum_w_vr
            .saturating_sub(u128::from(task.weight) * u128::from(task.vruntime));
        Some(task)
    }

    /// 按 pid 摘除运行账, 返回原账目副本。
    fn remove_running_task(&mut self, pid: Pid) -> Option<QueuedTask> {
        let task = self.running_ledger.remove(&pid)?;
        self.running_weight = self.running_weight.saturating_sub(task.weight);
        self.running_sum_w_vr = self
            .running_sum_w_vr
            .saturating_sub(u128::from(task.weight) * u128::from(task.vruntime));
        Some(task)
    }

    /// 拣出树账中的 `pid`: 同一笔账从树移到运行账 (`W` / `S` 不变)。
    fn activate_tree_task(&mut self, pid: Pid) -> Option<QueuedTask> {
        let task = self.remove_tree_task(pid)?;
        let _ = self.remove_running_task(pid);
        self.record_running(task);
        self.sync_total_weight();
        Some(task)
    }
}

// ============================================================================
// Deadline 运行队列 (EDF)
// ============================================================================

/// Deadline 运行队列 — 以 (绝对截止期, pid) 为键, 并跟踪总利用率。
///
/// 可运行性判据与 CFS 同款取结构真相 (IC2: 树即真相, 不并行维护计数器)。
pub struct DlRunQueue {
    /// 截止期任务树, 键 = (绝对截止期, pid)
    pub tree: BTreeSet<(u64, Pid)>,
    /// 总利用率 (百分比之和)
    pub total_utilization: u64,
}

impl Default for DlRunQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl DlRunQueue {
    /// 构造空的 Deadline 运行队列
    #[must_use]
    pub fn new() -> Self {
        Self {
            tree: BTreeSet::new(),
            total_utilization: 0,
        }
    }

    /// 入队: 利用率超限时拒绝并返回 `false`, 否则插入并累加利用率
    pub fn enqueue(&mut self, pid: Pid, deadline_abs: u64, util_pct: u64) -> bool {
        if self.total_utilization.saturating_add(util_pct) > DL_MAX_UTILIZATION_PCT {
            return false;
        }
        self.tree.insert((deadline_abs, pid));
        self.total_utilization += util_pct;
        true
    }

    /// 出队: 仅在树中命中时扣减利用率
    pub fn dequeue(&mut self, pid: Pid, deadline_abs: u64, util_pct: u64) {
        if self.tree.remove(&(deadline_abs, pid)) {
            self.total_utilization = self.total_utilization.saturating_sub(util_pct);
        }
    }

    /// 拣出最早截止期任务, 返回 (pid, 绝对截止期)
    pub fn pick_next(&mut self) -> Option<(Pid, u64)> {
        let &(dl_abs, pid) = self.tree.first()?;
        self.tree.remove(&(dl_abs, pid));
        Some((pid, dl_abs))
    }

    /// 重新插入任务到新的绝对截止期 (周期续期)
    pub fn reinsert(&mut self, pid: Pid, dl_abs: u64) {
        self.tree.insert((dl_abs, pid));
    }

    /// 树上是否无截止期任务
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    /// 树上的截止期任务数 (结构真相; IC2)。
    #[must_use]
    pub fn len(&self) -> usize {
        self.tree.len()
    }
}

// ============================================================================
// EEVDF 纯函数 (E-03; 口径冻结于 E-01 参考模型)
// ============================================================================

/// 单 tick 虚拟时间增量 `Δvr = max(1, (NICE0 << 16) / w)`; `w == 0` 按 NICE0 退化。
///
/// 生产 tick 的唯一增量来源 (E-04 起 `cfs_vruntime` 即 Q16.16, 不再有整数域
/// 换算桥): nice -20..19 全 40 档的增量严格随权重递减。
#[inline]
#[must_use]
pub fn calc_vruntime_delta_virt(weight: u64) -> u64 {
    let effective = if weight == 0 { NICE0_WEIGHT } else { weight };
    ((NICE0_WEIGHT << VIRT_SHIFT) / effective).max(1)
}

/// 默认真实切片 (tick): `max(MIN, min(TARGET, TARGET * w / W_inc))`。
///
/// `W_inc` 必须**已包含本任务** (Wake 用 `W_before + w`, Requeue 用当前已含本任务
/// 的 `W`); `w == 0` 或 `W_inc == 0` 退化为最小粒度。
#[inline]
#[must_use]
pub fn default_slice_real(weight: u64, weight_incl: u64) -> u64 {
    if weight == 0 || weight_incl == 0 {
        return MIN_GRANULARITY_TICKS;
    }
    let fair = TARGET_LATENCY_TICKS.saturating_mul(weight) / weight_incl;
    fair.clamp(MIN_GRANULARITY_TICKS, TARGET_LATENCY_TICKS)
}

/// 虚拟切片 `slice_virt = Δvr(w) * slice_real` —— 与逐 tick 记账严格对齐
/// (切片到期时 `vr` 恰好推进一个 `slice_virt`)。
#[inline]
#[must_use]
pub fn slice_virt_of(slice_real: u64, weight: u64) -> u64 {
    calc_vruntime_delta_virt(weight).saturating_mul(slice_real)
}

/// D8③ 请求切片统一夹取到 `[MIN_GRANULARITY, TARGET_LATENCY]`。
#[inline]
#[must_use]
pub fn clamp_requested_slice(requested: u64) -> u64 {
    requested.clamp(MIN_GRANULARITY_TICKS, TARGET_LATENCY_TICKS)
}

/// 建账时的真实切片: 有请求用请求 (夹取后), 否则用默认公式。
#[inline]
#[must_use]
pub fn effective_slice_real(weight: u64, weight_incl: u64, requested: Option<u64>) -> u64 {
    requested.map_or_else(
        || default_slice_real(weight, weight_incl),
        clamp_requested_slice,
    )
}

/// 加权平均虚拟时间 `V = floor(S / W)`; `W == 0` 返回 0, 商溢出饱和到 `u64::MAX`。
#[inline]
#[must_use]
pub fn weighted_avg_vruntime(total_weight: u64, sum_w_vr: u128) -> u64 {
    if total_weight == 0 {
        return 0;
    }
    u64::try_from(sum_w_vr / u128::from(total_weight)).unwrap_or(u64::MAX)
}

/// 合格判定 `vr * W <= S` (u128 交叉乘法, 避免除法舍入改变符号)。
#[inline]
#[must_use]
pub fn is_eligible(vr: u64, total_weight: u64, sum_w_vr: u128) -> bool {
    if total_weight == 0 {
        return false;
    }
    u128::from(vr) * u128::from(total_weight) <= sum_w_vr
}

/// 虚拟截止期 `vd = vr + slice_virt`。
#[inline]
#[must_use]
pub fn virtual_deadline(vruntime: u64, slice_virt: u64) -> u64 {
    vruntime.saturating_add(slice_virt)
}

/// Wake 落点: `max(vr_raw, V_before - slice_virt)`。
///
/// 睡眠信用上界恰好一个虚拟切片; `vr_raw > V_before` 时保留负 lag (欠账),
/// 不为睡眠时间制造信用。
#[inline]
#[must_use]
pub fn wake_vruntime(vr_raw: u64, avg_before: u64, slice_virt: u64) -> u64 {
    vr_raw.max(avg_before.saturating_sub(slice_virt))
}

// ============================================================================
// EEVDF 入队语义三分 (E-05a)
// ============================================================================

/// 入队语义: 决定落点钳制与切片是否重建。
///
/// 三分口径冻结于 E-01 参考模型: `Wake` 重置切片并钳制睡眠信用; `Requeue`
/// 只在本轮切片耗尽时补充; `Migrate` 原样携带全部字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementKind {
    /// 唤醒 / 新任务: 睡眠信用钳制 + 重建切片与截止期。
    Wake,
    /// 原样重投 (prev 收尾 / 屏障补投): 切片未耗尽时携带原值, 耗尽时重建。
    Requeue,
    /// 跨核迁移: 原样携带 `vr` / 切片 / `vd`, 不钳制也不重建。
    Migrate,
}

/// 入队计划: 落点与需要重建的切片。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementPlan {
    /// 落点 (尚未经队列 floor 钳制)。
    pub vruntime: u64,
    /// `Some((slice_real, slice_virt))` = 重建本轮切片; `None` = 携带原值。
    pub slice: Option<(u64, u64)>,
}

/// 目的队列的入队前账目视图 (E-05a)。
#[derive(Debug, Clone, Copy)]
pub struct PlacementLedger {
    /// 可运行集权重和 `W` (树账 + 运行账)。
    pub total_weight: u64,
    /// 加权虚拟时间和 `S`。
    pub sum_w_vr: u128,
    /// 本任务是否已计入上述账目。
    ///
    /// 已计入者重建切片时**不得**重复加权重 (`W_inc` 虚增会压小切片)。
    pub includes_self: bool,
}

/// 按 [`PlacementKind`] 计算入队落点与切片重建 (E-01 冻结口径)。
///
/// `requested` 为 D8③ 请求切片 (调用方须把 0 过滤为 `None`); `ledger` 取
/// **目的队列**本次入队前的读数。
#[must_use]
pub fn plan_placement(
    kind: PlacementKind,
    vruntime: u64,
    weight: u64,
    slice_left: u64,
    requested: Option<u64>,
    ledger: PlacementLedger,
) -> PlacementPlan {
    let rebuild = match kind {
        PlacementKind::Wake => true,
        PlacementKind::Requeue => slice_left == 0,
        PlacementKind::Migrate => false,
    };
    if !rebuild {
        return PlacementPlan {
            vruntime,
            slice: None,
        };
    }
    let weight_incl = if ledger.includes_self {
        ledger.total_weight
    } else {
        ledger.total_weight.saturating_add(weight)
    };
    let slice_real = effective_slice_real(weight, weight_incl, requested);
    let slice_virt = slice_virt_of(slice_real, weight);
    let placed = match kind {
        PlacementKind::Wake => wake_vruntime(
            vruntime,
            weighted_avg_vruntime(ledger.total_weight, ledger.sum_w_vr),
            slice_virt,
        ),
        _ => vruntime,
    };
    PlacementPlan {
        vruntime: placed,
        slice: Some((slice_real, slice_virt)),
    }
}

// ============================================================================
// 单元测试 — 调度策略契约
// ============================================================================
//
// 覆盖:
// - nice_to_weight / weight_to_nice: NICE 双向转换 (含 -20..19 边界 + clamp)
// - mlfq_level_to_nice: 层级 → nice
// - DeadlineParams 校验: is_valid + utilization_pct
// - CfsRunQueue: enqueue/dequeue/pick_next + EEVDF 账目/选择
// - EEVDF 纯函数: Q16.16 增量 / 切片 / V / 合格 / vd / Wake 钳制

#[cfg(test)]
mod tests {
    use super::*;

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
        // 被切走者经幂等 `enqueue` 原位放回 (凭证 true ⇒ 不二次记账).
        assert_eq!(q.enqueue(1, 100, 1024, true), 100);
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

    /// 7. `calc_vruntime_delta_virt`: 权重 0 (非法态) 必须按 NICE0 退化, 不得为 0.
    ///
    /// 回归: 历史实现 `if weight == 0 { return NICE0_WEIGHT; }` 让权重 0 的进程
    /// vruntime 以 1024 倍速度增长, 经 `placement` 把所在核的 `min_vruntime` 钉在
    /// 高位, 使同核任务长期无法获得应有的 CPU 份额. E-04 的 Q16.16 口径下
    /// 退化增量恰为 `VIRT_ONE` (一个虚拟 tick), 而不是 1024。
    #[test]
    fn test_sched_calc_vruntime_delta_virt_zero_weight() {
        assert_eq!(
            calc_vruntime_delta_virt(0),
            VIRT_ONE,
            "权重 0 按 NICE0 退化 ⇒ 一个虚拟 tick"
        );
        assert_eq!(calc_vruntime_delta_virt(NICE0_WEIGHT), VIRT_ONE);
        // nice=19 最低合法权重 (15) → 1024 * 65536 / 15
        assert_eq!(
            calc_vruntime_delta_virt(nice_to_weight(19)),
            (NICE0_WEIGHT << VIRT_SHIFT) / 15
        );
        // nice=-20 最高权重 (88761) → (1024 << 16) / 88761 = 756
        assert_eq!(
            calc_vruntime_delta_virt(nice_to_weight(-20)),
            (NICE0_WEIGHT << VIRT_SHIFT) / 88761
        );
        // 增量下限: 权重极大时整除仍 ≥ 1 (不得为 0, 否则 vr 永不推进)
        assert_eq!(calc_vruntime_delta_virt(u64::MAX), 1);
    }

    /// 8. IC1 + IC3: 落点钳制与下界单调 (Q16.16 时基; S-1 饿死根的直接回归)
    ///
    /// 回归: 旧 `sync_min_vruntime` 把 floor "重设为树最小键" —— 被钳到 floor 的
    /// 唤醒者反过来推高 floor, 形成棘轮; 而运行任务的 `cfs_vruntime` 从不与
    /// floor 对齐, 两者差值最终被 `saturating_sub` 压成 0 ⇒ 本核永不请求重调度.
    #[test]
    fn test_sched_cfs_placement_and_monotonic_floor() {
        let mut q = CfsRunQueue::new();
        let floor = 1000u64 << VIRT_SHIFT;
        // IC3: floor 只增不减 —— 往低推进必须是 no-op.
        q.advance_floor(floor);
        q.advance_floor(floor / 2);
        assert_eq!(q.floor(), floor);
        // IC1: 远低于 floor 的 vr (睡久了 / boot 期从未落点的任务) 被抬到
        // floor - MIN_GRANULARITY, 而不是原样入队.
        assert_eq!(q.placement(45), floor - MIN_GRANULARITY_VIRT);
        let placed = q.enqueue(10, 45, NICE0_WEIGHT, false);
        assert_eq!(placed, floor - MIN_GRANULARITY_VIRT);
        // 高于 floor 的值原样保留 (已消耗较多 CPU 的任务应排在后面).
        let far = 6000u64 << VIRT_SHIFT;
        assert_eq!(q.placement(far), far);
        // 唤醒者落在队列最前 → 下一个拣出点即选中它 (有界延迟).
        assert_eq!(q.leftmost_vruntime(), Some(placed));
        assert_eq!(q.pick_next().unwrap().0, 10);
    }

    /// 8b. EEVDF 选择必须在同一时基内比较 (旧模型下差值恒 0).
    ///
    /// 旧判据 `cfs_should_preempt` 的跨域比较已被 D6① 删除; 本用例锁定其替代:
    /// 运行任务与唤醒任务都经 `placement` 收进 floor 邻域后, `pick_eevdf` 用
    /// `vr <= V` 的合格前缀判定谁该上核 —— 运行任务多跑一个最小粒度后不再合格,
    /// 唤醒者必须胜出.
    #[test]
    fn test_sched_cfs_preempt_uses_shared_time_base() {
        let mut q = CfsRunQueue::new();
        let floor = 5152u64 << VIRT_SHIFT;
        q.advance_floor(floor);
        // 旧模型的致命情形: 运行任务 vr=45 (整数 tick 域), 队列 floor=5152.
        // 新模型由 tick 经 placement 先把运行任务抬进 floor 邻域, 差值即有意义.
        let reconciled = q.placement(45);
        assert_eq!(reconciled, floor - MIN_GRANULARITY_VIRT);
        let wakee = q.enqueue(10, 0, NICE0_WEIGHT, false);
        assert_eq!(wakee, reconciled, "唤醒落点与运行任务必须同域");

        let slice_real = default_slice_real(NICE0_WEIGHT, 2 * NICE0_WEIGHT);
        let slice_virt = slice_virt_of(slice_real, NICE0_WEIGHT);
        let current = EevdfTaskView {
            pid: 9,
            vruntime: reconciled,
            vdeadline: virtual_deadline(reconciled, slice_virt),
            weight: NICE0_WEIGHT,
        };
        let vdeadline = virtual_deadline(wakee, slice_virt);
        // 同 vr / 同 vd 时按 pid 决胜: pid 9 < 10, current 保持上核.
        let pick = q
            .pick_eevdf(Some(current), |pid| (pid == 10).then_some(vdeadline))
            .unwrap();
        assert_eq!(pick.pid, 9);
        assert!(!pick.from_tree, "current 胜出时不得动树账");

        // 运行任务多跑超过最小粒度后不再合格 (vr > V), 唤醒者必然胜出.
        // 这正是旧 `saturating_sub` 判据在跨域时永远得不出的事件.
        let current = EevdfTaskView {
            vruntime: reconciled
                .saturating_add(MIN_GRANULARITY_VIRT)
                .saturating_add(calc_vruntime_delta_virt(NICE0_WEIGHT)),
            ..current
        };
        let pick = q
            .pick_eevdf(Some(current), |pid| (pid == 10).then_some(vdeadline))
            .unwrap();
        assert_eq!(pick.pid, 10, "同域下运行任务欠账即应让出 CPU");
        assert!(pick.from_tree);
    }

    /// 8c. 端到端时基仿真: S-1 故障场景 (CPU 密集任务在核上长跑, 交互任务在
    /// floor 已推进很远之后才唤醒) 下, 唤醒者必须在**有界 tick** 内被拣出.
    ///
    /// 与 `scheduler.rs` 的实际路径同构: tick 内 `placement` 对齐 → 累加 Q16.16
    /// 增量 → `advance_running_vruntime` → `advance_floor` → `pick_eevdf` 的合格
    /// 判定. 旧模型下跨域差值被 `saturating_sub` 压成 0 ⇒ 该核永不请求重调度
    /// ⇒ 唤醒者永久饿死 (实测 4~10 s, 跨过宿主 5 s 超时).
    #[test]
    fn test_sched_cfs_wakeup_bounded_latency_under_tick_loop() {
        let mut q = CfsRunQueue::new();
        let weight = NICE0_WEIGHT;
        let delta = calc_vruntime_delta_virt(weight);
        // 后台 CPU 密集任务入队后立即上核运行 (树中不保留, 但仍在运行账中).
        q.enqueue(1, 0, weight, false);
        let pick = q.pick_eevdf(None, |_| None).unwrap();
        assert_eq!(pick.pid, 1);
        let mut curr_vr = pick.vruntime;
        let mut wakee_vd = 0u64;
        let mut wake_tick = 0u64;
        let mut picked_wakee = false;

        for tick in 1..=2000u64 {
            // tick 路径: 运行任务时基累加 + 运行账增量 + 推进队列下界 (IC1 + IC3)
            let base = q.placement(curr_vr);
            curr_vr = base.saturating_add(delta);
            assert!(
                q.advance_running_vruntime(1, curr_vr),
                "运行账中的任务必须接受 O(1) 增量"
            );
            q.advance_floor(curr_vr);

            if tick == 1000 {
                // 交互任务唤醒: 自身 vr 仍停留在很久以前 (旧模型的两个时基域)
                let placed = q.enqueue(2, 0, weight, false);
                assert!(
                    placed >= q.floor().saturating_sub(MIN_GRANULARITY_VIRT),
                    "IC1: 唤醒落点必须收进 floor 邻域"
                );
                assert_eq!(q.leftmost_vruntime(), Some(placed));
                wakee_vd = placed.saturating_add(delta);
            }

            let current = EevdfTaskView {
                pid: 1,
                vruntime: curr_vr,
                vdeadline: curr_vr,
                weight,
            };
            let pick = q.pick_eevdf(Some(current), |pid| (pid == 2).then_some(wakee_vd));
            let Some(pick) = pick else {
                continue;
            };
            if pick.pid == 2 {
                wake_tick = tick;
                // 切走者原位放回 (凭证 true ⇒ 不动账), 唤醒者已由树账移入运行账.
                q.enqueue(1, curr_vr, weight, true);
                picked_wakee = true;
                break;
            }
        }

        assert!(picked_wakee, "2000 tick 内唤醒者从未被拣出 = S-1 饿死重现");
        assert!(
            wake_tick <= 1000 + MIN_GRANULARITY_TICKS + 2,
            "唤醒到抢占的延迟必须有界 (实得 tick={wake_tick})"
        );
        // 记账守恒: 后台任务在上树 (enqueue) 后仍在可运行集, 运行账只剩唤醒者.
        assert_eq!(q.runnable_weight(), 2 * weight);
        assert!(q.is_running(2));
        // IC3: floor 只增不减, 且不小于运行任务的时基
        assert!(q.floor() >= 1000);
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

    /// 12. EEVDF 纯函数: Q16.16 增量 / 切片 / 请求夹取 / 平均 / 合格 / 唤醒钳制.
    #[test]
    fn test_sched_eevdf_pure_functions() {
        // Δvr 全 40 档随权重严格递减, nice0 = 1.0.
        let mut previous = u64::MAX;
        for nice in (-20..=19).rev() {
            let weight = nice_to_weight(nice);
            let delta = calc_vruntime_delta_virt(weight);
            assert_eq!(delta, (NICE0_WEIGHT << VIRT_SHIFT) / weight);
            assert!(delta < previous, "nice {nice} 的 Δvr 未随权重严格递减");
            previous = delta;
        }
        assert_eq!(calc_vruntime_delta_virt(NICE0_WEIGHT), VIRT_ONE);
        assert_eq!(calc_vruntime_delta_virt(0), VIRT_ONE, "w=0 按 NICE0 退化");
        assert_eq!(calc_vruntime_delta_virt(u64::MAX), 1, "商为 0 时下限为 1");

        // 默认切片: W_inc 含本任务, 结果落在 [MIN, TARGET].
        assert_eq!(
            default_slice_real(NICE0_WEIGHT, NICE0_WEIGHT),
            TARGET_LATENCY_TICKS
        );
        assert_eq!(
            default_slice_real(NICE0_WEIGHT, 2 * NICE0_WEIGHT),
            TARGET_LATENCY_TICKS / 2
        );
        assert_eq!(
            default_slice_real(NICE0_WEIGHT, 100 * NICE0_WEIGHT),
            MIN_GRANULARITY_TICKS
        );
        assert_eq!(default_slice_real(0, NICE0_WEIGHT), MIN_GRANULARITY_TICKS);
        assert_eq!(default_slice_real(NICE0_WEIGHT, 0), MIN_GRANULARITY_TICKS);

        // 请求切片夹取; slice_virt 与逐 tick 记账对齐.
        assert_eq!(clamp_requested_slice(0), MIN_GRANULARITY_TICKS);
        assert_eq!(clamp_requested_slice(1000), TARGET_LATENCY_TICKS);
        assert_eq!(clamp_requested_slice(30), 30);
        assert_eq!(
            effective_slice_real(NICE0_WEIGHT, 2 * NICE0_WEIGHT, None),
            TARGET_LATENCY_TICKS / 2
        );
        assert_eq!(
            effective_slice_real(NICE0_WEIGHT, 2 * NICE0_WEIGHT, Some(2)),
            MIN_GRANULARITY_TICKS
        );
        let requested = clamp_requested_slice(30);
        let virt = slice_virt_of(requested, NICE0_WEIGHT);
        assert_eq!(virt, 30 * VIRT_ONE);
        assert_eq!(virtual_deadline(100, virt), 100 + virt);

        // W/S/V 与合格判定.
        assert_eq!(weighted_avg_vruntime(0, 0), 0);
        assert_eq!(weighted_avg_vruntime(2048, u128::from(2048u64) * 100), 100);
        assert!(!is_eligible(0, 0, 0), "W=0 时无合格者");
        assert!(is_eligible(100, 2048, u128::from(2048u64) * 100));
        assert!(!is_eligible(101, 2048, u128::from(2048u64) * 100));

        // Wake 信用钳制: 上界 = 一个 slice_virt; 欠账不补偿.
        assert_eq!(wake_vruntime(0, 1000, 200), 800);
        assert_eq!(wake_vruntime(900, 1000, 200), 900);
    }

    /// 13. EEVDF 账目: 树账/运行账分解, 结构真相与权重守恒.
    #[test]
    fn test_sched_cfs_eevdf_ledgers_reconcile() {
        let mut q = CfsRunQueue::new();
        q.enqueue(1, 100, 1024, false);
        q.enqueue(2, 200, 2048, false);
        assert_eq!(q.tree_weight(), 3072);
        assert_eq!(q.running_weight(), 0);
        assert_eq!(q.get_weighted_load(), 3072);
        // (1024*100 + 2048*200) / 3072 = 166.67 -> 166
        assert_eq!(q.avg_vruntime(), 166);
        assert!(q.is_on_tree(1));
        assert_eq!(
            q.tree_task(1),
            Some(QueuedTask {
                pid: 1,
                vruntime: 100,
                weight: 1024
            })
        );

        // 拣出 = 同一笔账从树移到运行, W/S 不变.
        let (pid, vr) = q.pick_next().unwrap();
        assert_eq!((pid, vr), (1, 100));
        assert!(q.is_running(1));
        assert_eq!(q.running_count(), 1);
        assert_eq!(q.tree_weight(), 2048);
        assert_eq!(q.running_weight(), 1024);
        assert_eq!(q.get_weighted_load(), 3072);
        assert_eq!(
            q.sum_w_vr(),
            u128::from(1024u64) * 100 + u128::from(2048u64) * 200
        );

        // 排队期间权重变更: 只改账, 不改落点.
        assert!(q.reweight(2, 4096));
        assert_eq!(q.tree_weight(), 4096);
        assert_eq!(q.get_weighted_load(), 5120);
        assert!(!q.reweight(99, 1), "不在可运行集者不得产生幽灵账");
        assert!(q.reweight(1, 512));
        assert_eq!(q.running_weight(), 512);

        // 运行账 vruntime 单调推进: sum 随增量同步.
        let before = q.running_sum_w_vr();
        assert!(q.advance_running_vruntime(1, 140));
        assert_eq!(q.running_sum_w_vr(), before + u128::from(512u64) * 40);
        assert!(
            !q.advance_running_vruntime(1, 100),
            "vruntime 回退不得改动账目"
        );

        // 出队以结构记账为准: 传入陈旧权重不影响扣减量.
        assert!(!q.dequeue(1, u64::MAX, true), "运行账成员出树 -> false");
        assert!(q.running_task(1).is_none());
        assert_eq!(q.running_weight(), 0);
        assert_eq!(q.get_weighted_load(), 4096);
        assert!(q.dequeue(2, u64::MAX, true), "树账成员出队 -> true");
        assert_eq!(q.get_weighted_load(), 0);
        assert_eq!(q.total_weight.load(Ordering::Acquire), 0);
    }

    /// 14. EEVDF 选择: 合格前缀 + (vd, pid) 最小, 拣出即入运行账.
    #[test]
    fn test_sched_eevdf_pick_eligible_prefix_and_deadline() {
        let mut q = CfsRunQueue::new();
        // W = 3072, S = 460800, V = 150; 合格集合 = {1, 3}, pid 2 (vr=200) 不合格.
        q.enqueue(1, 100, 1024, false);
        q.enqueue(2, 200, 1024, false);
        q.enqueue(3, 150, 1024, false);
        let deadlines = [(1u32, 50u64), (2, 10), (3, 60)];
        let pick = q
            .pick_eevdf(None, |pid| {
                deadlines
                    .iter()
                    .find(|&&(candidate, _)| candidate == pid)
                    .map(|&(_, vd)| vd)
            })
            .unwrap();
        assert_eq!(pick.pid, 1, "合格集合中应取 (vd, pid) 最小者");
        assert!(pick.from_tree);
        assert!(q.is_running(1));
        assert!(!q.is_on_tree(1));
        assert_eq!(q.tree_weight(), 2048);
        assert_eq!(q.running_weight(), 1024);

        // 截止期并列时按 pid 决胜.
        let mut q = CfsRunQueue::new();
        q.enqueue(4, 100, 1024, false);
        q.enqueue(3, 100, 1024, false);
        let pick = q.pick_eevdf(None, |_| Some(10)).unwrap();
        assert_eq!(pick.pid, 3, "vd 并列必须按 pid 升序决胜");
    }

    /// 15. EEVDF current-aware: 合格 current 与树候选同台比较; 空集回退.
    #[test]
    fn test_sched_eevdf_current_aware_and_fallback() {
        // current 不合格 (lag < 0) 时, 即使 vd 更小也不得参选.
        let mut q = CfsRunQueue::new();
        q.enqueue(2, 200, 1024, false);
        let current = EevdfTaskView {
            pid: 9,
            vruntime: 100,
            vdeadline: 5,
            weight: 1024,
        };
        // W = 2048, S = 307200, V = 150; 树内 vr=200 在合格前缀之外.
        let pick = q.pick_eevdf(Some(current), |_| Some(1)).unwrap();
        assert_eq!(pick.pid, 9, "不合格的 current 不得因 vd 更小而胜出");
        assert!(!pick.from_tree);
        assert_eq!(q.running_count(), 0, "current 胜出时不动账目");

        // 两者都合格时按 (vd, pid) 比较, 树候选 vd 更小则切换.
        let mut q = CfsRunQueue::new();
        q.enqueue(2, 100, 1024, false);
        let current = EevdfTaskView {
            pid: 9,
            vruntime: 100,
            vdeadline: 70,
            weight: 1024,
        };
        let pick = q
            .pick_eevdf(Some(current), |pid| if pid == 2 { Some(50) } else { None })
            .unwrap();
        assert_eq!(pick.pid, 2, "合格集合内 (vd, pid) 最小者胜出");
        assert!(pick.from_tree);
        assert!(q.is_running(2));

        // 防御性回退: W = 0 时无合格者, 仍须给出确定选择 (vd 缺省取 vr).
        let mut q = CfsRunQueue::new();
        q.enqueue(7, 42, 0, false);
        let pick = q.pick_eevdf(None, |_| None).unwrap();
        assert_eq!(pick.pid, 7);
        assert!(pick.from_tree);
        assert_eq!(pick.vdeadline, 42, "未建账任务按 vd = vr 参与选择");
    }

    /// 16. EEVDF 入队语义三分 (E-05a): Wake 钳制 + 重建, Requeue 仅耗尽时重建,
    /// Migrate 原样携带; `W_inc` 不得重复计入已在账的任务.
    #[test]
    fn test_sched_eevdf_placement_kinds() {
        let w = NICE0_WEIGHT;
        let avg = 100 * VIRT_ONE;
        let slice_virt = TARGET_LATENCY_TICKS / 2 * VIRT_ONE;
        let total = 2 * w;
        let sum = u128::from(w) * u128::from(avg);
        let ledger = |total_weight: u64, includes_self: bool| PlacementLedger {
            total_weight,
            sum_w_vr: sum,
            includes_self,
        };

        // Wake: 睡眠信用上界 = 一个 slice_virt; 欠账 (raw > V) 不补偿.
        let plan = plan_placement(PlacementKind::Wake, 0, w, 0, None, ledger(w, false));
        assert_eq!(
            plan.vruntime,
            avg - slice_virt,
            "Wake 落点应为 V_before - slice_virt"
        );
        assert_eq!(
            plan.slice,
            Some((TARGET_LATENCY_TICKS / 2, slice_virt)),
            "W_inc = W_before + w 时切片减半"
        );
        let plan = plan_placement(
            PlacementKind::Wake,
            avg + VIRT_ONE,
            w,
            0,
            None,
            ledger(w, false),
        );
        assert_eq!(plan.vruntime, avg + VIRT_ONE, "raw > V 时保留负 lag");

        // D8③ 请求切片在 Wake 建账时生效.
        let plan = plan_placement(PlacementKind::Wake, 0, w, 0, Some(10), ledger(w, false));
        assert_eq!(plan.slice, Some((10, 10 * VIRT_ONE)));
        assert_eq!(plan.vruntime, avg - 10 * VIRT_ONE);

        // Requeue: 切片未耗尽时原样携带; 耗尽时按当前 W 重建 (不再钳制落点).
        let carried = plan_placement(PlacementKind::Requeue, 7, w, 3, None, ledger(total, true));
        assert_eq!(
            carried,
            PlacementPlan {
                vruntime: 7,
                slice: None
            },
            "Requeue 不得重置未耗尽的切片预算"
        );
        let rebuilt = plan_placement(PlacementKind::Requeue, 7, w, 0, None, ledger(total, true));
        assert_eq!(rebuilt.vruntime, 7, "Requeue 不做 Wake 落点钳制");
        assert_eq!(
            rebuilt.slice,
            Some((TARGET_LATENCY_TICKS / 2, slice_virt)),
            "耗尽后按已含本任务的 W 重建"
        );
        let outside = plan_placement(PlacementKind::Requeue, 7, w, 0, None, ledger(w, false));
        assert_eq!(
            outside.slice,
            Some((TARGET_LATENCY_TICKS / 2, slice_virt)),
            "未在账者重建时 W_inc 必须含本任务"
        );

        // Migrate: 任何字段都不重建、不钳制.
        let migrated = plan_placement(PlacementKind::Migrate, 7, w, 0, Some(10), ledger(w, false));
        assert_eq!(
            migrated,
            PlacementPlan {
                vruntime: 7,
                slice: None
            },
            "Migrate 必须原样携带切片与落点"
        );
    }
}
