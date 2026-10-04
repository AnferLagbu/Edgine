#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 符号链接/硬链接系统调用 — services 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 委托 `framework/fs/vfs::api` 完成真实现 (ramfs.link / ramfs.symlink / ramfs.readlink)

use crate::framework::sgeg;
use crate::framework::syscall::Errno;
use crate::framework::syscall::raw;
use crate::services::fs::api as fw;

// ============================================================================
// link
// ============================================================================

/// link(oldpath, newpath) — 创建硬链接
///
/// # Errors
/// 当任一路径指针为空或不在用户可访问范围内时返回 `EFAULT`;
/// 其余错误 (如源不存在、无权限、名称已存在等) 以对应的 `Errno` 返回.
pub fn link_syscall(oldpath_ptr: u64, newpath_ptr: u64) -> Result<usize, Errno> {
    if oldpath_ptr == 0 || newpath_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(oldpath_ptr) {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(newpath_ptr) {
        return Err(Errno::EFAULT);
    }
    let pwm = current_pwm()?;
    let r = fw::vfs_link(oldpath_ptr as *const u8, newpath_ptr as *const u8, pwm);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(0)
    }
}

// ============================================================================
// symlink
// ============================================================================

/// symlink(target, linkpath) — 创建符号链接
///
/// # Errors
/// 当任一路径指针为空或不在用户可访问范围内时返回 `EFAULT`;
/// 其余错误 (如目标不存在、无权限、名称已存在等) 以对应的 `Errno` 返回.
pub fn symlink_syscall(target_ptr: u64, linkpath_ptr: u64) -> Result<usize, Errno> {
    if target_ptr == 0 || linkpath_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(target_ptr) {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(linkpath_ptr) {
        return Err(Errno::EFAULT);
    }
    let pwm = current_pwm()?;
    let r = fw::vfs_symlink(target_ptr as *const u8, linkpath_ptr as *const u8, pwm);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(0)
    }
}

// ============================================================================
// readlink
// ============================================================================

/// readlink(path, buf, bufsiz) — 读符号链接目标
///
/// # Errors
/// 当路径或缓冲区指针为空/越界时返回 `EFAULT`; 当 `bufsiz` 为 0 时返回 `EINVAL`;
/// 其余错误 (如路径不是符号链接等) 以对应的 `Errno` 返回.
pub fn readlink_syscall(path_ptr: u64, buf_ptr: u64, bufsiz: u64) -> Result<usize, Errno> {
    if path_ptr == 0 || buf_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if bufsiz == 0 {
        return Err(Errno::EINVAL);
    }
    if !raw::check_user_ptr(path_ptr) {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_buf(buf_ptr, bufsiz) {
        return Err(Errno::EFAULT);
    }
    let pwm = current_pwm()?;
    let r = fw::vfs_readlink(path_ptr as *const u8, buf_ptr as *mut u8, bufsiz, pwm);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(r as usize)
    }
}

// ============================================================================
// 内部辅助
// ============================================================================

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 取当前进程凭证,无会话时直接返回 EACCES (历史硬编码 `TEST_PWM` 路径已弃用)。
fn current_pwm() -> Result<u64, Errno> {
    Ok(sgeg::api::pwm_get_current())
}
