#![allow(non_upper_case_globals)]
//! Edgine 用户态运行时 — POSIX 原生系统调用层
//! x86_64: syscall, rax=num, rdi=a1, rsi=a2, rdx=a3, r10=a4, r8=a5, rax=ret
//! aarch64: svc #0, x0=num, x1-x4=args, x0=返回
use core::arch::asm;

// ============================================================
// POSIX syscall 编号 — 与内核 syscall/types.rs 对应
// ============================================================

// 文件 I/O
pub const SYS_read: u64 = 0;
pub const SYS_write: u64 = 1;
pub const SYS_open: u64 = 2;
pub const SYS_close: u64 = 3;
pub const SYS_ioctl: u64 = 16;
pub const SYS_stat: u64 = 4;
pub const SYS_fstat: u64 = 5;
pub const SYS_lseek: u64 = 8;
pub const SYS_access: u64 = 21;
pub const SYS_pipe: u64 = 22;
pub const SYS_dup: u64 = 32;
pub const SYS_dup2: u64 = 33;
pub const SYS_getpid: u64 = 39;
pub const SYS_fork: u64 = 57;
pub const SYS_execve: u64 = 59;
pub const SYS_exit: u64 = 60;
pub const SYS_wait4: u64 = 61;
pub const SYS_uname: u64 = 63;
pub const SYS_getdents: u64 = 78;
pub const SYS_getcwd: u64 = 79;
pub const SYS_chdir: u64 = 80;
pub const SYS_rename: u64 = 82;
pub const SYS_mkdir: u64 = 83;
pub const SYS_rmdir: u64 = 84;
pub const SYS_unlink: u64 = 87;
pub const SYS_getuid: u64 = 102;
pub const SYS_getgid: u64 = 104;
pub const SYS_geteuid: u64 = 107;
pub const SYS_getegid: u64 = 108;
pub const SYS_getppid: u64 = 110;
pub const SYS_sync: u64 = 162;
pub const SYS_mount: u64 = 165;

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

#[repr(C)] pub struct UserDirEntry { pub node: u32, pub file_type: u8, pub name: [u8; 256] }
#[repr(C)] pub struct UserDiskInfo { pub disk_id: u32, pub present: u32, pub total_sectors: u32, pub sectors: u32, pub model: [u8; 64] }

// ============================================================
// 系统调用门 — 双架构
// ============================================================

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
    unsafe { asm!("syscall", in("rax") num, in("rdi") a1, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys2(num: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe { asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys3(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe { asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe { asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe { asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    // SAFETY: syscall 指令通过 C ABI 与内核互操作，调用方保证参数有效性
    unsafe { asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5, in("r9") a6, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}

#[cfg(target_arch = "aarch64")]
unsafe fn sys0(num: u64) -> i64 { let ret: i64; unsafe { asm!("svc #0", in("x0") num, lateout("x0") ret) }; ret }
#[cfg(target_arch = "aarch64")]
unsafe fn sys1(num: u64, a1: u64) -> i64 { let ret: i64; unsafe { asm!("svc #0", in("x0") num, in("x1") a1, lateout("x0") ret) }; ret }
#[cfg(target_arch = "aarch64")]
unsafe fn sys2(num: u64, a1: u64, a2: u64) -> i64 { let ret: i64; unsafe { asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, lateout("x0") ret) }; ret }
#[cfg(target_arch = "aarch64")]
unsafe fn sys3(num: u64, a1: u64, a2: u64, a3: u64) -> i64 { let ret: i64; unsafe { asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, lateout("x0") ret) }; ret }
#[cfg(target_arch = "aarch64")]
unsafe fn sys4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 { let ret: i64; unsafe { asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, lateout("x0") ret) }; ret }
#[cfg(target_arch = "aarch64")]
unsafe fn sys5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 { let ret: i64; unsafe { asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, lateout("x0") ret) }; ret }
#[cfg(target_arch = "aarch64")]
unsafe fn sys6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 { let ret: i64; unsafe { asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, in("x6") a6, lateout("x0") ret) }; ret }

// ============================================================
// POSIX 标准 syscall wrapper
// ============================================================

pub fn read(fd: i32, buf: &mut [u8]) -> i32               { unsafe { sys3(SYS_read, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as i32 } }
pub fn write(fd: i32, buf: &[u8]) -> i32                  { unsafe { sys3(SYS_write, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as i32 } }
pub fn open(path: &[u8], flags: i32, mode: i32) -> i32   { unsafe { sys3(SYS_open, path.as_ptr() as u64, flags as u64, mode as u64) as i32 } }
pub fn close(fd: i32) -> i32                              { unsafe { sys1(SYS_close, fd as u64) as i32 } }
pub fn getpid() -> u64                                    { unsafe { sys0(SYS_getpid) as u64 } }
pub fn fork() -> u64                                      { unsafe { sys0(SYS_fork) as u64 } }
pub fn getuid() -> u32                                    { unsafe { sys0(SYS_getuid) as u32 } }
pub fn geteuid() -> u32                                   { unsafe { sys0(SYS_geteuid) as u32 } }
pub fn sched_yield()                                      { unsafe { sys0(24); } }
pub fn sync()                                             { unsafe { sys0(SYS_sync); } }

// ============================================================
// Edgine 兼容 wrapper — 保留原有语义，内部映射到 POSIX/PWM 编号
// ============================================================

pub fn proc_exec(path: &[u8], argv: &[*const u8]) -> i64   { unsafe { sys3(SYS_execve, path.as_ptr() as u64, argv.as_ptr() as u64, 0) } }
pub fn proc_exit(code: i32) -> ! {
    unsafe { sys1(SYS_exit, code as u64); }
    loop {
        if cfg!(target_arch = "x86_64") { unsafe { asm!("hlt", options(nomem, nostack)); } }
        else { unsafe { asm!("wfi", options(nomem, nostack)); } }
    }
}
pub fn proc_get_pwm() -> u64                               { unsafe { sys0(SYS_SGEG_GET_PWM) as u64 } }
pub fn proc_yield()                                        { unsafe { sys0(24); } }

pub fn pipe_create(fds: &mut [i32; 2]) -> i32              { unsafe { sys1(SYS_pipe, fds.as_mut_ptr() as u64) as i32 } }
pub fn dup_fd(oldfd: i32) -> i32                           { unsafe { sys1(SYS_dup, oldfd as u64) as i32 } }
pub fn dup2_fd(oldfd: i32, newfd: i32) -> i32              { unsafe { sys2(SYS_dup2, oldfd as u64, newfd as u64) as i32 } }
pub fn wait_pid(pid: i32) -> i64                           { unsafe { sys3(SYS_wait4, pid as u64, 0, 0) } }

pub fn fs_open(path: &[u8], flags: i32, m: i32) -> i32    { unsafe { sys3(SYS_open, path.as_ptr() as u64, flags as u64, m as u64) as i32 } }
pub fn fs_close(fd: i32) -> i32                            { unsafe { sys1(SYS_close, fd as u64) as i32 } }
pub fn fs_read(fd: i32, buf: &mut [u8]) -> i32             { unsafe { sys3(SYS_read, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as i32 } }
pub fn fs_write(fd: i32, buf: &[u8]) -> i32                { unsafe { sys3(SYS_write, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as i32 } }

pub fn fs_mkdir(path: &[u8]) -> i32                        { unsafe { sys2(SYS_mkdir, path.as_ptr() as u64, 0o755) as i32 } }
pub fn fs_rmdir(path: &[u8]) -> i32                        { unsafe { sys1(SYS_rmdir, path.as_ptr() as u64) as i32 } }
pub fn fs_unlink(path: &[u8]) -> i32                       { unsafe { sys1(SYS_unlink, path.as_ptr() as u64) as i32 } }
pub fn fs_rename(old: &[u8], new: &[u8]) -> i32            { unsafe { sys2(SYS_rename, old.as_ptr() as u64, new.as_ptr() as u64) as i32 } }
pub fn fs_readdir(fd: i32, entry: &mut UserDirEntry) -> i32 { unsafe { sys2(SYS_getdents, fd as u64, entry as *mut UserDirEntry as u64) as i32 } }
pub fn fs_sync()                                           { unsafe { sys0(SYS_sync); } }

pub fn fs_mount(src: &[u8], tgt: &[u8], typ: &[u8], opt: &[u8]) -> i32 {
    unsafe { sys4(SYS_mount, src.as_ptr() as u64, tgt.as_ptr() as u64, typ.as_ptr() as u64, opt.as_ptr() as u64) as i32 }
}
pub fn fs_unmount(t: &[u8]) -> i32 { unsafe { sys1(166, t.as_ptr() as u64) as i32 } }

pub fn auth_login(n: &[u8], p: &[u8]) -> i64               { unsafe { sys2(SYS_SGEG_LOGIN, n.as_ptr() as u64, p.as_ptr() as u64) } }
pub fn auth_logout()                                       { unsafe { sys0(SYS_SGEG_LOGOUT); } }
pub fn auth_create_first(pw: &[u8]) -> i32                 { unsafe { sys1(SYS_SGEG_CREATE_FIRST, pw.as_ptr() as u64) as i32 } }
pub fn auth_change_password(o: &[u8], n: &[u8]) -> i32     { unsafe { sys2(SYS_SGEG_CHANGE_PASSWORD, o.as_ptr() as u64, n.as_ptr() as u64) as i32 } }

pub fn env_getcwd(buf: &mut [u8]) -> i32                   { unsafe { sys2(SYS_getcwd, buf.as_mut_ptr() as u64, buf.len() as u64) as i32 } }
pub fn env_chdir(path: &[u8]) -> i32                       { unsafe { sys1(SYS_chdir, path.as_ptr() as u64) as i32 } }

pub fn gethostname(buf: &mut [u8]) -> i32                  { unsafe { sys2(SYS_SGEG_GETHOSTNAME, buf.as_mut_ptr() as u64, buf.len() as u64) as i32 } }
pub fn sethostname(name: &[u8]) -> i32                     { unsafe { sys2(SYS_SGEG_SETHOSTNAME, name.as_ptr() as u64, name.len() as u64) as i32 } }
pub fn reboot(cmd: i32) -> i64                             { unsafe { sys1(SYS_SGEG_REBOOT, cmd as u64) } }
pub fn proc_list(buf: &mut [u8], max_entries: u32) -> i32  { unsafe { sys2(SYS_SGEG_PROC_LIST, buf.as_mut_ptr() as u64, max_entries as u64) as i32 } }

pub fn disk_list(disks: &mut [u64]) -> i32                 { unsafe { sys2(SYS_SGEG_DISK_LIST, disks.as_mut_ptr() as u64, disks.len() as u64) as i32 } }
pub fn disk_info(id: u32, info: &mut UserDiskInfo) -> i32  { unsafe { sys2(SYS_SGEG_DISK_INFO, id as u64, info as *mut UserDiskInfo as u64) as i32 } }
pub fn disk_format(id: u32) -> i32                         { unsafe { sys2(SYS_SGEG_DISK_FORMAT, id as u64, c"unkfs".as_ptr() as u64) as i32 } }
pub fn disk_partition(id: u32, sectors: u64) -> i64        { unsafe { sys2(SYS_SGEG_DISK_PARTITION, id as u64, sectors) } }
pub fn boot_install(id: u32) -> i64                        { unsafe { sys1(SYS_SGEG_DISK_INSTALL, id as u64) } }
pub fn fat_format(id: u32) -> i64                          { unsafe { sys1(SYS_SGEG_FAT_FORMAT, id as u64) } }
// T2 批 5 (syscall-followup): 补 wrapper — 原仅常量无 wrapper 无调用 (T3 登记)
pub fn hotplug_status(buf: &mut [u8]) -> i64               { unsafe { sys2(SYS_SGEG_HOTPLUG_STATUS, buf.as_mut_ptr() as u64, buf.len() as u64) } }

// ============================================================
// 新增 POSIX syscall wrapper
// ============================================================

pub const SYS_truncate: u64 = 76;
pub const SYS_ftruncate: u64 = 77;
pub const SYS_fsync: u64 = 170;
pub const SYS_getpgid: u64 = 111;
pub const SYS_setsid: u64 = 112;
pub const SYS_gettid: u64 = 186;
pub const SYS_nanosleep: u64 = 35;
pub const SYS_rt_sigaction: u64 = 13;
pub const SYS_rt_sigprocmask: u64 = 14;
pub const SYS_umask: u64 = 95;

#[repr(C)] pub struct Timespec { pub tv_sec: i64, pub tv_nsec: i64 }

pub fn truncate(path: &[u8], length: i64) -> i32           { unsafe { sys2(SYS_truncate, path.as_ptr() as u64, length as u64) as i32 } }
pub fn ftruncate(fd: i32, length: i64) -> i32              { unsafe { sys2(SYS_ftruncate, fd as u64, length as u64) as i32 } }
pub fn fsync(fd: i32) -> i32                               { unsafe { sys1(SYS_fsync, fd as u64) as i32 } }
pub fn getpgid(pid: i32) -> i64                            { unsafe { sys1(SYS_getpgid, pid as u64) } }
pub fn setsid() -> i64                                     { unsafe { sys0(SYS_setsid) } }
pub fn gettid() -> i64                                     { unsafe { sys0(SYS_gettid) } }
pub fn umask(mask: u32) -> u32                             { unsafe { sys1(SYS_umask, mask as u64) as u32 } }
pub fn nanosleep(req: &Timespec) -> i32                    { unsafe { sys2(SYS_nanosleep, req as *const Timespec as u64, 0) as i32 } }
pub fn kill(pid: i32, sig: i32) -> i32                     { unsafe { sys2(62, pid as u64, sig as u64) as i32 } }

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
    pub _pad: [u8; 3],
}

pub fn fb_open(info: &mut FbInfo) -> i64                    { unsafe { sys1(SYS_FB_OPEN, info as *mut FbInfo as u64) } }
pub fn fb_mmap(addr: u64, size: u64, prot: i32) -> i64      { unsafe { sys3(SYS_FB_MMAP, addr, size, prot as u64) } }
pub fn fb_release(addr: u64) -> i64                          { unsafe { sys1(SYS_FB_RELEASE, addr) } }

// ============================================================
// 网络 socket syscall 常量 — 与内核 syscall/types.rs 对应
// ============================================================

pub const SYS_socket: u64 = 41;
pub const SYS_connect: u64 = 42;
pub const SYS_accept: u64 = 43;
pub const SYS_sendto: u64 = 44;
pub const SYS_recvfrom: u64 = 45;
pub const SYS_sendmsg: u64 = 46;
pub const SYS_recvmsg: u64 = 47;
pub const SYS_shutdown: u64 = 48;
pub const SYS_bind: u64 = 49;
pub const SYS_listen: u64 = 50;
pub const SYS_getsockname: u64 = 51;
pub const SYS_getpeername: u64 = 52;
pub const SYS_setsockopt: u64 = 54;
pub const SYS_getsockopt: u64 = 55;
pub const SYS_getrusage: u64 = 98;

// 地址族
pub const AF_INET: i32 = 2;
pub const AF_INET6: i32 = 10;

// socket 类型
pub const SOCK_STREAM: i32 = 1;
pub const SOCK_DGRAM: i32 = 2;
pub const SOCK_RAW: i32 = 3;

// 协议
pub const IPPROTO_TCP: i32 = 6;
pub const IPPROTO_UDP: i32 = 17;
pub const IPPROTO_RAW: i32 = 255;

// socket 选项级别
pub const SOL_SOCKET: i32 = 1;
pub const IPPROTO_IP: i32 = 0;

// socket 选项
pub const SO_REUSEADDR: i32 = 2;
pub const SO_KEEPALIVE: i32 = 8;
pub const SO_BROADCAST: i32 = 32;

// 标志
pub const MSG_DONTWAIT: i32 = 0x80;
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

pub fn socket(domain: i32, sock_type: i32, protocol: i32) -> i32 {
    unsafe { sys3(SYS_socket, domain as u64, sock_type as u64, protocol as u64) as i32 }
}

pub fn bind(sockfd: i32, addr: *const u8, addrlen: u32) -> i32 {
    unsafe { sys3(SYS_bind, sockfd as u64, addr as u64, addrlen as u64) as i32 }
}

pub fn listen(sockfd: i32, backlog: i32) -> i32 {
    unsafe { sys2(SYS_listen, sockfd as u64, backlog as u64) as i32 }
}

pub fn accept(sockfd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe { sys3(SYS_accept, sockfd as u64, addr as u64, addrlen as u64) as i32 }
}

pub fn connect(sockfd: i32, addr: *const u8, addrlen: u32) -> i32 {
    unsafe { sys3(SYS_connect, sockfd as u64, addr as u64, addrlen as u64) as i32 }
}

// sendto/recvfrom 为 6 参数 syscall; 显式传入 a5/a6 (含 null) 以规避内核从
// r8/r9 采集残留寄存器值被误当作地址指针的隐患 (见 DECISION-032).
pub fn sendto(sockfd: i32, buf: *const u8, len: usize, flags: i32, dest: *const u8, destlen: u32) -> isize {
    unsafe { sys6(SYS_sendto, sockfd as u64, buf as u64, len as u64, flags as u64, dest as u64, destlen as u64) as isize }
}

pub fn recvfrom(sockfd: i32, buf: *mut u8, len: usize, flags: i32, src: *mut u8, srclen: *mut u32) -> isize {
    unsafe { sys6(SYS_recvfrom, sockfd as u64, buf as u64, len as u64, flags as u64, src as u64, srclen as u64) as isize }
}

pub fn send(sockfd: i32, buf: *const u8, len: usize, flags: i32) -> isize {
    unsafe { sys6(SYS_sendto, sockfd as u64, buf as u64, len as u64, flags as u64, 0, 0) as isize }
}

pub fn recv(sockfd: i32, buf: *mut u8, len: usize, flags: i32) -> isize {
    unsafe { sys6(SYS_recvfrom, sockfd as u64, buf as u64, len as u64, flags as u64, 0, 0) as isize }
}

pub fn sendmsg(sockfd: i32, msg: *const Msghdr, flags: i32) -> isize {
    unsafe { sys3(SYS_sendmsg, sockfd as u64, msg as u64, flags as u64) as isize }
}

pub fn recvmsg(sockfd: i32, msg: *mut Msghdr, flags: i32) -> isize {
    unsafe { sys3(SYS_recvmsg, sockfd as u64, msg as u64, flags as u64) as isize }
}

pub fn setsockopt(sockfd: i32, level: i32, optname: i32, optval: *const u8, optlen: u32) -> i32 {
    unsafe { sys5(SYS_setsockopt, sockfd as u64, level as u64, optname as u64, optval as u64, optlen as u64) as i32 }
}

pub fn getsockopt(sockfd: i32, level: i32, optname: i32, optval: *mut u8, optlen: *mut u32) -> i32 {
    unsafe { sys5(SYS_getsockopt, sockfd as u64, level as u64, optname as u64, optval as u64, optlen as u64) as i32 }
}

pub fn getsockname(sockfd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe { sys3(SYS_getsockname, sockfd as u64, addr as u64, addrlen as u64) as i32 }
}

pub fn close_socket(sockfd: i32) -> i32 {
    unsafe { sys1(SYS_close, sockfd as u64) as i32 }
}

pub fn ioctl(fd: i32, request: u64, arg: u64) -> i32 {
    unsafe { sys3(SYS_ioctl, fd as u64, request, arg) as i32 }
}
