// SPDX-License-Identifier: MPL-2.0
//! EEVDF 参考模型 (E-01 冻结口径) — Q16.16 纯整数, host 侧唯一口径.
//!
//! 本模块不是生产实现, 而是 `docs/plan/eevdf-scheduler.md` §3 的可执行口径:
//! E-02 起的内核实现以本模型的公式、舍入方向与不变量为唯一参照, 并由
//! `host-tests/tests/eevdf_reference_model_test.rs` 对拍.
//!
//! ## 冻结的公式 (全部为整数运算, 向下取整)
//!
//! - 定点: `X_v = X_real << 16`; 常量 [`TARGET_LATENCY_VIRT`] / [`MIN_GRANULARITY_VIRT`].
//! - 单 tick 虚拟推进: `Δvr = max(1, (NICE0 << 16) / w)`, `w == 0` 按 `NICE0` 退化.
//! - 默认真实切片: `slice_real = max(MIN_GRANULARITY, TARGET_LATENCY * w / W_inc)`,
//!   其中 `W_inc` 是**把本任务计入后**的可运行集权重和 (Wake 用 `W_before + w`,
//!   Requeue 用当前已含本任务的 `W`).
//! - 虚拟切片: `slice_virt = Δvr(w) * slice_real` —— 与逐 tick 记账严格对齐,
//!   slice 到期时 `vr` 恰好推进一个 `slice_virt`.
//! - D8③ 请求切片: 真实 tick 夹取到 `[MIN_GRANULARITY, TARGET_LATENCY]`, 同样按
//!   `slice_virt = Δvr(w) * slice_real` 换算; 请求只在下次建账 (Wake/Requeue)
//!   生效, 不中断当前切片.
//! - 加权平均: `W = tree_weight + running_weight`,
//!   `S = tree_sum_w_vr + running_sum_w_vr`, `V = S / W` (W=0 时 V=0).
//! - 合格: `lag_i = V - vr_i >= 0` ⇔ `vr_i * W <= S` (u128 交叉乘法).
//! - 虚拟截止期: `vd = vr + slice_virt` 在建账时写入; 运行期间 `vd` 固定,
//!   恒有 `vd - vr = Δvr(w) * slice_left` (IE1/IE6 的联合检查式).
//! - 选择: 合格集合 = 树内合格前缀 + 合格 current, 取 `(vd, pid)` 最小;
//!   若无合格者 (仅空集可达), 回退 `(vr, pid)` 最小并与 current 比较
//!   (`current.vr <= candidate.vr` 时保持 current).
//! - Wake: `vr = max(vr_raw, V_before - slice_virt)`; 睡眠信用上界 = 一个
//!   `slice_virt`. 若 `vr_raw > V_before`, 保留负 lag (欠账), 不制造信用.
//! - Migrate: 原样携带 `vr/slice_real/slice_virt/slice_left/vd`, 不做落点钳制.
//! - Requeue: `slice_left > 0` 时原样重投; `slice_left == 0` 时按默认/请求切片
//!   重新建账并令 `vd = vr + slice_virt`.
//!
//! ## 数学事实 (E-01 结论)
//!
//! 只要可运行集非空, `min(vr)` 必然合格: `min(vr) <= S / W`, 且两者都是整数,
//! 故 `min(vr) <= floor(S / W) = V`. 因此「全体 lag < 0」在账目完备时不可达,
//! current-aware fallback 只是防御性路径; 真正必须锁死的是**账目完备**
//! (IE2): 运行任务若漏出 `W/S`, 该证明立即失效.
//!
//! ## 不变式
//!
//! - IE1 同域: `V`/`vr`/`floor`/`vd`/`slice_*` 全部为 Q16.16.
//! - IE2 账目完备: 每个在账任务恰好计入树账或运行账一次.
//! - IE3 有界信用: Wake 后 `lag <= slice_virt`; `lag >= 0` 当且仅当
//!   `vr_raw <= V_before` (负 lag 是欠账, 不补偿).
//! - IE4 floor 单调: 本核 floor 只随运行任务 `vr` 前推, 只增不减.
//! - IE5 迁移守恒: 源/目的树账变化量互为相反数, 任务字段不变.
//! - IE6 切片生命周期: Wake 重置, Migrate 携带, Requeue 仅在耗尽时补充.

use std::collections::{BTreeMap, BTreeSet};

/// 虚拟时间定点小数位数 (Q16.16).
pub const VIRT_SHIFT: u32 = 16;

/// 1.0 的定点表示 (`1 << 16`).
pub const VIRT_ONE: u64 = 1 << VIRT_SHIFT;

/// CFS 目标延迟 (真实 tick), 与 `privileged::config::CFS_TARGET_LATENCY` 同源.
pub const TARGET_LATENCY: u64 = 60;

/// CFS 最小粒度 (真实 tick), 与 `privileged::config::CFS_MIN_GRANULARITY` 同源.
pub const MIN_GRANULARITY: u64 = 8;

/// nice=0 权重, 与 `privileged::config::CFS_NICE0_WEIGHT` 同源.
pub const NICE0_WEIGHT: u64 = 1024;

/// 参考模型的单核可运行任务上限 (与 CFS 队列容量口径一致).
pub const MAX_TASKS: usize = 255;

/// 目标延迟的定点表示.
pub const TARGET_LATENCY_VIRT: u64 = TARGET_LATENCY << VIRT_SHIFT;

/// 最小粒度的定点表示.
pub const MIN_GRANULARITY_VIRT: u64 = MIN_GRANULARITY << VIRT_SHIFT;

/// nice (-20..=19) → 权重查表 (索引 = nice + 20).
///
/// 与 `privileged/proc/cfs.rs` 的 `NICE_TO_WEIGHT` 逐项一致, 由 host 测试锁定;
/// 生产实现仍引用内核单源常量, 本表只作口径参照.
pub const NICE_TO_WEIGHT: [u64; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

/// nice 值映射为 CFS 权重; 越界收敛到 `-20`/`19` 端点.
#[must_use]
pub fn nice_to_weight(nice: i8) -> u64 {
    let index = usize::from(u8::try_from(nice.clamp(-20, 19) + 20).unwrap_or(0));
    NICE_TO_WEIGHT[index]
}

/// 单 tick 虚拟时间增量 `Δvr = max(1, (NICE0 << 16) / w)`, `w == 0` 按 `NICE0` 退化.
#[must_use]
pub const fn delta_vruntime(weight: u64) -> u64 {
    let effective = if weight == 0 { NICE0_WEIGHT } else { weight };
    let quotient = (NICE0_WEIGHT << VIRT_SHIFT) / effective;
    if quotient == 0 { 1 } else { quotient }
}

/// 默认真实切片: `max(MIN_GRANULARITY, min(TARGET_LATENCY, TARGET_LATENCY * w / W_inc))`.
///
/// `weight_incl` 必须**已包含本任务**; 非法 `w == 0` 或空集退化为最小粒度,
/// `W_inc < w` 的非法调用被上限夹到目标延迟 (合法状态不会出现).
#[must_use]
pub const fn default_slice_real(weight: u64, weight_incl: u64) -> u64 {
    if weight == 0 || weight_incl == 0 {
        return MIN_GRANULARITY;
    }
    let fair = TARGET_LATENCY.saturating_mul(weight) / weight_incl;
    if fair < MIN_GRANULARITY {
        MIN_GRANULARITY
    } else if fair > TARGET_LATENCY {
        TARGET_LATENCY
    } else {
        fair
    }
}

/// 虚拟切片 `slice_virt = Δvr(w) * slice_real` (与逐 tick 记账严格对齐).
#[must_use]
pub const fn slice_virt_of(slice_real: u64, weight: u64) -> u64 {
    delta_vruntime(weight).saturating_mul(slice_real)
}

/// D8③ 请求切片统一夹取: `[MIN_GRANULARITY, TARGET_LATENCY]`.
#[must_use]
pub const fn clamp_requested_slice(requested: u64) -> u64 {
    if requested < MIN_GRANULARITY {
        MIN_GRANULARITY
    } else if requested > TARGET_LATENCY {
        TARGET_LATENCY
    } else {
        requested
    }
}

/// 加权平均虚拟时间 `V = floor(S / W)`; `W == 0` 返回 0, 商溢出饱和到 `u64::MAX`.
#[must_use]
pub fn weighted_avg_vruntime(total_weight: u64, sum_w_vr: u128) -> u64 {
    if total_weight == 0 {
        return 0;
    }
    u64::try_from(sum_w_vr / u128::from(total_weight)).unwrap_or(u64::MAX)
}

/// 合格判定 `vr * W <= S` (u128 交叉乘法, 避免除法舍入改变符号).
#[must_use]
pub fn is_eligible(vr: u64, total_weight: u64, sum_w_vr: u128) -> bool {
    if total_weight == 0 {
        return false;
    }
    u128::from(vr) * u128::from(total_weight) <= sum_w_vr
}

/// 任务在参考模型中的位置.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// 在对应 CPU 的运行队列树上 (可运行, 未上核).
    Tree(usize),
    /// 正在对应 CPU 上运行.
    Running(usize),
}

/// EEVDF 调度实体 (进程即每线程 TCB, 多线程工程 D7 口径).
#[derive(Debug, Clone, Copy)]
pub struct Task {
    /// 进程号 (`PID` = `TID`).
    pub pid: u32,
    /// CFS 权重 (由 nice 派生).
    pub weight: u64,
    /// 虚拟运行时间 (Q16.16), 睡眠期间保留.
    pub vr: u64,
    /// 本轮虚拟截止期 (Q16.16).
    pub vd: u64,
    /// 本轮真实切片 (tick).
    pub slice_real: u64,
    /// 本轮虚拟切片 (Q16.16).
    pub slice_virt: u64,
    /// 本轮剩余真实切片 (tick).
    pub slice_left: u64,
    /// D8③ `sched_setattr` 请求切片 (未夹取的 tick 原值).
    pub requested_slice: Option<u64>,
}

impl Task {
    /// 构造不在任何运行队列上的任务 (睡眠态).
    #[must_use]
    pub fn new(pid: u32, nice: i8, vr: u64) -> Self {
        Self {
            pid,
            weight: nice_to_weight(nice),
            vr,
            vd: vr,
            slice_real: 0,
            slice_virt: 0,
            slice_left: 0,
            requested_slice: None,
        }
    }
}

/// 每核运行队列: 树账 + 运行账 (IE2 的唯一真相).
#[derive(Debug, Default)]
pub struct Rq {
    tree: BTreeSet<(u64, u32)>,
    tree_weight: u64,
    tree_sum_w_vr: u128,
    running: Option<u32>,
    running_weight: u64,
    running_sum_w_vr: u128,
}

impl Rq {
    /// 树内节点数.
    #[must_use]
    pub fn tree_len(&self) -> usize {
        self.tree.len()
    }

    /// 树是否为空 (结构真相).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    /// 指定 pid 是否在树上.
    #[must_use]
    pub fn contains_pid(&self, pid: u32) -> bool {
        self.tree.iter().any(|&(_, p)| p == pid)
    }

    /// 按 `(vr, pid)` 升序遍历树节点.
    pub fn tree_keys(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.tree.iter().copied()
    }

    /// 树账权重和.
    #[must_use]
    pub fn tree_weight(&self) -> u64 {
        self.tree_weight
    }

    /// 树账 `Σ w * vr`.
    #[must_use]
    pub fn tree_sum_w_vr(&self) -> u128 {
        self.tree_sum_w_vr
    }

    /// 当前运行任务 (若有).
    #[must_use]
    pub fn running_pid(&self) -> Option<u32> {
        self.running
    }

    /// 运行账权重 (无运行任务时为 0).
    #[must_use]
    pub fn running_weight(&self) -> u64 {
        self.running_weight
    }

    /// 运行账 `w * vr` (无运行任务时为 0).
    #[must_use]
    pub fn running_sum_w_vr(&self) -> u128 {
        self.running_sum_w_vr
    }

    /// 可运行集权重和 `W = tree_weight + running_weight`.
    #[must_use]
    pub fn total_weight(&self) -> u64 {
        self.tree_weight.saturating_add(self.running_weight)
    }

    /// 可运行集 `S = tree_sum_w_vr + running_sum_w_vr`.
    #[must_use]
    pub fn sum_w_vr(&self) -> u128 {
        self.tree_sum_w_vr.saturating_add(self.running_sum_w_vr)
    }

    /// 本核加权平均虚拟时间 `V`.
    #[must_use]
    pub fn avg_vruntime(&self) -> u64 {
        weighted_avg_vruntime(self.total_weight(), self.sum_w_vr())
    }
}

/// 单 tick 结果.
#[derive(Debug, Clone, Copy)]
pub struct TickOutcome {
    /// 本 tick 的 `Δvr`.
    pub delta: u64,
    /// 本 tick 是否耗尽切片 (已按 Requeue 语义重投).
    pub expired: bool,
}

/// 双核 EEVDF 参考模型 (单核选择 + 跨核迁移).
#[derive(Debug, Default)]
pub struct Model {
    tasks: BTreeMap<u32, Task>,
    rqs: [Rq; 2],
    need_resched: [bool; 2],
    floor: [u64; 2],
}

impl Model {
    /// 构造空模型 (两核).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取任务.
    #[must_use]
    pub fn task(&self, pid: u32) -> Option<&Task> {
        self.tasks.get(&pid)
    }

    /// 读取指定核的运行队列.
    #[must_use]
    pub fn rq(&self, cpu: usize) -> Option<&Rq> {
        self.rqs.get(cpu)
    }

    /// 读取指定核的 floor (IC3/IE4).
    #[must_use]
    pub fn floor(&self, cpu: usize) -> Option<u64> {
        self.floor.get(cpu).copied()
    }

    /// 指定核是否已请求重调度.
    #[must_use]
    pub fn needs_resched(&self, cpu: usize) -> bool {
        self.need_resched.get(cpu).copied().unwrap_or(false)
    }

    /// 遍历已登记任务的 pid.
    pub fn task_pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.tasks.keys().copied()
    }

    /// 派生任务当前位置 (不在任何队列上时为 `None`).
    #[must_use]
    pub fn location(&self, pid: u32) -> Option<Location> {
        for (cpu, rq) in self.rqs.iter().enumerate() {
            if rq.running == Some(pid) {
                return Some(Location::Running(cpu));
            }
            if rq.contains_pid(pid) {
                return Some(Location::Tree(cpu));
            }
        }
        None
    }

    /// 登记一个睡眠态任务; pid 已存在或达到 [`MAX_TASKS`] 时返回 `false`.
    pub fn add_task(&mut self, pid: u32, nice: i8, vr: u64) -> bool {
        if self.tasks.contains_key(&pid) || self.tasks.len() >= MAX_TASKS {
            return false;
        }
        self.tasks.insert(pid, Task::new(pid, nice, vr));
        true
    }

    /// 设置睡眠任务的原始 `vr` (模拟睡眠期间保留的欠账/信用); 在账任务返回 `false`.
    pub fn set_raw_vruntime(&mut self, pid: u32, vr: u64) -> bool {
        if self.location(pid).is_some() {
            return false;
        }
        match self.tasks.get_mut(&pid) {
            Some(task) => {
                task.vr = vr;
                task.vd = vr;
                true
            }
            None => false,
        }
    }

    /// 设置 D8③ 请求切片 (未夹取; 下次建账时按 [`clamp_requested_slice`] 生效).
    pub fn set_requested_slice(&mut self, pid: u32, requested: Option<u64>) -> bool {
        match self.tasks.get_mut(&pid) {
            Some(task) => {
                task.requested_slice = requested;
                true
            }
            None => false,
        }
    }

    /// Wake 入队: 睡眠信用钳制 + 重算切片/截止期 + 唤醒抢占判据.
    ///
    /// 返回是否请求重调度: 无 current 恒为 `true`; 否则仅当新任务合格且
    /// `vd_new < vd_current` 时为 `true` (frozen wake-preempt gate).
    pub fn wake(&mut self, cpu: usize, pid: u32) -> bool {
        if self.location(pid).is_some() {
            return false;
        }
        let Some(rq) = self.rqs.get(cpu) else {
            return false;
        };
        let Some(task) = self.tasks.get(&pid) else {
            return false;
        };
        let raw_vr = task.vr;
        let weight = task.weight;
        let weight_before = rq.total_weight();
        let sum_before = rq.sum_w_vr();
        let avg_before = weighted_avg_vruntime(weight_before, sum_before);
        let weight_incl = weight_before.saturating_add(weight);
        let slice_real = effective_slice_real(task, weight_incl);
        let slice_virt = slice_virt_of(slice_real, weight);
        let placed = raw_vr.max(avg_before.saturating_sub(slice_virt));
        let deadline = placed.saturating_add(slice_virt);
        let current_deadline = rq
            .running
            .and_then(|current| self.tasks.get(&current))
            .map(|current| current.vd);

        let Some(task) = self.tasks.get_mut(&pid) else {
            return false;
        };
        task.vr = placed;
        task.vd = deadline;
        task.slice_real = slice_real;
        task.slice_virt = slice_virt;
        task.slice_left = slice_real;

        let Some(rq) = self.rqs.get_mut(cpu) else {
            return false;
        };
        rq.tree.insert((placed, pid));
        rq.tree_weight = rq.tree_weight.saturating_add(weight);
        rq.tree_sum_w_vr = rq
            .tree_sum_w_vr
            .saturating_add(u128::from(weight) * u128::from(placed));

        let eligible = is_eligible(placed, rq.total_weight(), rq.sum_w_vr());
        let need = match current_deadline {
            Some(current_vd) => eligible && deadline < current_vd,
            None => true,
        };
        if let Some(flag) = self.need_resched.get_mut(cpu) {
            *flag |= need;
        }
        need
    }

    /// 阻塞/退出出队: 从树或运行槽撤账, 不重置任务字段 (Wake 才重置切片).
    pub fn block(&mut self, pid: u32) -> bool {
        for cpu in 0..self.rqs.len() {
            if self.rqs[cpu].running == Some(pid) {
                let Some(rq) = self.rqs.get_mut(cpu) else {
                    return false;
                };
                rq.running = None;
                rq.running_weight = 0;
                rq.running_sum_w_vr = 0;
                if let Some(flag) = self.need_resched.get_mut(cpu) {
                    *flag = true;
                }
                return true;
            }
            let Some(key) = self.find_tree_key(cpu, pid) else {
                continue;
            };
            let Some(task) = self.tasks.get(&pid) else {
                return false;
            };
            let weight = task.weight;
            let w_vr = u128::from(weight) * u128::from(key.0);
            let Some(rq) = self.rqs.get_mut(cpu) else {
                return false;
            };
            let (Some(tree_weight), Some(tree_sum)) = (
                rq.tree_weight.checked_sub(weight),
                rq.tree_sum_w_vr.checked_sub(w_vr),
            ) else {
                return false;
            };
            rq.tree.remove(&key);
            rq.tree_weight = tree_weight;
            rq.tree_sum_w_vr = tree_sum;
            return true;
        }
        false
    }

    /// 跨核迁移 (源必须是树节点): 原样携带 `vr/切片/vd`, 仅搬树账.
    pub fn migrate(&mut self, from: usize, to: usize, pid: u32) -> bool {
        if from == to || from >= self.rqs.len() || to >= self.rqs.len() {
            return false;
        }
        let Some(key) = self.find_tree_key(from, pid) else {
            return false;
        };
        let Some(task) = self.tasks.get(&pid) else {
            return false;
        };
        let weight = task.weight;
        let w_vr = u128::from(weight) * u128::from(key.0);
        let Some(src) = self.rqs.get_mut(from) else {
            return false;
        };
        if src.tree_weight < weight || src.tree_sum_w_vr < w_vr {
            return false;
        }
        src.tree.remove(&key);
        src.tree_weight -= weight;
        src.tree_sum_w_vr -= w_vr;
        let Some(dst) = self.rqs.get_mut(to) else {
            return false;
        };
        dst.tree.insert(key);
        dst.tree_weight = dst.tree_weight.saturating_add(weight);
        dst.tree_sum_w_vr = dst
            .tree_sum_w_vr
            .saturating_add(u128::from(weight) * u128::from(key.0));
        if let Some(flag) = self.need_resched.get_mut(to) {
            *flag = true;
        }
        true
    }

    /// 推进当前任务一个 tick: 运行账增量 + floor 前推 + 切片到期 Requeue.
    pub fn tick(&mut self, cpu: usize) -> Option<TickOutcome> {
        let pid = self.rqs.get(cpu)?.running?;
        let weight;
        let delta;
        {
            let task = self.tasks.get_mut(&pid)?;
            weight = task.weight;
            delta = delta_vruntime(weight);
            task.vr = task.vr.saturating_add(delta);
            task.slice_left = task.slice_left.saturating_sub(1);
        }
        let rq = self.rqs.get_mut(cpu)?;
        rq.running_sum_w_vr = rq
            .running_sum_w_vr
            .saturating_add(u128::from(weight) * u128::from(delta));
        let new_vr = self.tasks.get(&pid)?.vr;
        if let Some(floor) = self.floor.get_mut(cpu) {
            *floor = (*floor).max(new_vr);
        }
        let expired = self.tasks.get(&pid)?.slice_left == 0;
        if expired {
            self.requeue_running(cpu);
        }
        Some(TickOutcome { delta, expired })
    }

    /// 按 EEVDF 判据选出该核应运行的任务 (含 current-aware 兜底).
    #[must_use]
    pub fn pick_winner(&self, cpu: usize) -> Option<u32> {
        let rq = self.rqs.get(cpu)?;
        let total = rq.total_weight();
        let sum = rq.sum_w_vr();
        let avg = weighted_avg_vruntime(total, sum);
        let mut best: Option<(u64, u32)> = None;
        for &(_, pid) in rq.tree.range(..=(avg, u32::MAX)) {
            let Some(task) = self.tasks.get(&pid) else {
                continue;
            };
            let candidate = (task.vd, pid);
            if best.is_none_or(|current| candidate < current) {
                best = Some(candidate);
            }
        }
        if let Some(current_pid) = rq.running
            && let Some(current) = self.tasks.get(&current_pid)
            && is_eligible(current.vr, total, sum)
        {
            let candidate = (current.vd, current_pid);
            if best.is_none_or(|winner| candidate < winner) {
                best = Some(candidate);
            }
        }
        if let Some((_, winner)) = best {
            return Some(winner);
        }
        let fallback = rq.tree.iter().next().copied();
        match (rq.running, fallback) {
            (Some(current), Some((candidate_vr, candidate_pid))) => {
                let current_vr = self.tasks.get(&current).map_or(u64::MAX, |task| task.vr);
                if current_vr <= candidate_vr {
                    Some(current)
                } else {
                    Some(candidate_pid)
                }
            }
            (Some(current), None) => Some(current),
            (None, Some((_, candidate_pid))) => Some(candidate_pid),
            (None, None) => None,
        }
    }

    /// 执行一次调度: 保持 current 或把胜者从树移到运行槽.
    pub fn schedule(&mut self, cpu: usize) -> Option<u32> {
        let Some(winner) = self.pick_winner(cpu) else {
            if let Some(flag) = self.need_resched.get_mut(cpu) {
                *flag = false;
            }
            return None;
        };
        let current = self.rqs.get(cpu)?.running;
        if current == Some(winner) {
            if let Some(flag) = self.need_resched.get_mut(cpu) {
                *flag = false;
            }
            return current;
        }
        if current.is_some() {
            self.requeue_running(cpu);
        }
        if !self.activate_from_tree(cpu, winner) {
            return None;
        }
        Some(winner)
    }

    /// 账目对账 (IE1/IE2/IE3/IE6): 从任务状态暴力重算并逐项比较.
    ///
    /// # Panics
    ///
    /// 任一不变式被破坏时 panic, 消息给出核号与 pid.
    pub fn audit(&self) {
        let mut seen: BTreeMap<u32, Location> = BTreeMap::new();
        for (cpu, rq) in self.rqs.iter().enumerate() {
            let mut tree_weight = 0u64;
            let mut tree_sum = 0u128;
            let mut tree_pids = BTreeSet::new();
            for &(vr, pid) in &rq.tree {
                let task = self
                    .tasks
                    .get(&pid)
                    .unwrap_or_else(|| panic!("cpu {cpu} 树节点 pid {pid} 无任务"));
                assert_eq!(task.vr, vr, "cpu {cpu} pid {pid} 树键 vr 与任务字段背离");
                assert!(
                    tree_pids.insert(pid),
                    "cpu {cpu} 树内 pid {pid} 出现重复节点"
                );
                assert!(
                    seen.insert(pid, Location::Tree(cpu)).is_none(),
                    "pid {pid} 同时处于多个位置"
                );
                tree_weight = tree_weight
                    .checked_add(task.weight)
                    .expect("树账权重和溢出");
                tree_sum = tree_sum
                    .checked_add(u128::from(task.weight) * u128::from(vr))
                    .expect("树账 w*vr 和溢出");
                audit_slice_invariants(cpu, task);
            }
            assert_eq!(rq.tree_weight, tree_weight, "cpu {cpu} tree_weight 失真");
            assert_eq!(rq.tree_sum_w_vr, tree_sum, "cpu {cpu} tree_sum_w_vr 失真");

            if let Some(pid) = rq.running {
                let task = self
                    .tasks
                    .get(&pid)
                    .unwrap_or_else(|| panic!("cpu {cpu} 运行槽 pid {pid} 无任务"));
                assert!(
                    seen.insert(pid, Location::Running(cpu)).is_none(),
                    "pid {pid} 同时处于多个位置"
                );
                assert_eq!(rq.running_weight, task.weight, "cpu {cpu} 运行账权重失真");
                assert_eq!(
                    rq.running_sum_w_vr,
                    u128::from(task.weight) * u128::from(task.vr),
                    "cpu {cpu} 运行账 w*vr 失真"
                );
                audit_slice_invariants(cpu, task);
            } else {
                assert_eq!(rq.running_weight, 0, "cpu {cpu} 空运行槽仍有权重账");
                assert_eq!(rq.running_sum_w_vr, 0, "cpu {cpu} 空运行槽仍有 w*vr 账");
            }

            let total = rq.total_weight();
            let sum = rq.sum_w_vr();
            let avg = weighted_avg_vruntime(total, sum);
            for &(vr, pid) in &rq.tree {
                assert_eq!(
                    is_eligible(vr, total, sum),
                    vr <= avg,
                    "cpu {cpu} pid {pid} 交叉乘法合格判定与 V 前缀不一致"
                );
            }
        }
        for &pid in self.tasks.keys() {
            let derived = self.location(pid);
            assert_eq!(
                seen.get(&pid).copied(),
                derived,
                "pid {pid} 位置派生与账目扫描不一致"
            );
        }
    }

    fn find_tree_key(&self, cpu: usize, pid: u32) -> Option<(u64, u32)> {
        let rq = self.rqs.get(cpu)?;
        rq.tree.iter().find(|&&(_, p)| p == pid).copied()
    }

    fn requeue_running(&mut self, cpu: usize) -> bool {
        let Some(rq) = self.rqs.get_mut(cpu) else {
            return false;
        };
        let Some(pid) = rq.running.take() else {
            return false;
        };
        let weight = rq.running_weight;
        rq.running_weight = 0;
        rq.running_sum_w_vr = 0;
        let weight_incl = rq.tree_weight.saturating_add(weight);
        let tree_weight = rq.tree_weight;
        let tree_sum = rq.tree_sum_w_vr;

        let Some(task) = self.tasks.get_mut(&pid) else {
            return false;
        };
        if task.slice_left == 0 {
            let slice_real = effective_slice_real(task, weight_incl);
            task.slice_real = slice_real;
            task.slice_virt = slice_virt_of(slice_real, task.weight);
            task.slice_left = slice_real;
            task.vd = task.vr.saturating_add(task.slice_virt);
        }
        let vr = task.vr;
        let task_weight = task.weight;
        let w_vr = u128::from(task_weight) * u128::from(vr);
        let (Some(new_tree_weight), Some(new_tree_sum)) = (
            tree_weight.checked_add(task_weight),
            tree_sum.checked_add(w_vr),
        ) else {
            return false;
        };
        rq.tree.insert((vr, pid));
        rq.tree_weight = new_tree_weight;
        rq.tree_sum_w_vr = new_tree_sum;
        if let Some(flag) = self.need_resched.get_mut(cpu) {
            *flag = true;
        }
        true
    }

    fn activate_from_tree(&mut self, cpu: usize, pid: u32) -> bool {
        let Some(key) = self.find_tree_key(cpu, pid) else {
            return false;
        };
        let Some(task) = self.tasks.get(&pid) else {
            return false;
        };
        let weight = task.weight;
        let w_vr = u128::from(weight) * u128::from(key.0);
        let Some(rq) = self.rqs.get_mut(cpu) else {
            return false;
        };
        if rq.tree_weight < weight || rq.tree_sum_w_vr < w_vr {
            return false;
        }
        rq.tree.remove(&key);
        rq.tree_weight -= weight;
        rq.tree_sum_w_vr -= w_vr;
        rq.running = Some(pid);
        rq.running_weight = weight;
        rq.running_sum_w_vr = w_vr;
        if let Some(flag) = self.need_resched.get_mut(cpu) {
            *flag = false;
        }
        true
    }
}

fn effective_slice_real(task: &Task, weight_incl: u64) -> u64 {
    task.requested_slice
        .map_or(default_slice_real(task.weight, weight_incl), |requested| {
            clamp_requested_slice(requested)
        })
}

/// 建账切片有效性: `slice_virt = Δvr * slice_real`, `vd - vr = Δvr * slice_left`.
fn audit_slice_invariants(cpu: usize, task: &Task) {
    assert!(
        task.slice_real >= MIN_GRANULARITY,
        "cpu {cpu} pid {} 的 slice_real 小于最小粒度",
        task.pid
    );
    assert!(
        task.slice_left > 0 && task.slice_left <= task.slice_real,
        "cpu {cpu} pid {} 的 slice_left 越界",
        task.pid
    );
    assert_eq!(
        task.slice_virt,
        slice_virt_of(task.slice_real, task.weight),
        "cpu {cpu} pid {} 的 slice_virt 与 Δvr*slice_real 不一致",
        task.pid
    );
    assert_eq!(
        task.vd,
        task.vr
            .saturating_add(delta_vruntime(task.weight).saturating_mul(task.slice_left)),
        "cpu {cpu} pid {} 的 vd - vr 与 Δvr*slice_left 不一致",
        task.pid
    );
}
