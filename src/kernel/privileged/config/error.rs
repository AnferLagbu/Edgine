//! 配置校验结果类型 — privileged 机制类型
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原类型于 T6-9 (2026-06-16) 迁至 `functions::config::error`, 本文件壳已删
//! (第三批 eed99468)。按"机制持有的数据结构/常量归 privileged"统一判据反转：
//! `ConfigError` 为 `ConfigValidateHook` trait 的返回类型 (DECISION-K 项 2),
//! 被 privileged config 机制 (启动校验编排) 直接消费 — 属机制类型, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::config::ConfigError`
//! (config 子模块私有, 经顶层显式转发) 保持 API 兼容。
//! 本文件 0 unsafe (纯类型 + Display).

use core::fmt;

/// 配置校验结果.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// 检测到的 CPU 数超过 `MAX_CPUS`.
    CpuCountExceedsMax { actual: u32, max: usize },
    /// 内存布局不一致 (例如 `PAGE_SIZE` 不是 2 的幂).
    MemoryLayoutInvalid,
    /// 检测到的中断控制器 (APIC/IOAPIC/PIC) 未初始化.
    IrqControllerUnavailable,
    /// 跨模块常量冲突.
    InconsistentConstant {
        name: &'static str,
        lhs: u64,
        rhs: u64,
    },
    /// 驱动特定配置非法.
    DriverConfigInvalid(&'static str),
    /// Slab 默认大小不是 2 的幂.
    SlabNotPowerOfTwo,
    /// Slab 默认大小未与页大小对齐.
    SlabMisaligned,
    /// Slab 默认大小超过合理上限.
    SlabTooLarge,
    /// 栈大小不是页大小的整数倍.
    StackMisaligned,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CpuCountExceedsMax { actual, max } => {
                write!(f, "CPU count {actual} exceeds MAX_CPUS {max}")
            }
            Self::MemoryLayoutInvalid => write!(f, "memory layout invalid"),
            Self::IrqControllerUnavailable => {
                write!(f, "no interrupt controller initialized")
            }
            Self::InconsistentConstant { name, lhs, rhs } => {
                write!(
                    f,
                    "constant {name} mismatch: config.rs={lhs} vs submodule={rhs}"
                )
            }
            Self::DriverConfigInvalid(name) => {
                write!(f, "driver {name} misconfigured")
            }
            Self::SlabNotPowerOfTwo => write!(f, "slab default size is not power of two"),
            Self::SlabMisaligned => write!(f, "slab default size is not page-aligned"),
            Self::SlabTooLarge => write!(f, "slab default size exceeds upper bound"),
            Self::StackMisaligned => write!(f, "stack size is not a multiple of page size"),
        }
    }
}
