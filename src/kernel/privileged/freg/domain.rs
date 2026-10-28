#![deny(missing_docs)]

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::types::{
    BACKOFF_BASE_TICKS, DEFAULT_FREG_INTERVAL, DomainState, FREGSnapshot, MAX_ADDR_RANGES,
    MAX_CONSECUTIVE_FAILURES, MAX_DOMAIN_DEPENDENCIES, MAX_FREG_SNAPSHOTS,
};
use super::undo_log::UndoLog;

use crate::privileged::sync::IrqSpinLock;
/// 恢复域: 承载 FREG 运行时状态、撤销日志与依赖拓扑。
pub struct RecoveryDomain {
    /// 恢复域 ID (注册时写回, 注销时复位为 0; 仅经 `id()`/`assign_id()` 访问)
    id: AtomicU64,
    state: AtomicU32,
    /// FREG 快照代数 (每次成功捕获递增)
    pub freg_generation: AtomicU64,
    /// FREG 捕获间隔 (tick 数)
    pub freg_interval_ticks: u64,
    /// 下一次 FREG 捕获的 tick 时刻
    pub next_freg_tick: AtomicU64,
    /// 累计回滚次数
    pub rollback_count: AtomicU32,
    /// 连续失败次数 (成功恢复后清零)
    pub consecutive_failures: AtomicU32,
    /// 上一次崩溃指纹 (用于识别同一崩溃的重复回滚)
    pub last_crash_fingerprint: AtomicU64,
    /// 上一次回滚发生的 tick
    pub last_rollback_time: AtomicU64,
    /// 退避截止 tick (在此之前拒绝再次回滚)
    pub backoff_until: AtomicU64,
    /// 本域依赖的其它域 ID 列表
    pub depends_on: IrqSpinLock<[Option<u64>; MAX_DOMAIN_DEPENDENCIES]>,
    /// 依赖本域的其它域 ID 列表
    pub depended_by: IrqSpinLock<[Option<u64>; MAX_DOMAIN_DEPENDENCIES]>,
    /// 当前域能力掩码 (降级时被裁剪)
    pub dom_cap_mask: AtomicU64,
    /// 注册时的原始能力掩码 (恢复时还原)
    pub original_cap_mask: AtomicU64,
    /// CPU 配额上限 (0 = 不限)
    pub cpu_quota_max: u64,
    /// CPU 配额统计周期
    pub cpu_quota_period: u64,
    /// 本周期已消耗的 CPU 配额
    pub cpu_quota_consumed: AtomicU64,
    /// 配额超限累计次数
    pub cpu_quota_exceeded: AtomicU64,
    /// 进程数上限 (0 = 不限)
    pub proc_limit_max: u32,
    /// 当前进程数
    pub proc_limit_current: AtomicU32,
    /// 最近一次心跳 tick
    pub last_heartbeat: AtomicU64,
    /// 允许的最大心跳间隔 (0 = 不检查)
    pub heartbeat_max_gap: u64,
    /// 本域的撤销日志
    pub undo: IrqSpinLock<UndoLog>,
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    /// 捕获回调 (FREG 快照时调用)
    pub capture_cb: IrqSpinLock<Option<unsafe fn()>>,
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    /// 回滚回调 (返回 `true` 表示回滚成功)
    pub rollback_cb: IrqSpinLock<Option<unsafe fn() -> bool>>,
    /// FREG 快照环形栈
    pub freg_stack: IrqSpinLock<[FREGSnapshot; MAX_FREG_SNAPSHOTS]>,
    /// 快照栈顶计数 (单调递增, 取模索引)
    pub freg_stack_top: AtomicU32,
    /// 本域注册的地址范围 `[start, end)`
    pub addr_ranges: IrqSpinLock<[(u64, u64); MAX_ADDR_RANGES]>,
    addr_range_count: AtomicU32,
}

// SAFETY: RecoveryDomain 对状态使用 AtomicU32/AtomicU64,
// 对 addr_ranges 使用 spin::Mutex. 所有可变访问要么是原子的,
// 要么由 Mutex 保护. 不存在无同步的 UnsafeCell.
unsafe impl Send for RecoveryDomain {}
unsafe impl Sync for RecoveryDomain {}

impl RecoveryDomain {
    /// B03-10: 改造为 const fn, 支持静态预分配 (消除 Box::leak 泄漏)。
    /// IrqSpinLock::new / Atomic*::new 已是 const, 所有字段可在 const 上下文初始化。
    pub const fn new(id: u64) -> Self {
        Self {
            id: AtomicU64::new(id),
            state: AtomicU32::new(DomainState::Active as u32),
            freg_generation: AtomicU64::new(0),
            freg_interval_ticks: DEFAULT_FREG_INTERVAL,
            next_freg_tick: AtomicU64::new(DEFAULT_FREG_INTERVAL),
            rollback_count: AtomicU32::new(0),
            consecutive_failures: AtomicU32::new(0),
            last_crash_fingerprint: AtomicU64::new(0),
            last_rollback_time: AtomicU64::new(0),
            backoff_until: AtomicU64::new(0),
            depends_on: IrqSpinLock::named_lazy(
                "FREG.domain.depends_on",
                [None; MAX_DOMAIN_DEPENDENCIES],
            ),
            depended_by: IrqSpinLock::named_lazy(
                "FREG.domain.depended_by",
                [None; MAX_DOMAIN_DEPENDENCIES],
            ),
            dom_cap_mask: AtomicU64::new(u64::MAX),
            original_cap_mask: AtomicU64::new(u64::MAX),
            cpu_quota_max: 0,
            cpu_quota_period: 0,
            cpu_quota_consumed: AtomicU64::new(0),
            cpu_quota_exceeded: AtomicU64::new(0),
            proc_limit_max: 0,
            proc_limit_current: AtomicU32::new(0),
            last_heartbeat: AtomicU64::new(0),
            heartbeat_max_gap: 0,
            // lockdep 可见性 (P-30 引入并保留): 命名域内锁, 使 lockdep 能按真名打印
            // RECOVERY_MANAGER → 域内锁 这一层取锁边 (匿名时为盲区; 非 lockdep 构建零成本).
            undo: IrqSpinLock::named_lazy("FREG.domain.undo", UndoLog::new()),
            capture_cb: IrqSpinLock::named_lazy("FREG.domain.capture_cb", None),
            rollback_cb: IrqSpinLock::named_lazy("FREG.domain.rollback_cb", None),
            freg_stack: IrqSpinLock::named_lazy(
                "FREG.domain.freg_stack",
                [FREGSnapshot {
                    generation: 0,
                    tick: 0,
                    undo_offset: 0,
                }; MAX_FREG_SNAPSHOTS],
            ),
            freg_stack_top: AtomicU32::new(0),
            addr_ranges: IrqSpinLock::named_lazy(
                "FREG.domain.addr_ranges",
                [(0, 0); MAX_ADDR_RANGES],
            ),
            addr_range_count: AtomicU32::new(0),
        }
    }

    /// 读取恢复域 ID (注册后恒定).
    #[inline]
    pub fn id(&self) -> u64 {
        self.id.load(Ordering::Relaxed)
    }

    /// 写回恢复域 ID.
    ///
    /// 仅 `freg` 子树可见: 注册路径绑定 ID、注销路径复位为 0 (使池槽位可复用).
    /// 注册/注销均在启动期单线程执行, Relaxed 足够.
    #[inline]
    pub(super) fn assign_id(&self, id: u64) {
        self.id.store(id, Ordering::Relaxed);
    }

    /// 写入域状态
    pub fn set_state(&self, new_state: DomainState, ordering: Ordering) {
        self.state.store(new_state as u32, ordering);
    }

    /// 读取域状态 (非法值回落为 `Active`)
    pub fn get_state(&self) -> DomainState {
        DomainState::from_u32_fallback(self.state.load(Ordering::SeqCst))
    }

    /// 域是否仍可服务 (`Active` 或 `Degraded`)
    pub fn is_active(&self) -> bool {
        let s = self.get_state();
        s == DomainState::Active || s == DomainState::Degraded
    }

    /// 尝试回滚: 连续失败达阈值、同指纹重复崩溃或仍在退避期则拒绝
    pub fn try_rollback(&self, current_tick: u64, crash_fingerprint: u64) -> bool {
        let failures = self.consecutive_failures.load(Ordering::SeqCst);

        if failures >= MAX_CONSECUTIVE_FAILURES {
            self.set_state(DomainState::Quarantined, Ordering::SeqCst);
            return false;
        }

        let prev_fp = self
            .last_crash_fingerprint
            .swap(crash_fingerprint, Ordering::SeqCst);
        if crash_fingerprint != 0 && prev_fp == crash_fingerprint {
            self.consecutive_failures
                .store(MAX_CONSECUTIVE_FAILURES, Ordering::SeqCst);
            self.set_state(DomainState::Quarantined, Ordering::SeqCst);
            return false;
        }

        let backoff = (1u64 << failures.min(8)) * BACKOFF_BASE_TICKS;
        let bu = self.backoff_until.load(Ordering::SeqCst);
        if current_tick < bu {
            return false;
        }
        self.backoff_until
            .store(current_tick + backoff, Ordering::SeqCst);
        self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
        self.rollback_count.fetch_add(1, Ordering::SeqCst);
        self.last_rollback_time
            .store(current_tick, Ordering::SeqCst);

        self.apply_degradation();

        true
    }

    fn apply_degradation(&self) {
        let failures = self.consecutive_failures.load(Ordering::SeqCst);
        let original = self.original_cap_mask.load(Ordering::SeqCst);
        // 降级矩阵为策略 (degrade_policy trait, §6.3 拆分接口), 纯决策无副作用;
        // 状态写入由 privileged 依据决策执行.
        let decision =
            super::degrade_policy::current_freg_degrade_policy().degrade(failures, original);
        self.dom_cap_mask.store(decision.cap_mask, Ordering::SeqCst);
        if decision.mark_degraded {
            self.set_state(DomainState::Degraded, Ordering::SeqCst);
        }
        if decision.mark_quarantined {
            self.set_state(DomainState::Quarantined, Ordering::SeqCst);
        }
    }

    /// 标记域已恢复: 清零失败计数并还原能力掩码
    pub fn mark_recovered(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        let original = self.original_cap_mask.load(Ordering::SeqCst);
        self.dom_cap_mask.store(original, Ordering::SeqCst);
    }

    /// 追加一条"依赖其它域"边 (已存在时幂等返回 `true`)
    pub fn add_dependency(&self, dependency_id: u64) -> bool {
        {
            let mut deps = self.depends_on.lock();
            for slot in deps.iter() {
                if let Some(existing) = *slot {
                    if existing == dependency_id {
                        return true;
                    }
                }
            }
            for slot in deps.iter_mut() {
                if slot.is_none() {
                    *slot = Some(dependency_id);
                    break;
                }
            }
        }
        true
    }

    /// 追加一条"被其它域依赖"边; 表满且不存在时返回 `false`
    pub fn add_depended_by(&self, dependent_id: u64) -> bool {
        let mut deps = self.depended_by.lock();
        for slot in deps.iter() {
            if let Some(existing) = *slot {
                if existing == dependent_id {
                    return true;
                }
            }
        }
        for slot in deps.iter_mut() {
            if slot.is_none() {
                *slot = Some(dependent_id);
                return true;
            }
        }
        false
    }

    /// 本域依赖的域数量
    pub fn dependency_count(&self) -> usize {
        self.depends_on
            .lock()
            .iter()
            .filter(|s| s.is_some())
            .count()
    }

    /// 是否依赖指定域
    pub fn depends_on_id(&self, dep_id: u64) -> bool {
        self.depends_on.lock().contains(&Some(dep_id))
    }

    /// 消耗一个 tick 配额; 返回 `true` 表示本周期配额已用尽 (未配置配额时恒 `false`)
    pub fn consume_quota_tick(&self) -> bool {
        if self.cpu_quota_max == 0 {
            return false;
        }
        let c = self.cpu_quota_consumed.fetch_add(1, Ordering::SeqCst) + 1;
        c >= self.cpu_quota_max
    }

    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    /// 压入一条 FREG 快照 (环形栈, 超出容量覆盖最旧项)
    pub fn push_freg_snapshot(&self, tick: u64) {
        let r#gen = self.freg_generation.load(Ordering::SeqCst);
        let undo_count = self.undo.lock().count;
        let mut stack = self.freg_stack.lock();
        let top = self.freg_stack_top.load(Ordering::SeqCst) as usize;
        let idx = top % MAX_FREG_SNAPSHOTS;
        stack[idx] = FREGSnapshot {
            generation: r#gen,
            tick,
            undo_offset: undo_count,
        };
        self.freg_stack_top.store(
            crate::privileged::lib::low_u32_from_usize(top) + 1,
            Ordering::SeqCst,
        );
    }

    /// 读取回退 `levels_back` 层快照的代数 (无快照时返回 0)
    pub fn get_rollback_generation(&self, levels_back: u32) -> u64 {
        let stack = self.freg_stack.lock();
        let top = self.freg_stack_top.load(Ordering::SeqCst) as usize;
        if top == 0 {
            return 0;
        }
        let target = top.saturating_sub(levels_back as usize);
        let idx = target.saturating_sub(1) % MAX_FREG_SNAPSHOTS;
        stack[idx].generation
    }

    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    /// 登记一个地址范围 `[start, end)`; 表满时返回 `false`
    pub fn add_addr_range(&self, start: u64, end: u64) -> bool {
        let count = self.addr_range_count.load(Ordering::SeqCst) as usize;
        if count >= MAX_ADDR_RANGES {
            return false;
        }
        let mut ranges = self.addr_ranges.lock();
        ranges[count] = (start, end);
        self.addr_range_count.store(
            crate::privileged::lib::low_u32_from_usize(count) + 1,
            Ordering::SeqCst,
        );
        true
    }

    /// 地址是否落在本域登记的任一范围内
    pub fn contains_addr(&self, addr: u64) -> bool {
        let count = self.addr_range_count.load(Ordering::SeqCst) as usize;
        let ranges = self.addr_ranges.lock();
        for i in 0..count {
            if addr >= ranges[i].0 && addr < ranges[i].1 {
                return true;
            }
        }
        false
    }

    /// 记录一次心跳时刻
    pub fn heartbeat(&self, tick: u64) {
        self.last_heartbeat.store(tick, Ordering::SeqCst);
    }

    /// 检查并消耗 CPU 配额; 返回 `false` 表示本周期配额已超限
    pub fn check_quota(&self) -> bool {
        if self.cpu_quota_max == 0 || self.cpu_quota_period == 0 {
            return true;
        }
        let consumed = self.cpu_quota_consumed.fetch_add(1, Ordering::Relaxed) + 1;
        if consumed < self.cpu_quota_max {
            return true;
        }
        self.cpu_quota_exceeded.fetch_add(1, Ordering::Relaxed);
        false
    }

    /// 重置配额统计 (新周期开始时调用)
    pub fn reset_quota(&self) {
        self.cpu_quota_consumed.store(0, Ordering::Relaxed);
    }

    /// 检查心跳是否在允许间隔内 (首次心跳自动通过)
    pub fn check_health(&self, tick: u64) -> bool {
        if self.heartbeat_max_gap == 0 {
            return true;
        }
        let last = self.last_heartbeat.load(Ordering::SeqCst);
        if last == 0 {
            self.last_heartbeat.store(tick, Ordering::SeqCst);
            return true;
        }
        let gap = tick.saturating_sub(last);
        gap <= self.heartbeat_max_gap
    }

    /// 配额是否曾超限
    pub fn is_quota_exceeded(&self) -> bool {
        self.cpu_quota_exceeded.load(Ordering::Relaxed) > 0
    }

    /// 进程数是否仍在限额内 (未配置上限时恒 `true`)
    pub fn check_proc_limit(&self) -> bool {
        if self.proc_limit_max == 0 {
            return true;
        }
        self.proc_limit_current.load(Ordering::SeqCst) < self.proc_limit_max
    }

    /// 持久化崩溃指纹 (指纹为 0 时忽略)
    pub fn persist_crash_fingerprint(&self, fp: u64) {
        if fp == 0 {
            return;
        }
        self.last_crash_fingerprint.store(fp, Ordering::SeqCst);
    }

    /// 读取启动期崩溃指纹 (0 表示无残留崩溃记录)
    pub fn load_boot_fingerprint(&self) -> u64 {
        self.last_crash_fingerprint.load(Ordering::SeqCst)
    }

    /// 清除启动期崩溃指纹 (域成功启动后调用)
    pub fn clear_boot_fingerprint(&self) {
        self.last_crash_fingerprint.store(0, Ordering::SeqCst);
    }
}
