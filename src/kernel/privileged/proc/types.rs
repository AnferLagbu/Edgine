#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯类型定义和常量。
//! 进程类型定义 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义于 T6-2 (2026-06-16) 迁至 `functions::proc::types`, 本文件仅
//! re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：
//! PID/TID/ProcessState/Priority/Context 是**进程机制的核心类型**, 被
//! privileged user_proc/process/scheduler 消费 + functions 广泛消费 —
//! 属机制项, 迁回 (依赖闭包仅 privileged::config, 反转后同子树)。
//!
//! functions 侧改 `pub use crate::privileged::proc::types::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。

pub type Pid = u32;
/// 线程标识符 (TID) 的底层整数类型。
pub type Tid = u32;

/// 进程标识符包装类型 — 对裸 `Pid` 做类型区分, 避免与 TID 混用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessId(pub Pid);

/// 线程标识符包装类型 — 对裸 `Tid` 做类型区分, 避免与 PID 混用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThreadId(pub Tid);

/// ✅ 优化后的进程状态模型 (七状态完整实现)
///
/// 状态生命周期:
/// Created → Ready → Running ↔ Blocked  // 进程状态机
///                  ↓         ↓
///                Frozen   Zombie → Terminated  // 终止分支
///
/// 每个状态的含义:
/// - Created:    PCB 已分配, 资源初始化中 (尚未可运行)
/// - Ready:      除 CPU 外所有资源就绪, 在 MLFQ 队列中等待
/// - Running:    正在 CPU 上执行指令
/// - Blocked:    等待事件 (I/O/子进程/信号/睡眠)
/// - Zombie:     已调用 `exit()`, PCB 保留供父进程 `wait()`
/// - Terminated: PCB 已被回收, PID 可重用
/// - Frozen:     被挂起 (SIGSTOP/cgroup freezer/调试断点)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Created = 0,
    Ready = 1,
    Running = 2,
    Blocked = 3,
    Zombie = 4,
    Terminated = 5,
    Frozen = 6,
}

impl ProcessState {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 安全的从 u8 值转换为 `ProcessState`
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Created,
            1 => Self::Ready,
            2 => Self::Running,
            3 => Self::Blocked,
            4 => Self::Zombie,
            5 => Self::Terminated,
            6 => Self::Frozen,
            _ => Self::Created, // 无效值安全回退
        }
    }

    /// 从 u32 值转换 (兼容 `AtomicU32` 存储)
    pub fn from_u32(value: u32) -> Self {
        Self::from_u8(value as u8)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取状态名称 (用于日志和调试)
    pub fn name(&self) -> &'static str {
        match self {
            Self::Created => "Created",
            Self::Ready => "Ready",
            Self::Running => "Running",
            Self::Blocked => "Blocked",
            Self::Zombie => "Zombie",
            Self::Terminated => "Terminated",
            Self::Frozen => "Frozen",
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// ✅ 检查进程是否可调度 (在就绪队列或运行中)
    pub fn is_runnable(&self) -> bool {
        matches!(self, Self::Ready | Self::Running)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// ✅ 检查进程是否存活 (未终止或僵尸)
    pub fn is_alive(&self) -> bool {
        !matches!(self, Self::Zombie | Self::Terminated)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// ✅ 检查进程是否可以被冻结
    pub fn can_freeze(&self) -> bool {
        matches!(self, Self::Running | Self::Ready | Self::Blocked)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// ✅ 检查进程是否可以被唤醒 (从 Frozen 解冻后应转到的状态)
    pub fn thaw_target_state(&self) -> Option<Self> {
        match self {
            Self::Frozen => Some(Self::Ready), // 默认解冻到 Ready
            _ => None,
        }
    }
}

/// 进程阻塞原因 — 记录线程因何事件进入阻塞态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    WaitingForIo = 0,
    WaitingForChild = 1,
    WaitingForSignal = 2,
    Sleeping = 3,
    FutexWait = 4,
    Unknown = 255,
}

impl BlockReason {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::WaitingForIo,
            1 => Self::WaitingForChild,
            2 => Self::WaitingForSignal,
            3 => Self::Sleeping,
            4 => Self::FutexWait,
            _ => Self::Unknown,
        }
    }
}

/// 进程优先级等级 — 从 Idle 到 RealTime 共五档, 供调度器使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPriority {
    Idle = 0,
    Low = 1,
    Normal = 2,
    High = 3,
    RealTime = 4,
}

impl ProcessPriority {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Low,
            2 => Self::Normal,
            3 => Self::High,
            4 => Self::RealTime,
            _ => Self::Normal,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProcessFlags: u32 {
        const IS_KERNEL = 1 << 0;
        const IS_TRACED = 1 << 1;
        const IS_STOPPED = 1 << 2;
    }
}

/// 进程/线程的 CPU 上下文 — 保存切换时需恢复的通用寄存器、RIP/RSP、CR3 与段寄存器。
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProcessContext {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rax: u64, // ✅ fork 返回值控制
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
    pub cr3: u64,
    pub cs: u64,
    pub ds: u64,
    pub es: u64,
    pub fs: u64,
    pub gs: u64,
    pub ss: u64,
    /// 填充字段，确保 `fpu_state` 16 字节对齐
    /// fxsave/fxrstor 要求内存地址 16 字节对齐
    pub _fpu_pad: u64,
    /// FPU/SSE 状态预留区域 (512 bytes for V0-V31)
    ///
    /// Phase 1: 预留空间，不实际保存/恢复
    /// Phase 2: `x86_64` 使用 xsave/xrstor 保存 x87 + XMM
    /// Phase 3: aarch64 使用 stp/ldp 保存 V0-V31 + FPCR + FPSR
    /// Phase 4: 实现 lazy FPU 切换 (CR0.TS 位优化)
    pub fpu_state: [u64; 64],
    /// aarch64 浮点控制寄存器 (Floating-point Control Register)
    pub fpcr: u64,
    /// aarch64 浮点状态寄存器 (Floating-point Status Register)
    pub fpsr: u64,
    /// B05-55 修复: 额外 caller-saved 寄存器 (x86_64 调度切换不保存的).
    ///
    /// `process_switch_asm` 仅保存/恢复 r15-r12/rbx/rbp/rax/rip/rsp/rflags/cr3/段,
    /// **不含** rdi/rsi/rdx/rcx/r8-r11. 已运行过的进程返回用户态时, 寄存器来自
    /// syscall/中断栈上的 InterruptFrame (schedule 后 iretq 恢复), 不受影响.
    /// 但**首次被调度的进程** (如 fork 的子进程) 由 process_switch_asm 直接 iretq
    /// 进用户态, 无 InterruptFrame → 这些寄存器是调度器残留. 若用户代码依赖
    /// fork 返回后 rdi 等保持父进程值 (如 init 的 write 用 fork 前设置的 rdi=1),
    /// 残留值会导致 syscall 参数错乱 (fd=0 失败).
    ///
    /// 由 `proc_save_user_regs` 在每次 syscall 入口保存 (取 InterruptFrame 的
    /// rdi/rsi/rdx/rcx/r8/r9/r10/r11), fork/clone 复制 context 时继承, 子进程
    /// 首次切换时由 process_switch_asm 恢复. 布局偏移 672 (fpu_state 144+512,
    /// fpcr 656, fpsr 664).
    pub extra_regs: [u64; 8],
}

impl ProcessContext {
    pub const fn new() -> Self {
        Self {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbx: 0,
            rbp: 0,
            rax: 0,
            rip: 0,
            rsp: 0,
            rflags: 0,
            cr3: 0,
            cs: 0,
            ds: 0,
            es: 0,
            fs: 0,
            gs: 0,
            ss: 0,
            _fpu_pad: 0,
            fpu_state: [0; 64],
            fpcr: 0,
            fpsr: 0,
            extra_regs: [0; 8],
        }
    }

    pub fn set_user_mode(&mut self) {
        self.cs = 0x23;
        self.ds = 0x1B;
        self.es = 0x1B;
        self.fs = 0x1B;
        self.gs = 0x1B;
        self.ss = 0x1B;
        self.rflags = 0x202;
    }
}

impl Default for ProcessContext {
    fn default() -> Self {
        Self::new()
    }
}

// === 进程规模常量 (统一从 config.rs 引用) ===
//
// 集中式 re-export: 所有 proc 子模块 (types/process/thread/user_proc) 共享同一组常量,
// 避免分散定义与影子覆盖。user_proc.rs 等子模块通过 `use super::types::*;` 引入。
pub use crate::privileged::config::{
    // 栈规模
    KERNEL_STACK_SIZE,
    MAX_OPEN_FILES,
    // 进程规模
    MAX_PROCESSES,
    // 内存页
    PAGE_SIZE,
    // 调度参数
    SCHED_BOOST_INTERVAL,
    SCHED_LEVEL_0_QUANTUM,
    SCHED_LEVEL_1_QUANTUM,
    SCHED_LEVEL_2_QUANTUM,
    SCHED_LEVEL_3_QUANTUM,
    SCHED_RT_WATCHDOG_TICKS,
    USER_CODE_BASE,
    USER_KSTACK_SIZE,
    USER_STACK_GUARD,
    USER_STACK_MAX_SIZE,
    USER_STACK_SIZE,
    USER_STACK_TOP,
};

/// 线程调度优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThreadPriority {
    Idle = 0,
    Low = 1,
    Normal = 2,
    High = 3,
    Realtime = 4,
}

impl ThreadPriority {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Low,
            2 => Self::Normal,
            3 => Self::High,
            4 => Self::Realtime,
            _ => Self::Normal,
        }
    }
}

/// 线程状态 (七状态模型)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Created = 0,
    Ready = 1,
    Running = 2,
    Blocked = 3,
    Zombie = 4,
    Terminated = 5,
    Frozen = 6,
}

impl ThreadState {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Created,
            1 => Self::Ready,
            2 => Self::Running,
            3 => Self::Blocked,
            4 => Self::Zombie,
            5 => Self::Terminated,
            6 => Self::Frozen,
            _ => Self::Created,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_runnable(&self) -> bool {
        matches!(self, Self::Ready | Self::Running)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_alive(&self) -> bool {
        !matches!(self, Self::Zombie | Self::Terminated)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn can_freeze(&self) -> bool {
        matches!(self, Self::Running | Self::Ready | Self::Blocked)
    }
}
