#![deny(unsafe_code)]
//! timerfd — functions 层安全代理
//!
//! 为 timerfd 系统调用提供参数验证和类型安全封装:
//! - `timerfd_create_syscall`: 验证 clockid 和 flags
//! - `timerfd_settime_syscall`: 验证 fd, flags, 指针
//! - `timerfd_gettime_syscall`: 验证 fd, 指针
//! - `timerfd_read_syscall`: 验证 fd 和 buf
//! - `timerfd_close_syscall`: 验证 fd
//!
//! ## 安全边界
//!
//! - functions 层验证标量参数 (clockid/flags/fd)
//! - 原始指针 (`new_value_ptr` / `old_value_ptr` / `curr_value_ptr` / `buf`)
//!   委托给 privileged 层 (指针合法性由 syscall 入口 `check_user_ptr` 保证)

use crate::privileged::syscall::Errno;

/// `TFD_CLOEXEC` 标志
pub const TFD_CLOEXEC: i32 = crate::privileged::syscall::timerfd::TFD_CLOEXEC;
/// `TFD_NONBLOCK` 标志
pub const TFD_NONBLOCK: i32 = crate::privileged::syscall::timerfd::TFD_NONBLOCK;
/// `TFD_TIMER_ABSTIME` 标志
pub const TFD_TIMER_ABSTIME: i32 = crate::privileged::syscall::timerfd::TFD_TIMER_ABSTIME;

/// `CLOCK_MONOTONIC`
pub const CLOCK_MONOTONIC: i32 = crate::privileged::syscall::timerfd::CLOCK_MONOTONIC;
/// `CLOCK_REALTIME`
pub const CLOCK_REALTIME: i32 = crate::privileged::syscall::timerfd::CLOCK_REALTIME;

/// timerfd FD 空间起始
pub const TFD_FD_BASE: i32 = crate::privileged::syscall::timerfd::TFD_FD_BASE;

/// `timerfd_create` 安全代理
///
/// 验证: clockid 有效 (MONOTONIC/REALTIME), flags 仅包含已知标志位
///
/// # Errors
///
/// - `clockid` 非法或 flags 含未知标志位 → `EINVAL`
/// - 底层 `sys_timerfd_create` 返回负值时转换为对应的 `Errno`
pub fn timerfd_create_syscall(clockid: i32, flags: i32) -> Result<usize, Errno> {
    // clockid 校验
    if clockid != CLOCK_MONOTONIC && clockid != CLOCK_REALTIME {
        return Err(Errno::EINVAL);
    }

    // flags 校验
    let valid_flags = TFD_CLOEXEC | TFD_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Err(Errno::EINVAL);
    }

    let ret = crate::privileged::syscall::timerfd::sys_timerfd_create(clockid, flags);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `timerfd_settime` 安全代理
///
/// 验证: fd 在 timerfd 空间内, flags 仅包含 `TFD_TIMER_ABSTIME`, 指针非零
///
/// # Errors
///
/// - `fd` 不在 timerfd FD 空间 → `EBADF`
/// - flags 含 `TFD_TIMER_ABSTIME` 之外的位 → `EINVAL`
/// - `new_value_ptr == 0` → `EFAULT`
/// - 底层 `sys_timerfd_settime` 返回负值时转换为对应的 `Errno`
pub fn timerfd_settime_syscall(
    fd: i32,
    flags: i32,
    new_value_ptr: u64,
    old_value_ptr: u64,
) -> Result<usize, Errno> {
    if !crate::privileged::syscall::timerfd::is_timerfd_fd(fd) {
        return Err(Errno::EBADF);
    }

    // flags 校验
    if flags & !TFD_TIMER_ABSTIME != 0 {
        return Err(Errno::EINVAL);
    }

    // new_value_ptr 必须非零
    if new_value_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    let ret = crate::privileged::syscall::timerfd::sys_timerfd_settime(
        fd,
        flags,
        new_value_ptr,
        old_value_ptr,
    );
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `timerfd_gettime` 安全代理
///
/// 验证: fd 在 timerfd 空间内, 指针非零
///
/// # Errors
///
/// - `fd` 不在 timerfd FD 空间 → `EBADF`
/// - `curr_value_ptr == 0` → `EFAULT`
/// - 底层 `sys_timerfd_gettime` 返回负值时转换为对应的 `Errno`
pub fn timerfd_gettime_syscall(fd: i32, curr_value_ptr: u64) -> Result<usize, Errno> {
    if !crate::privileged::syscall::timerfd::is_timerfd_fd(fd) {
        return Err(Errno::EBADF);
    }
    if curr_value_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    let ret = crate::privileged::syscall::timerfd::sys_timerfd_gettime(fd, curr_value_ptr);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// timerfd read 安全代理
///
/// 验证: fd 在 timerfd 空间内, buf 非零
///
/// # Errors
///
/// - `fd` 不在 timerfd FD 空间 → `EBADF`
/// - `buf == 0` → `EFAULT`
/// - 底层 `sys_timerfd_read` 返回负值时转换为对应的 `Errno`
pub fn timerfd_read_syscall(fd: i32, buf: u64) -> Result<usize, Errno> {
    if !crate::privileged::syscall::timerfd::is_timerfd_fd(fd) {
        return Err(Errno::EBADF);
    }
    if buf == 0 {
        return Err(Errno::EFAULT);
    }

    let ret = crate::privileged::syscall::timerfd::sys_timerfd_read(fd, buf);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// timerfd close 安全代理
///
/// 验证: fd 在 timerfd 空间内
///
/// # Errors
///
/// - `fd` 不在 timerfd FD 空间 → `EBADF`
/// - 底层 `sys_timerfd_close` 返回负值时转换为对应的 `Errno`
pub fn timerfd_close_syscall(fd: i32) -> Result<usize, Errno> {
    if !crate::privileged::syscall::timerfd::is_timerfd_fd(fd) {
        return Err(Errno::EBADF);
    }

    let ret = crate::privileged::syscall::timerfd::sys_timerfd_close(fd);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}
