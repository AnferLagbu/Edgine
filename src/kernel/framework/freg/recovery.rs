//! `RecoveryDomain` Trait — 子系统级崩溃恢复接口
//!
//! 将"域"的概念推广到子系统级: 文件系统、网络栈、驱动各为一个 recovery domain。
//! 当一个域崩溃时，隔离并重启该域而不影响其他服务。
//!
//! ## 拓扑恢复
//!
//! 按依赖拓扑序执行级联恢复: 子域先恢复, 父域后恢复。
//!
//! ## 使用
//!
//! ```text
//! UNKFS init:  recovery_domain_register("unkfs", 2, &[SPA_DOMAIN]);
//! Net init:   recovery_domain_register("net",  5, &[]);
//! ```
//!
//! ## SAFETY
//!
//! 子系统的 save/restore/reset 回调在中断上下文中执行, 必须无阻塞、无锁竞争。

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::framework::sync::IrqSpinLock;
pub type DomainId = u64;

pub const DOMAIN_ID_UNKFS: DomainId = 2;
pub const DOMAIN_ID_NET: DomainId = 5;

pub trait RecoverableDomain: Send + Sync {
    fn name(&self) -> &'static str;
    fn save_checkpoint(&self);
    fn restore_checkpoint(&self);
    fn reset(&self);
    fn dependencies(&self) -> &'static [DomainId];
    fn is_healthy(&self) -> bool {
        true
    }
}

pub(crate) struct RegisteredDomain {
    id: DomainId,
    name: String,
    deps: &'static [DomainId],
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    save_fn: unsafe fn(),
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    restore_fn: unsafe fn(),
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    reset_fn: unsafe fn(),
}

impl RegisteredDomain {}

pub struct RecoveryRegistry {
    pub(crate) registered: Vec<RegisteredDomain>,
    pub next_id: AtomicU64,
    pub initialized: bool,
}

impl RecoveryRegistry {
    pub const fn new() -> Self {
        Self {
            registered: Vec::new(),
            next_id: AtomicU64::new(16),
            initialized: false,
        }
    }
}

static RECOVERY_REGISTRY: IrqSpinLock<RecoveryRegistry> = IrqSpinLock::new(RecoveryRegistry::new());

pub fn recovery_registry_init() {
    let mut reg = RECOVERY_REGISTRY.lock();
    if reg.initialized {
        return;
    }
    reg.registered = Vec::new();
    reg.initialized = true;
}

pub fn recovery_domain_register(
    name: &'static str,
    prefer_id: DomainId,
    deps: &'static [DomainId],
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    save_fn: unsafe fn(),
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    restore_fn: unsafe fn(),
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    reset_fn: unsafe fn(),
) -> DomainId {
    let mut reg = RECOVERY_REGISTRY.lock();
    let id = if prefer_id != 0 {
        for r in &reg.registered {
            if r.id == prefer_id {
                return prefer_id;
            }
        }
        prefer_id
    } else {
        reg.next_id.fetch_add(1, Ordering::Relaxed)
    };

    reg.registered.push(RegisteredDomain {
        id,
        name: String::from(name),
        deps,
        save_fn,
        restore_fn,
        reset_fn,
    });
    id
}

pub fn recovery_subdomain_save_checkpoint(domain_id: DomainId) {
    let reg = RECOVERY_REGISTRY.lock();
    for r in &reg.registered {
        if r.id == domain_id {
            raw::invoke_save(r.save_fn);
            return;
        }
    }
}

fn has_dependency(sub_id: DomainId, on_id: DomainId) -> bool {
    let reg = RECOVERY_REGISTRY.lock();
    for r in &reg.registered {
        if r.id == sub_id {
            return r.deps.contains(&on_id);
        }
    }
    false
}

pub fn compute_recovery_order(root_id: DomainId) -> Vec<DomainId> {
    let reg = RECOVERY_REGISTRY.lock();
    let all_ids: Vec<DomainId> = reg.registered.iter().map(|r| r.id).collect();

    let mut order = Vec::<DomainId>::new();
    let mut visited = alloc::vec![false; 64];
    let mut stack = Vec::<DomainId>::new();

    stack.push(root_id);

    while let Some(id) = stack.pop() {
        let idx = all_ids.iter().position(|&x| x == id);
        if idx.is_none() {
            continue;
        }
        if let Some(i) = idx {
            if visited[i] {
                continue;
            }
            visited[i] = true;

            for r in &reg.registered {
                if r.id == id {
                    for &dep in r.deps {
                        let dep_idx = all_ids.iter().position(|&x| x == dep);
                        if let Some(di) = dep_idx {
                            if !visited[di] {
                                stack.push(dep);
                            }
                        }
                    }
                    break;
                }
            }
        }
        order.push(id);
    }

    for &id in &all_ids {
        if let Some(idx) = all_ids.iter().position(|&x| x == id) {
            if !visited[idx] && id != root_id {
                if has_dependency(id, root_id) {
                    order.push(id);
                }
            }
        }
    }

    order
}

pub fn cascade_recover(domain_id: DomainId) -> usize {
    let order = compute_recovery_order(domain_id);
    let reg = RECOVERY_REGISTRY.lock();
    let mut recovered = 0usize;

    for &id in &order {
        for r in &reg.registered {
            if r.id == id {
                crate::klog_info!(Kernel, "freg: recovering domain {} (id={})", r.name, id);
                raw::invoke_restore(r.restore_fn);
                recovered += 1;
                break;
            }
        }
    }
    recovered
}

pub fn hard_reset_domain(domain_id: DomainId) {
    let reg = RECOVERY_REGISTRY.lock();
    for r in &reg.registered {
        if r.id == domain_id {
            crate::klog_warn!(
                Kernel,
                "freg: hard reset domain {} (id={})",
                r.name,
                domain_id
            );
            raw::invoke_reset(r.reset_fn);
            return;
        }
    }
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 FFI 恢复回调调用
// ============================================================================
//
// `RecoveryRegistration` 的 `save_fn`/`restore_fn`/`reset_fn` 是 `unsafe fn()`,
// 必须在 unsafe 上下文中调用。集中到 raw 子模块便于审计。

pub(crate) mod raw {
    /// 调用恢复注册项的 save 回调
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    pub fn invoke_save(f: unsafe fn()) {
        // SAFETY: f 由域所有者注册, 契约由 framework::freg::recovery 维护。
        unsafe { f() };
    }

    /// 调用恢复注册项的 restore 回调
    // SAFETY: f 由域所有者注册, 契约由 framework::freg::recovery 维护.
    pub fn invoke_restore(f: unsafe fn()) {
        // SAFETY: 同上。
        unsafe { f() };
    }

    /// 调用恢复注册项的 reset 回调
    // SAFETY: f 由域所有者注册, 契约由 framework::freg::recovery 维护.
    pub fn invoke_reset(f: unsafe fn()) {
        // SAFETY: 同上。
        unsafe { f() };
    }
}
