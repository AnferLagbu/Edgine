#![deny(unsafe_code)]
//! 中断安全自旋锁 (functions 层 — 类型别名指向 privileged)
//!
//! ## 与 `SpinLock` 的区别
//!
//! | 特性 | `SpinLock` | `IrqSpinLock` |
//! |------|-----------|---------------|
//! | 中断屏蔽 | ❌ | ✅ (lock 时屏蔽, unlock 时恢复) |
//! | 中断上下文 | ❌ | ❌ (嵌套屏蔽会丢中断) |
//! | 临界区开销 | 低 | 中 (save/restore IF) |
//!
//! ## @SAFE
//! 本文件不含 `unsafe`. 内部委托 `privileged::sync::IrqSpinLock` (TCB)。
//!
//! ## 使用约束
//!
//! - 不可在中断上下文使用 (会嵌套屏蔽, 丢失中断)。
//! - 不可与 `SpinLock` 嵌套使用 (顺序无定义)。
//!
//! ## 示例
//!
//! ```ignore
//! let data = IrqSpinLock::new(0u32);
//! data.with_mut(|g| *g += 1);
//! ```

/// 中断安全自旋锁 (类型别名, 指向 privileged 提供的 TCB 实现)。
pub type IrqSpinLock<T> = crate::privileged::sync::IrqSpinLock<T>;

// ============================================================================
// 单元自检
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_lock_basic() {
        let l = IrqSpinLock::new(42u32);
        assert_eq!(*l.lock(), 42);
    }

    #[test]
    fn irq_lock_with_mut() {
        let l = IrqSpinLock::new(0u32);
        l.with_mut(|g| *g += 1);
        l.with_mut(|g| *g += 10);
        assert_eq!(*l.lock(), 11);
    }
}
