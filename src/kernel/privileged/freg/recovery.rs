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
//! UNKFS init:  recovery_domain_register("unkfs", 2, &[]);
//! Net init:   recovery_domain_register("net",  5, &[]);
//! ```
//!
//! 当前注册点均未声明依赖 (`deps` 为空), 拓扑字段为后续跨域依赖预留.
//!
//! ## SAFETY
//!
//! 子系统的 save/restore/reset 回调在中断上下文中执行, 必须无阻塞、无锁竞争。

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::sync::IrqSpinLock;
/// 恢复域标识符 (u64 别名)。
pub type DomainId = u64;

pub const DOMAIN_ID_UNKFS: DomainId = 2;
pub const DOMAIN_ID_NET: DomainId = 5;

pub trait RecoverableDomain: Send + Sync {
    /// 返回该域名称 (用于日志与拓扑展示)。
    fn name(&self) -> &'static str;
    /// 保存该域检查点 (中断上下文执行, 须无阻塞、无锁竞争)。
    fn save_checkpoint(&self);
    /// 从最近检查点恢复该域状态。
    fn restore_checkpoint(&self);
    /// 将该域重置到初始状态。
    fn reset(&self);
    /// 返回该域依赖的域 ID 列表 (恢复拓扑排序用)。
    fn dependencies(&self) -> &'static [DomainId];
    /// 返回该域当前是否健康 (默认视为健康)。
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

/// 恢复域注册表: 维护已注册域及其 save/restore/reset 回调。
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

/// 初始化恢复域注册表 (幂等; 清空注册项并置初始化标志)。
pub fn recovery_registry_init() {
    let mut reg = RECOVERY_REGISTRY.lock();
    if reg.initialized {
        return;
    }
    reg.registered = Vec::new();
    reg.initialized = true;
}

/// 注册一个恢复域并返回其域 ID; `prefer_id` 非 0 时优先复用该 ID。
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

/// 对指定域调用其保存检查点回调 (未注册则忽略)。
pub fn recovery_subdomain_save_checkpoint(domain_id: DomainId) {
    let reg = RECOVERY_REGISTRY.lock();
    for r in &reg.registered {
        if r.id == domain_id {
            raw::invoke_save(r.save_fn);
            return;
        }
    }
}

/// 后序 DFS: 先递归恢复依赖, 再把当前域入序 (子域先于父域).
///
/// `visited` 在递归前置位, 既避免重复入序, 也防止依赖成环时无限递归;
/// `in_scope` 限制只在目标闭包内展开, 闭包外的域直接跳过.
fn push_post_order_edges(
    id: DomainId,
    edges: &[(DomainId, &[DomainId])],
    in_scope: &[bool],
    visited: &mut [bool],
    order: &mut Vec<DomainId>,
) {
    let Some(idx) = edges.iter().position(|(x, _)| *x == id) else {
        return;
    };
    if visited[idx] || !in_scope[idx] {
        return;
    }
    visited[idx] = true;

    for &dep in edges[idx].1 {
        push_post_order_edges(dep, edges, in_scope, visited, order);
    }
    order.push(id);
}

/// 按依赖边计算以 `root_id` 为根的恢复拓扑序 (依赖先于被依赖者).
///
/// 纳入范围是 root 的**双向可达闭包**:
/// - root 递归依赖的域 (必须先恢复, 否则 root 恢复时依赖不可用);
/// - **直接或间接**依赖 root 的域 (必须在 root 之后恢复), 及其自身依赖.
///
/// 早期实现只纳入「直接依赖 root」的一层, 间接依赖者 (A→B→root) 永不进入恢复
/// 序列; 本函数以双向扩展补齐该闭包. 递归深度受注册域数量约束 (注册表是
/// 无界 `Vec`, 域数量本身很小), `visited` 前置位同时兜住成环.
///
/// `edges` 为 `(域 ID, 该域依赖的域 ID 列表)`; 纯函数, 便于单测覆盖线性链 /
/// 菱形 / 成环 / root 未注册等拓扑. root 未注册时返回空序.
pub(crate) fn order_from_edges(
    root_id: DomainId,
    edges: &[(DomainId, &[DomainId])],
) -> Vec<DomainId> {
    if !edges.iter().any(|(id, _)| *id == root_id) {
        return Vec::new();
    }

    // 双向闭包扩展: 依赖方向 + 反向依赖方向各走一次.
    let mut in_scope = alloc::vec![false; edges.len()];
    if let Some(root_idx) = edges.iter().position(|(id, _)| *id == root_id) {
        in_scope[root_idx] = true;
    }
    let mut pending = alloc::vec![root_id];
    while let Some(cur) = pending.pop() {
        // 正向: cur 的依赖
        if let Some(i) = edges.iter().position(|(id, _)| *id == cur) {
            for &dep in edges[i].1 {
                if let Some(j) = edges.iter().position(|(id, _)| *id == dep) {
                    if !in_scope[j] {
                        in_scope[j] = true;
                        pending.push(dep);
                    }
                }
            }
        }
        // 反向: 依赖 cur 的域
        for (i, (id, deps)) in edges.iter().enumerate() {
            if !in_scope[i] && deps.contains(&cur) {
                in_scope[i] = true;
                pending.push(*id);
            }
        }
    }

    let mut order = Vec::<DomainId>::new();
    let mut visited = alloc::vec![false; edges.len()];
    push_post_order_edges(root_id, edges, &in_scope, &mut visited, &mut order);
    // 仅经反向边可达的域从 root 起 DFS 不可达, 按注册顺序补齐 (结果确定).
    for (i, (id, _)) in edges.iter().enumerate() {
        if in_scope[i] {
            push_post_order_edges(*id, edges, &in_scope, &mut visited, &mut order);
        }
    }
    order
}

/// 计算以 `root_id` 为根的恢复拓扑序 (依赖先于被依赖者).
pub fn compute_recovery_order(root_id: DomainId) -> Vec<DomainId> {
    // 先复制边表再释放锁: DFS 不持 `RECOVERY_REGISTRY` 锁 (避免长临界区/关中断).
    let edges: Vec<(DomainId, &'static [DomainId])> = {
        let reg = RECOVERY_REGISTRY.lock();
        reg.registered.iter().map(|r| (r.id, r.deps)).collect()
    };

    order_from_edges(root_id, &edges)
}

/// 按拓扑序级联恢复以 `domain_id` 为根的域, 返回成功恢复的域数量。
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

/// 对指定域调用其硬重置回调 (未注册则忽略)。
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
        // SAFETY: f 由域所有者注册, 契约由 privileged::freg::recovery 维护。
        unsafe { f() };
    }

    /// 调用恢复注册项的 restore 回调
    // SAFETY: f 由域所有者注册, 契约由 privileged::freg::recovery 维护.
    pub fn invoke_restore(f: unsafe fn()) {
        // SAFETY: 同上。
        unsafe { f() };
    }

    /// 调用恢复注册项的 reset 回调
    // SAFETY: f 由域所有者注册, 契约由 privileged::freg::recovery 维护.
    pub fn invoke_reset(f: unsafe fn()) {
        // SAFETY: 同上。
        unsafe { f() };
    }
}
