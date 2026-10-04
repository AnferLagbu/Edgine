//! `UserMode` — 进入 Ring 3 / EL0 的安全句柄 (TCB)
//!
//! 封装 `sysret` (`x86_64`) / `eret` (aarch64) 指令，
//! 确保回内核后栈/状态正确。
//!
//! ## 与 Asterinas OSTD `UserMode` 的关系
//!
//! 等价于 OSTD 的 `enter_usermode()` 入口。
//!
//! ## SAFETY 不变量
//!
//! - 必须在进程的内核栈上调用（非中断栈）。
//! - 返回时内核栈必须恢复到调用前状态。
//! - 调用前必须调用 `VmSpace::activate()` 切换到正确的页表。

use super::arch::Arch;
use super::userctx::UserContext;
use super::vmspace::VmSpace;

/// 进入用户模式执行直到下一次陷入（syscall / interrupt / exception）。
///
/// 委托到 `Arch::enter_user` 触发硬件上下文切换:
/// - `x86_64`: cli + 装载 ds/es/fs/gs + swapgs + iretq
/// - aarch64: msr `sp_el0/elr_el1/spsr_el1` + eret (EL0 用户态)
///
/// # SAFETY
/// - 必须在进程的内核栈上调用（非中断栈）。
/// - 调用前必须调用 `vmspace.activate()` 切换到正确的页表。
/// - `ctx.rip` (`x86_64`) / `ctx.elr_el1` (aarch64) 必须指向合法用户态代码。
/// - `ctx.rsp` / `ctx.sp_el0` 必须指向合法用户态栈。
/// - `ctx.rdi` (`x86_64`) / `ctx.x0` (aarch64) 是用户态入口的第一个参数。
/// - 此函数 `noreturn`: 仅在用户态下次陷入时返回 (经由 syscall/interrupt/exception 入口),
///   不会以函数返回值方式返回。
// SAFETY: 见上方文档约定. 此函数 noreturn, 必须满足: 内核栈调用 + 页表已切 +
// ctx.{rip,rsp,rdi} (x86_64) / ctx.{elr_el1,sp_el0,x0} (aarch64) 指向合法用户态
// 内存. 汇编入口 (iretq/eret) 不会返回.
#[cfg(target_arch = "x86_64")]
pub unsafe fn enter_user_mode(vmspace: &VmSpace, ctx: &UserContext) -> ! {
    <crate::privileged::arch::X8664 as Arch>::enter_user(
        ctx.rip as usize,    // ELR/rip
        ctx.rsp as usize,    // stack pointer
        ctx.rdi as usize,    // arg0 (x86_64 calling convention)
        vmspace.pt_root().0, // user_cr3
        0,                   // kstack: 由 user_proc 直接调用 enter_user 时传入
    )
}

#[cfg(target_arch = "aarch64")]
/// 进入用户模式 (aarch64 架构版)。
///
/// # SAFETY
/// 契约同 x86_64 版本; `Arch::enter_user` 内部执行 msr sp_el0/elr_el1/spsr_el1 + eret.
pub unsafe fn enter_user_mode(vmspace: &VmSpace, ctx: &UserContext) -> ! {
    <crate::privileged::arch::Aarch64 as Arch>::enter_user(
        ctx.elr_el1 as usize, // ELR_EL1
        ctx.sp_el0 as usize,  // SP_EL0
        ctx.x0 as usize,      // arg0 (aarch64 calling convention)
        vmspace.pt_root().0,  // user_cr3 (TTBR0)
        0,                    // kstack: aarch64 不需要
    )
}

/// 安全分发系统调用 (functions 层入口)。
///
/// 封装 `unsafe extern "C" fn syscall_dispatch` 调用,
/// 使 functions 层无需直接接触 unsafe FFI。
///
/// # 安全约束
/// - 调用方保证 `num` 是有效的系统调用号。
/// - 参数 `a0..a3` 来自用户态寄存器, 由 `UserContext` 提取。
pub fn dispatch_syscall(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    // SAFETY: syscall_dispatch 是 unsafe extern "C" 因为处理原始用户态参数.
    // 框架 (TCB) 负责在传递给 functions 之前校验这些参数.
    unsafe { crate::privileged::syscall::syscall_dispatch(num, a0, a1, a2, a3, a4, a5) }
}
