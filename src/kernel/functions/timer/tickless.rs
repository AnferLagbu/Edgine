#![deny(unsafe_code)]
//! Tickless (`NO_HZ`) 安全代理 — functions 层 (0 unsafe)
//!
//! 封装 `privileged::timer::tickless` 的安全 API.

// 重导出强类型
pub use crate::privileged::timer::{
    DEFAULT_HZ, MAX_HZ, MIN_HZ, TicklessCpuState, TicklessMode, TicklessSubsystem,
};

use crate::privileged::timer::{
    sys_tickless, tickless_init, tickless_is_initialized, tickless_subsystem,
};

/// 初始化 Tickless
pub fn init(num_cpus: u32) {
    tickless_init(num_cpus);
}

/// Tickless 是否已初始化
pub fn is_initialized() -> bool {
    tickless_is_initialized()
}

/// 获取全局 Tickless 子系统
pub fn subsystem() -> &'static TicklessSubsystem {
    tickless_subsystem()
}

/// Tickless 系统调用 (安全封装)
pub fn tickless_syscall(cmd: u64, a1: u64, a2: u64) -> i64 {
    sys_tickless(cmd, a1, a2)
}
