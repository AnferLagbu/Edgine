#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! 文件状态系统调用 — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 委托 `privileged/fs/vfs::api` 完成
//!
//! ## POSIX 语义
//!
//! - [`stat_syscall`] 跟随符号链接
//! - [`lstat_syscall`] 不跟随符号链接
//! - [`fstat_syscall`] 按 FD 查询

use crate::functions::fs::api as fw;
use crate::functions::fs::{VfsFileType, VfsStat};
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::api;

use super::subject;

/// Linux `struct stat` 的 x86_64 / aarch64 布局与 `VfsStat` 的转换。
///
/// `VfsStat` 是内核内部元数据；`stat`/`fstat`/`newfstatat` 面向用户态时必须
/// 按 Linux UAPI 布局写回，否则 glibc 的 `ld.so` 会按 144 字节结构读取并
/// 把 size/mode 解释到错误字段（G0 实测 `libc.so.6: cannot stat`）。
#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Copy, Clone)]
struct LinuxTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Copy, Clone)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_nlink: u64,
    st_mode: u32,
    st_uid: u32,
    st_gid: u32,
    __pad0: i32,
    st_rdev: u64,
    st_size: i64,
    st_blksize: i64,
    st_blocks: i64,
    st_atim: LinuxTimespec,
    st_mtim: LinuxTimespec,
    st_ctim: LinuxTimespec,
    __reserved: [i64; 3],
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Copy, Clone)]
struct LinuxTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Copy, Clone)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad1: u64,
    st_size: i64,
    st_blksize: u32,
    __pad2: u32,
    st_blocks: i64,
    st_atim: LinuxTimespec,
    st_mtim: LinuxTimespec,
    st_ctim: LinuxTimespec,
    __unused: [u32; 2],
}

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFCHR: u32 = 0o020_000;
const S_IFLNK: u32 = 0o120_000;

fn linux_stat_from_vfs(s: &VfsStat) -> LinuxStat {
    let type_bits = match VfsFileType::from_u8(s.file_type) {
        Some(VfsFileType::Dir) => S_IFDIR,
        Some(VfsFileType::Dev) => S_IFCHR,
        Some(VfsFileType::Symlink) => S_IFLNK,
        _ => S_IFREG,
    };
    let mode = type_bits | (u32::from(s.perm) & !S_IFMT);
    let size = i64::from(s.size);
    let blocks = (size + 511) / 512;

    #[cfg(target_arch = "x86_64")]
    {
        LinuxStat {
            st_dev: 0,
            st_ino: u64::from(s.node_id),
            st_nlink: 1,
            st_mode: mode,
            st_uid: s.uid,
            st_gid: s.gid,
            __pad0: 0,
            st_rdev: 0,
            st_size: size,
            st_blksize: 4096,
            st_blocks: blocks,
            st_atim: LinuxTimespec {
                tv_sec: s.atime.cast_signed(),
                tv_nsec: 0,
            },
            st_mtim: LinuxTimespec {
                tv_sec: s.mtime.cast_signed(),
                tv_nsec: 0,
            },
            st_ctim: LinuxTimespec {
                tv_sec: s.ctime.cast_signed(),
                tv_nsec: 0,
            },
            __reserved: [0; 3],
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        LinuxStat {
            st_dev: 0,
            st_ino: u64::from(s.node_id),
            st_mode: mode,
            st_nlink: 1,
            st_uid: s.uid,
            st_gid: s.gid,
            st_rdev: 0,
            __pad1: 0,
            st_size: size,
            st_blksize: 4096,
            __pad2: 0,
            st_blocks: blocks,
            st_atim: LinuxTimespec {
                tv_sec: s.atime.cast_signed(),
                tv_nsec: 0,
            },
            st_mtim: LinuxTimespec {
                tv_sec: s.mtime.cast_signed(),
                tv_nsec: 0,
            },
            st_ctim: LinuxTimespec {
                tv_sec: s.ctime.cast_signed(),
                tv_nsec: 0,
            },
            __unused: [0; 2],
        }
    }
}

fn write_linux_stat(st_buf_ptr: u64, s: &VfsStat) -> Result<usize, Errno> {
    if !api::validate_user_buf(st_buf_ptr, core::mem::size_of::<LinuxStat>() as u64) {
        return Err(Errno::Efault);
    }
    let stat = linux_stat_from_vfs(s);
    if api::write_struct_to_user::<LinuxStat>(st_buf_ptr, &stat) {
        Ok(0)
    } else {
        Err(Errno::Efault)
    }
}

// ============================================================================
// stat
// ============================================================================

/// stat(path, `st_buf`) — 跟随符号链接查询文件元数据
///
/// # Errors
/// 当路径或缓冲区指针为空/越界时返回 `EFAULT`; 当底层 stat 失败时返回 `EIO`.
pub fn stat_syscall(path_ptr: u64, st_buf_ptr: u64) -> Result<usize, Errno> {
    if path_ptr == 0 || st_buf_ptr == 0 {
        return Err(Errno::Efault);
    }
    if !api::validate_user_ptr(path_ptr) {
        return Err(Errno::Efault);
    }
    let pwm = current_pwm();
    // privileged safe API: 返回 VfsStat (无 raw pointer 跨边界)
    let stat = fw::vfs_stat_safe(path_ptr as *const u8, pwm).ok_or(Errno::Eio)?;
    write_linux_stat(st_buf_ptr, &stat)
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
        return Err(Errno::Ebadf);
    }
    if st_buf_ptr == 0 {
        return Err(Errno::Efault);
    }
    let pwm = current_pwm();
    let stat = fw::vfs_fstat_safe(fd.cast_unsigned(), pwm).ok_or(Errno::Eio)?;
    write_linux_stat(st_buf_ptr, &stat)
}

const AT_FDCWD: i32 = -100;
const AT_EMPTY_PATH: u32 = 0x1000;

/// `newfstatat(dirfd, path, statbuf, flags)` — Linux 路径 stat ABI。
///
/// 当前 VFS 没有按 fd 解析相对路径的入口，因此只支持 `AT_FDCWD`（相对 cwd）与
/// 绝对路径；`AT_EMPTY_PATH` 走 fd stat。`AT_SYMLINK_NOFOLLOW` 暂不另做分支
/// （`RamFS` 的 `stat` 本身不跟随符号链接）。
///
/// # Errors
/// 指针为空/越界返回 `EFAULT`；路径 stat 失败返回 `EIO`；非 `AT_FDCWD` 的
/// 相对路径返回 `EINVAL`。
pub fn newfstatat_syscall(
    dirfd: i32,
    path_ptr: u64,
    st_buf_ptr: u64,
    flags: u32,
) -> Result<usize, Errno> {
    if path_ptr == 0 || st_buf_ptr == 0 {
        return Err(Errno::Efault);
    }
    if !api::validate_user_ptr(path_ptr) {
        return Err(Errno::Efault);
    }

    // glibc 的 ld.so 用 newfstatat 读取候选共享库元数据；AT_EMPTY_PATH 是
    // fd 版本，其余绝对路径/AT_FDCWD 交给路径 stat。
    if flags & AT_EMPTY_PATH != 0 && dirfd >= 0 {
        return fstat_syscall(dirfd, st_buf_ptr);
    }
    if dirfd == AT_FDCWD {
        return stat_syscall(path_ptr, st_buf_ptr);
    }
    Err(Errno::Einval)
}

// ============================================================================
// statx (T1 G1, syscall-followup 功能实装)
// ============================================================================

/// Linux `struct statx` (x86_64, 256 字节) — 扩展文件状态
///
/// SIMPLIFIED: 仅填充基础字段 (mode/uid/gid/size/nlink/ino/时间戳), dev/
/// `rdev/btime/attributes/mnt_id` 等置 0; 影响面: 依赖这些字段的调用方语义
/// 不完整; 何时需扩展: VFS 提供 dev/rdev/btime 元数据后补充.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Statx {
    /// 请求/返回的字段掩码
    pub mask: u32,
    /// 文件系统块大小
    pub blksize: u32,
    /// 文件属性 (文件系统特有)
    pub attributes: u64,
    /// 硬链接数
    pub nlink: u32,
    /// 属主用户 ID
    pub uid: u32,
    /// 属组 ID
    pub gid: u32,
    /// 文件类型与权限位
    pub mode: u16,
    /// 保留字段
    pub spare0: [u16; 1],
    /// inode 编号
    pub ino: u64,
    /// 文件字节数
    pub size: u64,
    /// 占用的 512 字节块数
    pub blocks: u64,
    /// 属性支持掩码
    pub attributes_mask: u64,
    /// 访问时间 (秒)
    pub atime_sec: i64,
    /// 访问时间 (纳秒)
    pub atime_nsec: u32,
    /// 保留字段
    pub atime_reserved: i32,
    /// 创建时间 (秒)
    pub btime_sec: i64,
    /// 创建时间 (纳秒)
    pub btime_nsec: u32,
    /// 保留字段
    pub btime_reserved: i32,
    /// 状态变更时间 (秒)
    pub ctime_sec: i64,
    /// 状态变更时间 (纳秒)
    pub ctime_nsec: u32,
    /// 保留字段
    pub ctime_reserved: i32,
    /// 修改时间 (秒)
    pub mtime_sec: i64,
    /// 修改时间 (纳秒)
    pub mtime_nsec: u32,
    /// 保留字段
    pub mtime_reserved: i32,
    /// 设备号主设备号
    pub rdev_major: u32,
    /// 设备号次设备号
    pub rdev_minor: u32,
    /// 所在文件系统主设备号
    pub dev_major: u32,
    /// 所在文件系统次设备号
    pub dev_minor: u32,
    /// 挂载 ID
    pub mnt_id: u64,
    /// 直接 I/O 内存对齐要求
    pub dio_mem_align: u32,
    /// 直接 I/O 偏移对齐要求
    pub dio_offset_align: u32,
    /// 保留字段
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
            atime_sec: s.atime.cast_signed(),
            atime_nsec: 0,
            atime_reserved: 0,
            btime_sec: 0,
            btime_nsec: 0,
            btime_reserved: 0,
            ctime_sec: s.ctime.cast_signed(),
            ctime_nsec: 0,
            ctime_reserved: 0,
            mtime_sec: s.mtime.cast_signed(),
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
        return Err(Errno::Efault);
    }
    if !api::validate_user_ptr(path_ptr) {
        return Err(Errno::Efault);
    }
    if !api::validate_user_buf(buf_ptr, stx_size) {
        return Err(Errno::Efault);
    }
    let pwm = current_pwm();
    let stat = fw::vfs_stat_safe(path_ptr as *const u8, pwm).ok_or(Errno::Eio)?;
    let stx = Statx::from_vfs_stat(&stat);
    if !api::write_struct_to_user::<Statx>(buf_ptr, &stx) {
        return Err(Errno::Efault);
    }
    Ok(0)
}

/// utimensat(dirfd, path, times, flags) 策略 (T1 G1 实装)
///
/// SIMPLIFIED: 仅支持 `dirfd == AT_FDCWD` (绝对路径); `times == NULL` 时设为
/// 当前时间 (tick/frequency 换算秒); 非 NULL 时读取用户 `timespec[2]` 数组
/// (`{sec, nsec}` 各 8 字节), `sec == -1` 表示该时间戳不修改 (Linux 语义,
/// 内部以 `u64::MAX` 传递). 委托 privileged `vfs_utimensat_safe`.
#[must_use]
pub fn utimensat_syscall(dirfd: i32, path_ptr: u64, times_ptr: u64, _flags: i32) -> i64 {
    const AT_FDCWD: i32 = -100;
    if dirfd != AT_FDCWD {
        return Errno::Enotsup.as_ret();
    }
    if path_ptr == 0 || !api::validate_user_ptr(path_ptr) {
        return Errno::Efault.as_ret();
    }
    let (atime, mtime) = if times_ptr == 0 {
        // NULL times: 设为当前时间 (Linux utimensat 语义)
        let now = crate::privileged::syscall::api::get_ticks()
            / u64::from(crate::privileged::timer::get_frequency());
        (now, now)
    } else {
        // timespec[2]: {atime{sec,nsec}, mtime{sec,nsec}} = 32 字节 = 4 × u64
        let mut times = [0u64; 4];
        if !crate::privileged::syscall::api::read_struct_from_user(times_ptr, &mut times) {
            return Errno::Efault.as_ret();
        }
        let atime_sec = times[0].cast_signed();
        let mtime_sec = times[2].cast_signed();
        let atime_v = if atime_sec == -1 { u64::MAX } else { times[0] };
        let mtime_v = if mtime_sec == -1 { u64::MAX } else { times[2] };
        (atime_v, mtime_v)
    };
    let Ok(path) = crate::privileged::mm::copy_user::copy_string_from_user(path_ptr, 4096) else {
        return Errno::Efault.as_ret();
    };
    let pwm = subject::current_raw();
    let r = crate::functions::fs::vfs_utimensat_safe(&path, atime, mtime, pwm);
    if r < 0 { Errno::Eio.as_ret() } else { 0 }
}

// ============================================================================
// 内部辅助
// ============================================================================

/// 取当前进程凭证 (历史硬编码 `TEST_PWM` 路径已弃用)。
fn current_pwm() -> u64 {
    subject::current_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_stat_size_matches_linux_uapi() {
        let expected = if cfg!(target_arch = "x86_64") {
            144
        } else {
            128
        };
        assert_eq!(core::mem::size_of::<LinuxStat>(), expected);
        assert_eq!(core::mem::align_of::<LinuxStat>(), 8);
    }

    #[test]
    fn linux_stat_maps_vfs_fields() {
        let vfs = VfsStat {
            node_id: 7,
            perm: 0o644,
            uid: 1000,
            gid: 1001,
            size: 1024,
            atime: 1,
            mtime: 2,
            ctime: 3,
            file_type: VfsFileType::File.as_u8(),
            ..VfsStat::default()
        };
        let stat = linux_stat_from_vfs(&vfs);
        assert_eq!(stat.st_ino, 7);
        assert_eq!(stat.st_mode, 0o100_644);
        assert_eq!(stat.st_size, 1024);
        assert_eq!(stat.st_blocks, 2);
        assert_eq!(stat.st_uid, 1000);
        assert_eq!(stat.st_gid, 1001);
        assert_eq!(stat.st_atim.tv_sec, 1);
        assert_eq!(stat.st_mtim.tv_sec, 2);
        assert_eq!(stat.st_ctim.tv_sec, 3);
    }
}
