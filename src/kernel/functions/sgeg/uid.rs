#![deny(unsafe_code)]
//! UID/GID 系统调用 — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe,纯类型安全
//! - 委托 privileged/sgeg/session 实际完成
//!
//! ## POSIX 语义
//!
//! - getuid/getgid/geteuid/getegid: 取当前真实/有效 ID
//! - setuid/setgid/seteuid/setegid: 设置 ID,失败 -EPERM
//! - setreuid/setregid: 同时设置真实与有效 ID
//!
//! ## Framekernel 简化
//!
//! root 拥有特权,可任意设置;非 root 仅可设置为自己/euid,否则 EPERM。

use crate::privileged::sgeg::session as fw;
use crate::privileged::syscall::Errno;

// ============================================================================
// 读类
// ============================================================================

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// `getuid()` — 取真实 UID
///
/// # Errors
/// 当前实现总是返回 `Ok`, 不会返回错误.
pub fn getuid_syscall() -> Result<usize, Errno> {
    Ok(fw::get_current_uid() as usize)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// `getgid()` — 取真实 GID
///
/// # Errors
/// 当前实现总是返回 `Ok`, 不会返回错误.
pub fn getgid_syscall() -> Result<usize, Errno> {
    Ok(fw::get_current_gid() as usize)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// `geteuid()` — 取有效 UID
///
/// # Errors
/// 当前实现总是返回 `Ok`, 不会返回错误.
pub fn geteuid_syscall() -> Result<usize, Errno> {
    Ok(fw::get_euid() as usize)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// `getegid()` — 取有效 GID
///
/// # Errors
/// 当前实现总是返回 `Ok`, 不会返回错误.
pub fn getegid_syscall() -> Result<usize, Errno> {
    Ok(fw::get_egid() as usize)
}

// ============================================================================
// 写类
// ============================================================================

/// setuid(uid) — 设置 UID
///
/// # Errors
/// 当 `uid` 不是当前真实/有效/保存的 UID 且 `try_setuid` 失败时, 返回 `Errno::EPERM`.
pub fn setuid_syscall(uid: u32) -> Result<usize, Errno> {
    if uid == fw::get_current_uid() || uid == fw::get_euid() || uid == fw::get_saved_euid() {
        return Ok(0);
    }
    if fw::try_setuid(uid) {
        Ok(0)
    } else {
        Err(Errno::EPERM)
    }
}

/// setgid(gid) — 设置 GID
///
/// # Errors
/// 当 `gid` 不是当前真实/有效/保存的 GID 且 `try_setgid` 失败时, 返回 `Errno::EPERM`.
pub fn setgid_syscall(gid: u32) -> Result<usize, Errno> {
    if gid == fw::get_current_gid() || gid == fw::get_egid() || gid == fw::get_saved_egid() {
        return Ok(0);
    }
    if fw::try_setgid(gid) {
        Ok(0)
    } else {
        Err(Errno::EPERM)
    }
}

/// seteuid(euid) — 设置有效 UID
///
/// # Errors
/// 当 `euid` 不是当前真实/有效/保存的 UID 且 `try_seteuid` 失败时, 返回 `Errno::EPERM`.
pub fn seteuid_syscall(euid: u32) -> Result<usize, Errno> {
    if euid == fw::get_current_uid() || euid == fw::get_euid() || euid == fw::get_saved_euid() {
        return Ok(0);
    }
    if fw::try_seteuid(euid) {
        Ok(0)
    } else {
        Err(Errno::EPERM)
    }
}

/// setegid(egid) — 设置有效 GID
///
/// # Errors
/// 当 `egid` 不是当前真实/有效/保存的 GID 且 `try_setegid` 失败时, 返回 `Errno::EPERM`.
pub fn setegid_syscall(egid: u32) -> Result<usize, Errno> {
    if egid == fw::get_current_gid() || egid == fw::get_egid() || egid == fw::get_saved_egid() {
        return Ok(0);
    }
    if fw::try_setegid(egid) {
        Ok(0)
    } else {
        Err(Errno::EPERM)
    }
}

/// setreuid(ruid, euid) — 同时设置真实与有效 UID
///
/// POSIX 允许 ruid/euid 之一为 (uid_t)-1 表示不变。
///
/// # Errors
/// 当 `try_setreuid` 失败 (无特权) 时返回 `Errno::EPERM`.
pub fn setreuid_syscall(ruid: u32, euid: u32) -> Result<usize, Errno> {
    if fw::try_setreuid(ruid, euid) {
        Ok(0)
    } else {
        Err(Errno::EPERM)
    }
}

/// setregid(rgid, egid) — 同时设置真实与有效 GID
///
/// # Errors
/// 当 `try_setregid` 失败 (无特权) 时返回 `Errno::EPERM`.
pub fn setregid_syscall(rgid: u32, egid: u32) -> Result<usize, Errno> {
    if fw::try_setregid(rgid, egid) {
        Ok(0)
    } else {
        Err(Errno::EPERM)
    }
}
