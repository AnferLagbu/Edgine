use core::sync::atomic::{AtomicU32, Ordering};

use super::domain::RecoveryDomain;
use super::types::{
    DIRECT_MAP_SIZE, DomainState, MAX_RECOVERY_DOMAINS, MAX_ROLLBACK_LOG, RollbackEvent,
};

use crate::framework::sync::IrqSpinLock;
pub static ROLLBACK_LOG: IrqSpinLock<[Option<RollbackEvent>; MAX_ROLLBACK_LOG]> =
    IrqSpinLock::new([None; MAX_ROLLBACK_LOG]);
static ROLLBACK_LOG_IDX: AtomicU32 = AtomicU32::new(0);

fn log_rollback_event(event: RollbackEvent) {
    let mut log = ROLLBACK_LOG.lock();
    let idx = ROLLBACK_LOG_IDX.fetch_add(1, Ordering::SeqCst) as usize;
    log[idx % MAX_ROLLBACK_LOG] = Some(event);

    crate::klog_ffi!(
        klog_ffi_error,
        "[FREG] Rollback: dom={} gen={}->{} entries={} fp=0x{:X} depth={} result={}",
        event.domain_id,
        event.generation_from,
        event.generation_to,
        event.entries_rolled_back,
        event.crash_fingerprint,
        event.cascade_depth,
        event.result
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanicDomainMapping {
    pub prefix: &'static [u8],
    pub domain_id: u64,
}

const PANIC_DOMAIN_MAP: [PanicDomainMapping; 6] = [
    PanicDomainMapping {
        prefix: b"PMM",
        domain_id: 3,
    },
    PanicDomainMapping {
        prefix: b"PROC",
        domain_id: 4,
    },
    PanicDomainMapping {
        prefix: b"NET",
        domain_id: 5,
    },
    PanicDomainMapping {
        prefix: b"VFS",
        domain_id: 2,
    },
    PanicDomainMapping {
        prefix: b"UNKFS",
        domain_id: 2,
    },
    PanicDomainMapping {
        prefix: b"RAMFS",
        domain_id: 2,
    },
];

pub struct RecoveryManager {
    pub domains: [Option<&'static RecoveryDomain>; MAX_RECOVERY_DOMAINS],
    pub direct_map: [Option<&'static RecoveryDomain>; DIRECT_MAP_SIZE],
    pub count: AtomicU32,
}

impl RecoveryManager {
    pub const fn new() -> Self {
        const NONE: Option<&'static RecoveryDomain> = None;
        Self {
            domains: [NONE; MAX_RECOVERY_DOMAINS],
            direct_map: [NONE; DIRECT_MAP_SIZE],
            count: AtomicU32::new(0),
        }
    }

    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    pub fn register(&mut self, domain: &'static RecoveryDomain) -> Option<u64> {
        let idx = self.count.load(Ordering::SeqCst) as usize;
        if idx >= MAX_RECOVERY_DOMAINS {
            return None;
        }
        let id = domain.id as usize;
        if id < DIRECT_MAP_SIZE {
            self.direct_map[id] = Some(domain);
        }
        self.domains[idx] = Some(domain);
        self.count.fetch_add(1, Ordering::SeqCst);
        Some(domain.id)
    }

    pub fn tick(&self, current_tick: u64) {
        if self.count.load(Ordering::Relaxed) == 0 {
            return;
        }
        let count = self.count.load(Ordering::SeqCst) as usize;
        for i in 0..count {
            if let Some(dom) = self.domains[i] {
                if dom.is_active() && current_tick >= dom.next_freg_tick.load(Ordering::SeqCst) {
                    dom.freg_generation.fetch_add(1, Ordering::SeqCst);
                    dom.next_freg_tick
                        .store(current_tick + dom.freg_interval_ticks, Ordering::SeqCst);
                    dom.push_freg_snapshot(current_tick);
                    raw::invoke_capture_cb(*dom.capture_cb.lock());
                }
                // 健康监测: 心跳丢失则升级为 BSR
                if !dom.check_health(current_tick) {
                    let gap =
                        current_tick.saturating_sub(dom.last_heartbeat.load(Ordering::SeqCst));
                    crate::klog_ffi!(
                        klog_ffi_warn,
                        "[FREG] domain {} heartbeat lost ({gap} ticks)",
                        dom.id
                    );

                    dom.consecutive_failures.fetch_add(1, Ordering::SeqCst);
                    let failures = dom.consecutive_failures.load(Ordering::SeqCst);

                    if failures <= 2 {
                        crate::klog_ffi!(
                            klog_ffi_info,
                            "[FREG] domain {} attempt domain-level rollback (failures={})",
                            dom.id,
                            failures
                        );
                        if dom.try_rollback(current_tick, 0) {
                            let (entries, _, _, _) = self.rollback_domain(dom, current_tick, 0, 0);
                            crate::klog_ffi!(
                                klog_ffi_info,
                                "[FREG] domain {} rollback recovered (entries={})",
                                dom.id,
                                entries
                            );
                            dom.mark_recovered();
                            dom.consecutive_failures.store(0, Ordering::SeqCst);
                            continue;
                        }
                    }

                    if failures >= 3 {
                        crate::klog_ffi!(
                            klog_ffi_warn,
                            "[FREG] domain {} persistent failures ({failures}), escalating to BSR",
                            dom.id
                        );
                        crate::framework::freg::NEED_BSR_ESCALATION.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                // 每个周期重置 CPU 配额
                if dom.cpu_quota_period > 0 && current_tick.is_multiple_of(dom.cpu_quota_period) {
                    dom.reset_quota();
                }
            }
        }
    }

    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    pub fn find(&self, id: u64) -> Option<&'static RecoveryDomain> {
        let idx = id as usize;
        if idx < DIRECT_MAP_SIZE {
            self.direct_map[idx]
        } else {
            (0..self.count.load(Ordering::SeqCst) as usize)
                .find_map(|i| self.domains[i].filter(|d| d.id == id))
        }
    }

    pub fn check_boot_fingerprints(&self) {
        let count = self.count.load(Ordering::SeqCst) as usize;
        for i in 0..count {
            if let Some(dom) = self.domains[i] {
                let fp = dom.load_boot_fingerprint();
                if fp != 0 && dom.consecutive_failures.load(Ordering::SeqCst) >= 3 {
                    crate::klog_ffi!(
                        klog_ffi_warn,
                        "[FREG] domain {} has persistent crash fingerprint 0x{:X} ({} failures), starting in degraded mode",
                        dom.id,
                        fp,
                        dom.consecutive_failures.load(Ordering::SeqCst)
                    );
                    dom.set_state(super::types::DomainState::Degraded, Ordering::SeqCst);
                    dom.clear_boot_fingerprint();
                }
            }
        }
    }

    pub fn locate_domain_by_addr(&self, fault_rip: u64) -> Option<u64> {
        let count = self.count.load(Ordering::SeqCst) as usize;
        for i in 0..count {
            if let Some(dom) = self.domains[i] {
                if dom.contains_addr(fault_rip) {
                    return Some(dom.id);
                }
            }
        }
        None
    }

    pub fn locate_domain_by_panic_msg(&self) -> Option<u64> {
        let msg = super::PANIC_MSG.lock();
        let count = self.count.load(Ordering::SeqCst) as usize;

        for mapping in &PANIC_DOMAIN_MAP {
            if msg.len() >= mapping.prefix.len() && msg[..mapping.prefix.len()] == *mapping.prefix {
                for i in 0..count {
                    if let Some(dom) = self.domains[i] {
                        if dom.id == mapping.domain_id {
                            return Some(mapping.domain_id);
                        }
                    }
                }
            }
        }
        None
    }

    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn rollback_domain(
        &self,
        dom: &RecoveryDomain,
        tick: u64,
        fingerprint: u64,
        depth: usize,
    ) -> (usize, u64, u64, i32) {
        let gen_from = dom.freg_generation.load(Ordering::SeqCst);
        let target_gen = if depth > 0 {
            dom.get_rollback_generation(depth as u32)
        } else {
            gen_from
        };

        dom.set_state(DomainState::RollingBack, Ordering::SeqCst);

        let entries_rolled_back;
        {
            let mut undo = dom.undo.lock();
            undo.current_generation = dom.freg_generation.load(Ordering::SeqCst);
            entries_rolled_back = undo.rollback_to(target_gen);
        }

        raw::invoke_rollback_cb(*dom.rollback_cb.lock());

        let result = if entries_rolled_back > 0 { 0 } else { -1 };
        dom.set_state(DomainState::Active, Ordering::SeqCst);
        dom.mark_recovered();

        log_rollback_event(RollbackEvent {
            tick,
            domain_id: dom.id,
            generation_from: gen_from,
            generation_to: target_gen,
            entries_rolled_back,
            crash_fingerprint: fingerprint,
            cascade_depth: depth,
            result,
        });

        (entries_rolled_back, gen_from, target_gen, result)
    }

    pub fn cascade_rollback(&self, domain_id: u64, tick: u64, fingerprint: u64) -> usize {
        let count = self.count.load(Ordering::SeqCst) as usize;
        let mut queue: [u64; MAX_RECOVERY_DOMAINS] = [0; MAX_RECOVERY_DOMAINS];
        let mut queue_head = 0usize;
        let mut queue_tail = 0usize;
        let mut visited = [false; MAX_RECOVERY_DOMAINS];
        let mut rolled_back = 0usize;

        let target_idx = (0..count).find(|&i| self.domains[i].map_or(false, |d| d.id == domain_id));

        if let Some(idx) = target_idx {
            visited[idx] = true;
            queue[queue_tail] = domain_id;
            queue_tail += 1;
        }

        while queue_head < queue_tail {
            let current_id = queue[queue_head];
            queue_head += 1;

            if let Some(dom) = self.find(current_id) {
                if dom.try_rollback(tick, fingerprint) {
                    let (entries, _, _, _) =
                        self.rollback_domain(dom, tick, fingerprint, rolled_back);
                    if entries > 0 {
                        rolled_back += 1;
                    }

                    let depended_by = dom.depended_by.lock();
                    for slot in depended_by.iter() {
                        if let Some(dep_id) = *slot {
                            // BFS queue overflow guard
                            if queue_tail >= MAX_RECOVERY_DOMAINS {
                                crate::klog_ffi!(
                                    klog_ffi_warn,
                                    "[FREG] cascade BFS queue overflow at dom={}",
                                    dep_id
                                );
                                break;
                            }
                            let dep_idx = (0..count)
                                .find(|&i| self.domains[i].map_or(false, |d| d.id == dep_id));
                            if let Some(di) = dep_idx {
                                if !visited[di] {
                                    visited[di] = true;
                                    queue[queue_tail] = dep_id;
                                    queue_tail += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        rolled_back
    }
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 FFI 回调调用
// ============================================================================
//
// `RecoveryDomain::capture_cb` 和 `rollback_cb` 是 `unsafe fn()` 函数指针,
// 调用必须用 unsafe 块。这是 FFI 类型边界, 无安全抽象可消除。
// 本子模块提供 `invoke_capture_cb` / `invoke_rollback_cb` 集中调用点。

pub(crate) mod raw {
    /// 调用域的 capture 回调 (freg 快照前)
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    pub fn invoke_capture_cb(cb: Option<unsafe fn()>) {
        if let Some(f) = cb {
            // SAFETY: f 由域所有者注册, 契约由 framework::freg::api 维护。
            unsafe { f() };
        }
    }

    /// 调用域的 rollback 回调 (回滚完成后)
    // SAFETY: cb 由域所有者注册; 本函数在 unsafe 上下文调用回调, 契约由 framework::freg::api 维护.
    pub fn invoke_rollback_cb(cb: Option<unsafe fn() -> bool>) {
        if let Some(f) = cb {
            // SAFETY: 同上。
            unsafe { f() };
        }
    }
}
