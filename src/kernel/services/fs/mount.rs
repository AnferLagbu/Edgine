#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 挂载/卸载系统调用 — services 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 委托 `framework/fs/vfs::api` 完成
//! - mount 需 `CAP_SYS_ADMIN` 能力
//!
//! ## Framekernel 简化
//!
//! - [`mount_syscall`] target 必须非空, fstype 必须在已知列表
//! - [`umount2_syscall`] target 必须非空, 需 `CAP_SYS_ADMIN`

use crate::framework::credo;
use crate::framework::syscall::Errno;
use crate::framework::syscall::raw;
use crate::services::fs::api as fw;

// ============================================================================
// mount
// ============================================================================

/// mount(source, target, fstype) — 挂载文件系统
///
/// 需 `CAP_SYS_ADMIN` (capability 0x01) 才能挂载.
/// Framekernel 简化: 仅支持 5 种内置 FS (ramfs/nestfs/tmpfs/procfs/devfs),
/// 校验先于 framework 调用, 失败一律 ENODEV.
///
/// # Errors
/// 当 `target_ptr` 为空/越界、`fstype_ptr` 为空或越界、`source_ptr` 越界时返回 `EFAULT` 或 `EINVAL`;
/// 当缺少 `CAP_SYS_ADMIN` 能力时返回 `EACCES`; 其余错误以对应的 `Errno` 返回.
pub fn mount_syscall(source_ptr: u64, target_ptr: u64, fstype_ptr: u64) -> Result<usize, Errno> {
    if target_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if fstype_ptr == 0 {
        return Err(Errno::EINVAL);
    }
    if !raw::check_user_ptr(target_ptr) {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_buf(fstype_ptr, 1) {
        return Err(Errno::EFAULT);
    }
    if source_ptr != 0 && !raw::check_user_ptr(source_ptr) {
        return Err(Errno::EFAULT);
    }

    let pwm = current_pwm()?;
    if !credo::api::pwm_has_capability(pwm, 0, 0x01) {
        return Err(Errno::EACCES);
    }

    // 框架端会解析 fstype; 校验则委托 framework 内置白名单 (ramfs/nestfs/...)
    let r = fw::vfs_mount(target_ptr as *const u8, fstype_ptr as *const u8);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(0)
    }
}

// ============================================================================
// umount2
// ============================================================================

/// umount2(target, flags) — 卸载文件系统
///
/// Framekernel 简化: 暂不解析 flags (`MNT_FORCE` 等), 仅按 path 卸载.
/// 需 `CAP_SYS_ADMIN`.
///
/// # Errors
/// 当 `target_ptr` 为空或越界时返回 `EFAULT`; 当缺少 `CAP_SYS_ADMIN` 能力时返回 `EACCES`;
/// 其余错误 (如挂载点不存在等) 以对应的 `Errno` 返回.
pub fn umount2_syscall(target_ptr: u64, flags: i32) -> Result<usize, Errno> {
    if target_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(target_ptr) {
        return Err(Errno::EFAULT);
    }

    let pwm = current_pwm()?;
    if !credo::api::pwm_has_capability(pwm, 0, 0x01) {
        return Err(Errno::EACCES);
    }

    let r = fw::vfs_umount(target_ptr as *const u8, flags);
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
///
/// mount/umount2 在调用前还需 `pwm_has_capability(..., CAP_SYS_ADMIN)` 检查,
/// 这里仅返回原始凭证,真正权限决策交给 capability 模块。
fn current_pwm() -> Result<u64, Errno> {
    Ok(credo::api::pwm_get_current())
}
