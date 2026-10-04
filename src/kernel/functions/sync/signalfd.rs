#![deny(unsafe_code)]
//! signalfd — functions 层安全代理
//!
//! 为 signalfd 系统调用提供参数验证和类型安全封装:
//! - `signalfd_syscall`: 验证 fd 和 flags
//! - `signalfd_read_syscall`: 验证 fd 和 buf
//! - `signalfd_close_syscall`: 验证 fd
//!
//! ## 安全边界
//!
//! - functions 层验证标量参数 (fd/flags)
//! - 原始指针 (`mask_ptr` / `buf`) 委托给 privileged 层
//!   (指针合法性由 syscall 入口 `check_user_ptr` 保证)

use crate::privileged::syscall::Errno;

/// `SFD_NONBLOCK` 标志
pub const SFD_NONBLOCK: i32 = crate::privileged::syscall::signalfd::SFD_NONBLOCK;
/// `SFD_CLOEXEC` 标志
pub const SFD_CLOEXEC: i32 = crate::privileged::syscall::signalfd::SFD_CLOEXEC;

/// signalfd FD 空间起始
pub const SFD_FD_BASE: i32 = crate::privileged::syscall::signalfd::SFD_FD_BASE;

/// `signalfd_siginfo` 大小
pub const SIGNALFD_SIGINFO_SIZE: usize =
    crate::privileged::syscall::signalfd::SIGNALFD_SIGINFO_SIZE;

/// signalfd 安全代理
///
/// 验证: fd 有效 (-1 或在 signalfd 空间), flags 仅包含已知标志位
///
/// # Errors
///
/// - flags 含未知标志位 → `EINVAL`
/// - `fd` 既不是 -1 也不在 signalfd FD 空间 → `EBADF`
/// - `mask_ptr == 0` → `EFAULT`
/// - 底层 `sys_signalfd` 返回负值时转换为对应的 `Errno`
pub fn signalfd_syscall(fd: i32, mask_ptr: u64, flags: i32) -> Result<usize, Errno> {
    // flags 校验
    let valid_flags = SFD_CLOEXEC | SFD_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Err(Errno::EINVAL);
    }

    // fd 校验
    if fd != -1 && !crate::privileged::syscall::signalfd::is_signalfd_fd(fd) {
        return Err(Errno::EBADF);
    }

    // mask_ptr 校验
    if mask_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    let ret = crate::privileged::syscall::signalfd::sys_signalfd(fd, mask_ptr, flags);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// signalfd read 安全代理
///
/// 验证: fd 在 signalfd 空间内, buf 非零
///
/// # Errors
///
/// - `fd` 不在 signalfd FD 空间 → `EBADF`
/// - `buf == 0` → `EFAULT`
/// - 底层 `sys_signalfd_read` 返回负值时转换为对应的 `Errno`
pub fn signalfd_read_syscall(fd: i32, buf: u64) -> Result<usize, Errno> {
    if !crate::privileged::syscall::signalfd::is_signalfd_fd(fd) {
        return Err(Errno::EBADF);
    }
    if buf == 0 {
        return Err(Errno::EFAULT);
    }

    let ret = crate::privileged::syscall::signalfd::sys_signalfd_read(fd, buf);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// signalfd close 安全代理
///
/// 验证: fd 在 signalfd 空间内
///
/// # Errors
///
/// - `fd` 不在 signalfd FD 空间 → `EBADF`
/// - 底层 `sys_signalfd_close` 返回负值时转换为对应的 `Errno`
pub fn signalfd_close_syscall(fd: i32) -> Result<usize, Errno> {
    if !crate::privileged::syscall::signalfd::is_signalfd_fd(fd) {
        return Err(Errno::EBADF);
    }

    let ret = crate::privileged::syscall::signalfd::sys_signalfd_close(fd);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}
