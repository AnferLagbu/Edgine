#![deny(missing_docs)]

use core::sync::atomic::{AtomicU32, Ordering};

use super::domain::RecoveryDomain;
use super::types::{
    DIRECT_MAP_SIZE, DomainState, MAX_RECOVERY_DOMAINS, MAX_ROLLBACK_LOG, RollbackEvent,
};

use crate::privileged::sync::IrqSpinLock;
/// 回滚事件环形日志 (最近 `MAX_ROLLBACK_LOG` 条, 供故障取证读取).
// lockdep 可见性 (P-30 引入并保留): 命名 FREG 回滚日志锁 (lockdep 盲区消除; 非 lockdep 构建零成本).
pub static ROLLBACK_LOG: IrqSpinLock<[Option<RollbackEvent>; MAX_ROLLBACK_LOG]> =
    IrqSpinLock::named_lazy("FREG.ROLLBACK_LOG", [None; MAX_ROLLBACK_LOG]);
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

/// panic 前缀到恢复域标识的静态映射 (如 "PMM" → 3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanicDomainMapping {
    /// panic 消息前缀 (ASCII 字节串, 如 `b"PMM"`)
    pub prefix: &'static [u8],
    /// 该前缀对应的恢复域标识
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

/// FREG 恢复管理器: 登记所有恢复域并提供查找与回滚能力。
pub struct RecoveryManager {
    /// 已注册恢复域的紧凑列表, 有效区间为 `domains[..count]`
    pub domains: [Option<&'static RecoveryDomain>; MAX_RECOVERY_DOMAINS],
    /// 域标识 → 域指针的直接索引 (标识小于 `DIRECT_MAP_SIZE` 时命中)
    pub direct_map: [Option<&'static RecoveryDomain>; DIRECT_MAP_SIZE],
    /// 已注册域数量
    pub count: AtomicU32,
}

impl Default for RecoveryManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryManager {
    /// 构造空的恢复管理器 (无已注册域)
    #[must_use]
    pub const fn new() -> Self {
        const NONE: Option<&'static RecoveryDomain> = None;
        Self {
            domains: [NONE; MAX_RECOVERY_DOMAINS],
            direct_map: [NONE; DIRECT_MAP_SIZE],
            count: AtomicU32::new(0),
        }
    }

    /// 注册恢复域, 返回其标识; 重复注册 (`core::ptr::eq` 判等) 与表满返回既有标识 / `None`.
    pub fn register(&mut self, domain: &'static RecoveryDomain) -> Option<u64> {
        let count = self.count.load(Ordering::SeqCst) as usize;
        // 指针等价去重: 同一域重复注册会让 tick / 快照 / 回滚路径对同域执行两次
        // (且 `domains` 线性扫描会命中首项, 后者成为不可达重复项).
        if let Some(existing) = self.domains[..count]
            .iter()
            .flatten()
            .find(|d| core::ptr::eq(**d, domain))
        {
            return Some(existing.id());
        }
        if count >= MAX_RECOVERY_DOMAINS {
            return None;
        }
        let id = crate::privileged::glue::usize_from_u64(domain.id());
        if id < DIRECT_MAP_SIZE {
            self.direct_map[id] = Some(domain);
        }
        self.domains[count] = Some(domain);
        self.count.store(
            crate::privileged::glue::low_u32_from_usize(count) + 1,
            Ordering::SeqCst,
        );
        Some(domain.id())
    }

    /// 注销指定 id 的恢复域, 返回被移除的域指针 (未注册返回 `None`).
    ///
    /// 采用 swap-remove: 末项填坑并递减 `count`, 保持 `domains[..count]` 无空洞
    /// (否则 tick / 回滚的线性扫描会提前结束而漏掉后续域). 同步清理 `direct_map`
    /// 中指向该域的快速索引 —— 只在索引确实指向被移除域时清除, 避免误伤同槽位
    /// 的新注册项.
    pub fn unregister(&mut self, domain_id: u64) -> Option<&'static RecoveryDomain> {
        let count = self.count.load(Ordering::SeqCst) as usize;
        let idx = (0..count).find(|&i| self.domains[i].is_some_and(|d| d.id() == domain_id))?;
        let removed = self.domains[idx].take()?;

        let last = count - 1;
        if idx != last {
            self.domains[idx] = self.domains[last];
            self.domains[last] = None;
        }
        self.count.fetch_sub(1, Ordering::SeqCst);

        let direct_idx = crate::privileged::glue::usize_from_u64(domain_id);
        if direct_idx < DIRECT_MAP_SIZE
            && self.direct_map[direct_idx].is_some_and(|d| core::ptr::eq(d, removed))
        {
            self.direct_map[direct_idx] = None;
        }

        Some(removed)
    }

    /// 周期 tick: 推进 FREG 代际与快照, 检查心跳并升级失败域, 按配额周期重置 CPU 配额.
    pub fn tick(&self, current_tick: u64) {
        if self.count.load(Ordering::Relaxed) == 0 {
            return;
        }
        let count = self.count.load(Ordering::SeqCst) as usize;
        for slot in &self.domains[..count] {
            if let Some(dom) = *slot {
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
                        dom.id()
                    );

                    dom.consecutive_failures.fetch_add(1, Ordering::SeqCst);
                    let failures = dom.consecutive_failures.load(Ordering::SeqCst);

                    if failures <= 2 {
                        crate::klog_ffi!(
                            klog_ffi_info,
                            "[FREG] domain {} attempt domain-level rollback (failures={})",
                            dom.id(),
                            failures
                        );
                        if dom.try_rollback(current_tick, 0) {
                            let (entries, _, _, _) = Self::rollback_domain(dom, current_tick, 0, 0);
                            crate::klog_ffi!(
                                klog_ffi_info,
                                "[FREG] domain {} rollback recovered (entries={})",
                                dom.id(),
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
                            dom.id()
                        );
                        crate::privileged::freg::NEED_BSR_ESCALATION.store(true, Ordering::SeqCst);
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

    /// 按域标识查找恢复域 (`direct_map` 命中则 O(1), 否则线性扫描).
    pub fn find(&self, id: u64) -> Option<&'static RecoveryDomain> {
        let idx = crate::privileged::glue::usize_from_u64(id);
        if idx < DIRECT_MAP_SIZE {
            self.direct_map[idx]
        } else {
            (0..self.count.load(Ordering::SeqCst) as usize)
                .find_map(|i| self.domains[i].filter(|d| d.id() == id))
        }
    }

    /// 启动时核对各域的持久化崩溃指纹: 连续失败已达 3 次的域置为 Degraded 并清除指纹.
    pub fn check_boot_fingerprints(&self) {
        let count = self.count.load(Ordering::SeqCst) as usize;
        for slot in &self.domains[..count] {
            if let Some(dom) = *slot {
                let fp = dom.load_boot_fingerprint();
                if fp != 0 && dom.consecutive_failures.load(Ordering::SeqCst) >= 3 {
                    crate::klog_ffi!(
                        klog_ffi_warn,
                        "[FREG] domain {} has persistent crash fingerprint 0x{:X} ({} failures), starting in degraded mode",
                        dom.id(),
                        fp,
                        dom.consecutive_failures.load(Ordering::SeqCst)
                    );
                    dom.set_state(super::types::DomainState::Degraded, Ordering::SeqCst);
                    dom.clear_boot_fingerprint();
                }
            }
        }
    }

    /// 按故障指令地址 (`fault_rip`) 归属到所属恢复域, 返回域标识.
    pub fn locate_domain_by_addr(&self, fault_rip: u64) -> Option<u64> {
        let count = self.count.load(Ordering::SeqCst) as usize;
        for slot in &self.domains[..count] {
            if let Some(dom) = *slot
                && dom.contains_addr(fault_rip)
            {
                return Some(dom.id());
            }
        }
        None
    }

    /// 按 panic 消息前缀 (静态映射表) 归属到已注册的恢复域, 返回域标识.
    pub fn locate_domain_by_panic_msg(&self) -> Option<u64> {
        let msg = super::PANIC_MSG.lock();
        let count = self.count.load(Ordering::SeqCst) as usize;

        for mapping in &PANIC_DOMAIN_MAP {
            if msg.len() >= mapping.prefix.len() && msg[..mapping.prefix.len()] == *mapping.prefix {
                for slot in &self.domains[..count] {
                    if let Some(dom) = *slot
                        && dom.id() == mapping.domain_id
                    {
                        return Some(mapping.domain_id);
                    }
                }
            }
        }
        None
    }

    /// 对单个域执行指定深度的回滚, 返回 (回滚条目数, 起始代际, 目标代际, 结果码).
    pub fn rollback_domain(
        dom: &RecoveryDomain,
        tick: u64,
        fingerprint: u64,
        depth: usize,
    ) -> (usize, u64, u64, i32) {
        let gen_from = dom.freg_generation.load(Ordering::SeqCst);
        let target_gen = if depth > 0 {
            dom.get_rollback_generation(crate::privileged::glue::low_u32_from_usize(depth))
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
            domain_id: dom.id(),
            generation_from: gen_from,
            generation_to: target_gen,
            entries_rolled_back,
            crash_fingerprint: fingerprint,
            cascade_depth: depth,
            result,
        });

        (entries_rolled_back, gen_from, target_gen, result)
    }

    /// 以 BFS 沿依赖边级联回滚 (起点为 `domain_id`), 返回实际完成回滚的域数.
    pub fn cascade_rollback(&self, domain_id: u64, tick: u64, fingerprint: u64) -> usize {
        let count = self.count.load(Ordering::SeqCst) as usize;
        let mut queue: [u64; MAX_RECOVERY_DOMAINS] = [0; MAX_RECOVERY_DOMAINS];
        let mut queue_head = 0usize;
        let mut queue_tail = 0usize;
        let mut visited = [false; MAX_RECOVERY_DOMAINS];
        let mut rolled_back = 0usize;

        let target_idx = (0..count).find(|&i| self.domains[i].is_some_and(|d| d.id() == domain_id));

        if let Some(idx) = target_idx {
            visited[idx] = true;
            queue[queue_tail] = domain_id;
            queue_tail += 1;
        }

        while queue_head < queue_tail {
            let current_id = queue[queue_head];
            queue_head += 1;

            if let Some(dom) = self.find(current_id)
                && dom.try_rollback(tick, fingerprint)
            {
                let (entries, _, _, _) = Self::rollback_domain(dom, tick, fingerprint, rolled_back);
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
                        let dep_idx =
                            (0..count).find(|&i| self.domains[i].is_some_and(|d| d.id() == dep_id));
                        if let Some(di) = dep_idx
                            && !visited[di]
                        {
                            visited[di] = true;
                            queue[queue_tail] = dep_id;
                            queue_tail += 1;
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
            // SAFETY: f 由域所有者注册, 契约由 privileged::freg::api 维护。
            unsafe { f() };
        }
    }

    /// 调用域的 rollback 回调 (回滚完成后)
    // SAFETY: cb 由域所有者注册; 本函数在 unsafe 上下文调用回调, 契约由 privileged::freg::api 维护.
    pub fn invoke_rollback_cb(cb: Option<unsafe fn() -> bool>) {
        if let Some(f) = cb {
            // SAFETY: 同上。
            unsafe { f() };
        }
    }
}
