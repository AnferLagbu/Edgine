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
use crate::privileged::syscall::raw;

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

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 取当前进程凭证,无会话时直接返回 EACCES。
///
/// 历史警告: 此前在 `pwm == 0` 时替换为硬编码 `TEST_PWM` (魔法值),
/// 实际会让所有未登录态调用落入 "匿名管理员" 路径,绕过访问控制。
/// 现在严格走 session 模块,无会话即拒绝。
fn current_pwm() -> Result<u64, Errno> {
    Ok(sgeg::api::pwm_get_current())
}

/// 验证 open flags 合法: 只接受低 3 位访问模式, 其他位 POSIX 定义.
fn validate_flags(flags: i32) -> Result<(), Errno> {
    if flags < 0 {
        return Err(Errno::EINVAL);
    }
    // 访问模式必须恰好是 O_RDONLY / O_WRONLY / O_RDWR 其一
    let access = flags & 0o3;
    if access > 0o2 {
        return Err(Errno::EINVAL);
    }
    // O_CREAT 必须配合 O_WRONLY 或 O_RDWR
    if (flags & O_CREAT) != 0 && access == O_RDONLY {
        return Err(Errno::EINVAL);
    }
    // O_TRUNC 隐含可写
    if (flags & O_TRUNC) != 0 && access == O_RDONLY {
        return Err(Errno::EINVAL);
    }
    // O_DIRECTORY 与 O_CREAT 不能同用
    if (flags & O_DIRECTORY) != 0 && (flags & O_CREAT) != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
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
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(path_ptr) {
        return Err(Errno::EFAULT);
    }
    validate_flags(flags)?;
    // mode 仅在 O_CREAT 生效, 否则忽略 (POSIX 允许 mode 参数始终存在, 内核忽略)
    // 注意: 此前此处错误地拒绝 mode != 0 且无 O_CREAT 的情况, 导致所有非 O_CREAT
    // 的 open 调用若携带 mode 参数即返回 EINVAL, 违反 POSIX 语义.
    // 修复: 移除该虚假校验, 仅在 O_CREAT 时校验 mode 范围.
    if (flags & O_CREAT) != 0 && !(0..=0o7777).contains(&mode) {
        return Err(Errno::EINVAL);
    }
    let pwm = current_pwm()?;
    let r = fw::vfs_open(path_ptr as *const u8, flags as u32, pwm);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(r as usize)
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
        return Err(Errno::EBADF);
    }
    // pidfd 不经过 VFS fd_table, 由 pidfd 子系统自行关闭.
    if crate::functions::proc::pidfd::is_pidfd_fd(fd) {
        if crate::functions::proc::pidfd::free_entry(fd) {
            return Ok(0);
        }
        return Err(Errno::EBADF);
    }
    // T1 G4: userfaultfd 不经过 VFS fd_table, 由 uffd 机制自行回收
    // (并唤醒阻塞在该 fd 上的缺页进程 / read() 线程).
    if crate::privileged::mm::is_uffd_fd(fd) {
        if crate::privileged::mm::uffd_release(fd) {
            return Ok(0);
        }
        return Err(Errno::EBADF);
    }
    // 方案 C (socket P3): sm socket fd 不在 VFS FdTable 内, vfs_close 对其是
    // no-op (accept 循环 FD 无法回收). FdPlan 命名空间去重叠后 subsystem_of
    // 判定成立, 分流至 net 机制 sm_close (含 G10 free_fd, 与 shutdown_syscall
    // 同路径; F2 走顶层 re-export net_socket).
    if crate::privileged::proc::fd_alloc::subsystem_of(fd)
        == Some(crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp)
    {
        let rc = crate::privileged::net_socket::sm_close(fd);
        return if rc < 0 {
            Err(Errno::from_ret(i64::from(rc)))
        } else {
            Ok(0)
        };
    }
    let r = fw::vfs_close(fd as u32);
    if r < 0 {
        Err(Errno::from_ret(i64::from(r)))
    } else {
        Ok(0)
    }
}
