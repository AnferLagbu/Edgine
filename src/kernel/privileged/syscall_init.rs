//! Syscall 子系统初始化 — privileged TCB
//!
//! ## 职责
//!
//! 这是 functions 层与 `kernel::syscall::syscall_init` 之间的**唯一** unsafe 边界。
//! functions 层 0 unsafe。

use crate::privileged::syscall;

/// 初始化 syscall 子系统 (仅 privileged 层: MSR/STAR/LSTAR 配置)
///
/// # Safety
///
/// 启动阶段单线程调用, 内部仅输出日志。
pub fn syscall_init() {
    // SAFETY: klog_write 是 C-ABI 日志函数; 启动阶段单线程
    unsafe { syscall::syscall_init() }
}
