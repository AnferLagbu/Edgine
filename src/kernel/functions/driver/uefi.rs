#![deny(unsafe_code)]
//! UEFI 运行时服务安全代理 — functions 层 (0 unsafe)
//!
//! 封装 `privileged::driver::uefi` 的安全 API.

// 重导出强类型
pub use crate::privileged::driver::{
    EFI_MAX_VAR_DATA, EFI_MAX_VAR_NAME, EFI_VARIABLE_BOOTSERVICE_ACCESS, EFI_VARIABLE_NON_VOLATILE,
    EFI_VARIABLE_RUNTIME_ACCESS, EfiGopModeInfo, EfiMemoryDescriptor, EfiMemoryType,
    EfiPixelFormat, EfiTime, EfiVariable, UefiSubsystem,
};

use crate::privileged::driver::{sys_uefi, uefi_init, uefi_is_initialized, uefi_subsystem};

/// 初始化 UEFI
pub fn init(system_table_addr: u64) {
    uefi_init(system_table_addr);
}

/// UEFI 是否已初始化
pub fn is_initialized() -> bool {
    uefi_is_initialized()
}

/// 获取全局 UEFI 子系统
pub fn subsystem() -> &'static UefiSubsystem {
    uefi_subsystem()
}

/// UEFI 系统调用 (安全封装)
pub fn uefi_syscall(cmd: u64, a1: u64, a2: u64) -> i64 {
    sys_uefi(cmd, a1, a2)
}
