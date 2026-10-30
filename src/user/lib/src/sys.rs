//! Edgine 用户态运行时 — POSIX 原生系统调用层
//! x86_64: syscall, rax=num, rdi=a1, rsi=a2, rdx=a3, r10=a4, r8=a5, rax=ret
//! aarch64: svc #0, x0=num, x1-x4=args, x0=返回
use core::arch::asm;

// ============================================================
// POSIX syscall 编号 — 与内核 syscall/types.rs 对应
// ============================================================

// 文件 I/O
pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_FCNTL: u64 = 72;
pub const SYS_IOCTL: u64 = 16;
pub const SYS_STAT: u64 = 4;
pub const SYS_FSTAT: u64 = 5;
pub const SYS_LSEEK: u64 = 8;
pub const SYS_ACCESS: u64 = 21;
pub const SYS_PIPE: u64 = 22;
pub const SYS_DUP: u64 = 32;
pub const SYS_DUP2: u64 = 33;
pub const SYS_GETPID: u64 = 39;
pub const SYS_FORK: u64 = 57;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_UNAME: u64 = 63;
pub const SYS_GETDENTS: u64 = 78;
pub const SYS_GETCWD: u64 = 79;
pub const SYS_CHDIR: u64 = 80;
pub const SYS_RENAME: u64 = 82;
pub const SYS_MKDIR: u64 = 83;
pub const SYS_RMDIR: u64 = 84;
pub const SYS_UNLINK: u64 = 87;
pub const SYS_GETUID: u64 = 102;
pub const SYS_GETGID: u64 = 104;
pub const SYS_GETEUID: u64 = 107;
pub const SYS_GETEGID: u64 = 108;
pub const SYS_GETPPID: u64 = 110;
pub const SYS_SYNC: u64 = 162;
pub const SYS_MOUNT: u64 = 165;

// Edgine 私有 syscall (SGEG 400-499, 与内核 syscall/types.rs 权威源对齐)
pub const SYS_SGEG_LOGIN: u64 = 400;
pub const SYS_SGEG_LOGOUT: u64 = 401;
pub const SYS_SGEG_CREATE_IDENTITY: u64 = 402;
pub const SYS_SGEG_CHANGE_PASSWORD: u64 = 405;
pub const SYS_SGEG_CREATE_FIRST: u64 = 407;
pub const SYS_SGEG_GET_PWM: u64 = 412;
pub const SYS_SGEG_DISK_LIST: u64 = 420;
pub const SYS_SGEG_DISK_INFO: u64 = 421;
pub const SYS_SGEG_DISK_FORMAT: u64 = 422;
pub const SYS_SGEG_DISK_PARTITION: u64 = 423;
pub const SYS_SGEG_DISK_INSTALL: u64 = 453;
pub const SYS_SGEG_FAT_FORMAT: u64 = 454;
pub const SYS_SGEG_PROC_LIST: u64 = 455;
pub const SYS_SGEG_GETHOSTNAME: u64 = 459;
pub const SYS_SGEG_SETHOSTNAME: u64 = 460;
pub const SYS_SGEG_REBOOT: u64 = 462;
pub const SYS_SGEG_HOTPLUG_STATUS: u64 = 463;

// POSIX open flags
pub const O_RDONLY: i32 = 0;
pub const O_WRONLY: i32 = 1;
pub const O_RDWR: i32 = 2;
pub const O_CREAT: i32 = 0o100;
pub const O_TRUNC: i32 = 0o1000;

pub const FT_FILE: u8 = 0;
pub const FT_DIR: u8 = 1;
pub const FT_DEV: u8 = 2;

#[repr(C)]
pub struct UserDirEntry {
    pub node: u32,
    pub file_type: u8,
    pub name: [u8; 256],
}
#[repr(C)]
pub struct UserDiskInfo {
    pub disk_id: u32,
    pub present: u32,
    pub total_sectors: u32,
    pub sectors: u32,
    pub model: [u8; 64],
}

// ============================================================
// 系统调用门 — 双架构
// ============================================================

// syscall 寄存器参数: 有符号值按二补码零扩展为 u64 (与 Linux syscall ABI 位型一致, 不 panic)
fn arg_bits(v: i64) -> u64 {
    v.cast_unsigned()
}

// 内核 i64 返回值 → POSIX i32 返回面 (越界即 panic, 正常返回值恒在范围内)
fn ret_i32(v: i64) -> i32 {
    i32::try_from(v).expect("内核返回值超出 i32 范围")
}

// 内核 i64 返回值 → u32 (仅用于 uid/gid/umask 等无符号结果)
fn ret_u32(v: i64) -> u32 {
    u32::try_from(v).expect("内核返回值超出 u32 范围")
}

// 内核 i64 返回值 → isize (sendto/recvfrom 族的字节数与错误码返回面)
fn ret_isize(v: i64) -> isize {
    isize::try_from(v).expect("内核返回值超出 isize 范围")
}

#[cfg(target_arch = "x86_64")]
unsafe fn sys0(num: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe { asm!("syscall", in("rax") num, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys1(num: u64, a1: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys2(num: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys3(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5, in("r9") a6, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}

#[cfg(target_arch = "aarch64")]
unsafe fn sys0(num: u64) -> i64 {
    let ret: i64;
    unsafe { asm!("svc #0", in("x0") num, lateout("x0") ret) };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys1(num: u64, a1: u64) -> i64 {
    let ret: i64;
    unsafe { asm!("svc #0", in("x0") num, in("x1") a1, lateout("x0") ret) };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys2(num: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe { asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, lateout("x0") ret) };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys3(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, lateout("x0") ret);
    };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, lateout("x0") ret);
    };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, lateout("x0") ret);
    };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, in("x6") a6, lateout("x0") ret);
    };
    ret
}

// ============================================================
// POSIX 标准 syscall wrapper
// ============================================================

pub fn read(fd: i32, buf: &mut [u8]) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_READ,
            arg_bits(i64::from(fd)),
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ))
    }
}
#[must_use]
pub fn write(fd: i32, buf: &[u8]) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_WRITE,
            arg_bits(i64::from(fd)),
            buf.as_ptr() as u64,
            buf.len() as u64,
        ))
    }
}
#[must_use]
pub fn open(path: &[u8], flags: i32, mode: i32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_OPEN,
            path.as_ptr() as u64,
            arg_bits(i64::from(flags)),
            arg_bits(i64::from(mode)),
        ))
    }
}
#[must_use]
pub fn close(fd: i32) -> i32 {
    unsafe { ret_i32(sys1(SYS_CLOSE, arg_bits(i64::from(fd)))) }
}
#[must_use]
pub fn getpid() -> u64 {
    unsafe { sys0(SYS_GETPID).cast_unsigned() }
}
#[must_use]
pub fn fork() -> u64 {
    unsafe { sys0(SYS_FORK).cast_unsigned() }
}
#[must_use]
pub fn getuid() -> u32 {
    unsafe { ret_u32(sys0(SYS_GETUID)) }
}
#[must_use]
pub fn geteuid() -> u32 {
    unsafe { ret_u32(sys0(SYS_GETEUID)) }
}
pub fn sched_yield() {
    unsafe {
        sys0(24);
    }
}
pub fn sync() {
    unsafe {
        sys0(SYS_SYNC);
    }
}

// ============================================================
// Edgine 兼容 wrapper — 保留原有语义，内部映射到 POSIX/PWM 编号
// ============================================================

#[must_use]
pub fn proc_exec(path: &[u8], argv: &[*const u8]) -> i64 {
    unsafe { sys3(SYS_EXECVE, path.as_ptr() as u64, argv.as_ptr() as u64, 0) }
}
pub fn proc_exit(code: i32) -> ! {
    unsafe {
        sys1(SYS_EXIT, arg_bits(i64::from(code)));
    }
    loop {
        if cfg!(target_arch = "x86_64") {
            unsafe {
                asm!("hlt", options(nomem, nostack));
            }
        } else {
            unsafe {
                asm!("wfi", options(nomem, nostack));
            }
        }
    }
}
#[must_use]
pub fn proc_get_pwm() -> u64 {
    unsafe { sys0(SYS_SGEG_GET_PWM).cast_unsigned() }
}
pub fn proc_yield() {
    unsafe {
        sys0(24);
    }
}

pub fn pipe_create(fds: &mut [i32; 2]) -> i32 {
    unsafe { ret_i32(sys1(SYS_PIPE, fds.as_mut_ptr() as u64)) }
}
#[must_use]
pub fn dup_fd(oldfd: i32) -> i32 {
    unsafe { ret_i32(sys1(SYS_DUP, arg_bits(i64::from(oldfd)))) }
}
#[must_use]
pub fn dup2_fd(oldfd: i32, newfd: i32) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_DUP2,
            arg_bits(i64::from(oldfd)),
            arg_bits(i64::from(newfd)),
        ))
    }
}
#[must_use]
pub fn wait_pid(pid: i32) -> i64 {
    unsafe { sys3(SYS_WAIT4, arg_bits(i64::from(pid)), 0, 0) }
}

#[must_use]
pub fn fs_open(path: &[u8], flags: i32, m: i32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_OPEN,
            path.as_ptr() as u64,
            arg_bits(i64::from(flags)),
            arg_bits(i64::from(m)),
        ))
    }
}
#[must_use]
pub fn fs_close(fd: i32) -> i32 {
    unsafe { ret_i32(sys1(SYS_CLOSE, arg_bits(i64::from(fd)))) }
}
pub fn fs_read(fd: i32, buf: &mut [u8]) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_READ,
            arg_bits(i64::from(fd)),
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ))
    }
}
#[must_use]
pub fn fs_write(fd: i32, buf: &[u8]) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_WRITE,
            arg_bits(i64::from(fd)),
            buf.as_ptr() as u64,
            buf.len() as u64,
        ))
    }
}

#[must_use]
pub fn fs_mkdir(path: &[u8]) -> i32 {
    unsafe { ret_i32(sys2(SYS_MKDIR, path.as_ptr() as u64, 0o755)) }
}
#[must_use]
pub fn fs_rmdir(path: &[u8]) -> i32 {
    unsafe { ret_i32(sys1(SYS_RMDIR, path.as_ptr() as u64)) }
}
#[must_use]
pub fn fs_unlink(path: &[u8]) -> i32 {
    unsafe { ret_i32(sys1(SYS_UNLINK, path.as_ptr() as u64)) }
}
#[must_use]
pub fn fs_rename(old: &[u8], new: &[u8]) -> i32 {
    unsafe { ret_i32(sys2(SYS_RENAME, old.as_ptr() as u64, new.as_ptr() as u64)) }
}
pub fn fs_readdir(fd: i32, entry: &mut UserDirEntry) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_GETDENTS,
            arg_bits(i64::from(fd)),
            core::ptr::from_mut::<UserDirEntry>(entry) as u64,
        ))
    }
}
pub fn fs_sync() {
    unsafe {
        sys0(SYS_SYNC);
    }
}

#[must_use]
pub fn fs_mount(src: &[u8], tgt: &[u8], typ: &[u8], opt: &[u8]) -> i32 {
    unsafe {
        ret_i32(sys4(
            SYS_MOUNT,
            src.as_ptr() as u64,
            tgt.as_ptr() as u64,
            typ.as_ptr() as u64,
            opt.as_ptr() as u64,
        ))
    }
}
#[must_use]
pub fn fs_unmount(t: &[u8]) -> i32 {
    unsafe { ret_i32(sys1(166, t.as_ptr() as u64)) }
}

#[must_use]
pub fn auth_login(n: &[u8], p: &[u8]) -> i64 {
    unsafe { sys2(SYS_SGEG_LOGIN, n.as_ptr() as u64, p.as_ptr() as u64) }
}
pub fn auth_logout() {
    unsafe {
        sys0(SYS_SGEG_LOGOUT);
    }
}
#[must_use]
pub fn auth_create_first(pw: &[u8]) -> i32 {
    unsafe { ret_i32(sys1(SYS_SGEG_CREATE_FIRST, pw.as_ptr() as u64)) }
}
#[must_use]
pub fn auth_change_password(o: &[u8], n: &[u8]) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_CHANGE_PASSWORD,
            o.as_ptr() as u64,
            n.as_ptr() as u64,
        ))
    }
}

pub fn env_getcwd(buf: &mut [u8]) -> i32 {
    unsafe { ret_i32(sys2(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64)) }
}
#[must_use]
pub fn env_chdir(path: &[u8]) -> i32 {
    unsafe { ret_i32(sys1(SYS_CHDIR, path.as_ptr() as u64)) }
}

pub fn gethostname(buf: &mut [u8]) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_GETHOSTNAME,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        ))
    }
}
#[must_use]
pub fn sethostname(name: &[u8]) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_SETHOSTNAME,
            name.as_ptr() as u64,
            name.len() as u64,
        ))
    }
}
#[must_use]
pub fn reboot(cmd: i32) -> i64 {
    unsafe { sys1(SYS_SGEG_REBOOT, arg_bits(i64::from(cmd))) }
}
pub fn proc_list(buf: &mut [u8], max_entries: u32) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_PROC_LIST,
            buf.as_mut_ptr() as u64,
            u64::from(max_entries),
        ))
    }
}

pub fn disk_list(disks: &mut [u64]) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_DISK_LIST,
            disks.as_mut_ptr() as u64,
            disks.len() as u64,
        ))
    }
}
pub fn disk_info(id: u32, info: &mut UserDiskInfo) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_DISK_INFO,
            u64::from(id),
            core::ptr::from_mut::<UserDiskInfo>(info) as u64,
        ))
    }
}
#[must_use]
pub fn disk_format(id: u32) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_DISK_FORMAT,
            u64::from(id),
            c"egfs".as_ptr() as u64,
        ))
    }
}
#[must_use]
pub fn disk_partition(id: u32, sectors: u64) -> i64 {
    unsafe { sys2(SYS_SGEG_DISK_PARTITION, u64::from(id), sectors) }
}
#[must_use]
pub fn boot_install(id: u32) -> i64 {
    unsafe { sys1(SYS_SGEG_DISK_INSTALL, u64::from(id)) }
}
#[must_use]
pub fn fat_format(id: u32) -> i64 {
    unsafe { sys1(SYS_SGEG_FAT_FORMAT, u64::from(id)) }
}
// T2 批 5 (syscall-followup): 补 wrapper — 原仅常量无 wrapper 无调用 (T3 登记)
pub fn hotplug_status(buf: &mut [u8]) -> i64 {
    unsafe {
        sys2(
            SYS_SGEG_HOTPLUG_STATUS,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

// ============================================================
// 新增 POSIX syscall wrapper
// ============================================================

pub const SYS_TRUNCATE: u64 = 76;
pub const SYS_FTRUNCATE: u64 = 77;
pub const SYS_FSYNC: u64 = 170;
pub const SYS_GETPGID: u64 = 111;
pub const SYS_SETSID: u64 = 112;
pub const SYS_GETTID: u64 = 186;
pub const SYS_NANOSLEEP: u64 = 35;
pub const SYS_RT_SIGACTION: u64 = 13;
pub const SYS_RT_SIGPROCMASK: u64 = 14;
pub const SYS_UMASK: u64 = 95;

#[repr(C)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[must_use]
pub fn truncate(path: &[u8], length: i64) -> i32 {
    unsafe { ret_i32(sys2(SYS_TRUNCATE, path.as_ptr() as u64, arg_bits(length))) }
}
#[must_use]
pub fn ftruncate(fd: i32, length: i64) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_FTRUNCATE,
            arg_bits(i64::from(fd)),
            arg_bits(length),
        ))
    }
}
#[must_use]
pub fn fsync(fd: i32) -> i32 {
    unsafe { ret_i32(sys1(SYS_FSYNC, arg_bits(i64::from(fd)))) }
}
#[must_use]
pub fn getpgid(pid: i32) -> i64 {
    unsafe { sys1(SYS_GETPGID, arg_bits(i64::from(pid))) }
}
#[must_use]
pub fn setsid() -> i64 {
    unsafe { sys0(SYS_SETSID) }
}
#[must_use]
pub fn gettid() -> i64 {
    unsafe { sys0(SYS_GETTID) }
}
#[must_use]
pub fn umask(mask: u32) -> u32 {
    unsafe { ret_u32(sys1(SYS_UMASK, u64::from(mask))) }
}
#[must_use]
pub fn nanosleep(req: &Timespec) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_NANOSLEEP,
            core::ptr::from_ref::<Timespec>(req) as u64,
            0,
        ))
    }
}
#[must_use]
pub fn kill(pid: i32, sig: i32) -> i32 {
    unsafe { ret_i32(sys2(62, arg_bits(i64::from(pid)), arg_bits(i64::from(sig)))) }
}

// ============================================================
// 热插拔状态查询
// ============================================================

// ============================================================
// 帧缓冲设备 (FB syscalls — Edgine 私有, 与内核 720-722 对齐)
// ============================================================

pub const SYS_FB_OPEN: u64 = 720;
pub const SYS_FB_MMAP: u64 = 721;
pub const SYS_FB_RELEASE: u64 = 722;

#[repr(C)]
pub struct FbInfo {
    pub phys_addr: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u8,
    pub reserved: [u8; 3],
}

pub fn fb_open(info: &mut FbInfo) -> i64 {
    unsafe { sys1(SYS_FB_OPEN, core::ptr::from_mut::<FbInfo>(info) as u64) }
}
#[must_use]
pub fn fb_mmap(addr: u64, size: u64, prot: i32) -> i64 {
    unsafe { sys3(SYS_FB_MMAP, addr, size, arg_bits(i64::from(prot))) }
}
#[must_use]
pub fn fb_release(addr: u64) -> i64 {
    unsafe { sys1(SYS_FB_RELEASE, addr) }
}

// ============================================================
// 网络 socket syscall 常量 — 与内核 syscall/types.rs 对应
// ============================================================

pub const SYS_SOCKET: u64 = 41;
pub const SYS_CONNECT: u64 = 42;
pub const SYS_ACCEPT: u64 = 43;
pub const SYS_SENDTO: u64 = 44;
pub const SYS_RECVFROM: u64 = 45;
pub const SYS_SENDMSG: u64 = 46;
pub const SYS_RECVMSG: u64 = 47;
pub const SYS_SHUTDOWN: u64 = 48;
pub const SYS_BIND: u64 = 49;
pub const SYS_LISTEN: u64 = 50;
pub const SYS_GETSOCKNAME: u64 = 51;
pub const SYS_GETPEERNAME: u64 = 52;
pub const SYS_SETSOCKOPT: u64 = 54;
pub const SYS_GETSOCKOPT: u64 = 55;
pub const SYS_GETRUSAGE: u64 = 98;

// 地址族
pub const AF_INET: i32 = 2;
pub const AF_INET6: i32 = 10;

// socket 类型
pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
pub const SOCK_RAW: i32 = 3;

// 协议 (IPPROTO_TCP/UDP 由 edgine-abi 单源提供, 见下方 re-export;
// IPPROTO_RAW 非 sockopt 分派值域, 留本处).
pub const IPPROTO_RAW: i32 = 255;

// Socket ABI 常量 (level + 选项 + 组播成员): 单一权威源 `edgine-abi`,
// Linux x86_64 asm-generic 值域, 与内核 `privileged::net::init::sm_fi`
// 引用同源. 历史漂移 `SO_KEEPALIVE`=8 / `SO_BROADCAST`=32 随单源引用修正为
// 9/6 (plan socket-option-constants-unification 方案 A, 2026-10-19 裁定);
// 内核侧 SO_BROADCAST 为"接受但忽略", SO_KEEPALIVE 已实装 (7200s 缺省).
pub use edgine_abi::socket::{
    IP_ADD_MEMBERSHIP, IP_DROP_MEMBERSHIP, IPPROTO_IP, IPPROTO_IPV6, IPPROTO_TCP, IPPROTO_UDP,
    IPV6_ADD_MEMBERSHIP, IPV6_DROP_MEMBERSHIP, SO_BROADCAST, SO_ERROR, SO_KEEPALIVE, SO_PASSCRED,
    SO_RCVTIMEO, SO_REUSEADDR, SO_REUSEPORT, SO_SNDTIMEO, SO_TYPE, SOL_SOCKET, TCP_NODELAY,
};

// D8b: poll 事件位 (与内核 `sm_fi.rs` D8b 同值域; poll 族不属 sockopt, 不在
// 本轮 abi 统一范围, 见 plan S1 范围界定).
pub const POLLIN: i16 = 1;
pub const POLLOUT: i16 = 4;

// 标志 (Linux x86_64 / generic ABI 值域, 与内核侧 `MSG_DONTWAIT` /
// `MSG_DONTWAIT_FLAG` 逐字一致 —— 由 host-tests 的 ABI 契约测试锁位).
// `MSG_DONTWAIT` = 0x40 (曾误取 0x80, 会让用户态传该标志而内核视为未知位).
pub const MSG_DONTWAIT: i32 = 0x40;
pub const MSG_PEEK: i32 = 0x02;

// shutdown 方式
pub const SHUT_RD: i32 = 0;
pub const SHUT_WR: i32 = 1;
pub const SHUT_RDWR: i32 = 2;

// ============================================================
// 网络结构体 (POSIX socket ABI)
// ============================================================

// 双栈 (DECISION-032): 用户态 sockaddr 采用 Linux 布局 —— `sa_family_t` 为
// 主机序 (NE) u16, 与内核 `parse_endpoint_trait` 的 `read_unaligned::<u16>`
// 读取契约及 `raw_read_sockaddr_in` 的 `from_ne_bytes` 一致; 端口保持网络序 (BE).
#[repr(C)]
pub struct InAddr {
    pub s_addr: [u8; 4],
}

#[repr(C)]
pub struct SockaddrIn {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: InAddr,
    pub sin_zero: [u8; 8],
}

/// `struct ip_mreq` (IPv4 组播成员选项参数, 8 字节): 组地址 + 本地接口地址,
/// 两者均为网络序; `imr_interface` 全零表示"由内核选接口".
///
/// 本内核为单网络接口, 该字段当前被忽略 (见内核 `sm_setsockopt` 的 SIMPLIFIED 注).
#[repr(C)]
pub struct IpMreq {
    pub imr_multiaddr: InAddr,
    pub imr_interface: InAddr,
}

#[repr(C)]
pub struct In6Addr {
    pub s6_addr: [u8; 16],
}

#[repr(C)]
pub struct SockaddrIn6 {
    pub sin6_family: u16,
    pub sin6_port: u16,
    pub sin6_flowinfo: u32,
    pub sin6_addr: In6Addr,
    pub sin6_scope_id: u32,
}

/// `struct ipv6_mreq` (IPv6 组播成员选项参数, 20 字节): 组地址 + 接口索引,
/// 地址为网络序; `ipv6mr_interface` 为 0 表示"由内核选接口".
///
/// 本内核为单网络接口, 该字段当前被忽略 (见内核 `sm_setsockopt` 的 SIMPLIFIED 注).
#[repr(C)]
pub struct Ipv6Mreq {
    pub ipv6mr_multiaddr: In6Addr,
    pub ipv6mr_interface: u32,
}

#[repr(C)]
pub struct Iovec {
    pub iov_base: *mut u8,
    pub iov_len: usize,
}

#[repr(C)]
pub struct Msghdr {
    pub msg_name: *mut u8,
    pub msg_namelen: u32,
    pub msg_iov: *mut Iovec,
    pub msg_iovlen: usize,
    pub msg_control: *mut u8,
    pub msg_controllen: usize,
    pub msg_flags: i32,
}

#[repr(C)]
pub struct RUsage {
    pub ru_utime_sec: i64,
    pub ru_utime_usec: i64,
    pub ru_stime_sec: i64,
    pub ru_stime_usec: i64,
}

// ============================================================
// 网络 syscall wrapper
// ============================================================

#[must_use]
pub fn socket(domain: i32, sock_type: i32, protocol: i32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_SOCKET,
            arg_bits(i64::from(domain)),
            arg_bits(i64::from(sock_type)),
            arg_bits(i64::from(protocol)),
        ))
    }
}

#[must_use]
pub fn bind(sockfd: i32, addr: *const u8, addrlen: u32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_BIND,
            arg_bits(i64::from(sockfd)),
            addr as u64,
            u64::from(addrlen),
        ))
    }
}

#[must_use]
pub fn listen(sockfd: i32, backlog: i32) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_LISTEN,
            arg_bits(i64::from(sockfd)),
            arg_bits(i64::from(backlog)),
        ))
    }
}

pub fn accept(sockfd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_ACCEPT,
            arg_bits(i64::from(sockfd)),
            addr as u64,
            addrlen as u64,
        ))
    }
}

#[must_use]
pub fn connect(sockfd: i32, addr: *const u8, addrlen: u32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_CONNECT,
            arg_bits(i64::from(sockfd)),
            addr as u64,
            u64::from(addrlen),
        ))
    }
}

// sendto/recvfrom 为 6 参数 syscall; 显式传入 a5/a6 (含 null) 以规避内核从
// r8/r9 采集残留寄存器值被误当作地址指针的隐患 (见 DECISION-032).
#[must_use]
pub fn sendto(
    sockfd: i32,
    buf: *const u8,
    len: usize,
    flags: i32,
    dest: *const u8,
    destlen: u32,
) -> isize {
    unsafe {
        ret_isize(sys6(
            SYS_SENDTO,
            arg_bits(i64::from(sockfd)),
            buf as u64,
            len as u64,
            arg_bits(i64::from(flags)),
            dest as u64,
            u64::from(destlen),
        ))
    }
}

pub fn recvfrom(
    sockfd: i32,
    buf: *mut u8,
    len: usize,
    flags: i32,
    src: *mut u8,
    srclen: *mut u32,
) -> isize {
    unsafe {
        ret_isize(sys6(
            SYS_RECVFROM,
            arg_bits(i64::from(sockfd)),
            buf as u64,
            len as u64,
            arg_bits(i64::from(flags)),
            src as u64,
            srclen as u64,
        ))
    }
}

#[must_use]
pub fn send(sockfd: i32, buf: *const u8, len: usize, flags: i32) -> isize {
    unsafe {
        ret_isize(sys6(
            SYS_SENDTO,
            arg_bits(i64::from(sockfd)),
            buf as u64,
            len as u64,
            arg_bits(i64::from(flags)),
            0,
            0,
        ))
    }
}

pub fn recv(sockfd: i32, buf: *mut u8, len: usize, flags: i32) -> isize {
    unsafe {
        ret_isize(sys6(
            SYS_RECVFROM,
            arg_bits(i64::from(sockfd)),
            buf as u64,
            len as u64,
            arg_bits(i64::from(flags)),
            0,
            0,
        ))
    }
}

#[must_use]
pub fn sendmsg(sockfd: i32, msg: *const Msghdr, flags: i32) -> isize {
    unsafe {
        ret_isize(sys3(
            SYS_SENDMSG,
            arg_bits(i64::from(sockfd)),
            msg as u64,
            arg_bits(i64::from(flags)),
        ))
    }
}

pub fn recvmsg(sockfd: i32, msg: *mut Msghdr, flags: i32) -> isize {
    unsafe {
        ret_isize(sys3(
            SYS_RECVMSG,
            arg_bits(i64::from(sockfd)),
            msg as u64,
            arg_bits(i64::from(flags)),
        ))
    }
}

#[must_use]
pub fn setsockopt(sockfd: i32, level: i32, optname: i32, optval: *const u8, optlen: u32) -> i32 {
    unsafe {
        ret_i32(sys5(
            SYS_SETSOCKOPT,
            arg_bits(i64::from(sockfd)),
            arg_bits(i64::from(level)),
            arg_bits(i64::from(optname)),
            optval as u64,
            u64::from(optlen),
        ))
    }
}

pub fn getsockopt(sockfd: i32, level: i32, optname: i32, optval: *mut u8, optlen: *mut u32) -> i32 {
    unsafe {
        ret_i32(sys5(
            SYS_GETSOCKOPT,
            arg_bits(i64::from(sockfd)),
            arg_bits(i64::from(level)),
            arg_bits(i64::from(optname)),
            optval as u64,
            optlen as u64,
        ))
    }
}

pub fn getsockname(sockfd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_GETSOCKNAME,
            arg_bits(i64::from(sockfd)),
            addr as u64,
            addrlen as u64,
        ))
    }
}

pub fn getpeername(sockfd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_GETPEERNAME,
            arg_bits(i64::from(sockfd)),
            addr as u64,
            addrlen as u64,
        ))
    }
}

/// `shutdown(sockfd, how)` — D7 半关语义透传 (`SHUT_RD`/`SHUT_WR`/`SHUT_RDWR`).
#[must_use]
pub fn shutdown(sockfd: i32, how: i32) -> i32 {
    unsafe {
        ret_i32(sys2(
            SYS_SHUTDOWN,
            arg_bits(i64::from(sockfd)),
            arg_bits(i64::from(how)),
        ))
    }
}

#[must_use]
pub fn close_socket(sockfd: i32) -> i32 {
    unsafe { ret_i32(sys1(SYS_CLOSE, arg_bits(i64::from(sockfd)))) }
}

#[must_use]
pub fn ioctl(fd: i32, request: u64, arg: u64) -> i32 {
    unsafe { ret_i32(sys3(SYS_IOCTL, arg_bits(i64::from(fd)), request, arg)) }
}

// ============================================================
// fcntl — 文件描述符状态标志 (D10: socket fd 的 O_NONBLOCK 经内核落到 per-slot 阻塞标志)
// ============================================================

/// `fcntl` 命令: `F_SETFL` (设置 status flag, Linux/x86 值 4).
pub const F_SETFL: i32 = 4;
/// `O_NONBLOCK` (Linux/x86 `0o4000`): socket fd 经此位请求非阻塞.
pub const O_NONBLOCK: i32 = 0o4000;

/// `fcntl(fd, cmd, arg)` — 对 socket fd `F_SETFL | O_NONBLOCK` 使其 recv/recvfrom
/// 立即返回 `-EAGAIN` (非阻塞轮询语义), 供有界重试探针使用.
#[must_use]
pub fn fcntl(fd: i32, cmd: i32, arg: u64) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_FCNTL,
            arg_bits(i64::from(fd)),
            arg_bits(i64::from(cmd)),
            arg,
        ))
    }
}

// ============================================================
// poll 多路复用 (D8b 接线: 用户态 poll 经内核 Smoltcp 路由取真实 socket revents)
// ============================================================

pub const SYS_POLL: u64 = 7;

/// `struct pollfd` (Linux ABI): fd + 请求事件 + 返回事件, 定长 8 字节.
#[repr(C)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// `poll(fds, nfds, timeout)` — 内核当前为单次扫描非阻塞语义 (timeout 不生效,
/// 归 P5 阻塞睡眠), 就绪计数即时返回.
pub fn poll(fds: &mut [PollFd], timeout: i32) -> i32 {
    unsafe {
        ret_i32(sys3(
            SYS_POLL,
            fds.as_mut_ptr() as u64,
            fds.len() as u64,
            arg_bits(i64::from(timeout)),
        ))
    }
}
