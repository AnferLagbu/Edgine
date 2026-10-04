//! 物理页帧分配决策 trait — 策略-机制分离接口
//!
//! T-02: 物理页帧分配策略 (是否允许分配、NUMA 节点选择、分配失败回退)
//! 由 functions 实现, privileged 仅保留 buddy 分配器机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackAllocPolicy`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_alloc_decision()` 注册自己的策略实现
//!
//! ## 策略边界
//!
//! buddy 分配器的分裂/合并/链表操作是机制 (必须 unsafe), 不可提取.
//! 可提取的策略决策:
//! - 分配前: 是否允许分配 (内存压力判定)
//! - 分配时: NUMA 节点选择
//! - 分配失败: 是否触发回收/压缩后重试

/// 分配请求上下文 — 传递给策略决策的只读信息
#[derive(Debug, Clone, Copy)]
pub struct AllocContext {
    /// 请求的页数
    pub requested_pages: usize,
    /// 当前空闲页数
    pub free_pages: u64,
    /// 总页数
    pub total_pages: u64,
    /// 当前内存压力级别 (0=Normal, 1=Warning, 2=Critical, 3=Emergency)
    pub pressure_level: u8,
    /// 请求的 NUMA 节点 (None 表示无偏好)
    pub preferred_node: Option<u8>,
}

/// 分配决策结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocDecision {
    /// 允许分配
    Allow,
    /// 拒绝分配 (内存压力过高)
    Deny,
    /// 拒绝本次, 但建议触发回收后重试
    RetryAfterReclaim,
}

/// 物理页帧分配决策接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait FrameAllocDecision: Send + Sync {
    /// 分配前决策: 是否允许此次分配
    fn decide_alloc(&self, ctx: AllocContext) -> AllocDecision;

    /// 分配失败后决策: 是否触发回收后重试
    fn on_alloc_failed(&self, ctx: AllocContext) -> AllocDecision;

    /// NUMA 节点选择: 为此次分配选择目标节点
    ///
    /// 返回 None 表示使用默认 (本地节点) 策略.
    fn select_numa_node(&self, ctx: AllocContext) -> Option<u8>;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略 — 总是允许分配, 不做 NUMA 选择
///
/// 在 functions 注册策略之前, PMM 使用此策略.
/// 逻辑与原 `pmm.rs` 硬编码行为一致 (无限制分配).
pub struct FallbackAllocPolicy;

impl FrameAllocDecision for FallbackAllocPolicy {
    fn decide_alloc(&self, _ctx: AllocContext) -> AllocDecision {
        AllocDecision::Allow
    }

    fn on_alloc_failed(&self, _ctx: AllocContext) -> AllocDecision {
        // 默认行为: 分配失败就是失败, 不重试
        AllocDecision::Deny
    }

    fn select_numa_node(&self, _ctx: AllocContext) -> Option<u8> {
        None
    }
}

static FALLBACK_POLICY: FallbackAllocPolicy = FallbackAllocPolicy;

/// 全局策略注册表 — functions 通过 `register_alloc_decision` 注册
static ALLOC_DECISION: crate::privileged::sync::OnceLock<&'static dyn FrameAllocDecision> =
    crate::privileged::sync::OnceLock::new();

/// 注册分配决策策略 (由 `functions::mm::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
/// # Errors
/// 策略已被注册过时返回 Err。
pub fn register_alloc_decision(
    policy: &'static dyn FrameAllocDecision,
) -> Result<(), &'static dyn FrameAllocDecision> {
    match ALLOC_DECISION.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的分配决策策略 (未注册时返回内建回退)
#[inline]
pub fn current_alloc_decision() -> &'static dyn FrameAllocDecision {
    match ALLOC_DECISION.get() {
        Some(&p) => p,
        None => &FALLBACK_POLICY,
    }
}
