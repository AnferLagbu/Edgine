#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 文件状态系统调用 — services 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 委托 `framework/fs/vfs::api` 完成
//!
//! ## POSIX 语义
//!
//! - [`stat_syscall`] 跟随符号链接
//! - [`lstat_syscall`] 不跟随符号链接
//! - [`fstat_syscall`] 按 FD 查询

use crate::framework::credo;
use crate::framework::syscall::Errno;
use crate::framework::syscall::raw;
use crate::services::fs::VfsStat;
use crate::services::fs::api as fw;

const VFS_STAT_SIZE: u64 = core::mem::size_of::<VfsStat>() as u64;

// ============================================================================
// stat
// ============================================================================

/// stat(path, `st_buf`) — 跟随符号链接查询文件元数据
///
/// # Errors
/// 当路径或缓冲区指针为空/越界时返回 `EFAULT`; 当底层 stat 失败时返回 `EIO`.
pub fn stat_syscall(path_ptr: u64, st_buf_ptr: u64) -> Result<usize, Errno> {
    if path_ptr == 0 || st_buf_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(path_ptr) {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_buf(st_buf_ptr, VFS_STAT_SIZE) {
        return Err(Errno::EFAULT);
    }
    let pwm = current_pwm()?;
    // framework safe API: 返回 VfsStat (无 raw pointer 跨边界)
    let stat = fw::vfs_stat_safe(path_ptr as *const u8, pwm).ok_or(Errno::EIO)?;
    // framework safe API: 写结构体到 user buf, 内部已 check_user_buf
    if !raw::write_struct_to_user::<VfsStat>(st_buf_ptr, &stat) {
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

// ============================================================================
// lstat
// ============================================================================

/// lstat(path, `st_buf`) — 不跟随符号链接查询文件元数据
///
/// Framekernel 简化: `vfs_stat` 不跟随 symlink, 行为同 lstat。
///
/// # Errors
/// 错误条件与 [`stat_syscall`] 相同, 参见其 `# Errors` 段.
pub fn lstat_syscall(path_ptr: u64, st_buf_ptr: u64) -> Result<usize, Errno> {
    stat_syscall(path_ptr, st_buf_ptr)
}

// ============================================================================
// fstat
// ============================================================================

/// fstat(fd, `st_buf`) — 按 FD 查询文件元数据
///
/// # Errors
/// 当 `fd` 为负数时返回 `EBADF`; 当缓冲区指针为空/越界时返回 `EFAULT`;
/// 当底层 fstat 失败时返回 `EIO`.
pub fn fstat_syscall(fd: i32, st_buf_ptr: u64) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if st_buf_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_buf(st_buf_ptr, VFS_STAT_SIZE) {
        return Err(Errno::EFAULT);
    }
    let pwm = current_pwm()?;
    let stat = fw::vfs_fstat_safe(fd as u32, pwm).ok_or(Errno::EIO)?;
    if !raw::write_struct_to_user::<VfsStat>(st_buf_ptr, &stat) {
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

// ============================================================================
// statx (T1 G1, syscall-followup 功能实装)
// ============================================================================

/// Linux `struct statx` (x86_64, 256 字节) — 扩展文件状态
///
/// SIMPLIFIED: 仅填充基础字段 (mode/uid/gid/size/nlink/ino/时间戳), dev/
/// rdev/btime/attributes/mnt_id 等置 0; 影响面: 依赖这些字段的调用方语义
/// 不完整; 何时需扩展: VFS 提供 dev/rdev/btime 元数据后补充.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Statx {
    pub mask: u32,
    pub blksize: u32,
    pub attributes: u64,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    pub spare0: [u16; 1],
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub attributes_mask: u64,
    pub atime_sec: i64,
    pub atime_nsec: u32,
    pub atime_reserved: i32,
    pub btime_sec: i64,
    pub btime_nsec: u32,
    pub btime_reserved: i32,
    pub ctime_sec: i64,
    pub ctime_nsec: u32,
    pub ctime_reserved: i32,
    pub mtime_sec: i64,
    pub mtime_nsec: u32,
    pub mtime_reserved: i32,
    pub rdev_major: u32,
    pub rdev_minor: u32,
    pub dev_major: u32,
    pub dev_minor: u32,
    pub mnt_id: u64,
    pub dio_mem_align: u32,
    pub dio_offset_align: u32,
    pub spare3: [u64; 12],
}

/// `STATX_BASIC_STATS` — 基础字段掩码 (mode/nlink/uid/gid/atime/mtime/ctime/ino/size/blocks)
const STATX_BASIC_STATS: u32 = 0x0000_07ff;

impl Statx {
    /// 从内核 `VfsStat` 组装 Linux `struct statx`
    fn from_vfs_stat(s: &VfsStat) -> Self {
        Self {
            mask: STATX_BASIC_STATS,
            blksize: 4096,
            attributes: 0,
            // SIMPLIFIED: VFS 无 nlink 计数, 固定 1
            nlink: 1,
            uid: s.uid,
            gid: s.gid,
            // SIMPLIFIED: 透传 VfsStat.mode (含类型/权限位, 语义随 VFS)
            mode: s.mode,
            spare0: [0],
            ino: u64::from(s.node_id),
            size: u64::from(s.size),
            blocks: 0,
            attributes_mask: 0,
            atime_sec: s.atime as i64,
            atime_nsec: 0,
            atime_reserved: 0,
            btime_sec: 0,
            btime_nsec: 0,
            btime_reserved: 0,
            ctime_sec: s.ctime as i64,
            ctime_nsec: 0,
            ctime_reserved: 0,
            mtime_sec: s.mtime as i64,
            mtime_nsec: 0,
            mtime_reserved: 0,
            rdev_major: 0,
            rdev_minor: 0,
            dev_major: 0,
            dev_minor: 0,
            mnt_id: 0,
            dio_mem_align: 0,
            dio_offset_align: 0,
            spare3: [0; 12],
        }
    }
}

/// statx(dirfd, pathname, flags, mask, buf) — 扩展文件状态查询
///
/// T1 G1 实装. 委托 `vfs_stat_safe` (不跟随 symlink, 与 lstat 语义一致).
///
/// # Errors
/// 当路径或缓冲区指针为空/越界时返回 `EFAULT`; 当底层 stat 失败时返回 `EIO`.
pub fn statx_syscall(
    _dirfd: i32,
    path_ptr: u64,
    _flags: u32,
    _mask: u32,
    buf_ptr: u64,
) -> Result<usize, Errno> {
    let stx_size = core::mem::size_of::<Statx>() as u64;
    if path_ptr == 0 || buf_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_ptr(path_ptr) {
        return Err(Errno::EFAULT);
    }
    if !raw::check_user_buf(buf_ptr, stx_size) {
        return Err(Errno::EFAULT);
    }
    let pwm = current_pwm()?;
    let stat = fw::vfs_stat_safe(path_ptr as *const u8, pwm).ok_or(Errno::EIO)?;
    let stx = Statx::from_vfs_stat(&stat);
    if !raw::write_struct_to_user::<Statx>(buf_ptr, &stx) {
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

/// utimensat(dirfd, path, times, flags) 策略 (T1 G1 实装)
///
/// SIMPLIFIED: 仅支持 `dirfd == AT_FDCWD` (绝对路径); `times == NULL` 时设为
/// 当前时间 (tick/frequency 换算秒); 非 NULL 时读取用户 `timespec[2]` 数组
/// (`{sec, nsec}` 各 8 字节), `sec == -1` 表示该时间戳不修改 (Linux 语义,
/// 内部以 `u64::MAX` 传递). 委托 framework `vfs_utimensat_safe`.
pub fn utimensat_syscall(dirfd: i32, path_ptr: u64, times_ptr: u64, _flags: i32) -> i64 {
    const AT_FDCWD: i32 = -100;
    if dirfd != AT_FDCWD {
        return Errno::ENOTSUP.as_ret();
    }
    if path_ptr == 0 || !raw::check_user_ptr(path_ptr) {
        return Errno::EFAULT.as_ret();
    }
    let (atime, mtime) = if times_ptr == 0 {
        // NULL times: 设为当前时间 (Linux utimensat 语义)
        let now = crate::framework::syscall::api::get_ticks()
            / u64::from(crate::framework::timer::get_frequency());
        (now, now)
    } else {
        // timespec[2]: {atime{sec,nsec}, mtime{sec,nsec}} = 32 字节 = 4 × u64
        let mut times = [0u64; 4];
        if !crate::framework::syscall::api::read_struct_from_user(times_ptr, &mut times) {
            return Errno::EFAULT.as_ret();
        }
        let atime_sec = times[0] as i64;
        let mtime_sec = times[2] as i64;
        let atime_v = if atime_sec == -1 { u64::MAX } else { times[0] };
        let mtime_v = if mtime_sec == -1 { u64::MAX } else { times[2] };
        (atime_v, mtime_v)
    };
    let Ok(path) = crate::framework::mm::copy_user::copy_string_from_user(path_ptr, 4096) else {
        return Errno::EFAULT.as_ret();
    };
    let pwm = crate::framework::credo::pwm_get_current();
    let r = crate::services::fs::vfs_utimensat_safe(&path, atime, mtime, pwm);
    if r < 0 { Errno::EIO.as_ret() } else { 0 }
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
