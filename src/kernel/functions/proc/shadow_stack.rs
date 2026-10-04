#![deny(unsafe_code)]
//! Shadow Stack (CET) 安全代理 — functions 层 (0 unsafe)
//!
//! 封装 `privileged::arch::shadow_stack` 的安全 API.

// 重导出强类型
pub use crate::privileged::arch::{
    CetCapabilities, CetSubsystem, SHADOW_STACK_ALIGN, SHADOW_STACK_DEFAULT_SIZE,
    SHADOW_STACK_PAGE_SIZE, ShadowStack,
};

use crate::privileged::arch::{cet_init, cet_is_initialized, cet_subsystem, sys_cet};

/// 初始化 CET
pub fn init() {
    cet_init();
}

/// CET 是否已初始化
pub fn is_initialized() -> bool {
    cet_is_initialized()
}

/// 获取全局 CET 子系统
pub fn subsystem() -> &'static CetSubsystem {
    cet_subsystem()
}

/// CET 系统调用 (安全封装)
pub fn cet_syscall(cmd: u64, a1: u64, a2: u64) -> i64 {
    sys_cet(cmd, a1, a2)
}
