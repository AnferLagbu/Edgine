//! 管道 (Pipe) FFI 边界 — T6-1 策略已迁移至 functions/ipc/pipe.rs
//!
//! 本模块仅保留:
//! - `is_pipe_fd()` 公开接口 (供 sendfile/splice 使用)
//! - `ipc_pipe_create()` 安全机制 (策略分发, 供 functions 层消费)
//! - FFI 函数 (用户空间指针转换, 委托 functions 策略)
//!
//! ## SAFETY
//!
//! - FFI 函数通过 `RacyCell::get_mut()` 安全访问全局 IPC_NAMESPACE.
//! - 用户空间指针通过 `UserReadPtr/WritePtr` 安全访问.

use crate::privileged::errno::Errno;
use crate::privileged::ipc::strategy::current_ipc_strategy;
use crate::privileged::proc::process_get_current_pid;
use crate::privileged::userptr::{UserReadPtr, UserWritePtr};

/// 判断 fd 是否为 pipe fd (公开接口, 供 sendfile/splice 使用)
pub fn is_pipe_fd(fd: i32) -> bool {
    current_ipc_strategy().map_or(false, |s| s.is_pipe_fd(fd))
}

/// POSIX `pipe()` 内核机制 — 经 IpcStrategy 创建管道, 返回 `(read_fd, write_fd)`.
///
/// 阶段 2-A (framekernel 范式): 用户态指针写入已下沉至 functions
/// (`functions::fs::io::pipe_syscall`); 本函数仅保留策略分发机制, 不触碰用户内存.
///
/// # Errors
/// - IpcStrategy 未注册 → `ENOSYS` (DECISION-K: 降级 + 日志, 不 panic)
/// - 策略创建失败 → `EBUSY`
pub fn ipc_pipe_create() -> Result<(i32, i32), Errno> {
    let ns = super::IPC_NAMESPACE.get_mut();
    let next_id = super::NEXT_IPC_ID.get_mut();
    let current_pid = process_get_current_pid();

    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_pipe_create");
        return Err(Errno::ENOSYS);
    };
    s.pipe_create(ns, next_id, current_pid)
        .map_err(|_| Errno::EBUSY)
}

/// POSIX `read(fd, buf, count)` 内核实现 (仅 pipe fd)。
///
/// # Safety
/// `buf` 必须是有效可写指针, 至少 `count` 字节, 内存必须在调用期间保持有效。
/// 由 `sys_read` 分发, cred 校验已通过。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipc_pipe_read(fd: i32, buf: *mut u8, count: u32) -> i32 {
    if buf.is_null() || count == 0 {
        return -1;
    }

    let ns = super::IPC_NAMESPACE.get_mut();
    // SAFETY: buf 已校验非空; 调用方保证其指向用户态内存中
    // 至少 `count` 个有效字节.
    let mut user_buf = unsafe { UserWritePtr::new(buf, count as usize) };
    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_pipe_read");
        return -(Errno::ENOSYS as i32);
    };
    s.pipe_read(ns, fd, user_buf.as_mut_slice(), count)
        .map_or(-1, |n| n as i32)
}

/// POSIX `write(fd, buf, count)` 内核实现 (仅 pipe fd)。
///
/// # Safety
/// `buf` 必须是有效可读指针, 至少 `count` 字节, 内存必须在调用期间保持有效。
/// 由 `sys_write` 分发, cred 校验已通过。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipc_pipe_write(fd: i32, buf: *const u8, count: u32) -> i32 {
    if buf.is_null() || count == 0 {
        return -1;
    }

    let ns = super::IPC_NAMESPACE.get_mut();
    // SAFETY: buf 已校验非空; 调用方保证其指向用户态内存中
    // 至少 `count` 个有效字节.
    let user_buf = unsafe { UserReadPtr::new(buf, count as usize) };
    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_pipe_write");
        return -(Errno::ENOSYS as i32);
    };
    s.pipe_write(ns, fd, user_buf.as_slice(), count)
        .map_or(-1, |n| n as i32)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ipc_pipe_close(fd: i32) -> i32 {
    let ns = super::IPC_NAMESPACE.get_mut();
    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_pipe_close");
        return -(Errno::ENOSYS as i32);
    };
    match s.pipe_close(ns, fd) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
