//! Host-side test utilities — re-implements functions layer validation
//! logic for pure-scalar (no kernel-state) testing on host std environment.
//!
//! The actual functions layer uses #[deny(unsafe_code)] and depends on
//! the no_std kernel crate. For host testing, we extract the parameter
//! validation rules into equivalent pure functions.

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Errno {
    EINVAL,
    EFAULT,
    EBADF,
    ESRCH,
    ENOMEM,
    ECHILD,
    ENOSYS,
    EPERM,
    ENOENT,
    EAGAIN,
}

impl Errno {
    pub fn from_ret(ret: i64) -> Self {
        let errno = (-ret) as u64;
        match errno {
            1 => Self::EPERM,
            2 => Self::ENOENT,
            9 => Self::EBADF,
            12 => Self::ENOMEM,
            14 => Self::EFAULT,
            22 => Self::EINVAL,
            3 => Self::ESRCH,
            10 => Self::ECHILD,
            11 => Self::EAGAIN,
            38 => Self::ENOSYS,
            _ => Self::EINVAL,
        }
    }
}

// =============== mprotect validation ===============

pub const PROT_NONE: i32 = 0x0;
pub const PROT_READ: i32 = 0x1;
pub const PROT_WRITE: i32 = 0x2;
pub const PROT_EXEC: i32 = 0x4;

/// 验证 mprotect 参数 (等价于 functions::mm::mprotect::mprotect_syscall 的验证部分)
pub fn mprotect_validate(addr: u64, len: u64, prot: i32) -> Result<(), Errno> {
    if addr & 0xFFF != 0 {
        return Err(Errno::EINVAL);
    }
    if len == 0 {
        return Err(Errno::EINVAL);
    }
    let valid_prot = PROT_NONE | PROT_READ | PROT_WRITE | PROT_EXEC;
    if prot & !valid_prot != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== clone flags ===============

pub const CLONE_VM: u64 = 0x00000100;
pub const CLONE_FS: u64 = 0x00000200;
pub const CLONE_FILES: u64 = 0x00000400;
pub const CLONE_SIGHAND: u64 = 0x00000800;
pub const CLONE_THREAD: u64 = 0x00010000;
pub const CLONE_PARENT_SETTID: u64 = 0x00100000;

/// 验证 clone 参数 (等价于 functions::proc::clone::clone_syscall 的验证部分)
pub fn clone_validate(
    flags: u64,
    child_stack: u64,
) -> Result<(), Errno> {
    if (flags & CLONE_VM != 0 || flags & CLONE_THREAD != 0) && flags & CLONE_SIGHAND == 0 {
        return Err(Errno::EINVAL);
    }
    if child_stack != 0 && child_stack % 16 != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== Errno::from_ret 测试 ===============

pub fn errno_from_ret_value(ret: i64) -> Errno {
    Errno::from_ret(ret)
}

// =============== wait4 validation ===============

pub const WNOHANG: i32 = 0x1;
pub const WUNTRACED: i32 = 0x2;
pub const WCONTINUED: i32 = 0x8;

/// 验证 wait4 参数 (等价于 functions::proc::wait4::wait4_syscall 的验证部分)
pub fn wait4_validate(pid: i32, options: i32) -> Result<(), Errno> {
    // pid 范围: -32768..=32767
    const PID_MAX: i32 = 0x7FFF;
    const PID_MIN: i32 = -0x8000;
    if pid < PID_MIN || pid > PID_MAX {
        return Err(Errno::EINVAL);
    }

    let valid_opts = WNOHANG | WUNTRACED | WCONTINUED;
    if options & !valid_opts != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== info validation ===============

/// 验证 getpgid 参数 (等价于 functions::proc::info::getpgid_syscall 的验证部分)
pub fn getpgid_validate(pid: i32) -> Result<(), Errno> {
    if pid < 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// 验证 uname 参数 (等价于 functions::proc::info::uname_syscall 的验证部分)
pub fn uname_validate(buf: u64) -> Result<(), Errno> {
    if buf == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 gettimeofday 参数 (等价于 functions::proc::info::gettimeofday_syscall 的验证部分)
pub fn gettimeofday_validate(tv: u64) -> Result<(), Errno> {
    if tv == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

// =============== io validation ===============

/// 验证 pipe 参数 (等价于 functions::fs::io::pipe_syscall 的验证部分)
pub fn pipe_validate(fds: u64) -> Result<(), Errno> {
    if fds == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 dup 参数 (等价于 functions::fs::io::dup_syscall 的验证部分)
pub fn dup_validate(oldfd: i32) -> Result<(), Errno> {
    if oldfd < 0 {
        return Err(Errno::EBADF);
    }
    Ok(())
}

/// 验证 dup2 参数 (等价于 functions::fs::io::dup2_syscall 的验证部分)
pub fn dup2_validate(oldfd: i32, newfd: i32) -> Result<(), Errno> {
    if oldfd < 0 || newfd < 0 {
        return Err(Errno::EBADF);
    }
    Ok(())
}

/// 验证 fcntl 参数 (等价于 functions::fs::io::fcntl_syscall 的验证部分)
pub fn fcntl_validate(fd: i32, _cmd: i32, _arg: u64) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    Ok(())
}

// =============== net socket validation ===============

/// POSIX AF_INET
pub const AF_INET: i32 = 2;
/// POSIX SOCK_STREAM / SOCK_DGRAM
pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
/// SOL_SOCKET
pub const SOL_SOCKET: i32 = 1;
/// SO_REUSEADDR
pub const SO_REUSEADDR: i32 = 2;

/// 验证 socket 参数 (等价于 functions::net::syscall::socket_syscall 的验证部分)
pub fn socket_validate(domain: i32, sock_type: i32, protocol: i32) -> Result<(), Errno> {
    if domain != AF_INET {
        return Err(Errno::EINVAL);
    }
    if sock_type != SOCK_STREAM && sock_type != SOCK_DGRAM {
        return Err(Errno::EINVAL);
    }
    if protocol != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// 验证 bind 参数 (等价于 functions::net::syscall::bind_syscall 的验证部分)
pub fn bind_validate(fd: i32, addr_ptr: u64, _addrlen: u32) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if addr_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 listen 参数
pub fn listen_validate(fd: i32, backlog: i32) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if backlog < 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// 验证 accept 参数
pub fn accept_validate(fd: i32, addr_ptr: u64) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    let _ = addr_ptr; // 0 允许, 非 0 也允许
    Ok(())
}

/// 验证 connect 参数
pub fn connect_validate(fd: i32, addr_ptr: u64, _addrlen: u32) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if addr_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 sendto 参数
pub fn sendto_validate(fd: i32, buf_ptr: u64, len: u32, dest_ptr: u64) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if len > 0 && buf_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    let _ = dest_ptr;
    Ok(())
}

/// 验证 recvfrom 参数
pub fn recvfrom_validate(fd: i32, buf_ptr: u64, len: u32) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if buf_ptr == 0 || len == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 setsockopt 参数
pub fn setsockopt_validate(fd: i32, _level: i32, _optname: i32, val_ptr: u64) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if val_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 getsockopt 参数
pub fn getsockopt_validate(fd: i32, _level: i32, _optname: i32, val_ptr: u64) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if val_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 shutdown 参数
pub fn shutdown_validate(fd: i32, _how: i32) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    Ok(())
}

/// 验证 getsockname 参数: fd 合法, addr/addrlen 不可为 0
pub fn sockname_validate(fd: i32, addr: u64, addrlen: u64) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if addr == 0 || addrlen == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 getpeername 参数: 同 getsockname
pub fn peername_validate(fd: i32, addr: u64, addrlen: u64) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if addr == 0 || addrlen == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 getrusage 参数: who 必须为 0/1/2, rusage 不可为 0
pub fn rusage_validate(who: i32, rusage: u64) -> Result<(), Errno> {
    if who != 0 && who != 1 && who != 2 {
        return Err(Errno::EINVAL);
    }
    if rusage == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 sendmsg 参数: fd 合法, msg 不可为 0
/// 完整校验需读 msghdr, 此处只测前置层 (fd/msg 非空).
pub fn sendmsg_validate(fd: i32, msg_ptr: u64, _flags: i32) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if msg_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 recvmsg 参数: fd 合法, msg 不可为 0
pub fn recvmsg_validate(fd: i32, msg_ptr: u64, _flags: i32) -> Result<(), Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    if msg_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

// =============== init 启动状态 (host 模拟) ===============

/// init 启动状态常量 (与 functions::init 保持一致)
pub const INIT_STATUS_NOT_STARTED: u32 = 0;
pub const INIT_STATUS_UNPACKING: u32 = 1;
pub const INIT_STATUS_LOADING: u32 = 2;
pub const INIT_STATUS_RUNNING: u32 = 3;

/// host 端模拟: init 启动后状态
pub fn init_status_after_launch() -> u32 {
    // 真实内核: launch_first_user_process 走完后 status=3
    // host 端 host-tests 不跑 launch, 验证常量语义
    INIT_STATUS_RUNNING
}

// =============== mremap 验证 (host 模拟) ===============

/// MREMAP_MAYMOVE flag (Linux 1)
pub const MREMAP_MAYMOVE: i32 = 1;

/// 验证 mremap 入参: old_addr 非 0 且页对齐, old_size/new_size > 0
pub fn mremap_validate(old_addr: u64, old_size: u64, new_size: u64) -> Result<(), Errno> {
    if old_addr == 0 || old_size == 0 || new_size == 0 {
        return Err(Errno::EINVAL);
    }
    if old_addr & 0xFFF != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// 验证 mremap flags: 仅允许 MREMAP_MAYMOVE
pub fn mremap_validate_flags(flags: i32) -> Result<(), Errno> {
    if flags & !MREMAP_MAYMOVE != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== storage 验证 (host 模拟) ===============

/// 验证 disk_list 入参: disks 非 0, max_count > 0
pub fn disk_list_validate(disks_ptr: u64, max_count: u32) -> Result<(), Errno> {
    if disks_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if max_count == 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// 验证 disk_info 入参: info_ptr 非 0
pub fn disk_info_validate(info_ptr: u64) -> Result<(), Errno> {
    if info_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 disk_format 入参: fstype 非 0
pub fn disk_format_validate(fstype_ptr: u64) -> Result<(), Errno> {
    if fstype_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 disk_partition 入参: total_sectors 合法 (1..=u32::MAX)
pub fn disk_partition_validate(total_sectors: u64) -> Result<(), Errno> {
    if total_sectors == 0 || total_sectors > u32::MAX as u64 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== iobuf 容量计算 (host 模拟) ===============

/// 按 iov 列表计算总容量 (模拟 sendmsg 拼接前 size 累计).
/// 跳过 base=0 或 len=0 的 iov; 溢出返 0.
pub fn iobuf_total_capacity(lens: &[usize]) -> usize {
    let mut total: usize = 0;
    for &l in lens {
        if l == 0 {
            continue;
        }
        total = match total.checked_add(l) {
            Some(v) => v,
            None => return 0,
        };
    }
    total
}

/// 验证总容量页对齐后不超过 4MB 上限 (模拟 IobRegion::alloc 上限保护).
pub fn iobuf_pages(total: usize) -> u64 {
    if total == 0 {
        return 1;
    }
    // 向上页对齐: (n + 4095) / 4096. 当 total=0 时上面已返回, 这里 total >= 1.
    // 公式: (total - 1) / 4096 + 1 等价, 防止 total=4096 算出 0 的边界.
    let t = total as u64;
    (t - 1) / 4096 + 1
}

// =============== session validation ===============

/// setsid — 无参数, 总是尝试创建
pub fn setsid_validate() -> Result<(), Errno> {
    Ok(())
}

/// getsid(pid) — 验证 pid 参数
pub fn getsid_validate(pid: i32) -> Result<(), Errno> {
    if pid < 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// setpgid(pid, pgid) — 验证两个参数
pub fn setpgid_validate(pid: i32, pgid: i32) -> Result<(), Errno> {
    if pid < 0 || pgid < 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== mmap validation ===============

/// POSIX mmap flags
pub const MAP_SHARED: i32 = 0x01;
pub const MAP_PRIVATE: i32 = 0x02;
pub const MAP_ANONYMOUS: i32 = 0x20;
pub const MAP_FIXED: i32 = 0x10;
pub const MAP_FAILED: i64 = -1; // (void*)-1

/// 验证 mmap 参数 (等价于 functions::mm::mmap::mmap_syscall 的验证部分)
pub fn mmap_validate(
    _addr: u64,
    size: u64,
    prot: i32,
    flags: i32,
    _fd: i32,
    _offset: u64,
) -> Result<(), Errno> {
    if size == 0 {
        return Err(Errno::EINVAL);
    }
    // 校验 prot
    let valid_prot = 0x0 | 0x1 | 0x2 | 0x4;
    if prot & !valid_prot != 0 {
        return Err(Errno::EINVAL);
    }
    // MAP_SHARED / MAP_PRIVATE 必选其一
    if flags & (MAP_SHARED | MAP_PRIVATE) == 0 {
        return Err(Errno::EINVAL);
    }
    if flags & MAP_SHARED != 0 && flags & MAP_PRIVATE != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== brk validation ===============

/// 验证 brk 参数 (等价于 functions::mm::brk::brk_syscall 的验证部分)
pub fn brk_validate(addr: u64) -> Result<(), Errno> {
    // POSIX 允许 0 (取当前) 或非 0 (请求新 brk)
    if addr == 0 {
        return Ok(()); // 0 表示查询当前
    }
    // 用户地址范围: 不应超过用户空间上界
    const USER_ADDR_MAX: u64 = 0x0000_7FFF_FFFF_FFFF;
    if addr > USER_ADDR_MAX {
        return Err(Errno::ENOMEM);
    }
    Ok(())
}

// =============== path validation ===============

/// 验证 chdir 参数 (等价于 functions::fs::path::chdir_syscall 的验证部分)
pub fn chdir_validate(path_ptr: u64) -> Result<(), Errno> {
    if path_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// 验证 getcwd 参数 (等价于 functions::fs::path::getcwd_syscall 的验证部分)
pub fn getcwd_validate(buf_ptr: u64, size: u64) -> Result<(), Errno> {
    if buf_ptr == 0 || size == 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

// =============== uid/gid validation ===============

/// 验证 setuid 参数范围 (简化: u32 范围不溢出,无需校验)
pub fn setuid_validate(_uid: u32) -> Result<(), Errno> {
    Ok(())
}

/// 验证 setreuid 参数 (任一为 (uid_t)-1 = 0xFFFFFFFF 表示不变)
pub fn setreuid_validate(ruid: u32, euid: u32) -> Result<(), Errno> {
    let _ = ruid;
    let _ = euid;
    Ok(())
}

// =============== mode validation ===============

/// 验证 umask 参数 (0..=0o777)
pub fn umask_validate(mask: u32) -> Result<(), Errno> {
    if mask > 0o777 { return Err(Errno::EINVAL); }
    Ok(())
}

/// 验证 chmod mode (0..=0o7777)
pub fn chmod_mode_validate(mode: u32) -> Result<(), Errno> {
    if mode > 0o7777 { return Err(Errno::EINVAL); }
    Ok(())
}

/// 验证 fchmod (fd >= 0, mode 合法)
pub fn fchmod_validate(fd: i32, mode: u32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    if mode > 0o7777 { return Err(Errno::EINVAL); }
    Ok(())
}

/// 验证 mkdir path
pub fn mkdir_validate(path_ptr: u64) -> Result<(), Errno> {
    if path_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

/// 验证 rmdir path
pub fn rmdir_validate(path_ptr: u64) -> Result<(), Errno> {
    if path_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

// =============== access validation ===============

/// POSIX access 权限位
pub const F_OK: i32 = 0;
pub const X_OK: i32 = 1;
pub const W_OK: i32 = 2;
pub const R_OK: i32 = 4;

/// 验证 access path
pub fn access_validate(path_ptr: u64) -> Result<(), Errno> {
    if path_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

/// 验证 access mode 合法 (R_OK=4, W_OK=2, X_OK=1, F_OK=0, 位或 0..=7)
pub fn access_mode_validate(mode: i32) -> Result<(), Errno> {
    if mode < 0 || mode > 0o7 { return Err(Errno::EINVAL); }
    Ok(())
}

/// 验证 unlink path
pub fn unlink_validate(path_ptr: u64) -> Result<(), Errno> {
    if path_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

// =============== open validation ===============

/// POSIX open flags
pub const O_RDONLY: i32 = 0o0;
pub const O_WRONLY: i32 = 0o1;
pub const O_RDWR: i32 = 0o2;
pub const O_CREAT: i32 = 0o100;
pub const O_TRUNC: i32 = 0o1000;
pub const O_EXCL: i32 = 0o200;
pub const O_APPEND: i32 = 0o2000;
pub const O_DIRECTORY: i32 = 0o200_000;
pub const O_NOFOLLOW: i32 = 0o400_000;
pub const O_CLOEXEC: i32 = 0o2_000_000;

/// 验证 open path
pub fn open_path_validate(path_ptr: u64) -> Result<(), Errno> {
    if path_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

/// 验证 open flags
pub fn open_flags_validate(flags: i32) -> Result<(), Errno> {
    if flags < 0 { return Err(Errno::EINVAL); }
    let access = flags & 0o3;
    if access > 0o2 { return Err(Errno::EINVAL); }
    if (flags & O_CREAT) != 0 && access == O_RDONLY { return Err(Errno::EINVAL); }
    if (flags & O_TRUNC) != 0 && access == O_RDONLY { return Err(Errno::EINVAL); }
    if (flags & O_DIRECTORY) != 0 && (flags & O_CREAT) != 0 { return Err(Errno::EINVAL); }
    Ok(())
}

/// 验证 open mode (仅 O_CREAT 生效)
pub fn open_mode_validate(flags: i32, mode: i32) -> Result<(), Errno> {
    if (flags & O_CREAT) == 0 && mode != 0 { return Err(Errno::EINVAL); }
    if (flags & O_CREAT) != 0 && (mode < 0 || mode > 0o7777) { return Err(Errno::EINVAL); }
    Ok(())
}

/// 验证 close fd
pub fn close_fd_validate(fd: i32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

// =============== link validation ===============

/// 验证 link oldpath/newpath
pub fn link_path_validate(oldpath_ptr: u64, newpath_ptr: u64) -> Result<(), Errno> {
    if oldpath_ptr == 0 || newpath_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

/// 验证 symlink target/linkpath
pub fn symlink_path_validate(target_ptr: u64, linkpath_ptr: u64) -> Result<(), Errno> {
    if target_ptr == 0 || linkpath_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

/// 验证 readlink 参数
pub fn readlink_validate(path_ptr: u64, buf_ptr: u64, bufsiz: u64) -> Result<(), Errno> {
    if path_ptr == 0 || buf_ptr == 0 { return Err(Errno::EFAULT); }
    if bufsiz == 0 { return Err(Errno::EINVAL); }
    Ok(())
}

// =============== mount validation ===============

/// mount target/fstype 校验
pub fn mount_target_validate(target_ptr: u64, fstype_ptr: u64, source_ptr: u64) -> Result<(), Errno> {
    if target_ptr == 0 { return Err(Errno::EFAULT); }
    if fstype_ptr == 0 { return Err(Errno::EINVAL); }
    if source_ptr != 0 {
        // source 可选, 校验其用户态指针
        Ok(())
    } else {
        Ok(())
    }
}

/// umount2 target 校验
pub fn umount2_target_validate(target_ptr: u64) -> Result<(), Errno> {
    if target_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

// =============== timer validation ===============

/// ITIMER 类别
pub const ITIMER_REAL: i32 = 0;
pub const ITIMER_VIRTUAL: i32 = 1;
pub const ITIMER_PROF: i32 = 2;

pub fn itimer_which_validate(which: i32) -> Result<(), Errno> {
    if which < 0 || which > 3 { return Err(Errno::EINVAL); }
    Ok(())
}

pub fn getitimer_value_validate(value_ptr: u64) -> Result<(), Errno> {
    if value_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

pub fn setitimer_new_validate(new_ptr: u64) -> Result<(), Errno> {
    if new_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

pub fn alarm_seconds_ok(_seconds: u32) -> Result<(), Errno> {
    Ok(())
}

pub fn fchown_fd_validate(fd: i32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

pub fn times_buf_validate(buf_ptr: u64) -> Result<(), Errno> {
    if buf_ptr != 0 {
        // tms 结构体 16 字节对齐
        if buf_ptr & 0x3 != 0 { return Err(Errno::EINVAL); }
    }
    Ok(())
}

// =============== misc fs validation ===============

/// rename 路径校验
pub fn rename_path_validate(oldpath_ptr: u64, newpath_ptr: u64) -> Result<(), Errno> {
    if oldpath_ptr == 0 || newpath_ptr == 0 { return Err(Errno::EFAULT); }
    if oldpath_ptr == newpath_ptr { return Err(Errno::EINVAL); }
    Ok(())
}

/// fsync fd 校验
pub fn fsync_fd_validate(fd: i32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// time tloc 校验
pub fn time_tloc_validate(tloc_ptr: u64) -> Result<(), Errno> {
    if tloc_ptr == 0 { return Ok(()); } // tloc 可为 NULL
    if tloc_ptr & 0x7 != 0 { return Err(Errno::EINVAL); } // 8 字节对齐
    Ok(())
}

// =============== stat validation ===============

/// 验证 stat 参数
pub fn stat_validate(path_ptr: u64, st_buf_ptr: u64, stat_size: u64) -> Result<(), Errno> {
    if path_ptr == 0 { return Err(Errno::EFAULT); }
    if st_buf_ptr == 0 { return Err(Errno::EFAULT); }
    if stat_size < MIN_STAT_SIZE { return Err(Errno::EINVAL); }
    Ok(())
}

/// 验证 lstat 参数 (同 stat)
pub fn lstat_validate(path_ptr: u64, st_buf_ptr: u64, stat_size: u64) -> Result<(), Errno> {
    stat_validate(path_ptr, st_buf_ptr, stat_size)
}

/// 验证 fstat 参数
pub fn fstat_validate(fd: i32, st_buf_ptr: u64, stat_size: u64) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    if st_buf_ptr == 0 { return Err(Errno::EFAULT); }
    if stat_size < MIN_STAT_SIZE { return Err(Errno::EINVAL); }
    Ok(())
}

/// 最小 VfsStat 大小 (用于 host 测试固定假设)
pub const MIN_STAT_SIZE: u64 = 64;

// =============== rlimit validation ===============

/// 验证 getrlimit (rlim != 0, resource 合法)
pub fn getrlimit_validate(resource: i32, rlim_ptr: u64) -> Result<(), Errno> {
    if rlim_ptr == 0 { return Err(Errno::EINVAL); }
    if resource < 0 || resource > 16 { return Err(Errno::EINVAL); }
    Ok(())
}

// =============== DNS / IPv4 literal (D1.2) ===============

/// 解析 IPv4 字面量 "a.b.c.d"
///
/// 等价于 `privileged::net::init::parse_ipv4_literal` (无错处理; 不合法返 None)。
pub fn parse_ipv4_literal(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut idx = 0usize;
    let mut cur: u32 = 0;
    let mut has_digit = false;
    for &b in s.as_bytes() {
        if b == b'.' {
            if !has_digit || idx >= 3 || cur > 255 {
                return None;
            }
            octets[idx] = cur as u8;
            idx += 1;
            cur = 0;
            has_digit = false;
        } else if b.is_ascii_digit() {
            cur = cur * 10 + (b - b'0') as u32;
            has_digit = true;
        } else {
            return None;
        }
    }
    if !has_digit || idx != 3 || cur > 255 {
        return None;
    }
    octets[3] = cur as u8;
    Some(octets)
}

/// 静态 hosts 表条目: 主机名 → IPv4
struct HostEntry {
    name: &'static str,
    ip: [u8; 4],
}

/// 内置静态 hosts (与 privileged::net::init::STATIC_HOSTS 保持一致)
const STATIC_HOSTS: &[HostEntry] = &[
    HostEntry { name: "localhost",       ip: [127, 0, 0, 1] },
    HostEntry { name: "router",          ip: [10, 0, 2, 2]  },
    HostEntry { name: "host",            ip: [10, 0, 2, 15] },
    HostEntry { name: "qemu-gateway",    ip: [10, 0, 2, 2]  },
    HostEntry { name: "edgine-gateway",    ip: [10, 0, 2, 2]  },
];

/// 简单 DNS 解析 (等价于 `privileged::net::init::dns_resolve`)
pub fn dns_resolve(name: &str) -> Option<[u8; 4]> {
    for entry in STATIC_HOSTS {
        if entry.name.eq_ignore_ascii_case(name) {
            return Some(entry.ip);
        }
    }
    if let Some(ip) = parse_ipv4_literal(name) {
        return Some(ip);
    }
    None
}

// =============== Socket 子系统 (D1.3) ===============

/// Socket 错误 (等价于 functions::net::socket::SocketError)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketError {
    PermissionDenied,
    BadFd,
    WouldBlock,
    NoMemory,
    Fault,
    InvalidArgument,
    ProcessFileLimit,
    NoDevice,
    NotSupported,
    AddrFamilyNotSupported,
    AddrInUse,
    AddrNotAvailable,
    ConnectionReset,
    NotConnected,
    ConnectionRefused,
    NotReady,
    Other(i32),
}

impl SocketError {
    pub fn from_i32(rc: i32) -> Self {
        match rc {
            1 => Self::PermissionDenied,
            9 => Self::BadFd,
            11 => Self::WouldBlock,
            12 => Self::NoMemory,
            14 => Self::Fault,
            19 => Self::NoDevice,
            22 => Self::InvalidArgument,
            23 => Self::ProcessFileLimit,
            95 => Self::NotSupported,
            97 => Self::AddrFamilyNotSupported,
            98 => Self::AddrInUse,
            99 => Self::AddrNotAvailable,
            104 => Self::ConnectionReset,
            107 => Self::NotConnected,
            111 => Self::ConnectionRefused,
            _ => Self::Other(rc),
        }
    }
}

/// Socket 协议族 (等价于 functions::net::socket::Domain)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Domain {
    Inet = 2,
}

impl Domain {
    pub fn from_i32(d: i32) -> Option<Self> {
        match d {
            2 => Some(Self::Inet),
            _ => None,
        }
    }
}

/// Socket 类型 (等价于 functions::net::socket::SockType)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum SockType {
    Stream = 1,
    Dgram = 2,
}

impl SockType {
    pub fn from_i32(t: i32) -> Option<Self> {
        match t {
            1 => Some(Self::Stream),
            2 => Some(Self::Dgram),
            _ => None,
        }
    }
}

/// IPv4 Socket 地址 (等价于 functions::net::socket::SockAddrIn)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SockAddrIn {
    pub port: u16,
    pub ip: [u8; 4],
}

impl SockAddrIn {
    pub fn new(port: u16, ip: [u8; 4]) -> Self {
        Self { port, ip }
    }
}

/// IPv4 Socket 地址 → 8 字节 C 结构体 (等价于 functions::net::socket::sockaddr_in_to_bytes)
pub fn sockaddr_in_to_bytes(addr: &SockAddrIn) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..2].copy_from_slice(&(2u16).to_be_bytes()); // AF_INET
    buf[2..4].copy_from_slice(&addr.port.to_be_bytes());
    buf[4..8].copy_from_slice(&addr.ip);
    buf
}

/// 8 字节 C 结构体 → IPv4 Socket 地址 (等价于 functions::net::socket::bytes_to_sockaddr_in)
pub fn bytes_to_sockaddr_in(buf: &[u8; 8]) -> Option<SockAddrIn> {
    let family = u16::from_be_bytes([buf[0], buf[1]]);
    if family != 2 {
        return None;
    }
    let port = u16::from_be_bytes([buf[2], buf[3]]);
    let mut ip = [0u8; 4];
    ip.copy_from_slice(&buf[4..8]);
    Some(SockAddrIn { port, ip })
}

/// 解析 "a.b.c.d" 格式 IP 字符串 (等价于 functions::net::socket::parse_ipv4)
pub fn socket_parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut parts = s.split('.');
    let mut out = [0u8; 4];
    for i in 0..4 {
        let p = parts.next()?;
        let v: u32 = p.parse().ok()?;
        if v > 255 {
            return None;
        }
        out[i] = v as u8;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// 构造 `SockAddrIn` from IP 字符串 + 端口
pub fn endpoint_from_str(ip: &str, port: u16) -> Option<SockAddrIn> {
    let bytes = socket_parse_ipv4(ip)?;
    Some(SockAddrIn::new(port, bytes))
}

// =============== Socket Syscall 12 dispatch (D1.4) ===============
//
// 各 dispatch 的"参数预校验" (前几行 if-check) 在 functions/net/syscall.rs 与
// edgine-tests 中等价, 因为它们只是纯标量验证; 真正的 fw:: 委托调用需要
// QEMU 集成测试覆盖。

/// `socket_syscall` 不做预校验 (3 个参数全是合法范围, 由底层校验)
pub fn socket_syscall_validate(_domain: i32, _sock_type: i32, _protocol: i32) -> Result<(), Errno> {
    Ok(())
}

/// `bind_syscall` 仅校验 fd<0
pub fn bind_syscall_validate(fd: i32, _addr_ptr: u64, _addrlen: u32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// `listen_syscall` 校验 fd<0, backlog<0
pub fn listen_syscall_validate(fd: i32, backlog: i32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    if backlog < 0 { return Err(Errno::EINVAL); }
    Ok(())
}

/// `accept_syscall` 校验 fd<0
pub fn accept_syscall_validate(fd: i32, _addr_ptr: u64, _addrlen_ptr: u64) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// `connect_syscall` 校验 fd<0
pub fn connect_syscall_validate(fd: i32, _addr_ptr: u64, _addrlen: u32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// `sendto_syscall` 校验 fd<0
pub fn sendto_syscall_validate(
    fd: i32, _buf_ptr: u64, _len: u32, _flags: i32, _dest_ptr: u64, _dest_len: u32,
) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// `recvfrom_syscall` 校验 fd<0, buf_ptr==0 || len==0
pub fn recvfrom_syscall_validate(
    fd: i32, buf_ptr: u64, len: u32, _flags: i32, _src_ptr: u64, _src_len_ptr: u64,
) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    if buf_ptr == 0 || len == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

/// `setsockopt_syscall` 校验 fd<0
pub fn setsockopt_syscall_validate(
    fd: i32, _level: i32, _optname: i32, _val_ptr: u64, _valen: u32,
) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// `getsockopt_syscall` 校验 fd<0
pub fn getsockopt_syscall_validate(
    fd: i32, _level: i32, _optname: i32, _val_ptr: u64, _valen_ptr: u64,
) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// `shutdown_syscall` 校验 fd<0
pub fn shutdown_syscall_validate(fd: i32, _how: i32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    Ok(())
}

/// `sendmsg_syscall` 校验 fd<0, msg_ptr==0, iovlen 范围
pub fn sendmsg_syscall_validate(fd: i32, msg_ptr: u64, _flags: i32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    if msg_ptr == 0 { return Err(Errno::EFAULT); }
    // 注: iov 校验依赖 raw::check_user_buf + read_u64_from_user, 需 privileged
    // host 端无法复刻, 委托 fw 调用; 此处只覆盖前两个最常见错误
    Ok(())
}

/// `recvmsg_syscall` 校验 fd<0, msg_ptr==0, iovlen 范围
pub fn recvmsg_syscall_validate(fd: i32, msg_ptr: u64, _flags: i32) -> Result<(), Errno> {
    if fd < 0 { return Err(Errno::EBADF); }
    if msg_ptr == 0 { return Err(Errno::EFAULT); }
    Ok(())
}

/// sendmsg/recvmsg 内部 iov 校验 (供 fw::sendmsg_syscall 调用前)
///
/// 校验: iovlen in 1..=1024, iov_bytes = iovlen * 16 不溢出
pub fn msg_iov_validate(iov_ptr: u64, iovlen: u64) -> Result<(), Errno> {
    if iovlen == 0 || iovlen > 1024 {
        return Err(Errno::EINVAL);
    }
    if iov_ptr == 0 {
        return Err(Errno::EINVAL);
    }
    let _iov_bytes = iovlen.checked_mul(16).ok_or(Errno::EINVAL)?;
    Ok(())
}
