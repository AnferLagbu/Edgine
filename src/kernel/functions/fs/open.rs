#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! 文件打开/关闭系统调用 — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 委托 `privileged/fs/vfs::api` 完成
//!
//! ## POSIX 语义
//!
//! - [`open_syscall`] 打开一个文件, 返回 fd
//! - [`close_syscall`] 关闭一个 fd
//! - [`creat_syscall`] 等价于 open(path, `O_WRONLY|O_CREAT|O_TRUNC`, mode)

use crate::functions::fs::api as fw;
use crate::privileged::sgeg;
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::api;

// ============================================================================
// open flags
// ============================================================================

/// 只读打开
pub const O_RDONLY: i32 = 0o0;
/// 只写打开
pub const O_WRONLY: i32 = 0o1;
/// 读写打开
pub const O_RDWR: i32 = 0o2;
/// 若不存在则创建 (必须与 `O_WRONLY` 或 `O_RDWR` 之一同用)
pub const O_CREAT: i32 = 0o100;
/// 若已存在且是常规文件且以写方式打开, 则长度截断为 0
pub const O_TRUNC: i32 = 0o1000;
/// 若已存在则打开失败
pub const O_EXCL: i32 = 0o200;
/// 追加模式, 每次写都在文件末尾
pub const O_APPEND: i32 = 0o2000;
/// 必须是目录
pub const O_DIRECTORY: i32 = 0o200_000;
/// 跟随符号链接末尾的 too-many-symlinks
pub const O_NOFOLLOW: i32 = 0o400_000;
/// close-on-exec: exec 时关闭 fd
pub const O_CLOEXEC: i32 = 0o2_000_000;

// ============================================================================
// 内部辅助
// ============================================================================

/// 取当前进程凭证。
///
/// 历史警告: 此前在 `pwm == 0` 时替换为硬编码 `TEST_PWM` (魔法值),
/// 实际会让所有未登录态调用落入 "匿名管理员" 路径,绕过访问控制。
/// 现在严格走 session 模块,无会话即拒绝。
fn current_pwm() -> u64 {
    sgeg::api::pwm_get_current()
}

/// 验证 open flags 合法: 只接受低 3 位访问模式, 其他位 POSIX 定义.
fn validate_flags(flags: i32) -> Result<(), Errno> {
    if flags < 0 {
        return Err(Errno::Einval);
    }
    // 访问模式必须恰好是 O_RDONLY / O_WRONLY / O_RDWR 其一
    let access = flags & 0o3;
    if access > 0o2 {
        return Err(Errno::Einval);
    }
    // O_CREAT 必须配合 O_WRONLY 或 O_RDWR
    if (flags & O_CREAT) != 0 && access == O_RDONLY {
        return Err(Errno::Einval);
    }
    // O_TRUNC 隐含可写
    if (flags & O_TRUNC) != 0 && access == O_RDONLY {
        return Err(Errno::Einval);
    }
    // O_DIRECTORY 与 O_CREAT 不能同用
    if (flags & O_DIRECTORY) != 0 && (flags & O_CREAT) != 0 {
        return Err(Errno::Einval);
    }
    Ok(())
}

/// Linux `O_*` → VFS `VfsOpenFlags` 编码翻译 (用户态 open/openat/creat 边界).
///
/// VFS 内部层 (persist / coredump / WASI / pipe / ramfs 等) 一律使用
/// [`VfsOpenFlags`](super::vfs_types::VfsOpenFlags) 编码; 用户态 syscall 传入的
/// 却是 Linux `O_*` 数值。两者 accmode 与 `CREAT` 位不同: Linux accmode 是 2 位
/// 字段 (RDONLY/WRONLY/RDWR = 0/1/2), VFS 是独立位 (1/2/4); Linux `O_CREAT`=0o100,
/// VFS `CREAT`=0x0100。故必须在本边界翻译, 否则 `O_CREAT` 恒不命中 (创建不存在
/// 文件恒得 `ENOENT`)、accmode 被误读 (`O_WRONLY`→RDONLY、`O_RDWR`→WRONLY)。
///
/// `O_TRUNC` / `O_APPEND` / `O_CLOEXEC` 两侧数值本就一致, 仍显式经 `VfsOpenFlags`
/// 常量重建以保持单源。`O_EXCL` / `O_DIRECTORY` / `O_NOFOLLOW` 在 VFS 无对应位,
/// 不翻译 (VFS 尚未实装其语义, 与翻译前被忽略的行为一致)。
// SIMPLIFIED: O_EXCL 未翻译 (VFS fs_create 无存在性检查, 不保证独占创建);
// 影响面: 依赖 O_EXCL 原子独占创建的程序 (部分 mkstemp/锁文件实现) 语义不闭合;
// 何时需扩展: VFS fs_create 支持 "已存在即 EEXIST" 后在翻译里补 EXCL 位并下传。
fn linux_o_flags_to_vfs(flags: i32) -> u32 {
    use super::vfs_types::VfsOpenFlags;
    let mut out = match flags & 0o3 {
        O_WRONLY => VfsOpenFlags::WRONLY.bits(),
        O_RDWR => VfsOpenFlags::RDWR.bits(),
        _ => VfsOpenFlags::RDONLY.bits(),
    };
    if flags & O_CREAT != 0 {
        out |= VfsOpenFlags::CREAT.bits();
    }
    if flags & O_TRUNC != 0 {
        out |= VfsOpenFlags::TRUNC.bits();
    }
    if flags & O_APPEND != 0 {
        out |= VfsOpenFlags::APPEND.bits();
    }
    if flags & O_CLOEXEC != 0 {
        out |= VfsOpenFlags::CLOEXEC.bits();
    }
    out
}

// ============================================================================
// open
// ============================================================================

/// open(path, flags, mode) — 打开一个文件
///
/// 返回新文件描述符 (>= 0), 失败返回负 errno.
///
/// # Errors
/// 当 `path_ptr` 为空或越界时返回 `EFAULT`; 当 `flags` 非法或 `O_CREAT` 下 `mode` 越界时返回 `EINVAL`;
/// 其余错误 (如文件不存在、无权限、fd 已满等) 以对应的 `Errno` 返回.
pub fn open_syscall(path_ptr: u64, flags: i32, mode: i32) -> Result<usize, Errno> {
    if path_ptr == 0 {
        return Err(Errno::Efault);
    }
    if !api::validate_user_ptr(path_ptr) {
        return Err(Errno::Efault);
    }
    validate_flags(flags)?;
    // mode 仅在 O_CREAT 生效, 否则忽略 (POSIX 允许 mode 参数始终存在, 内核忽略)
    // 注意: 此前此处错误地拒绝 mode != 0 且无 O_CREAT 的情况, 导致所有非 O_CREAT
    // 的 open 调用若携带 mode 参数即返回 EINVAL, 违反 POSIX 语义.
    // 修复: 移除该虚假校验, 仅在 O_CREAT 时校验 mode 范围.
    if (flags & O_CREAT) != 0 && !(0..=0o7777).contains(&mode) {
        return Err(Errno::Einval);
    }
    let pwm = current_pwm();
    let r = fw::vfs_open(path_ptr as *const u8, linux_o_flags_to_vfs(flags), pwm);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(crate::functions::intcast::usize_from_i32(r))
    }
}

// ============================================================================
// creat
// ============================================================================

/// creat(path, mode) — 等价于 open(path, `O_WRONLY|O_CREAT|O_TRUNC`, mode)
///
/// # Errors
/// 错误条件与 [`open_syscall`] 相同, 参见其 `# Errors` 段.
pub fn creat_syscall(path_ptr: u64, mode: i32) -> Result<usize, Errno> {
    const CREAT_FLAGS: i32 = O_WRONLY | O_CREAT | O_TRUNC;
    open_syscall(path_ptr, CREAT_FLAGS, mode)
}

// ============================================================================
// close
// ============================================================================

/// close(fd) — 关闭文件描述符
///
/// # Errors
/// 当 `fd` 为负数时返回 `EBADF`; 其余错误 (如 fd 无效等) 以对应的 `Errno` 返回.
pub fn close_syscall(fd: i32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let r = fw::vfs_close(fd.cast_unsigned());
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::fs::vfs_types::VfsOpenFlags;

    /// accmode 三态必须逐一映射到 VFS 独立位 (Linux 2 位字段 → VFS 1/2/4).
    #[test]
    fn accmode_maps_to_vfs_independent_bits() {
        assert_eq!(linux_o_flags_to_vfs(O_RDONLY), VfsOpenFlags::RDONLY.bits());
        assert_eq!(linux_o_flags_to_vfs(O_WRONLY), VfsOpenFlags::WRONLY.bits());
        assert_eq!(linux_o_flags_to_vfs(O_RDWR), VfsOpenFlags::RDWR.bits());
    }

    /// headline 回归: Linux `O_CREAT`(0o100) 必须翻译成 VFS `CREAT`(0x0100).
    /// 修复前直传原始 flags, 0o100 恒不命中 0x0100 ⇒ 创建不存在文件恒 ENOENT.
    #[test]
    fn linux_o_creat_hits_vfs_creat_bit() {
        let out = linux_o_flags_to_vfs(O_CREAT | O_WRONLY);
        assert_ne!(
            out & VfsOpenFlags::CREAT.bits(),
            0,
            "O_CREAT 必须命中 VFS CREAT 位"
        );
        assert_ne!(out & VfsOpenFlags::WRONLY.bits(), 0);
        // 负控制: 不带 O_CREAT 时 CREAT 位不得被置起.
        assert_eq!(
            linux_o_flags_to_vfs(O_WRONLY) & VfsOpenFlags::CREAT.bits(),
            0
        );
    }

    /// TRUNC / APPEND / CLOEXEC 两侧同值, 翻译后仍须置起对应 VFS 位.
    #[test]
    fn trunc_append_cloexec_pass_through() {
        let out = linux_o_flags_to_vfs(O_RDWR | O_TRUNC | O_APPEND | O_CLOEXEC);
        assert_ne!(out & VfsOpenFlags::TRUNC.bits(), 0);
        assert_ne!(out & VfsOpenFlags::APPEND.bits(), 0);
        assert_ne!(out & VfsOpenFlags::CLOEXEC.bits(), 0);
        assert_ne!(out & VfsOpenFlags::RDWR.bits(), 0);
    }

    /// 组合 flags 全量重建: creat 语义 (O_WRONLY|O_CREAT|O_TRUNC) 的翻译结果.
    #[test]
    fn creat_combination_rebuilds_all_bits() {
        let out = linux_o_flags_to_vfs(O_WRONLY | O_CREAT | O_TRUNC);
        let expect =
            VfsOpenFlags::WRONLY.bits() | VfsOpenFlags::CREAT.bits() | VfsOpenFlags::TRUNC.bits();
        assert_eq!(out, expect);
    }

    /// VFS 无对应位的 Linux 专属位 (O_EXCL/O_DIRECTORY/O_NOFOLLOW) 不翻译,
    /// 与翻译前被忽略的行为一致 (不得凭空造出 VFS 位).
    #[test]
    fn linux_only_bits_are_dropped() {
        let base = linux_o_flags_to_vfs(O_RDONLY);
        let with_extras = linux_o_flags_to_vfs(O_RDONLY | O_EXCL | O_DIRECTORY | O_NOFOLLOW);
        assert_eq!(with_extras, base);
    }
}
