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

#![deny(missing_docs)]

/// `x86_64` 用户态寄存器快照
///
/// 由 `isr.asm` 的 `int 0x80` / `syscall` stub 在栈上填充.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext {
    /// 通用寄存器 RAX (系统调用号 / 返回值)
    pub rax: u64,
    /// 通用寄存器 RBX
    pub rbx: u64,
    /// 通用寄存器 RCX
    pub rcx: u64,
    /// 通用寄存器 RDX (参数 2)
    pub rdx: u64,
    /// 通用寄存器 RSI (参数 1)
    pub rsi: u64,
    /// 通用寄存器 RDI (参数 0)
    pub rdi: u64,
    /// 通用寄存器 RBP
    pub rbp: u64,
    /// 通用寄存器 R8 (参数 4)
    pub r8: u64,
    /// 通用寄存器 R9 (参数 5)
    pub r9: u64,
    /// 通用寄存器 R10 (参数 3)
    pub r10: u64,
    /// 通用寄存器 R11
    pub r11: u64,
    /// 通用寄存器 R12
    pub r12: u64,
    /// 通用寄存器 R13
    pub r13: u64,
    /// 通用寄存器 R14
    pub r14: u64,
    /// 通用寄存器 R15
    pub r15: u64,
    /// 用户态返回地址 RIP
    pub rip: u64,
    /// 用户态标志寄存器 RFLAGS
    pub rflags: u64,
    /// 用户态栈指针 RSP
    pub rsp: u64,
}

/// aarch64 用户态寄存器快照
///
/// 由 `exception.rs` 的同步异常处理在栈上填充.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext {
    /// 通用寄存器 X0 (返回值)
    pub x0: u64,
    /// 通用寄存器 X1 (参数 1)
    pub x1: u64,
    /// 通用寄存器 X2 (参数 2)
    pub x2: u64,
    /// 通用寄存器 X3 (参数 3)
    pub x3: u64,
    /// 通用寄存器 X4 (参数 4)
    pub x4: u64,
    /// 通用寄存器 X5 (参数 5)
    pub x5: u64,
    /// 通用寄存器 X6
    pub x6: u64,
    /// 通用寄存器 X7
    pub x7: u64,
    /// 通用寄存器 X8 (系统调用号)
    pub x8: u64,
    /// 通用寄存器 X9
    pub x9: u64,
    /// 通用寄存器 X10
    pub x10: u64,
    /// 通用寄存器 X11
    pub x11: u64,
    /// 通用寄存器 X12
    pub x12: u64,
    /// 通用寄存器 X13
    pub x13: u64,
    /// 通用寄存器 X14
    pub x14: u64,
    /// 通用寄存器 X15
    pub x15: u64,
    /// 通用寄存器 X16
    pub x16: u64,
    /// 通用寄存器 X17
    pub x17: u64,
    /// 通用寄存器 X18
    pub x18: u64,
    /// 异常返回地址 (`ELR_EL1`)
    pub elr_el1: u64, // exception link register (返回地址)
    /// 已保存的程序状态寄存器 (`SPSR_EL1`)
    pub spsr_el1: u64, // 已保存的程序状态寄存器 (SPSR)
    /// 用户态栈指针 (`SP_EL0`)
    pub sp_el0: u64, // user stack pointer
}

impl UserContext {
    /// 系统调用号
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn syscall_number(&self) -> u64 {
        self.rax
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 系统调用号
    #[must_use]
    pub fn syscall_number(&self) -> u64 {
        self.x8
    }

    /// 设置返回值
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub fn set_return_value(&mut self, val: u64) {
        self.rax = val;
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 设置返回值
    pub fn set_return_value(&mut self, val: u64) {
        self.x0 = val;
    }

    /// 参数 0
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn arg0(&self) -> u64 {
        self.rdi
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 参数 0
    #[must_use]
    pub fn arg0(&self) -> u64 {
        self.x0
    }

    /// 参数 1
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn arg1(&self) -> u64 {
        self.rsi
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 参数 1
    #[must_use]
    pub fn arg1(&self) -> u64 {
        self.x1
    }

    /// 参数 2
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn arg2(&self) -> u64 {
        self.rdx
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 参数 2
    #[must_use]
    pub fn arg2(&self) -> u64 {
        self.x2
    }

    /// 参数 3
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn arg3(&self) -> u64 {
        self.r10
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 参数 3
    #[must_use]
    pub fn arg3(&self) -> u64 {
        self.x3
    }

    /// 参数 4
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn arg4(&self) -> u64 {
        self.r8
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 参数 4
    #[must_use]
    pub fn arg4(&self) -> u64 {
        self.x4
    }

    /// 参数 5
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn arg5(&self) -> u64 {
        self.r9
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 参数 5
    #[must_use]
    pub fn arg5(&self) -> u64 {
        self.x5
    }

    /// 用户态栈指针
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn user_sp(&self) -> u64 {
        self.rsp
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 用户态栈指针
    #[must_use]
    pub fn user_sp(&self) -> u64 {
        self.sp_el0
    }

    /// 用户态返回地址
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[must_use]
    pub fn user_ip(&self) -> u64 {
        self.rip
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    /// 用户态返回地址
    #[must_use]
    pub fn user_ip(&self) -> u64 {
        self.elr_el1
    }
}
