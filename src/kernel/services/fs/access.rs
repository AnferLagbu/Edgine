#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 文件访问系统调用 — services 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 委托 `framework/fs/vfs::api` 完成
//!
//! ## POSIX 语义
//!
//! - [`access_syscall`] 检查可访问性 (`R_OK/W_OK/X_OK/F_OK`)
//! - [`unlink_syscall`] 解除链接 (删除文件)

use crate::framework::credo;
use crate::framework::credo::capability::{FS_CAP_EXECUTE, FS_CAP_READ, FS_CAP_WRITE};
use crate::services::fs::api as fw;
use crate::framework::syscall::Errno;
use crate::framework::syscall::raw;

// ============================================================================
// 权限位
// ============================================================================

/// `F_OK` - 文件存在性检查
pub const F_OK: i32 = 0;
/// `R_OK` - 可读
pub const R_OK: i32 = 4;
/// `W_OK` - 可写
pub const W_OK: i32 = 2;
/// `X_OK` - 可执行
pub const X_OK: i32 = 1;

// ============================================================================
// access
// ============================================================================

/// access(path, mode) — 检查当前用户对路径的访问权
///
/// mode 是 `R_OK/W_OK/X_OK` 的位或, `F_OK` 表示存在性检查.
/// 权限语义 (DECISION-077 方案 A): 复用能力制 — `R_OK/W_OK/X_OK` 映射到
/// FS 能力域位 (`FS_CAP_READ/WRITE/EXECUTE`), 与 open/read/write 路径
/// (ramfs/nestfs `check_permission`) 一致; `F_OK` 仅做存在性检查.
///
/// # Errors
/// 当 `path_ptr` 为空指针或不在用户可访问范围内时返回 `EFAULT`;
/// 当 `mode` 超出合法范围 (`0..=0o7`) 时返回 `EINVAL`;
/// 当路径不存在或进程缺少对应 FS 能力时返回 `EACCES`.
pub fn access_syscall(path_ptr: u64, mode: i32) -> Result<usize, Errno> {
    if path_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(path_ptr) {
        return Err(Errno::EFAULT);
    }
    // mode 仅取低 6 位 (POSIX R|W|X|F)
    if !(0..=0o7).contains(&mode) {
        return Err(Errno::EINVAL);
    }
    let pwm = current_pwm()?;
    // DECISION-077 方案 A: 能力制校验 — mode 位映射到 FS 能力域位.
    // 与 open/read/write 路径的 check_permission (ramfs/nestfs) 语义一致:
    // R_OK 对应 `FS_CAP_READ`, W_OK 对应 `FS_CAP_WRITE`, X_OK 对应 `FS_CAP_EXECUTE`.
    // F_OK (mode=0) 不要求任何能力, 仅做存在性检查.
    let mut required_caps: u64 = 0;
    if mode & R_OK != 0 {
        required_caps |= FS_CAP_READ;
    }
    if mode & W_OK != 0 {
        required_caps |= FS_CAP_WRITE;
    }
    if mode & X_OK != 0 {
        required_caps |= FS_CAP_EXECUTE;
    }
    if required_caps != 0
        && !credo::api::pwm_has_capability(pwm, credo::CAP_DOMAIN_FS, required_caps)
    {
        return Err(Errno::EACCES);
    }
    // 调用 vfs_stat_safe 验证存在性; 不需要 VfsStat 内容.
    let _stat = fw::vfs_stat_safe(path_ptr as *const u8, pwm).ok_or(Errno::EACCES)?;
    Ok(0)
}

// ============================================================================
// faccessat (简化: 同 access)
// ============================================================================

/// faccessat(dirfd, path, mode, flags) — 相对目录 fd 的 access.
///
/// Framekernel 简化: 不支持 `AT_EACCESS/AT_SYMLINK_NOFOLLOW`, 行为同 access.
///
/// # Errors
/// 错误条件与 [`access_syscall`] 相同, 参见其 `# Errors` 段.
pub fn faccessat_syscall(
    _dirfd: i32,
    path_ptr: u64,
    mode: i32,
    _flags: i32,
) -> Result<usize, Errno> {
    access_syscall(path_ptr, mode)
}

// ============================================================================
// unlink
// ============================================================================

/// unlink(path) — 删除一个名称到 inode 的链接
///
/// 若为最后链接且无进程打开该文件, 则删除文件.
///
/// # Errors
/// 当 `path_ptr` 为空指针或不在用户可访问范围内时返回 `EFAULT`;
/// 其余错误 (如路径不存在、无权限、目录非空等) 以对应的 `Errno` 返回.
pub fn unlink_syscall(path_ptr: u64) -> Result<usize, Errno> {
    if path_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(path_ptr) {
        return Err(Errno::EFAULT);
    }
    let pwm = current_pwm()?;
    let r = fw::vfs_unlink(path_ptr as *const u8, pwm);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(0)
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
    Ok(credo::api::pwm_get_current())
}
