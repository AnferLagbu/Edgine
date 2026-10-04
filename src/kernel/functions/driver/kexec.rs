#![deny(unsafe_code)]
//! kexec 安全代理 — functions 层 (0 unsafe)
//!
//! 封装 `privileged::driver::kexec` 的安全 API.

// 重导出强类型
pub use crate::privileged::driver::{
    KEXEC_DEFAULT_INITRD_ADDR, KEXEC_DEFAULT_LOAD_ADDR, KEXEC_MAX_CMDLINE, KEXEC_MAX_INITRD_SIZE,
    KEXEC_MAX_KERNEL_SIZE, KexecSegType, KexecSegment, KexecState, KexecSubsystem,
};

use crate::privileged::driver::{kexec_init, kexec_is_initialized, kexec_subsystem, sys_kexec};

/// 初始化 kexec
pub fn init() {
    kexec_init();
}

/// kexec 是否已初始化
pub fn is_initialized() -> bool {
    kexec_is_initialized()
}

/// 获取全局 kexec 子系统
pub fn subsystem() -> &'static KexecSubsystem {
    kexec_subsystem()
}

/// kexec 系统调用 (安全封装)
pub fn kexec_syscall(cmd: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    sys_kexec(cmd, a1, a2, a3)
}
