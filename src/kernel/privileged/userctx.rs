//! `UserContext` — 用户态 CPU 寄存器快照 (TCB)
//!
//! ## DECISION-039 迁移记录 (2026-08-03)
//!
//! 原属 `functions::userctx`, 按"用户态 CPU 状态按 I3 不变式归 privileged"原则
//! 迁回 `privileged::userctx`. `functions::userctx` 改为反向 re-export 保持调用方兼容.
//!
//! 纯类型定义 (寄存器快照结构体), 0 unsafe, 0 外部依赖.
//! `privileged::usermode::enter_user_mode` 通过本类型安全读取用户态寄存器
//! (TCB 内部 unsafe 已通过 `// SAFETY:` 注释保护).
//!
//! ## 与 Asterinas OSTD `UserContext` 的关系
//!
//! 等价于 OSTD 的 `UserContext` + `crate::arch::Registers`.
//! x86_64 和 aarch64 的寄存器布局不同, 通过 `#[cfg]` 适配.
//!
//! ## SAFETY 不变量
//!
//! - `UserContext` 实例由 asm stub 创建 (架构特定), 仅传递指针给 Rust.
//! - privileged 层的 unsafe 代码可读取/写入字段, functions 层只通过 `UserContext`
//!   类型化访问, 不直接操作 asm stub.

/// `x86_64` 用户态寄存器快照
///
/// 由 `isr.asm` 的 `int 0x80` / `syscall` stub 在栈上填充.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
}

/// aarch64 用户态寄存器快照
///
/// 由 `exception.rs` 的同步异常处理在栈上填充.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext {
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,
    pub elr_el1: u64,  // exception link register (返回地址)
    pub spsr_el1: u64, // 已保存的程序状态寄存器 (SPSR)
    pub sp_el0: u64,   // user stack pointer
}

impl UserContext {
    /// 系统调用号
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn syscall_number(&self) -> u64 {
        self.rax
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn syscall_number(&self) -> u64 {
        self.x8
    }

    /// 设置返回值
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn set_return_value(&mut self, val: u64) {
        self.rax = val;
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn set_return_value(&mut self, val: u64) {
        self.x0 = val;
    }

    /// 参数 0
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn arg0(&self) -> u64 {
        self.rdi
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn arg0(&self) -> u64 {
        self.x0
    }

    /// 参数 1
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn arg1(&self) -> u64 {
        self.rsi
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn arg1(&self) -> u64 {
        self.x1
    }

    /// 参数 2
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn arg2(&self) -> u64 {
        self.rdx
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn arg2(&self) -> u64 {
        self.x2
    }

    /// 参数 3
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn arg3(&self) -> u64 {
        self.r10
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn arg3(&self) -> u64 {
        self.x3
    }

    /// 参数 4
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn arg4(&self) -> u64 {
        self.r8
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn arg4(&self) -> u64 {
        self.x4
    }

    /// 参数 5
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn arg5(&self) -> u64 {
        self.r9
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn arg5(&self) -> u64 {
        self.x5
    }

    /// 用户态栈指针
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn user_sp(&self) -> u64 {
        self.rsp
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn user_sp(&self) -> u64 {
        self.sp_el0
    }

    /// 用户态返回地址
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub fn user_ip(&self) -> u64 {
        self.rip
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    pub fn user_ip(&self) -> u64 {
        self.elr_el1
    }
}
