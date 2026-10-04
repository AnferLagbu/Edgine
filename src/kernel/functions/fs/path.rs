#![deny(unsafe_code)]
//! 路径/工作目录系统调用 — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe,纯类型安全
//! - 委托 `privileged/fs/vfs::api` 完成
//!
//! ## POSIX 语义
//!
//! - [`chdir_syscall`] 切换当前工作目录
//! - [`getcwd_syscall`] 取当前工作目录到用户缓冲
//! - [`chroot_syscall`] 切换进程视图根 (需 `CAP_SYS_ADMIN`)
//! - [`pivot_root_syscall`] 替换视图根并校验 `new_root`/`put_old` 关系
//!
//! ## 根语义 (T1 G7)
//!
//! `chroot` / `pivot_root` 通过 `VfsManager` 的根前缀实现: 所有用户路径在
//! `resolve_user_path` 中先做视图归一化 (`..` 钳制在视图根内), 再拼接根前缀
//! 得到真实路径. 默认根 "/" 时行为与改造前等价.

use crate::functions::fs::api as fw;
use crate::functions::fs::vfs_manager::VFS_MANAGER;
use crate::functions::fs::vfs_types::{VFS_MAX_PATH, VfsFileType};
use crate::privileged::lib::CStrExt;
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::raw;

// ============================================================================
// chdir
// ============================================================================

/// chdir(path) — 切换工作目录
///
/// # 参数
/// - `path_ptr`: 用户空间 NUL 终止 C 字符串地址
///
/// # Errors
/// 当 `path_ptr` 为空或不在用户可访问范围内时返回 `EFAULT`.
pub fn chdir_syscall(path_ptr: u64) -> Result<usize, Errno> {
    if path_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(path_ptr) {
        return Err(Errno::EFAULT);
    }
    // SAFETY: path_ptr 由 check_user_ptr 验证为可读
    let ptr = path_ptr as *const u8;
    fw::vfs_set_cwd(ptr);
    Ok(0)
}

// ============================================================================
// getcwd
// ============================================================================

/// getcwd(buf, size) — 取当前工作目录到用户缓冲
///
/// # 参数
/// - `buf_ptr`:  用户空间缓冲地址
/// - `size`:     缓冲长度
///
/// # Errors
/// 当 `buf_ptr` 为空或 `size` 为 0 时返回 `EINVAL`; 当缓冲区越界时返回 `EFAULT`;
/// 其余错误以对应的 `Errno` 返回.
pub fn getcwd_syscall(buf_ptr: u64, size: u64) -> Result<usize, Errno> {
    if buf_ptr == 0 || size == 0 {
        return Err(Errno::EINVAL);
    }
    if !raw::check_user_buf(buf_ptr, size) {
        return Err(Errno::EFAULT);
    }
    // SAFETY: buf_ptr 由 check_user_buf 验证为可写
    let buf = buf_ptr as *mut u8;
    let n = fw::vfs_get_cwd(buf, size as u32);
    if n < 0 {
        Err(Errno::from_ret(i64::from(n)))
    } else {
        Ok(n as usize)
    }
}

// ============================================================================
// chroot / pivot_root (T1 G7)
// ============================================================================

/// 取当前凭证 + 校验 `CAP_SYS_ADMIN` (SYSTEM 域 bit0).
///
/// 与 `mount`/`umount2`/`open_by_handle_at` 先例一致 (functions/fs/mount.rs:52).
fn require_sys_admin() -> Result<u64, Errno> {
    let pwm = crate::privileged::sgeg::session::get_current_pwm();
    if !crate::privileged::sgeg::api::pwm_has_capability(pwm, 0, 0x01) {
        return Err(Errno::EACCES);
    }
    Ok(pwm)
}

/// 校验用户路径指针并取 `&str`.
fn checked_user_path(ptr: u64) -> Result<&'static str, Errno> {
    if ptr == 0 || !raw::check_user_ptr(ptr) {
        return Err(Errno::EFAULT);
    }
    // ptr 经 check_user_ptr 验证为可读的 NUL 终止用户字符串
    let path = (ptr as *const u8).as_kstr();
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }
    Ok(path)
}

/// 校验路径存在且为目录 (chroot/pivot_root 的共同前置).
fn require_directory(ptr: u64, pwm: u64) -> Result<(), Errno> {
    let Some(st) = fw::vfs_stat_safe(ptr as *const u8, pwm) else {
        return Err(Errno::ENOENT);
    };
    if VfsFileType::from_u8(st.file_type) != Some(VfsFileType::Dir) {
        return Err(Errno::ENOTDIR);
    }
    Ok(())
}

/// chroot(path) — 切换进程视图根.
///
// SIMPLIFIED: 根前缀为全局 VFS 状态, 非 per-process root; 影响面: `chroot` 影响
// 全系统视图 (其他进程亦受新根约束); 何时需扩展: 引入 per-process root 或 mount
// namespace 时改为进程级根字段.
///
/// # Errors
/// 指针为空/越界返回 `EFAULT`; 缺少 `CAP_SYS_ADMIN` 返回 `EACCES`;
/// 路径不存在返回 `ENOENT`; 非目录返回 `ENOTDIR`; 路径超长返回 `ENAMETOOLONG`.
pub fn chroot_syscall(path_ptr: u64) -> Result<usize, Errno> {
    let path = checked_user_path(path_ptr)?;
    let pwm = require_sys_admin()?;
    require_directory(path_ptr, pwm)?;

    let mut buf = [0u8; VFS_MAX_PATH];
    let Some(real_root) = VFS_MANAGER.resolve_user_path(path, &mut buf) else {
        return Err(Errno::ENAMETOOLONG);
    };
    VFS_MANAGER.set_root(real_root);
    Ok(0)
}

/// pivot_root 关系校验 (纯逻辑, 便于直接验证).
///
/// 校验项 (对齐 Linux `pivot_root(2)`):
/// - `new_root` 等于当前根 → `EBUSY` (不可 pivot 到自身)
/// - `put_old` 与 `new_root` 相同或不在 `new_root` 之下 → `EINVAL`
///
/// 入参均为**真实路径** (已归一化).
pub(crate) fn pivot_root_plan(
    new_root_real: &str,
    put_old_real: &str,
    current_root: &str,
) -> Result<(), Errno> {
    if new_root_real == current_root {
        return Err(Errno::EBUSY);
    }
    if !is_strictly_under(put_old_real, new_root_real) {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// `child` 是否严格位于 `parent` 之下 (路径边界感知, 不含相等情形)
fn is_strictly_under(child: &str, parent: &str) -> bool {
    if parent == "/" {
        return child.len() > 1 && child.starts_with('/');
    }
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes().get(parent.len()) == Some(&b'/')
}

/// pivot_root(new_root, put_old) — 替换视图根。
///
// SIMPLIFIED: 仅切换根前缀, 不摘除旧根挂载点 (无 per-process mount namespace
// 可摘); 影响面: `put_old` 仅参与关系校验, 旧根经绝对路径仍可达; 何时需扩展:
// 需要 Linux "旧根不可达" 强语义时, 在 `VfsManager` 摘除旧根挂载点.
///
/// # Errors
/// 指针为空/越界返回 `EFAULT`; 缺少 `CAP_SYS_ADMIN` 返回 `EACCES`;
/// 路径不存在返回 `ENOENT`; 非目录返回 `ENOTDIR`; 关系非法返回 `EINVAL`;
/// `new_root` 即当前根返回 `EBUSY`.
pub fn pivot_root_syscall(new_root_ptr: u64, put_old_ptr: u64) -> Result<usize, Errno> {
    let new_view = checked_user_path(new_root_ptr)?;
    let old_view = checked_user_path(put_old_ptr)?;
    let pwm = require_sys_admin()?;
    require_directory(new_root_ptr, pwm)?;
    require_directory(put_old_ptr, pwm)?;

    let mut new_buf = [0u8; VFS_MAX_PATH];
    let mut old_buf = [0u8; VFS_MAX_PATH];
    let (Some(new_real), Some(old_real)) = (
        VFS_MANAGER.resolve_user_path(new_view, &mut new_buf),
        VFS_MANAGER.resolve_user_path(old_view, &mut old_buf),
    ) else {
        return Err(Errno::ENAMETOOLONG);
    };

    let current_root = VFS_MANAGER.get_root();
    pivot_root_plan(new_real, old_real, &current_root)?;
    VFS_MANAGER.set_root(new_real);
    Ok(0)
}
