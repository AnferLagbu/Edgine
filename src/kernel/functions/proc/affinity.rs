#![deny(unsafe_code)]
//! CPU 亲和性策略 — sched_setaffinity / sched_getaffinity
//!
//! 从 privileged/syscall/mod.rs 迁移的策略代码:
//! - sched_setaffinity_syscall: 设置进程 CPU 亲和性掩码
//! - sched_getaffinity_syscall: 获取进程 CPU 亲和性掩码
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 通过 privileged::proc 和 privileged::syscall 公开 API 访问
//! - 无 unsafe, 无裸指针

use crate::privileged::syscall::Errno;

/// `sched_setaffinity(pid`, cpusetsize, mask) 策略
///
/// Linux 兼容 ABI:
/// - `pid == 0` 表示当前进程
/// - `cpusetsize` 必须 >= 8 (u64 掩码大小)
/// - `mask` 用户空间指针, 指向 64-bit 位图
#[must_use]
pub fn sched_setaffinity_syscall(pid: i32, cpusetsize: u32, mask_ptr: u64) -> i64 {
    if cpusetsize < 8 {
        return Errno::Einval.as_ret();
    }
    if mask_ptr == 0 || !crate::privileged::syscall::api::validate_user_buf(mask_ptr, 8) {
        return Errno::Efault.as_ret();
    }

    let Some(mask) = crate::privileged::syscall::api::read_u64_from_user(mask_ptr) else {
        return Errno::Efault.as_ret();
    };

    let target_pid = match pid {
        0 => crate::privileged::proc::process_get_current_pid(),
        p if p > 0 => p.cast_unsigned(),
        _ => return Errno::Einval.as_ret(),
    };

    if target_pid == 0 {
        return Errno::Esrch.as_ret();
    }

    let ok = crate::privileged::proc::process_with_mut(target_pid, |p| {
        use core::sync::atomic::Ordering;
        p.cpuset_allowed.store(mask, Ordering::Release);
    })
    .is_some();

    if !ok {
        return Errno::Esrch.as_ret();
    }

    0
}

/// `sched_getaffinity(pid`, cpusetsize, mask) 策略
///
/// Linux 兼容 ABI:
/// - `pid == 0` 表示当前进程
/// - 返回写入的字节数 (8) 成功
#[must_use]
pub fn sched_getaffinity_syscall(pid: i32, cpusetsize: u32, mask_ptr: u64) -> i64 {
    if cpusetsize < 8 {
        return Errno::Einval.as_ret();
    }
    if mask_ptr == 0 || !crate::privileged::syscall::api::validate_user_buf(mask_ptr, 8) {
        return Errno::Efault.as_ret();
    }

    let target_pid = match pid {
        0 => crate::privileged::proc::process_get_current_pid(),
        p if p > 0 => p.cast_unsigned(),
        _ => return Errno::Einval.as_ret(),
    };

    if target_pid == 0 {
        return Errno::Esrch.as_ret();
    }

    let mask = crate::privileged::proc::process_with(target_pid, |p| {
        use core::sync::atomic::Ordering;
        p.cpuset_allowed.load(Ordering::Acquire)
    })
    .unwrap_or(u64::MAX);

    if !crate::privileged::syscall::api::write_u64_to_user(mask_ptr, mask) {
        return Errno::Efault.as_ret();
    }

    8
}
