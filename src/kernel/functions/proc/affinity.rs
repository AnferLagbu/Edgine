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

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
#[expect(
    clippy::comparison_chain,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// `sched_setaffinity(pid`, cpusetsize, mask) 策略
///
/// Linux 兼容 ABI:
/// - `pid == 0` 表示当前进程
/// - `cpusetsize` 必须 >= 8 (u64 掩码大小)
/// - `mask` 用户空间指针, 指向 64-bit 位图
pub fn sched_setaffinity_syscall(pid: i32, cpusetsize: u32, mask_ptr: u64) -> i64 {
    if cpusetsize < 8 {
        return Errno::EINVAL.as_ret();
    }
    if mask_ptr == 0 || !crate::privileged::syscall::api::validate_user_buf(mask_ptr, 8) {
        return Errno::EFAULT.as_ret();
    }

    let mask = match crate::privileged::syscall::api::read_u64_from_user(mask_ptr) {
        Some(v) => v,
        None => return Errno::EFAULT.as_ret(),
    };

    let target_pid = if pid == 0 {
        crate::privileged::proc::process_get_current_pid()
    } else if pid > 0 {
        pid as u32
    } else {
        return Errno::EINVAL.as_ret();
    };

    if target_pid == 0 {
        return Errno::ESRCH.as_ret();
    }

    let ok = crate::privileged::proc::process_with_mut(target_pid, |p| {
        use core::sync::atomic::Ordering;
        p.cpuset_allowed.store(mask, Ordering::Release);
    })
    .is_some();

    if !ok {
        return Errno::ESRCH.as_ret();
    }

    0
}

#[expect(
    clippy::comparison_chain,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// `sched_getaffinity(pid`, cpusetsize, mask) 策略
///
/// Linux 兼容 ABI:
/// - `pid == 0` 表示当前进程
/// - 返回写入的字节数 (8) 成功
pub fn sched_getaffinity_syscall(pid: i32, cpusetsize: u32, mask_ptr: u64) -> i64 {
    if cpusetsize < 8 {
        return Errno::EINVAL.as_ret();
    }
    if mask_ptr == 0 || !crate::privileged::syscall::api::validate_user_buf(mask_ptr, 8) {
        return Errno::EFAULT.as_ret();
    }

    let target_pid = if pid == 0 {
        crate::privileged::proc::process_get_current_pid()
    } else if pid > 0 {
        pid as u32
    } else {
        return Errno::EINVAL.as_ret();
    };

    if target_pid == 0 {
        return Errno::ESRCH.as_ret();
    }

    let mask = crate::privileged::proc::process_with(target_pid, |p| {
        use core::sync::atomic::Ordering;
        p.cpuset_allowed.load(Ordering::Acquire)
    })
    .unwrap_or(u64::MAX);

    if !crate::privileged::syscall::api::write_u64_to_user(mask_ptr, mask) {
        return Errno::EFAULT.as_ret();
    }

    8
}
