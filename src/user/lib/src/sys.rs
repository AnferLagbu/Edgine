//! Edgine 用户态运行时 — POSIX 原生系统调用层
//! x86_64: syscall, rax=num, rdi=a1, rsi=a2, rdx=a3, r10=a4, r8=a5, r9=a6, rax=ret
//! aarch64: svc #0, x0=num, x1-x6=args, x0=返回
//!
//! ## unsafe 依据单源 (逐处 `// SAFETY:` 引用本节, 不重复抄写)
//!
//! 本文件的 `unsafe` 只做一件事: 经 `sys0..sys6` 门陷入内核. 依据分两层.
//!
//! **门 G — 寄存器与 clobber 契约** (门自身的 `unsafe` 块):
//! 门只搬运寄存器值, 不访存. x86_64 的 `syscall` 由硬件破坏 `rcx` (用户 RIP)
//! 与 `r11` (用户 RFLAGS), 二者以 `out` 声明, 编译器不会把存活值放这两格;
//! 内核入口 `privileged/boot/isr.asm` 的 `syscall_entry` 压入全部 15 个通用
//! 寄存器, 返回前逐一弹回再 `iretq`, 故其余用户寄存器逐位保全. aarch64 的
//! `svc #0` 不破坏通用寄存器, 内核 `arch/aarch64/exception.rs` 的 `svc_handler`
//! 取帧 `x0` 为编号、`x1..x6` 为六个参数, 结果写回帧 `x0`; 陷入帧在入口全量
//! 保存 `x0..x30`、返回 EL0 前全量弹回, 故门只声明 `x0` 为 `lateout` 即与内核
//! 保全范围一致 (`proc/proc_ops.rs` 与 `arch/aarch64/context.rs` 的注释据此把
//! `x8..x18` 全量快照, 供 fork/clone 子进程首次进入 EL0 时恢复).
//! 参数值本身的有效性不由门承担, 见下.
//!
//! **包装层 — 按入参形态分五类** (调用门的那个 `unsafe` 块):
//! - `S` 纯标量: 无指针跨边界 (fd / pid / 编号 / 标志 / 地址数值), 内核只操作
//!   自己的对象, 用户内存不被触碰.
//! - `B` 借用缓冲: 指针与长度取自同一 `&[u8]` / `&mut [u8]` 活借用, 故
//!   `[ptr, ptr+len)` 必在本进程可访问区间内, 且借用到 syscall 返回前一直存活
//!   (内核同步完成搬运); 内核侧再经 `copy_from_user` / `copy_to_user` /
//!   `validate_user_buf` 等入口校验区间落在用户半区后才访问, 越界返 `-EFAULT`
//!   而非 panic (逐处注释写明该 syscall 实际走哪个入口).
//! - `P` 路径串: 指针取自 `&[u8]` 或 `c"..."` 字面量, NUL 结尾由调用方契约保证
//!   (各包装 doc 已写明); 内核 `glue::cstr::as_kstr` 以 4096 字节为上限扫描
//!   NUL, 不会无界读, 空指针降级为空串.
//! - `T` 结构体: `#[repr(C)]` 定长结构 (与内核同名布局同源, 多取自
//!   `edgine-abi`), 指针由 `core::ptr::from_ref` / `from_mut` 从活借用取得,
//!   大小与对齐由类型系统保证; 内核经 `write_struct_to_user` /
//!   `write_u64_to_user` 先校验用户区间再写, 或 `check_user_ptr` + 只读访问.
//! - `R` 裸指针: 包装层收 `*const` / `*mut` 或裸地址值而非借用 (调用方来自
//!   无切片的入口栈或 C 风格 ABI), 指向内存的有效性 / 容量 / 生命周期由
//!   **调用方契约**承担, 逐处在 SAFETY 注释里写明该契约; 本层不解引用,
//!   内核侧按长度校验用户区间后才访问.
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
pub const SYS_PIVOT_ROOT: u64 = 155;
pub const SYS_CHROOT: u64 = 161;
pub const SYS_SYNC: u64 = 162;
pub const SYS_MOUNT: u64 = 165;
pub const SYS_TIMERFD_CREATE: u64 = 283;
pub const SYS_TIMERFD_SETTIME: u64 = 286;

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
/// SGEG 私有: 导出 EEVDF 运行期观测快照 (K-07b 批3, 单一权威源 `edgine-abi`)
pub const SYS_SGEG_SCHED_SNAPSHOT: u64 = edgine_abi::sched::SYS_SGEG_SCHED_SNAPSHOT;

/// reboot 命令码 (单一权威源 `edgine-abi`): 0 = 重启, 1 = 停机.
///
/// 内核 `privileged::syscall::api::reboot_mechanism` 按同一组常量分派;
/// 两侧共用可避免"命令语义各写裸数字"的漂移 (R3 修的正是该形态的写反).
pub use edgine_abi::reboot::{REBOOT_CMD_HALT, REBOOT_CMD_RESTART};

// POSIX open flags
pub const O_RDONLY: i32 = 0;
pub const O_WRONLY: i32 = 1;
pub const O_RDWR: i32 = 2;
pub const O_CREAT: i32 = 0o100;
pub const O_TRUNC: i32 = 0o1000;
pub const TFD_NONBLOCK: i32 = 0o4000;
pub const TFD_CLOEXEC: i32 = 0o200_0000;
pub const TFD_TIMER_ABSTIME: i32 = 1;
/// `EAGAIN` (Linux 11): 非阻塞读无数据.
pub const EAGAIN: i32 = 11;

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
    // SAFETY: 门 G (见模块级"unsafe 依据单源"): 仅 rax 载编号, 无参数寄存器,
    // 门本身不访存; 编号是标量, 无指针有效性问题.
    unsafe { asm!("syscall", in("rax") num, lateout("rax") ret, out("rcx") _, out("r11") _) };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys1(num: u64, a1: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: rax=编号, rdi=a1, 与内核取参寄存器一致; 门只搬运值不访存,
    // a1 若为指针则其有效性由调用它的包装层承担 (S/B/P/T/R 分类见模块级说明).
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys2(num: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: rax=编号, rdi/rsi=a1/a2; 门只搬运值不访存, 指针参数的
    // 有效性由包装层按分类承担 (见模块级说明).
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys3(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: rax=编号, rdi/rsi/rdx=a1/a2/a3; 门只搬运值不访存, 指针
    // 参数的有效性由包装层按分类承担 (见模块级说明).
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: rax=编号, rdi/rsi/rdx/r10=a1..a4 (第 4 参走 r10 而非 rcx,
    // 因 rcx 被 syscall 硬件破坏); 门只搬运值不访存, 指针参数有效性由包装层承担.
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: rax=编号, rdi/rsi/rdx/r10/r8=a1..a5; 门只搬运值不访存,
    // 指针参数的有效性由包装层按分类承担 (见模块级说明).
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}
#[cfg(target_arch = "x86_64")]
unsafe fn sys6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: rax=编号, rdi/rsi/rdx/r10/r8/r9=a1..a6 (六参全显式布置,
    // 不留残留寄存器被内核误采); 门只搬运值不访存, 指针参数有效性由包装层承担.
    unsafe {
        asm!("syscall", in("rax") num, in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4, in("r8") a5, in("r9") a6, lateout("rax") ret, out("rcx") _, out("r11") _);
    };
    ret
}

#[cfg(target_arch = "aarch64")]
unsafe fn sys0(num: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G (见模块级"unsafe 依据单源"): 仅 x0 载编号, 无参数寄存器;
    // svc #0 不破坏通用寄存器, 内核只把结果写回帧 x0, 故 lateout("x0") 已足.
    unsafe { asm!("svc #0", in("x0") num, lateout("x0") ret) };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys1(num: u64, a1: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: x0=编号, x1=a1, 与内核取帧 x1 为 arg0 一致; 门只搬运值
    // 不访存, a1 若为指针则其有效性由包装层承担 (分类见模块级说明).
    unsafe { asm!("svc #0", in("x0") num, in("x1") a1, lateout("x0") ret) };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys2(num: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: x0=编号, x1/x2=a1/a2; 门只搬运值不访存, 指针参数的有效性
    // 由包装层按分类承担 (见模块级说明).
    unsafe { asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, lateout("x0") ret) };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys3(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: x0=编号, x1/x2/x3=a1/a2/a3; 门只搬运值不访存, 指针参数的
    // 有效性由包装层按分类承担 (见模块级说明).
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, lateout("x0") ret);
    };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys4(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: x0=编号, x1..x4=a1..a4; 门只搬运值不访存, 指针参数的
    // 有效性由包装层按分类承担 (见模块级说明).
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, lateout("x0") ret);
    };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys5(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: x0=编号, x1..x5=a1..a5 (内核取帧 x5 为 arg4); 门只搬运值
    // 不访存, 指针参数的有效性由包装层按分类承担 (见模块级说明).
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, lateout("x0") ret);
    };
    ret
}
#[cfg(target_arch = "aarch64")]
unsafe fn sys6(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let ret: i64;
    // SAFETY: 门 G: x0=编号, x1..x6=a1..a6 (内核取帧 x6 为 arg5, 六参全显式
    // 布置); 门只搬运值不访存, 指针参数的有效性由包装层承担 (见模块级说明).
    unsafe {
        asm!("svc #0", in("x0") num, in("x1") a1, in("x2") a2, in("x3") a3, in("x4") a4, in("x5") a5, in("x6") a6, lateout("x0") ret);
    };
    ret
}

// ============================================================
// POSIX 标准 syscall wrapper
// ============================================================

pub fn read(fd: i32, buf: &mut [u8]) -> i32 {
    // SAFETY: B 类 — 指针与长度同取自 buf 这一个活借用, 内核 copy_to_user 校验
    // 用户区间后同步写入, 返回前借用一直存活, 不会写到借用之外.
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
    // SAFETY: B 类 — ptr/len 同取自 buf 的只读活借用, 内核 copy_from_user 只读
    // 该区间; 借用存活至 syscall 返回, 内核不会写用户内存.
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
    // SAFETY: P 类 — path 指针取自活借用, NUL 结尾由调用方保证 (内核 as_kstr
    // 以 4096 字节为上限扫描, 不会无界读); flags/mode 是 S 类标量.
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
    // SAFETY: S 类 — 只有 fd 标量跨边界, 内核只操作自己的 fd 表, 不触碰用户内存.
    unsafe { ret_i32(sys1(SYS_CLOSE, arg_bits(i64::from(fd)))) }
}
#[must_use]
pub fn getpid() -> u64 {
    // SAFETY: S 类 — 无参数, 内核只读当前进程控制块, 不写用户内存.
    unsafe { sys0(SYS_GETPID).cast_unsigned() }
}
#[must_use]
pub fn fork() -> u64 {
    // SAFETY: S 类 — 无指针参数; 子进程地址空间由内核复制父页表建立, 用户侧
    // 无内存被本次陷入改写, 返回值 (父=子 pid / 子=0) 是标量.
    unsafe { sys0(SYS_FORK).cast_unsigned() }
}
#[must_use]
pub fn getuid() -> u32 {
    // SAFETY: S 类 — 无参数, 内核只读当前凭证, 不写用户内存.
    unsafe { ret_u32(sys0(SYS_GETUID)) }
}
#[must_use]
pub fn geteuid() -> u32 {
    // SAFETY: S 类 — 无参数, 内核只读当前有效凭证, 不写用户内存.
    unsafe { ret_u32(sys0(SYS_GETEUID)) }
}
pub fn sched_yield() {
    // SAFETY: S 类 — 无参数 (编号 24 = sched_yield), 内核只重排调度队列.
    unsafe {
        sys0(24);
    }
}
pub fn sync() {
    // SAFETY: S 类 — 无参数, 内核只回写自己的文件系统缓存, 不触碰用户内存.
    unsafe {
        sys0(SYS_SYNC);
    }
}

// ============================================================
// 用户态 TLS 基址 (arch_prctl) — 常量单一权威源 `edgine-abi`
// ============================================================

pub use edgine_abi::arch_prctl::{ARCH_GET_FS, ARCH_SET_FS, SYS_ARCH_PRCTL};

/// `arch_prctl(code, addr)` — 用户态 TLS 基址读写.
///
/// `ARCH_SET_FS` 立即写当前进程的 TLS 基址寄存器 (x86_64 `IA32_FS_BASE` /
/// aarch64 `TPIDR_EL0`); `ARCH_GET_FS` 把当前基址写入 `addr` 指向的 `u64`.
#[must_use]
pub fn arch_prctl(code: u64, addr: u64) -> i64 {
    // SAFETY: R 类 — code 是标量子码; addr 在 ARCH_SET_FS 下只是基址数值 (内核
    // 不访存), 在 ARCH_GET_FS 下须指向调用方持有的可写 u64 (内核 write_u64_to_user
    // 先校验 8 字节用户区间再写), 该契约由调用方承担, 见本函数 doc.
    unsafe { sys2(SYS_ARCH_PRCTL, code, addr) }
}

// ============================================================
// Edgine 兼容 wrapper — 保留原有语义，内部映射到 POSIX/PWM 编号
// ============================================================

/// `execve(path, argv, envp)` — 映像替换; argv/envp 均以 `NULL` 指针结尾.
///
/// `envp` 为空切片时传空指针 (内核视为空环境), 否则须由调用方保证数组以
/// `core::ptr::null()` 终止.
#[must_use]
pub fn proc_exec(path: &[u8], argv: &[*const u8], envp: &[*const u8]) -> i64 {
    // 空切片 as_ptr 返回非空悬垂指针, 内核会按数组解析; 空向量必须显式传 NULL.
    let argv_ptr = if argv.is_empty() {
        0
    } else {
        argv.as_ptr() as u64
    };
    let envp_ptr = if envp.is_empty() {
        0
    } else {
        envp.as_ptr() as u64
    };
    // SAFETY: P + R 类 — path 指针取自活借用 (NUL 结尾由调用方保证, 内核有界
    // 扫描); argv/envp 非空时是 NULL 结尾的指针数组 (doc 契约), 空时上面已显式
    // 传 0 而非空切片的悬垂指针. 成功则映像被替换不返回, 失败返回时借用仍存活.
    unsafe { sys3(SYS_EXECVE, path.as_ptr() as u64, argv_ptr, envp_ptr) }
}
/// `EG_WASM_LOAD(path_ptr, argv_ptr, envp_ptr)` — 加载 wasm 模块到当前进程.
///
/// `path_ptr` 指向 NUL 结尾的路径串; `argv` / `envp` 指向 `NULL` 结尾的
/// 指针数组 (传 `core::ptr::null()` 表示空). 内核只解引用这些用户指针,
/// 故用裸指针而不是切片 (桩拿到的入口栈地址本就没有 Rust 切片)。
#[must_use]
pub fn wasm_load(path: *const u8, argv: *const u64, envp: *const u64) -> i64 {
    // SAFETY: R 类 — 三个裸指针来自桩的入口栈 (无 Rust 切片可用), 本层不解引用;
    // path 须 NUL 结尾、argv/envp 须 NULL 结尾或为空指针, 该契约由调用方按本函数
    // doc 承担; 内核只经用户内存安全代理读取这些地址.
    unsafe { sys3(EG_WASM_LOAD, path as u64, argv as u64, envp as u64) }
}

/// `EG_WASM_RUN(handle)` — 运行已加载实例 (按 gas 上限执行到结束).
#[must_use]
pub fn wasm_run(handle: u64) -> i64 {
    // SAFETY: S 类 — handle 是内核先前返回的实例句柄 (标量), 无指针跨边界.
    unsafe { sys1(EG_WASM_RUN, handle) }
}

/// `EG_WASM_GET_INFO(handle, out_ptr)` — 查询状态 / 退出码 / gas 消耗.
#[must_use]
pub fn wasm_get_info(handle: u64, info: &mut EgWasmInfo) -> i64 {
    // SAFETY: T 类 — EgWasmInfo 是 edgine-abi 单源的 repr(C) 定长结构 (内核回写
    // 同一类型), 指针由 from_mut 从活借用取得; 内核 write_struct_to_user 先校验
    // 该结构大小的用户区间再写, 越界返 -EFAULT.
    unsafe { sys2(EG_WASM_GET_INFO, handle, core::ptr::from_mut(info) as u64) }
}

/// `EG_WASM_UNLOAD(handle)` — 卸载实例.
#[must_use]
pub fn wasm_unload(handle: u64) -> i64 {
    // SAFETY: S 类 — handle 是标量句柄, 内核只操作自己的实例注册表.
    unsafe { sys1(EG_WASM_UNLOAD, handle) }
}

pub fn proc_exit(code: i32) -> ! {
    // SAFETY: S 类 — 只有退出码标量; 内核终止本线程后不返回用户态, 故下面的
    // 兜底循环正常路径不可达.
    unsafe {
        sys1(SYS_EXIT, arg_bits(i64::from(code)));
    }
    loop {
        if cfg!(target_arch = "x86_64") {
            // SAFETY: hlt 不访存也不改通用寄存器, 故 nomem/nostack 成立; 仅在
            // SYS_EXIT 意外返回时到达, 用户态执行 hlt 若触发 #GP 由内核异常路径
            // 终止本进程, 不构成内存不安全.
            unsafe {
                asm!("hlt", options(nomem, nostack));
            }
        } else {
            // SAFETY: wfi 不访存也不改通用寄存器, 故 nomem/nostack 成立; 仅在
            // SYS_EXIT 意外返回时到达. 本内核 SCTLR_EL1 只置 M/C/I 三位 (NTWI=0),
            // 故 EL0 执行 wfi 会陷入 EL1 走 sync_exception_handler (非 SVC 分支),
            // 由内核异常路径处置, 不构成内存不安全.
            unsafe {
                asm!("wfi", options(nomem, nostack));
            }
        }
    }
}
#[must_use]
pub fn proc_get_pwm() -> u64 {
    // SAFETY: S 类 — 无参数, 内核只读当前主体标识, 不写用户内存.
    unsafe { sys0(SYS_SGEG_GET_PWM).cast_unsigned() }
}
pub fn proc_yield() {
    // SAFETY: S 类 — 无参数 (编号 24 = sched_yield), 内核只重排调度队列.
    unsafe {
        sys0(24);
    }
}

pub fn pipe_create(fds: &mut [i32; 2]) -> i32 {
    // SAFETY: T 类 — 指针取自定长数组活借用, 内核恰写 2 个 i32 (读端/写端 fd),
    // 与数组容量一致, 不存在越界面.
    unsafe { ret_i32(sys1(SYS_PIPE, fds.as_mut_ptr() as u64)) }
}
#[must_use]
pub fn dup_fd(oldfd: i32) -> i32 {
    // SAFETY: S 类 — 只有 fd 标量, 内核只操作自己的 fd 表.
    unsafe { ret_i32(sys1(SYS_DUP, arg_bits(i64::from(oldfd)))) }
}
#[must_use]
pub fn dup2_fd(oldfd: i32, newfd: i32) -> i32 {
    // SAFETY: S 类 — 两个 fd 都是标量, 无指针跨边界.
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
    // SAFETY: S 类 — status/options 显式传 0, 内核不写任何用户内存, 只返回标量.
    unsafe { sys3(SYS_WAIT4, arg_bits(i64::from(pid)), 0, 0) }
}

/// `wait4(pid, &status, 0)` — 回收子进程并取回 `wstatus`.
///
/// 返回值语义与 [`wait_pid`] 一致 (成功 = 被回收子进程 pid); `status` 由内核
/// 按 `exit_code << 8` 写入 (无信号位), 判正常退出用 `status == 0`.
#[must_use]
pub fn wait_pid_status(pid: i32, status: &mut i32) -> i64 {
    // SAFETY: T 类 — 指针由 from_mut 从 status 活借用取得, 内核只写一个 i32
    // (exit_code << 8), 与借用对象大小一致; 借用存活至 syscall 返回.
    unsafe {
        sys3(
            SYS_WAIT4,
            arg_bits(i64::from(pid)),
            core::ptr::from_mut(status) as u64,
            0,
        )
    }
}

/// `wait4(pid, &status, WNOHANG)` — 非阻塞回收.
///
/// 子进程仍在运行时返回 0; 调用方自行按墙钟时间决定何时放弃等待.
#[must_use]
pub fn wait_pid_status_nohang(pid: i32, status: &mut i32) -> i64 {
    const WNOHANG: i32 = 1;
    // SAFETY: T 类 — 同 wait_pid_status: from_mut 活借用, 内核只写一个 i32;
    // WNOHANG 是 S 类标量, 子进程未退出时内核不写 status 直接返 0.
    unsafe {
        sys3(
            SYS_WAIT4,
            arg_bits(i64::from(pid)),
            core::ptr::from_mut(status) as u64,
            arg_bits(i64::from(WNOHANG)),
        )
    }
}

#[must_use]
pub fn fs_open(path: &[u8], flags: i32, m: i32) -> i32 {
    // SAFETY: P 类 — path 取自活借用且由调用方保证 NUL 结尾 (内核 4096 上限
    // 扫描); flags/m 是 S 类标量.
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
    // SAFETY: S 类 — 只有 fd 标量, 内核只操作自己的 fd 表.
    unsafe { ret_i32(sys1(SYS_CLOSE, arg_bits(i64::from(fd)))) }
}
pub fn fs_read(fd: i32, buf: &mut [u8]) -> i32 {
    // SAFETY: B 类 — ptr/len 同取自 buf 活借用, 内核 copy_to_user 校验区间后
    // 同步写入, 写入量不超过 buf.len().
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
    // SAFETY: B 类 — ptr/len 同取自 buf 只读活借用, 内核 copy_from_user 只读该
    // 区间, 不写用户内存.
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
    // SAFETY: P 类 — path 取自活借用, NUL 结尾由调用方保证; 模式 0o755 是标量.
    unsafe { ret_i32(sys2(SYS_MKDIR, path.as_ptr() as u64, 0o755)) }
}
#[must_use]
pub fn fs_rmdir(path: &[u8]) -> i32 {
    // SAFETY: P 类 — path 取自活借用且 NUL 结尾由调用方保证, 内核有界扫描.
    unsafe { ret_i32(sys1(SYS_RMDIR, path.as_ptr() as u64)) }
}
#[must_use]
pub fn fs_unlink(path: &[u8]) -> i32 {
    // SAFETY: P 类 — 同 fs_rmdir: 活借用指针 + 调用方 NUL 结尾契约.
    unsafe { ret_i32(sys1(SYS_UNLINK, path.as_ptr() as u64)) }
}
#[must_use]
pub fn fs_rename(old: &[u8], new: &[u8]) -> i32 {
    // SAFETY: P 类 — 两个路径各自取自活借用, NUL 结尾由调用方保证; 两借用在
    // syscall 返回前都存活, 内核只读不写.
    unsafe { ret_i32(sys2(SYS_RENAME, old.as_ptr() as u64, new.as_ptr() as u64)) }
}
pub fn fs_readdir(fd: i32, entry: &mut UserDirEntry) -> i32 {
    // SAFETY: T 类 — UserDirEntry 是 repr(C) 定长结构, 指针由 from_mut 从活借用
    // 取得; 内核经 write_struct_to_user 校验该结构大小的用户区间后写一条目录项.
    unsafe {
        ret_i32(sys2(
            SYS_GETDENTS,
            arg_bits(i64::from(fd)),
            core::ptr::from_mut::<UserDirEntry>(entry) as u64,
        ))
    }
}
pub fn fs_sync() {
    // SAFETY: S 类 — 无参数, 内核只回写自己的文件系统缓存.
    unsafe {
        sys0(SYS_SYNC);
    }
}

#[must_use]
pub fn fs_mount(src: &[u8], tgt: &[u8], typ: &[u8], opt: &[u8]) -> i32 {
    // SAFETY: P 类 — 四个串各自取自活借用, NUL 结尾由调用方保证 (内核 as_kstr
    // 逐个有界扫描); 四借用到 syscall 返回前均存活, 内核只读.
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
    // SAFETY: P 类 — 挂载点串取自活借用, NUL 结尾由调用方保证; 编号 166 =
    // umount2, 其 flags 形参在内核 vfs_umount_internal 中名为 _flags 且未被使用,
    // 故 sys1 不显式传 a1 (rsi 残留值) 不影响卸载语义.
    unsafe { ret_i32(sys1(166, t.as_ptr() as u64)) }
}

#[must_use]
pub fn auth_login(n: &[u8], p: &[u8]) -> i64 {
    // SAFETY: P 类 — 两串各取自一个活借用, NUL 结尾由调用方保证 (eash login
    // 显式补 0); 内核 pwm_login 经 as_kstr 做 4096 字节上限扫描后按值使用,
    // 借用在整个 syscall 期间存活, 不会读到借用之外.
    unsafe { sys2(SYS_SGEG_LOGIN, n.as_ptr() as u64, p.as_ptr() as u64) }
}
pub fn auth_logout() {
    // SAFETY: S 类 — 无参数, 门只搬运编号; 内核 pwm_logout 仅改会话状态,
    // 不触碰任何用户内存.
    unsafe {
        sys0(SYS_SGEG_LOGOUT);
    }
}
#[must_use]
pub fn auth_create_first(pw: &[u8]) -> i32 {
    // SAFETY: P 类 — 口令串取自活借用且由调用方保证 NUL 结尾; 内核
    // pwm_create_first_identity 经 as_kstr 上限扫描读取, 不解引用借用之外.
    unsafe { ret_i32(sys1(SYS_SGEG_CREATE_FIRST, pw.as_ptr() as u64)) }
}
#[must_use]
pub fn auth_change_password(o: &[u8], n: &[u8]) -> i32 {
    // SAFETY: P 类 — 新旧口令各取自一个活借用, NUL 结尾由调用方保证; 内核
    // pwm_change_password 对两指针分别 as_kstr 上限扫描, 借用存活至返回.
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_CHANGE_PASSWORD,
            o.as_ptr() as u64,
            n.as_ptr() as u64,
        ))
    }
}

pub fn env_getcwd(buf: &mut [u8]) -> i32 {
    // SAFETY: B 类 — 指针与长度同取自 buf 这一个活借用; 内核 getcwd_syscall 先
    // validate_user_buf(ptr, size) 校验整段可写, 再由 vfs_get_cwd 以 size 为上界
    // 写入, 借用存活至返回, 不会写到借用之外.
    unsafe { ret_i32(sys2(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64)) }
}
#[must_use]
pub fn env_chdir(path: &[u8]) -> i32 {
    // SAFETY: P 类 — 路径串取自活借用, NUL 结尾由调用方保证; 内核
    // checked_user_path 先校验用户地址下界再 as_kstr 上限扫描, 不越借用读.
    unsafe { ret_i32(sys1(SYS_CHDIR, path.as_ptr() as u64)) }
}
/// `chroot(path)` — 切换本进程视图根 (需 `CAP_SYS_ADMIN`).
#[must_use]
pub fn env_chroot(path: &[u8]) -> i32 {
    // SAFETY: P 类 — 同 env_chdir: 活借用 + 调用方 NUL 契约, 内核
    // checked_user_path 校验后 as_kstr 上限扫描.
    unsafe { ret_i32(sys1(SYS_CHROOT, path.as_ptr() as u64)) }
}
/// `pivot_root(new_root, put_old)` — 把视图根换成挂载点 `new_root` 并摘除旧根
/// (需 `CAP_SYS_ADMIN`). 两路径都须 NUL 结尾; `put_old` 须严格位于 `new_root` 之下.
#[must_use]
pub fn env_pivot_root(new_root: &[u8], put_old: &[u8]) -> i32 {
    // SAFETY: P 类 — 两路径各取自一个活借用, NUL 结尾由调用方保证 (见上文档);
    // 内核对两指针分别走 checked_user_path (校验 + as_kstr 上限扫描).
    unsafe {
        ret_i32(sys2(
            SYS_PIVOT_ROOT,
            new_root.as_ptr() as u64,
            put_old.as_ptr() as u64,
        ))
    }
}

pub fn gethostname(buf: &mut [u8]) -> i32 {
    // SAFETY: B 类 — 指针与长度同取自 buf 活借用; 内核 gethostname_syscall 把
    // 主机名拷进 65 字节栈缓冲后, 以 min(名长+1, buf.len()) 为长度经
    // copy_to_user 写入, 上界不超过借用长度.
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
    // SAFETY: B 类 (只读方向) — 指针与长度同取自 name 活借用, 长度显式传入
    // 而非依赖 NUL; 内核 set_uts_name_syscall 拒 len>63 后经 copy_from_user
    // 只读该长度, 借用存活至返回.
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
    // SAFETY: S 类 — 唯一参数是命令码数值 (经 arg_bits 转 u64), 不是指针;
    // 门只搬运值, 内核按数值分派重启命令, 无用户内存访问.
    // cmd 取 [`REBOOT_CMD_RESTART`] / [`REBOOT_CMD_HALT`] (单一权威源 `edgine-abi`).
    unsafe { sys1(SYS_SGEG_REBOOT, arg_bits(i64::from(cmd))) }
}
pub fn proc_list(buf: &mut [u8], max_entries: u32) -> i32 {
    // SAFETY: B 类 + 调用方容量契约 — 写入上界由 max_entries × 内核定长条目
    // (ProcListEntry) 决定, **不是** buf.len(); 调用方须保证 buf 容量足够容纳
    // max_entries 条. 内核逐条经 write_struct_to_user 校验该条大小的用户区间
    // 后写入, 指针取自 buf 活借用且存活至返回.
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_PROC_LIST,
            buf.as_mut_ptr() as u64,
            u64::from(max_entries),
        ))
    }
}

pub fn disk_list(disks: &mut [u64]) -> i32 {
    // SAFETY: B 类 — 指针与元素数同取自 disks 活借用; 内核 disk_list 以
    // max_count×8 校验整段可写 (checked_mul 防溢出), 再逐个 write_u64_to_user
    // 写 min(max_count, 设备数) 个 u64, 不越借用.
    unsafe {
        ret_i32(sys2(
            SYS_SGEG_DISK_LIST,
            disks.as_mut_ptr() as u64,
            disks.len() as u64,
        ))
    }
}
pub fn disk_info(id: u32, info: &mut UserDiskInfo) -> i32 {
    // SAFETY: T 类 — UserDiskInfo 是 repr(C) 定长结构, 指针由 from_mut 从活借用
    // 取得; 内核先 validate_user_buf(ptr, USER_DISK_INFO_SIZE) 再经
    // write_struct_to_user 写整个结构, 借用存活至返回.
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
    // SAFETY: P 类 — 文件系统名是 c"egfs" 编译期 C 字面量, 自带 NUL 终止符且
    // 位于 'static 只读段, 生命周期覆盖整个 syscall; 内核 disk_format 当前只
    // validate_user_ptr 校验该指针 (实现为占位, 尚不解引用该串).
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
    // SAFETY: S 类 — 两参均为数值 (设备 ID 与扇区数), 门只搬运值不访存;
    // 内核按数值定位设备与分区大小, 无用户指针解引用.
    unsafe { sys2(SYS_SGEG_DISK_PARTITION, u64::from(id), sectors) }
}
#[must_use]
pub fn boot_install(id: u32) -> i64 {
    // SAFETY: S 类 — 唯一参数是设备 ID 数值, 不是指针; 内核 boot_install_syscall
    // 按该数值选设备, 引导码来自内核自身缓冲, 不读用户内存.
    unsafe { sys1(SYS_SGEG_DISK_INSTALL, u64::from(id)) }
}
#[must_use]
pub fn fat_format(id: u32) -> i64 {
    // SAFETY: S 类 — 唯一参数是设备 ID 数值; 内核 fat_format 按数值选设备,
    // 无用户指针解引用.
    unsafe { sys1(SYS_SGEG_FAT_FORMAT, u64::from(id)) }
}
// T2 批 5 (syscall-followup): 补 wrapper — 原仅常量无 wrapper 无调用 (T3 登记)
pub fn hotplug_status(buf: &mut [u8]) -> i64 {
    // SAFETY: B 类 — 指针与长度同取自 buf 活借用; 内核 sys_hotplug_status 先
    // check_user_buf 校验整段, 再经 UserWritePtr::checked_new 收敛为等长切片,
    // 后续写入全走安全索引, 上界即 buf.len().
    unsafe {
        sys2(
            SYS_SGEG_HOTPLUG_STATUS,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

/// SGEG 私有: 导出 EEVDF 运行期观测快照 (K-07b 批3).
///
/// 内核向串口逐 CPU 输出账目 / lag / 唤醒计数器与线程组 lag 汇总;
/// 返回本次上报的在线 CPU 数.
#[must_use]
pub fn sched_snapshot() -> i64 {
    // SAFETY: S 类 — 无参数, 门只搬运编号; 内核只向串口输出账目, 不接收也不
    // 写入任何用户内存.
    unsafe { sys0(SYS_SGEG_SCHED_SNAPSHOT) }
}

// ============================================================
// 新增 POSIX syscall wrapper
// ============================================================

pub const SYS_TRUNCATE: u64 = 76;
pub const SYS_FTRUNCATE: u64 = 77;
pub const SYS_FSYNC: u64 = 170;
pub const SYS_GETPGID: u64 = 111;
pub const SYS_SETSID: u64 = 112;
pub const SYS_NANOSLEEP: u64 = 35;
pub const SYS_RT_SIGACTION: u64 = 13;
pub const SYS_RT_SIGPROCMASK: u64 = 14;
pub const SYS_UMASK: u64 = 95;
pub const SYS_CLOCK_GETTIME: u64 = 228;

/// POSIX 时钟 ID: 自启动单调时钟 (只读) — 与内核 `functions::timer::clock` 同值.
pub const CLOCK_MONOTONIC: i32 = 1;

/// 线程族 syscall 编号与 `clone` 标志位 — 单一权威源 `edgine-abi`.
pub use edgine_abi::thread::{
    CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID, CLONE_FILES, CLONE_FS, CLONE_PARENT_SETTID,
    CLONE_SETTLS, CLONE_SIGHAND, CLONE_THREAD, CLONE_VM, FUTEX_PRIVATE_FLAG, FUTEX_REQUEUE,
    FUTEX_WAIT, FUTEX_WAIT_BITSET, FUTEX_WAKE, FUTEX_WAKE_BITSET, SYS_CLONE, SYS_EXIT_GROUP,
    SYS_FUTEX, SYS_GETTID, SYS_KILL, SYS_TGKILL,
};

/// wasm 运行时私有 syscall 编号、实例状态与信息结构 — 单一权威源 `edgine-abi`.
pub use edgine_abi::wasm::{
    EG_WASM_GET_INFO, EG_WASM_LOAD, EG_WASM_RUN, EG_WASM_STATUS_FINISHED, EG_WASM_STATUS_READY,
    EG_WASM_STATUS_TRAPPED, EG_WASM_UNLOAD, EgWasmInfo,
};

#[repr(C)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// `itimerspec` — 与内核 `functions::fs::timerfd::Itimerspec` 同布局.
#[repr(C)]
pub struct Itimerspec {
    pub interval_sec: i64,
    pub interval_nsec: i64,
    pub value_sec: i64,
    pub value_nsec: i64,
}

#[must_use]
pub fn truncate(path: &[u8], length: i64) -> i32 {
    // SAFETY: P 类 + 标量 — 路径串取自活借用且 NUL 结尾由调用方保证, 内核
    // truncate_syscall 校验用户地址下界后交 vfs_open 按 NUL 串解析; length 是
    // 数值不是指针, 门只搬运值.
    unsafe { ret_i32(sys2(SYS_TRUNCATE, path.as_ptr() as u64, arg_bits(length))) }
}
#[must_use]
pub fn ftruncate(fd: i32, length: i64) -> i32 {
    // SAFETY: S 类 — fd 与 length 均为数值, 门只搬运值不访存; 内核按 fd 查
    // 已打开文件对象, 不解引用任何用户指针.
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
    // SAFETY: S 类 — 唯一参数是 fd 数值; 内核按 fd 查文件对象后落盘, 无用户
    // 内存访问.
    unsafe { ret_i32(sys1(SYS_FSYNC, arg_bits(i64::from(fd)))) }
}
#[must_use]
pub fn getpgid(pid: i32) -> i64 {
    // SAFETY: S 类 — 唯一参数是 pid 数值; 内核查进程表返回进程组 ID, 不访存
    // 用户内存.
    unsafe { sys1(SYS_GETPGID, arg_bits(i64::from(pid))) }
}
#[must_use]
pub fn setsid() -> i64 {
    // SAFETY: S 类 — 无参数, 门只搬运编号; 内核仅改当前进程的会话/进程组归属.
    unsafe { sys0(SYS_SETSID) }
}
#[must_use]
pub fn gettid() -> i64 {
    // SAFETY: S 类 — 无参数, 门只搬运编号; 内核返回当前线程 ID 数值.
    unsafe { sys0(SYS_GETTID) }
}
#[must_use]
pub fn umask(mask: u32) -> u32 {
    // SAFETY: S 类 — 唯一参数是掩码数值, 返回值也是数值; 内核只改当前进程的
    // umask 字段, 无用户指针解引用.
    unsafe { ret_u32(sys1(SYS_UMASK, u64::from(mask))) }
}
#[must_use]
pub fn nanosleep(req: &Timespec) -> i32 {
    // SAFETY: T 类 — Timespec 是 repr(C) 的两个 i64 (16 字节), 指针由 from_ref
    // 从活借用取得且借用存活至返回; 内核 sys_nanosleep 校验用户地址后
    // read_volatile 只读一个 Timespec, 不写回; 第二参传 0 (rem), 内核忽略它.
    unsafe {
        ret_i32(sys2(
            SYS_NANOSLEEP,
            core::ptr::from_ref::<Timespec>(req) as u64,
            0,
        ))
    }
}

/// `timerfd_create(clockid, flags)` — 创建 timerfd.
#[must_use]
pub fn timerfd_create(clockid: i32, flags: i32) -> i32 {
    // SAFETY: S 类 — clockid/flags 均为数值, 内核只创建自己的 timerfd 对象.
    unsafe {
        ret_i32(sys2(
            SYS_TIMERFD_CREATE,
            arg_bits(i64::from(clockid)),
            arg_bits(i64::from(flags)),
        ))
    }
}

/// `timerfd_settime(fd, flags, new, old)` — 设置 timerfd 到期.
///
/// `old` 为 `None` 时传空指针, 内核不写回旧值.
#[must_use]
pub fn timerfd_settime(fd: i32, flags: i32, new: &Itimerspec, old: Option<&mut Itimerspec>) -> i32 {
    let old_ptr = old.map_or(0, |value| core::ptr::from_mut(value) as u64);
    // SAFETY: T 类 — new/old 指针分别取自活借用与可选活借用; 内核
    // timerfd_settime_syscall 先校验用户区间再读 new、按需写 old, 不越界.
    unsafe {
        ret_i32(sys4(
            SYS_TIMERFD_SETTIME,
            arg_bits(i64::from(fd)),
            arg_bits(i64::from(flags)),
            core::ptr::from_ref(new) as u64,
            old_ptr,
        ))
    }
}

/// `clock_gettime(clk_id, tp)` — 读取指定时钟的时间.
///
/// 失败返回负错误码; 调用方不得把失败当作"时间为 0", 否则时间限界会退化成
/// 永久等待.
#[must_use]
pub fn clock_gettime(clk_id: i32, tp: &mut Timespec) -> i32 {
    // SAFETY: T 类 — tp 是 repr(C) 定长 Timespec, 指针由 from_mut 从活借用取得;
    // 内核 clock_gettime_syscall 经 write_struct_to_user 校验该结构大小的用户
    // 区间后写入, 借用存活至返回; clk_id 是数值不是指针.
    unsafe {
        ret_i32(sys2(
            SYS_CLOCK_GETTIME,
            arg_bits(i64::from(clk_id)),
            core::ptr::from_mut(tp) as u64,
        ))
    }
}
/// `rt_sigaction(signum, handler)` — 注册单值信号 handler.
///
/// Edgine 内核 ABI: `act` 指向一个 u64 (handler 地址); `oact = 0` 表示不查询旧值.
/// 返回 0 成功, 负错误码失败 (`EINVAL`: 非法编号或不可捕获信号).
#[must_use]
pub fn rt_sigaction(signum: i32, handler: u64) -> i64 {
    let act = handler;
    // SAFETY: T 类 — act 是本函数栈上的 u64 局部量, from_ref 取其地址, 自然
    // 对齐且存活至本函数返回 (覆盖整个 syscall); 内核 sys_rt_sigaction 先
    // check_user_buf(act, 8) 再读这 8 字节; 第三参传 0, 内核据此跳过旧值回写.
    unsafe {
        sys3(
            SYS_RT_SIGACTION,
            arg_bits(i64::from(signum)),
            core::ptr::from_ref(&act) as u64,
            0,
        )
    }
}

#[must_use]
pub fn kill(pid: i32, sig: i32) -> i32 {
    // SAFETY: S 类 — pid 与 sig 均为数值; 内核按数值查进程并置信号位, 不访存
    // 用户内存.
    unsafe {
        ret_i32(sys2(
            SYS_KILL,
            arg_bits(i64::from(pid)),
            arg_bits(i64::from(sig)),
        ))
    }
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
    // SAFETY: T 类 — FbInfo 是 repr(C) 定长结构, 指针由 from_mut 从活借用取得且
    // 借用存活至返回; 内核 sys_fb_open 校验用户起始地址后经 write_struct 写入
    // 整个结构 (两侧布局同为 repr(C) 定长); 其 flags 形参在内核名为 _flags 且
    // 未被使用, 故 sys1 不显式传 a1 不影响语义.
    unsafe { sys1(SYS_FB_OPEN, core::ptr::from_mut::<FbInfo>(info) as u64) }
}
#[must_use]
pub fn fb_mmap(addr: u64, size: u64, prot: i32) -> i64 {
    // SAFETY: S 类 — 三参全是数值: addr 是请求映射的目标虚拟地址值, size 是
    // 长度, prot 是保护标志; 本层不解引用 addr, 映射由内核页表机制建立.
    unsafe { sys3(SYS_FB_MMAP, addr, size, arg_bits(i64::from(prot))) }
}
#[must_use]
pub fn fb_release(addr: u64) -> i64 {
    // SAFETY: S 类 — addr 是先前 fb_mmap 用过的虚拟地址数值, 本层不解引用;
    // 内核 sys_fb_release 按映射记录解除页表映射.
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

/// 网络地址解析私有 syscall 编号与结果结构 — 单一权威源 `edgine-abi` (DNS 双栈 E1).
pub use edgine_abi::net::{EG_AF_INET, EG_AF_INET6, EG_GETADDRINFO, EgAddrInfo};

/// `DHCPv6` 运行时策略配置私有 syscall 编号与结构 — 单一权威源 `edgine-abi` (#298).
pub use edgine_abi::dhcpv6::{EG_DHCPV6_GET_POLICY, EG_DHCPV6_SET_POLICY, EgDhcpv6Policy};

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
    // SAFETY: S 类 — 三参全是协议族/类型/协议号数值, 门只搬运值不访存;
    // 内核 socket_syscall 按数值建对象, 无用户指针解引用.
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
    // SAFETY: R 类 — 调用方契约: addr 指向至少 addrlen 字节可读的 sockaddr
    // (sockaddr_un / sockaddr_in / sockaddr_in6), 且该内存在本调用期间存活.
    // 本层只把指针值搬进寄存器, 从不解引用; 内核 bind_syscall 先读 sun_family
    // 分流, UDS 路径经 raw_read_sockaddr_un 按 addrlen 校验并 copy-in,
    // INET/INET6 路径由 privileged 校验用户区间, 越界返 -EFAULT.
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
    // SAFETY: S 类 — fd 与 backlog 均为数值; 内核 listen_syscall 拒负值后按
    // fd 解析 socket 对象, 不访存用户内存.
    unsafe {
        ret_i32(sys2(
            SYS_LISTEN,
            arg_bits(i64::from(sockfd)),
            arg_bits(i64::from(backlog)),
        ))
    }
}

pub fn accept(sockfd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    // SAFETY: R 类 — 调用方契约: addr 指向可写的 sockaddr 缓冲, addrlen 指向
    // 一个 u32 (入参为缓冲容量, 出参为实际地址长度), 两者在调用期间存活且
    // 互不重叠. 本层不解引用; 内核 accept_syscall 经 privileged 校验这两个
    // 用户区间后回写, 无效则返 -EFAULT; UDS 分支忽略这两个出参.
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
    // SAFETY: R 类 — 调用方契约同 bind: addr 指向至少 addrlen 字节可读的
    // sockaddr 且存活至返回. 本层只搬运指针值; 内核 connect_syscall 读
    // sun_family 分流后按 addrlen 校验/copy-in (UDS) 或交 privileged 校验
    // 用户区间 (INET/INET6).
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
    // SAFETY: R 类 — 调用方契约: buf 指向至少 len 字节可读缓冲, dest 指向至少
    // destlen 字节可读的 sockaddr, 两者在调用期间存活. 本层只搬运指针值,
    // 不解引用; 内核 sendto_syscall 对 UDS 路径 raw_copy_in(buf, len) +
    // raw_read_sockaddr_un(dest, destlen), INET 路径交 privileged 校验用户区间,
    // 越界返 -EFAULT. a5/a6 显式传入 (见上 DECISION-032 注).
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
    // SAFETY: R 类 — 调用方契约: buf 指向至少 len 字节**可写**缓冲; src 若非 0
    // 须指向可写 sockaddr, srclen 若非 0 须指向一个 u32, 均在调用期间存活.
    // 本层不解引用; 内核 recvfrom_syscall 校验 buf 区间后 copy-out 至多 len
    // 字节. 注意 UDS 分支 (v1) 显式忽略 src/srclen 不回填, INET 分支才按
    // 校验后的区间写回源地址, 故不得假定返回后 src 一定被填.
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
    // SAFETY: R 类 — 调用方契约: buf 指向至少 len 字节可读缓冲且存活至返回;
    // 本层不解引用, 内核按 len 校验后 copy-in. 复用 SYS_SENDTO 但 a5/a6 显式
    // 传 0 (无目标地址 ⇒ 走已连接 socket 路径), 避免残留寄存器被当作指针.
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
    // SAFETY: R 类 — 调用方契约: buf 指向至少 len 字节可写缓冲且存活至返回;
    // 本层不解引用, 内核校验区间后 copy-out 至多 len 字节. 复用 SYS_RECVFROM
    // 且 a5/a6 显式传 0 ⇒ 不请求源地址回填.
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
    // SAFETY: R 类 — 调用方契约: msg 指向 56 字节 Msghdr (内核按该定长校验),
    // 其 msg_iov 指向 iovlen 个 16 字节 Iovec (iovlen ∈ 1..=1024), 各 iov_base
    // 指向 iov_len 字节可读缓冲; msg_control 若非 0 须指向 msg_controllen 字节
    // **可写**区域 (SO_PASSCRED 的 UDS 路径会回写 28 字节凭据 cmsg). 上述内存
    // 均须存活至返回. 本层只搬运 msg 指针值, 不解引用; 内核 sendmsg_syscall
    // 逐段 validate_user_buf 后经 read/write_u64_to_user 访问, 越界返 -EFAULT.
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
    // SAFETY: R 类 — 调用方契约: msg 指向 56 字节**可写** Msghdr, msg_iov 指向
    // iovlen 个 16 字节 Iovec 且各 iov_base 指向 iov_len 字节可写缓冲,
    // msg_control 若非 0 须可写 (内核回写对端注入的 SCM_CREDENTIALS cmsg 并
    // 更新 msg_controllen / msg_flags). 全部内存存活至返回. 本层不解引用;
    // 内核 recvmsg_syscall 逐段校验用户区间后搬运, 越界返 -EFAULT.
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
    // SAFETY: R 类 — 调用方契约: optval 指向至少 optlen 字节可读的选项值且
    // 存活至返回. 本层不解引用; 内核 setsockopt_syscall 对 SOL_SOCKET/
    // SO_PASSCRED 先 copy-in min(optlen, 20) 字节再要求 ≥4, 其余选项交
    // privileged 按 optlen 校验用户区间, 越界返 -EFAULT.
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
    // SAFETY: R 类 — 调用方契约: optval 指向可写缓冲, optlen 指向一个 u32
    // (入参为容量, 出参为实际长度), 两者存活至返回且互不重叠. 本层不解引用;
    // 内核 getsockopt_syscall 交 privileged 校验这两个用户区间后回写,
    // 无效返 -EFAULT.
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
    // SAFETY: R 类 — 调用方契约: addr 指向可写 sockaddr 缓冲, addrlen 指向一个
    // u32 (入参容量 / 出参实际长度), 两者存活至返回且互不重叠. 本层不解引用;
    // 内核 getsockname_syscall 交 privileged 校验这两个用户区间后回写本端地址.
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
    // SAFETY: R 类 — 调用方契约同 getsockname, 回写的是对端地址; 本层只搬运
    // 指针值, 内核校验用户区间后写入, 无效返 -EFAULT.
    unsafe {
        ret_i32(sys3(
            SYS_GETPEERNAME,
            arg_bits(i64::from(sockfd)),
            addr as u64,
            addrlen as u64,
        ))
    }
}

/// `EG_GETADDRINFO(name, out)` — 双栈 DNS 解析 (DNS 双栈 E1, IPv6 优先).
///
/// 成功返 0 并回写单个 [`EgAddrInfo`]; 解析失败返 `-ENOENT`, 主机名非 UTF-8
/// 返 `-EINVAL`, 用户缓冲无效返 `-EFAULT`. `name` 为主机名字节 (不含 NUL 终止符).
#[must_use]
pub fn getaddrinfo(name: &[u8], out: &mut EgAddrInfo) -> i64 {
    // SAFETY: B 类 (name) + T 类 (out) — name 的指针与长度同取自一个活借用,
    // 按文档契约不含 NUL, 内核按 name_len 校验区间后 copy-in 再做 UTF-8 校验;
    // out 是 repr(C) 定长 EgAddrInfo, 指针由 from_mut 从活借用取得, 内核经
    // write_struct_to_user 校验该结构大小的用户区间后回写. 两借用均存活至返回.
    unsafe {
        sys3(
            EG_GETADDRINFO,
            name.as_ptr() as u64,
            name.len() as u64,
            core::ptr::from_mut(out) as u64,
        )
    }
}

/// `EG_DHCPV6_SET_POLICY(policy)` — 设置 `DHCPv6` 运行时策略 + 启用开关 (#298).
///
/// 成功返 0; `enabled` 非 0/1 或策略字段非法返 `-EINVAL`, 用户缓冲无效返 `-EFAULT`.
/// `policy.enabled` 取 0 (关闭内核客户端, 用户态守护接管) / 1 (启用).
#[must_use]
pub fn dhcpv6_set_policy(policy: &EgDhcpv6Policy) -> i64 {
    // SAFETY: T 类 (只读方向) — EgDhcpv6Policy 是 edgine-abi 单源的 repr(C)
    // 定长结构, 指针由 from_ref 从活借用取得且存活至返回; 内核
    // dhcpv6_set_policy_syscall 经 read_struct_from_user 校验该结构大小的用户
    // 区间后只读拷入内核栈副本, 不回写用户内存.
    unsafe { sys1(EG_DHCPV6_SET_POLICY, core::ptr::from_ref(policy) as u64) }
}

/// `EG_DHCPV6_GET_POLICY(policy)` — 读取当前 `DHCPv6` 运行时策略 + 启用开关 (#298).
///
/// 成功返 0 并回写 [`EgDhcpv6Policy`] (客户端未构造时返待生效配置或默认值);
/// 用户缓冲无效返 `-EFAULT`.
#[must_use]
pub fn dhcpv6_get_policy(policy: &mut EgDhcpv6Policy) -> i64 {
    // SAFETY: T 类 — 同一 repr(C) 定长 ABI 结构, 指针由 from_mut 从活借用取得;
    // 内核 dhcpv6_get_policy_syscall 经 write_struct_to_user 校验该结构大小的
    // 用户区间后回写, 借用存活至返回.
    unsafe { sys1(EG_DHCPV6_GET_POLICY, core::ptr::from_mut(policy) as u64) }
}

/// `shutdown(sockfd, how)` — D7 半关语义透传 (`SHUT_RD`/`SHUT_WR`/`SHUT_RDWR`).
#[must_use]
pub fn shutdown(sockfd: i32, how: i32) -> i32 {
    // SAFETY: S 类 — fd 与 how 均为数值; 内核 shutdown_syscall 按 fd 解析对象
    // 后执行半关, 不访存用户内存.
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
    // SAFETY: S 类 — 唯一参数是 fd 数值; 内核按 fd 查表关闭对象, 无用户指针
    // 解引用.
    unsafe { ret_i32(sys1(SYS_CLOSE, arg_bits(i64::from(sockfd)))) }
}

#[must_use]
pub fn ioctl(fd: i32, request: u64, arg: u64) -> i32 {
    // SAFETY: R 类 — arg 的语义由 request 决定: TIOCGWINSZ 与 UFFDIO_* 下它是
    // 指向调用方**可写**结构 (Winsize / uffd ioctl 参数) 的用户指针, 须存活至
    // 返回; 其余 request 下它是数值. 本层只搬运该 u64, 从不解引用; 内核
    // ioctl_syscall 按 request 分流, 指针形态经 write_struct_to_user / uffd
    // 路径校验用户区间后访问, 无效返 -EFAULT.
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
    // SAFETY: S 类 — 本 wrapper 的既有消费者只用 F_SETFL, 此时 arg 是标志值
    // (O_NONBLOCK) 而非指针; F_GETFD / F_SETFD 的 arg 同为数值. 门只搬运值,
    // 内核 fcntl_syscall 按 cmd 改 fd 表 / per-slot 阻塞标志, 不解引用用户内存.
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
    // SAFETY: B 类 + T 类 — 指针与元素数同取自 fds 这一个活借用, PollFd 是
    // repr(C) 定长 8 字节 (与内核 poll_syscall 内的同名结构同布局); 内核按
    // nfds × 8 逐条 read_struct_from_user / write_struct_to_user 校验并读写,
    // 上界即借用长度, 借用存活至返回. timeout 是数值 (内核当前忽略它).
    unsafe {
        ret_i32(sys3(
            SYS_POLL,
            fds.as_mut_ptr() as u64,
            fds.len() as u64,
            arg_bits(i64::from(timeout)),
        ))
    }
}

// ============================================================
// 线程族 wrapper — clone / futex / exit_group / tgkill
// ============================================================

/// 线程入口函数类型: 由 `clone_thread` 在新线程自己的栈上调用.
pub type ThreadEntry = unsafe extern "C" fn(arg: u64);

/// `thread_trampoline` — 子线程分支的落点 (仅由 `clone_thread` 的汇编跳入).
///
/// 入口函数返回后只终止本线程 (`SYS_EXIT`): K-06 起 `exit` 是单 TCB 退出,
/// 组内其余线程不受影响; 整组退出由 `exit_group` 表达.
///
/// `pub` 而非私有: 本函数只被 `asm!` 的 `sym` 操作数引用, 若为私有项则不构成
/// 任何 Rust 调用边, 会被 `dead_code` 判为孤儿 (F9 禁止用属性压制).
///
/// # Safety
/// `entry` 必须是可调用入口 (内核态/用户态函数指针契约由调用方保证).
pub unsafe extern "C" fn thread_trampoline(entry: ThreadEntry, arg: u64) -> ! {
    // SAFETY: entry/arg 由 clone_thread 经被调用者保存寄存器偷渡后原样传入,
    // clone_thread 的 Safety 契约保证 entry 是合法的 ThreadEntry.
    unsafe { entry(arg) };
    proc_exit(0)
}

/// `clone` 的线程创建包装 — 父线程返回子 TID, 子线程不返回本函数.
///
/// ## 为什么子分支必须在汇编块内跳转
///
/// 内核把父线程**陷入时**的完整寄存器帧复制给子线程, 仅改写
/// `rax`/`x0` = 0 与栈指针 = `child_stack`. 子线程若继续执行本函数 syscall
/// 之后的 Rust 代码, 其 `rsp` 已是一条全新栈, 而编译器生成的溢出槽读写用的
/// 是继承来的 `rbp` (指向父线程栈) —— 父线程此刻正在别的核上改写同一批槽位,
/// "返回值为 0" 的判定随时可能被父线程的写入翻转. 故子分支在 `asm!` 内部
/// 直接跳入 `thread_trampoline`, 一步不落回 Rust 序言/收尾.
///
/// ## 入口与参数如何送到子线程
///
/// syscall 入参寄存器 (x86_64 `rdi/rsi/rdx/r10/r8`, aarch64 `x1..x5`) 在子线程
/// 里装的是 clone 自己的参数, 不能用来带 `entry`. 被调用者保存寄存器
/// (x86_64 `r12/r13`, aarch64 `x21/x22`) 由 `proc_save_user_regs*` 写入进程
/// 上下文并随 clone 整体复制, 故用它们偷渡.
///
/// # Safety
/// `child_stack` 必须 16 字节对齐且指向 `flags` 所许可的可写内存 (内核会校验
/// 对齐); `entry`/`arg` 按 `ThreadEntry` 契约使用; `parent_tid`/`child_tid`
/// 仅在 `flags` 带对应 `CLONE_*_TID` 位时被内核写入, 传 0 表示不用.
///
/// 返回值: 父线程 = 子 TID (>0) 或负 `Errno`; 子线程不执行返回路径.
#[cfg(target_arch = "x86_64")]
pub unsafe fn clone_thread(
    flags: u64,
    child_stack: u64,
    parent_tid: *mut i32,
    child_tid: *mut u32,
    tls: u64,
    entry: ThreadEntry,
    arg: u64,
) -> i64 {
    let ret: i64;
    // SAFETY: 本块是 clone(2) 的裸 syscall 序列. 入参寄存器按 Edgine x86_64
    // syscall ABI 布置 (rdi/rsi/rdx/r10/r8 = flags/stack/ptid/ctid/tls);
    // `syscall` 破坏 rcx/r11, 以 out 声明; r12/r13 只在本块内被子分支读取,
    // 用 inlateout 显式声明为被改写, 阻止 LLVM 把存活值放这两格.
    // 子分支改写 rsp (跳到 trampoline 前补一个伪返回地址槽维持 SysV 对齐),
    // 故不声明 nomem/nostack.
    unsafe {
        asm!(
            "syscall",
            "test rax, rax",
            "jne 2f",
            // 子线程: rdi/rsi 此刻是 clone 的 flags/stack, 换成 trampoline 的入参
            "sub rsp, 8",
            "mov rdi, r12",
            "mov rsi, r13",
            "jmp {tramp}",
            "2:",
            tramp = sym thread_trampoline,
            in("rax") SYS_CLONE,
            in("rdi") flags,
            in("rsi") child_stack,
            in("rdx") parent_tid as u64,
            in("r10") child_tid as u64,
            in("r8") tls,
            inlateout("r12") entry => _,
            inlateout("r13") arg => _,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

/// `clone` 的线程创建包装 (aarch64) — 语义同 x86_64 版.
///
/// aarch64 侧 `x21/x22` 映射到进程上下文的 `r13/r12` 槽 (见
/// `proc_save_user_regs_aarch64`), 随 clone 整体复制并在首次进入 EL0 前恢复;
/// `x8..x18` 不入上下文 (首次被调度时不保证是父线程的值), `x19/x20` 被 LLVM
/// 保留为内部用量 (不可作 inline asm 操作数), 故二者都不可用于偷渡.
///
/// # Safety
/// 同 x86_64 版: `child_stack` 16 字节对齐且可写, `entry`/`arg` 遵守
/// `ThreadEntry` 契约, `parent_tid`/`child_tid` 仅在对应 `CLONE_*_TID` 位下使用.
#[cfg(target_arch = "aarch64")]
pub unsafe fn clone_thread(
    flags: u64,
    child_stack: u64,
    parent_tid: *mut i32,
    child_tid: *mut u32,
    tls: u64,
    entry: ThreadEntry,
    arg: u64,
) -> i64 {
    let ret: i64;
    // SAFETY: svc #0 的编号在 x0, 参数在 x1..x5 (Edgine aarch64 syscall ABI);
    // 子分支把 entry/arg 搬到 x0/x1 后跳入 trampoline, 不回落到父路径.
    // AAPCS64 要求 SP 16 字节对齐: child_stack 已由内核校验, 无需再调整.
    unsafe {
        asm!(
            "svc #0",
            "cbnz x0, 2f",
            "mov x0, x21",
            "mov x1, x22",
            "b {tramp}",
            "2:",
            tramp = sym thread_trampoline,
            in("x0") SYS_CLONE,
            in("x1") flags,
            in("x2") child_stack,
            in("x3") parent_tid as u64,
            in("x4") child_tid as u64,
            in("x5") tls,
            inlateout("x21") entry => _,
            inlateout("x22") arg => _,
            lateout("x0") ret,
        );
    }
    ret
}

/// `futex(uaddr, op, val, timeout_or_uaddr2, val2)` — 内核支持面的直通入口.
///
/// op 的高位标志 (`FUTEX_PRIVATE_FLAG`) 由内核 `futex_base_op` 剥掉后再分派.
#[must_use]
pub fn futex(uaddr: *const u32, op: i32, val: i32, aux: u64, val2: u32) -> i64 {
    // SAFETY: R 类 — 调用方契约: uaddr 指向一个 **4 字节对齐**的 AtomicU32
    // (内核 futex_validate_uaddr 拒空指针与未对齐地址, 再以 AtomicU32 Acquire
    // 读该字), 该字在调用期间存活且其值由调用方以原子操作维护; 本层只搬运
    // 指针值, 从不解引用. aux 的语义随 op 变: WAIT_BITSET 下指向 16 字节
    // Timespec (内核 check_user_buf(ptr, 16) 后只读一次, 传 0 = 无超时),
    // REQUEUE 下是第二个 futex 字地址, WAIT/WAKE 下内核忽略 (本库传 0).
    // op / val / val2 均为数值.
    unsafe {
        sys5(
            SYS_FUTEX,
            uaddr as u64,
            arg_bits(i64::from(op)),
            arg_bits(i64::from(val)),
            aux,
            u64::from(val2),
        )
    }
}

/// 等待 `*word == val` (相等则入睡), 带 `FUTEX_PRIVATE_FLAG`.
///
/// 返回 0 = 被唤醒; 负值 = `-Errno` (值不等时为 `-EAGAIN`).
#[must_use]
pub fn futex_wait(word: &core::sync::atomic::AtomicU32, val: u32) -> i64 {
    futex(
        core::ptr::from_ref(word).cast::<u32>(),
        FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
        val.cast_signed(),
        0,
        0,
    )
}

/// 带**绝对**超时的等待 (`FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG`).
///
/// `timeout` 是 `CLOCK_MONOTONIC` 绝对时刻 (秒 + 纳秒, 与 `clock_gettime` 同
/// 时基); 到期由内核兜底唤醒并返回 `-ETIMEDOUT` (Linux errno 110). 位集参数取
/// 全 1, 即不按位过滤 (与 `FUTEX_WAKE_BITSET` 全体唤醒对称).
#[must_use]
pub fn futex_wait_bitset(
    word: &core::sync::atomic::AtomicU32,
    val: u32,
    timeout: &Timespec,
) -> i64 {
    futex(
        core::ptr::from_ref(word).cast::<u32>(),
        FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG,
        val.cast_signed(),
        core::ptr::from_ref(timeout) as u64,
        u32::MAX,
    )
}

/// 唤醒至多 `count` 个等待 `word` 的线程, 返回实际唤醒数 (负值 = `-Errno`).
#[must_use]
pub fn futex_wake(word: &core::sync::atomic::AtomicU32, count: u32) -> i64 {
    futex(
        core::ptr::from_ref(word).cast::<u32>(),
        FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
        count.cast_signed(),
        0,
        0,
    )
}

/// `exit_group(code)` — 终止整个线程组 (K-06: 组退出码 first-writer-wins).
pub fn exit_group(code: i32) -> ! {
    // SAFETY: S 类 — 唯一参数是退出码数值; 内核终止整个线程组后不再返回本
    // 进程, 故下面的兜底循环只在 syscall 意外返回时到达.
    unsafe {
        sys1(SYS_EXIT_GROUP, arg_bits(i64::from(code)));
    }
    loop {
        if cfg!(target_arch = "x86_64") {
            // SAFETY: hlt 不访存也不改通用寄存器, 故 nomem/nostack 成立; 仅在
            // SYS_EXIT_GROUP 意外返回时到达, 用户态执行 hlt 若触发 #GP 由内核
            // 异常路径终止本进程, 不构成内存不安全.
            unsafe {
                asm!("hlt", options(nomem, nostack));
            }
        } else {
            // SAFETY: wfi 不访存也不改通用寄存器, 故 nomem/nostack 成立; 仅在
            // SYS_EXIT_GROUP 意外返回时到达, 用户态执行 wfi 若被陷入则由内核
            // 异常路径处置, 不构成内存不安全.
            unsafe {
                asm!("wfi", options(nomem, nostack));
            }
        }
    }
}

/// `tgkill(tgid, tid, sig)` — 向线程组内指定线程投递信号 (`sig == 0` 仅作存在性探测).
#[must_use]
pub fn tgkill(tgid: i32, thread_id: i32, sig: i32) -> i64 {
    // SAFETY: S 类 — 三参全是数值 (线程组 ID / 线程 ID / 信号号); 内核按数值
    // 定位线程并置信号位, 不访存用户内存.
    unsafe {
        sys3(
            SYS_TGKILL,
            arg_bits(i64::from(tgid)),
            arg_bits(i64::from(thread_id)),
            arg_bits(i64::from(sig)),
        )
    }
}

// ============================================================
// 内存映射 (mmap/munmap — POSIX 编号 9/11, 与内核 dispatch_mm 对齐)
// ============================================================

pub const SYS_MMAP: u64 = 9;
pub const SYS_MUNMAP: u64 = 11;

// 保护位 (Linux asm-generic 值域, 与内核 functions::mm::mmap 的 prot_to_vma_flags 对齐)
pub const PROT_READ: i32 = 0x1;
pub const PROT_WRITE: i32 = 0x2;
pub const PROT_EXEC: i32 = 0x4;

// 映射标志 (Linux asm-generic 值域)
pub const MAP_SHARED: i32 = 0x01;
pub const MAP_PRIVATE: i32 = 0x02;
pub const MAP_FIXED: i32 = 0x10;
pub const MAP_ANONYMOUS: i32 = 0x20;

/// `mmap(addr, len, prot, flags, fd, offset)` — 成功返映射地址 (>0), 失败返 `-Errno`.
///
/// 匿名堆映射取 `addr=0` (内核自动选空闲区间) / `fd=-1` / `offset=0` /
/// `MAP_PRIVATE | MAP_ANONYMOUS`. 返回地址页对齐.
#[must_use]
pub fn mmap(addr: u64, len: u64, prot: i32, flags: i32, fd: i32, offset: i64) -> i64 {
    // SAFETY: S 类 — 六参全是数值: addr 是请求的虚拟地址值 (0 = 由内核选区间),
    // len / prot / flags / fd / offset 同为数值, 本层不解引用任何指针; 映射由
    // 内核页表机制建立, 返回的地址值才是后续访问的依据.
    unsafe {
        sys6(
            SYS_MMAP,
            addr,
            len,
            arg_bits(i64::from(prot)),
            arg_bits(i64::from(flags)),
            arg_bits(i64::from(fd)),
            arg_bits(offset),
        )
    }
}

/// `munmap(addr, len)` — 解除映射, 成功返 0, 失败返 `-Errno`.
#[must_use]
pub fn munmap(addr: u64, len: u64) -> i64 {
    // SAFETY: S 类 — addr / len 是先前 mmap 返回的区间数值, 本层不解引用;
    // 内核按该数值区间解除页表映射.
    unsafe { sys2(SYS_MUNMAP, addr, len) }
}
