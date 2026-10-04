//! 中断处理决策 trait — 策略-机制分离接口
//!
//! T-04: 中断处理策略 (IRQ handler 选择、softirq 优先级) 由 functions 实现,
//! privileged 仅保留 IDT/中断控制器/EOI 等机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackIrqDecision`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_irq_decision()` 注册自己的策略实现
//!
//! ## 策略边界
//!
//! privileged 保留 (机制):
//! - IDT 表设置与门描述符编程
//! - 8259A/APIC EOI 发送
//! - 假性 IRQ 检测
//! - 中断上下文保存/恢复
//! - softirq 执行循环
//!
//! functions 实现 (策略):
//! - IRQ 共享时的 handler 优先级
//! - softirq 执行顺序
//! - 中断亲和性决策

/// 中断请求上下文 — 传递给策略决策的只读信息
#[derive(Debug, Clone, Copy)]
pub struct IrqContext {
    /// IRQ 编号
    pub irq: u8,
    /// 是否为共享 IRQ
    pub is_shared: bool,
    /// 已注册 handler 数量
    pub handler_count: u32,
}

/// Softirq 优先级决策上下文
#[derive(Debug, Clone, Copy)]
pub struct SoftirqContext {
    /// 待处理的 softirq 位掩码
    pub pending_mask: u64,
}

/// 中断处理决策接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait IrqDecision: Send + Sync {
    /// 选择共享 IRQ 中的优先 handler 索引
    ///
    /// `ctx.handler_count` 为已注册 handler 数量.
    /// 返回 0 表示第一个 handler, 1 表示第二个, 以此类推.
    /// 默认返回 0 (第一个注册的 handler).
    fn select_handler_index(&self, ctx: IrqContext) -> usize;

    /// 决定 softirq 执行顺序
    ///
    /// 返回待处理的 softirq 位掩码中应优先执行的位.
    /// 默认返回最高优先级位.
    fn softirq_priority_mask(&self, ctx: SoftirqContext) -> u64;

    /// 是否允许在当前 CPU 唤醒 ksoftirqd
    ///
    /// 当 softirq 循环超过阈值时, 可选择将后续处理委托给 ksoftirqd 内核线程.
    fn should_wake_ksoftirqd(&self, loop_count: u32) -> bool;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略 — 固定优先级, 简单规则
///
/// 在 functions 注册策略之前, 中断处理使用此策略.
pub struct FallbackIrqDecision;

impl IrqDecision for FallbackIrqDecision {
    fn select_handler_index(&self, _ctx: IrqContext) -> usize {
        0
    }

    fn softirq_priority_mask(&self, ctx: SoftirqContext) -> u64 {
        // 返回最高位 (最高优先级)
        if ctx.pending_mask == 0 {
            0
        } else {
            1u64 << ctx.pending_mask.ilog2()
        }
    }

    fn should_wake_ksoftirqd(&self, loop_count: u32) -> bool {
        // 超过 10 次循环后唤醒 ksoftirqd
        loop_count > 10
    }
}

static FALLBACK_DECISION: FallbackIrqDecision = FallbackIrqDecision;

/// 全局策略注册表 — functions 通过 `register_irq_decision` 注册
static IRQ_DECISION: crate::privileged::sync::OnceLock<&'static dyn IrqDecision> =
    crate::privileged::sync::OnceLock::new();

/// 注册中断处理决策策略 (由 `functions::driver::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
/// # Errors
/// 策略已被注册过时返回 Err。
pub fn register_irq_decision(
    policy: &'static dyn IrqDecision,
) -> Result<(), &'static dyn IrqDecision> {
    match IRQ_DECISION.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的中断处理决策策略 (未注册时返回内建回退)
#[inline]
pub fn current_irq_decision() -> &'static dyn IrqDecision {
    match IRQ_DECISION.get() {
        Some(&p) => p,
        None => &FALLBACK_DECISION,
    }
}
