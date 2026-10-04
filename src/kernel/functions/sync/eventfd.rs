#![deny(unsafe_code)]
//! eventfd — functions 层安全代理
//!
//! 为 eventfd 系统调用提供参数验证和类型安全封装:
//! - `eventfd_syscall`: 验证 initval 和 flags
//! - `eventfd_read_syscall`: 验证 fd 和 buf
//! - `eventfd_write_syscall`: 验证 fd 和 value
//! - `eventfd_close_syscall`: 验证 fd
//!
//! ## 安全边界
//!
//! - functions 层验证标量参数 (initval/flags/fd/value)
//! - 原始指针 (`buf`) 委托给 privileged 层
//!   (指针合法性由 syscall 入口 `check_user_ptr` 保证)

use crate::privileged::syscall::Errno;

/// `EFD_SEMAPHORE` 标志
pub const EFD_SEMAPHORE: i32 = crate::privileged::syscall::eventfd::EFD_SEMAPHORE;
/// `EFD_NONBLOCK` 标志
pub const EFD_NONBLOCK: i32 = crate::privileged::syscall::eventfd::EFD_NONBLOCK;
/// `EFD_CLOEXEC` 标志
pub const EFD_CLOEXEC: i32 = crate::privileged::syscall::eventfd::EFD_CLOEXEC;

/// eventfd FD 空间起始
pub const EFD_FD_BASE: i32 = crate::privileged::syscall::eventfd::EFD_FD_BASE;

/// `eventfd_create` 安全代理
///
/// 验证: flags 仅包含已知标志位
///
/// # Errors
///
/// - flags 含未知标志位 → `EINVAL`
/// - 底层 `sys_eventfd` 返回负值时转换为对应的 `Errno`
pub fn eventfd_syscall(initval: u64, flags: i32) -> Result<usize, Errno> {
    // flags 校验: 只允许已知标志
    let valid_flags = EFD_CLOEXEC | EFD_NONBLOCK | EFD_SEMAPHORE;
    if flags & !valid_flags != 0 {
        return Err(Errno::EINVAL);
    }

    let ret = crate::privileged::syscall::eventfd::sys_eventfd(initval, flags);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// eventfd read 安全代理
///
/// 验证: fd 在 eventfd 空间内, buf 非零
///
/// # Errors
///
/// - `fd` 不在 eventfd FD 空间 → `EBADF`
/// - `buf == 0` → `EFAULT`
/// - 底层 `sys_eventfd_read` 返回负值时转换为对应的 `Errno`
pub fn eventfd_read_syscall(fd: i32, buf: u64) -> Result<usize, Errno> {
    if !crate::privileged::syscall::eventfd::is_eventfd_fd(fd) {
        return Err(Errno::EBADF);
    }
    if buf == 0 {
        return Err(Errno::EFAULT);
    }

    let ret = crate::privileged::syscall::eventfd::sys_eventfd_read(fd, buf);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// eventfd write 安全代理
///
/// 验证: fd 在 eventfd 空间内, value > 0
///
/// # Errors
///
/// - `fd` 不在 eventfd FD 空间 → `EBADF`
/// - `value == 0` → `EINVAL`
/// - 底层 `sys_eventfd_write` 返回负值时转换为对应的 `Errno`
pub fn eventfd_write_syscall(fd: i32, value: u64) -> Result<usize, Errno> {
    if !crate::privileged::syscall::eventfd::is_eventfd_fd(fd) {
        return Err(Errno::EBADF);
    }
    if value == 0 {
        return Err(Errno::EINVAL);
    }

    let ret = crate::privileged::syscall::eventfd::sys_eventfd_write(fd, value);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// eventfd close 安全代理
///
/// 验证: fd 在 eventfd 空间内
///
/// # Errors
///
/// - `fd` 不在 eventfd FD 空间 → `EBADF`
/// - 底层 `sys_eventfd_close` 返回负值时转换为对应的 `Errno`
pub fn eventfd_close_syscall(fd: i32) -> Result<usize, Errno> {
    if !crate::privileged::syscall::eventfd::is_eventfd_fd(fd) {
        return Err(Errno::EBADF);
    }

    let ret = crate::privileged::syscall::eventfd::sys_eventfd_close(fd);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}
