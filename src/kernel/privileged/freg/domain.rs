use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::types::{
    BACKOFF_BASE_TICKS, DEFAULT_FREG_INTERVAL, DomainState, FREGSnapshot, MAX_ADDR_RANGES,
    MAX_CONSECUTIVE_FAILURES, MAX_DOMAIN_DEPENDENCIES, MAX_FREG_SNAPSHOTS,
};
use super::undo_log::UndoLog;

use crate::privileged::sync::IrqSpinLock;
pub struct RecoveryDomain {
    pub id: u64,
    state: AtomicU32,
    pub freg_generation: AtomicU64,
    pub freg_interval_ticks: u64,
    pub next_freg_tick: AtomicU64,
    pub rollback_count: AtomicU32,
    pub consecutive_failures: AtomicU32,
    pub last_crash_fingerprint: AtomicU64,
    pub last_rollback_time: AtomicU64,
    pub backoff_until: AtomicU64,
    pub depends_on: IrqSpinLock<[Option<u64>; MAX_DOMAIN_DEPENDENCIES]>,
    pub depended_by: IrqSpinLock<[Option<u64>; MAX_DOMAIN_DEPENDENCIES]>,
    pub dom_cap_mask: AtomicU64,
    pub original_cap_mask: AtomicU64,
    pub cpu_quota_max: u64,
    pub cpu_quota_period: u64,
    pub cpu_quota_consumed: AtomicU64,
    pub cpu_quota_exceeded: AtomicU64,
    pub proc_limit_max: u32,
    pub proc_limit_current: AtomicU32,
    pub last_heartbeat: AtomicU64,
    pub heartbeat_max_gap: u64,
    pub undo: IrqSpinLock<UndoLog>,
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    pub capture_cb: IrqSpinLock<Option<unsafe fn()>>,
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    pub rollback_cb: IrqSpinLock<Option<unsafe fn() -> bool>>,
    pub freg_stack: IrqSpinLock<[FREGSnapshot; MAX_FREG_SNAPSHOTS]>,
    pub freg_stack_top: AtomicU32,
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
            id,
            state: AtomicU32::new(DomainState::Active as u32),
            freg_generation: AtomicU64::new(0),
            freg_interval_ticks: DEFAULT_FREG_INTERVAL,
            next_freg_tick: AtomicU64::new(DEFAULT_FREG_INTERVAL),
            rollback_count: AtomicU32::new(0),
            consecutive_failures: AtomicU32::new(0),
            last_crash_fingerprint: AtomicU64::new(0),
            last_rollback_time: AtomicU64::new(0),
            backoff_until: AtomicU64::new(0),
            depends_on: IrqSpinLock::new([None; MAX_DOMAIN_DEPENDENCIES]),
            depended_by: IrqSpinLock::new([None; MAX_DOMAIN_DEPENDENCIES]),
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
            undo: IrqSpinLock::new(UndoLog::new()),
            capture_cb: IrqSpinLock::new(None),
            rollback_cb: IrqSpinLock::new(None),
            freg_stack: IrqSpinLock::new(
                [FREGSnapshot {
                    generation: 0,
                    tick: 0,
                    undo_offset: 0,
                }; MAX_FREG_SNAPSHOTS],
            ),
            freg_stack_top: AtomicU32::new(0),
            addr_ranges: IrqSpinLock::new([(0, 0); MAX_ADDR_RANGES]),
            addr_range_count: AtomicU32::new(0),
        }
    }

    pub fn set_state(&self, new_state: DomainState, ordering: Ordering) {
        self.state.store(new_state as u32, ordering);
    }

    pub fn get_state(&self) -> DomainState {
        DomainState::from_u32_fallback(self.state.load(Ordering::SeqCst))
    }

    pub fn is_active(&self) -> bool {
        let s = self.get_state();
        s == DomainState::Active || s == DomainState::Degraded
    }

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

    pub fn mark_recovered(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        let original = self.original_cap_mask.load(Ordering::SeqCst);
        self.dom_cap_mask.store(original, Ordering::SeqCst);
    }

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

    pub fn dependency_count(&self) -> usize {
        self.depends_on
            .lock()
            .iter()
            .filter(|s| s.is_some())
            .count()
    }

    pub fn depends_on_id(&self, dep_id: u64) -> bool {
        self.depends_on.lock().contains(&Some(dep_id))
    }

    pub fn consume_quota_tick(&self) -> bool {
        if self.cpu_quota_max == 0 {
            return false;
        }
        let c = self.cpu_quota_consumed.fetch_add(1, Ordering::SeqCst) + 1;
        c >= self.cpu_quota_max
    }

    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
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
        self.freg_stack_top.store(top as u32 + 1, Ordering::SeqCst);
    }

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
    #[expect(clippy::cast_possible_truncation)]
    pub fn add_addr_range(&self, start: u64, end: u64) -> bool {
        let count = self.addr_range_count.load(Ordering::SeqCst) as usize;
        if count >= MAX_ADDR_RANGES {
            return false;
        }
        let mut ranges = self.addr_ranges.lock();
        ranges[count] = (start, end);
        self.addr_range_count
            .store(count as u32 + 1, Ordering::SeqCst);
        true
    }

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

    pub fn heartbeat(&self, tick: u64) {
        self.last_heartbeat.store(tick, Ordering::SeqCst);
    }

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

    pub fn reset_quota(&self) {
        self.cpu_quota_consumed.store(0, Ordering::Relaxed);
    }

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

    pub fn is_quota_exceeded(&self) -> bool {
        self.cpu_quota_exceeded.load(Ordering::Relaxed) > 0
    }

    pub fn check_proc_limit(&self) -> bool {
        if self.proc_limit_max == 0 {
            return true;
        }
        self.proc_limit_current.load(Ordering::SeqCst) < self.proc_limit_max
    }

    pub fn persist_crash_fingerprint(&self, fp: u64) {
        if fp == 0 {
            return;
        }
        self.last_crash_fingerprint.store(fp, Ordering::SeqCst);
    }

    pub fn load_boot_fingerprint(&self) -> u64 {
        self.last_crash_fingerprint.load(Ordering::SeqCst)
    }

    pub fn clear_boot_fingerprint(&self) {
        self.last_crash_fingerprint.store(0, Ordering::SeqCst);
    }
}
