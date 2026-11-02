#![deny(unsafe_code)]
//! 进程生命周期策略 — fork / exit / sched_yield
//!
//! 从 privileged/syscall/mod.rs 迁移的策略代码:
//! - fork_syscall: 进程创建
//! - exit_syscall: 进程退出
//! - sched_yield_syscall: 调度让步
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 通过 privileged::proc 公开 API 访问
//! - 无 unsafe, 无裸指针

/// `fork()` 策略
#[must_use]
pub fn fork_syscall() -> i64 {
    i64::from(crate::privileged::proc::sys_fork())
}

/// exit(status) 策略 — 仅结束调用线程 (非组长时组内其余线程继续运行)
#[must_use]
pub fn exit_syscall(status: i32) -> i64 {
    crate::privileged::proc::process_exit(status.cast_unsigned());
    0
}

/// `exit_group(status)` 策略 — 终止整个线程组, 组退出码以本值统一收口
#[must_use]
pub fn exit_group_syscall(status: i32) -> i64 {
    crate::privileged::proc::process_exit_group(status.cast_unsigned());
    0
}

/// `sched_yield()` 策略
#[must_use]
pub fn sched_yield_syscall() -> i64 {
    crate::privileged::proc::scheduler_yield();
    0
}
