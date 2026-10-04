//! 系统调用 API 层
//!
//! `Edgine` 原生 syscall (EG_*) + Linux 兼容 (SYS_*) + SGEG 私有 syscall 的统一分发入口,
//! 用户态→内核态的唯一合法路径。
//!
//! ## 编号空间
//! - 0-299   : Linux 兼容编号 (SYS_*), 直接使用 Linux 标准编号
//! - 400-499 : SGEG 私有 syscall
//! - 500+    : `Edgine` 原生编号 (EG_*)
//!
//! ## 调用方契约
//! - `boot::isr.asm` —— 中断/异常入口 (int 0x80 / syscall 指令)
//! - `idt::handlers` —— ISR 存根调用 `syscall_dispatch_from_frame`
//! - `proc::exec::load_elf` —— execve 时验证用户指针
//! - `sgeg::api` —— 能力检查路径复用 `validate_user_ptr`
//! - `egdf::user_driver` —— 用户态驱动透传
//!
//! ## 内部接口
//! - `types.rs` —— Errno, syscall 编号常量
//! - `mmap.rs` —— mmap/munmap/mprotect 实现
//! - `mod.rs` —— `syscall_dispatch()` 核心分发器 (所有 sys_* 实现)
//!
//! ## 安全约束
//! - 所有公开函数均通过 `validate_user_ptr` / `validate_user_buf` 检查用户指针
//! - 用户指针必须在 [1, 0x7FFFFFFFE000) 范围内
//! - `syscall_dispatch` / `syscall_dispatch_from_frame` 必须在中断上下文调用
//! - `syscall_register` 仅在启动阶段单线程调用
//!
//! ## 性能特征
//! - 分发路径: O(1) match 分支, 编译器优化为跳转表
//! - 指针验证: 两次比较, ≤ 5ns
//! - 覆盖 70+ POSIX syscall + 40+ SGEG 私有 syscall

pub use super::types::Errno;

// ============================================================================
// Edgine 原生 syscall 编号 (EG_*)
// ============================================================================

pub use super::types::{
    EG_CET, EG_CGROUP_ATTACH, EG_CGROUP_CREATE, EG_CGROUP_DESTROY, EG_CGROUP_GET_STAT,
    EG_CGROUP_SET_LIMIT, EG_FTRACE_DISABLE, EG_FTRACE_ENABLE, EG_FTRACE_READ, EG_FTRACE_STAT,
    EG_FW_DETACH, EG_FW_GET, EG_FW_GET_INFO, EG_FW_LOAD, EG_GET_CANARY, EG_KGDB_ENTER,
    EG_NF_ADD_RULE, EG_NF_DEL_RULE, EG_PM, EG_ROUTE_ADD, EG_ROUTE_DEL, EG_ROUTE_QUERY,
    EG_SECURE_BOOT, EG_TICKLESS, EG_TIMESYNC, EG_TPM, EG_UEFI,
};

// ============================================================================
// SYS_* 编号常量唯一定义于 `types.rs` (B09-17 归位); SGEG 基准 400 见 types.rs.
// ============================================================================

/// 验证用户态指针是否在合法范围内
pub fn validate_user_ptr(ptr: u64) -> bool {
    super::validate_user_ptr(ptr)
}

/// 验证用户态缓冲区是否在合法范围内
pub fn validate_user_buf(ptr: u64, len: u64) -> bool {
    super::validate_user_buf(ptr, len)
}

/// 安全写入 u64 到用户空间指针 (先校验后写入)
pub fn write_u64_to_user(ptr: u64, val: u64) -> bool {
    super::raw::write_u64_to_user(ptr, val)
}

/// 安全从用户空间指针读取 u64 (先校验后读取)
pub fn read_u64_from_user(ptr: u64) -> Option<u64> {
    super::raw::read_u64_from_user(ptr)
}

/// 安全写入结构体到用户空间指针 (先校验后写入)
pub fn write_struct_to_user<T: Copy>(ptr: u64, src: &T) -> bool {
    super::raw::write_struct_to_user(ptr, src)
}

/// 安全从用户空间指针读取结构体 (先校验后读取)
pub fn read_struct_from_user<T: Copy>(ptr: u64, dst: &mut T) -> bool {
    super::raw::read_struct_from_user(ptr, dst)
}

/// 安全写入 rlimit (两个 u64) 到用户空间指针
pub fn write_rlimit_to_user(ptr: u64, cur: u64, max: u64) -> bool {
    super::raw::write_rlimit_to_user(ptr, cur, max)
}

/// 获取系统 ticks 计数 (ms 精度)
pub fn get_ticks() -> u64 {
    super::raw::get_ticks()
}

/// nanosleep 系统调用实现 (TCB: 操作 hrtimer + 调度器)
pub fn sys_nanosleep(req: u64, rem: u64) -> i64 {
    super::sys_nanosleep(req, rem)
}

/// kill 系统调用实现 (TCB: 操作进程信号位)
pub fn sys_kill(pid: i32, sig: i32) -> i64 {
    super::sys_kill(pid, sig)
}

/// `rt_sigaction` 系统调用实现 (TCB: 操作 sigaction 表)
pub fn sys_rt_sigaction(signum: i32, act: u64, oact: u64) -> i64 {
    super::sys_rt_sigaction(signum, act, oact)
}

/// `rt_sigprocmask` 系统调用实现 (TCB: 操作信号掩码)
pub fn sys_rt_sigprocmask(how: i32, set: u64, oset: u64) -> i64 {
    super::sys_rt_sigprocmask(how, set, oset)
}

/// P1-I-45: sigaltstack 系统调用实现 (TCB: 替代栈注册/查询)
pub fn sys_sigaltstack(ss: u64, old_ss: u64) -> i64 {
    super::sys_sigaltstack(ss, old_ss)
}

/// `arch_prctl` 系统调用实现 (TCB: 用户态 TLS 基址读写)
pub fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    super::sys_arch_prctl(code, addr)
}

/// reboot 机制: cmd=0 停机, cmd=1 重启 (TCB: 操作 IDT/PSCI)
pub fn reboot_mechanism(cmd: i32) -> i64 {
    match cmd {
        0 => loop {},
        1 => match () {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: reboot_via_idt 仅在 x86_64 停机/重启路径调用,
            // 此时无其他线程运行, IDT 操作安全
            () => unsafe { super::raw::reboot_via_idt() },
            #[cfg(target_arch = "aarch64")]
            // SAFETY: reboot_via_psci 仅在 aarch64 停机/重启路径调用,
            // PSCI SMC 调用由固件保证幂等
            () => unsafe { super::raw::reboot_via_psci() },
            #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
            () => loop {},
        },
        _ => crate::framework::syscall::Errno::EINVAL.as_ret(),
    }
}

/// mmap 机制: 获取当前进程 mm 或分配裸页 (TCB: 操作页分配器)
pub fn mmap_get_mm_or_alloc(size: u64) -> Option<*mut u8> {
    if crate::framework::mm::vma_get_current_mm().is_some() {
        return None; // 有 mm, 走 VMA 路径
    }
    let pages = size.div_ceil(crate::framework::mm::PAGE_SIZE);
    let ptr = super::raw::alloc_pages(pages);
    if ptr.is_null() { None } else { Some(ptr) }
}

/// munmap 机制: 无 mm 时释放裸页 (TCB: 操作页分配器)
pub fn munmap_free_pages(addr: u64, size: u64) {
    let pages = size.div_ceil(crate::framework::mm::PAGE_SIZE);
    super::raw::free_pages(addr as *mut u8, pages);
}
