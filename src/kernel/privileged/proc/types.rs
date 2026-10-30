#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯类型定义和常量。
//! 进程类型定义 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义于 T6-2 (2026-06-16) 迁至 `functions::proc::types`, 本文件仅
//! re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：
//! PID/TID/ProcessState/Priority/Context 是**进程机制的核心类型**, 被
//! privileged `user_proc/process/scheduler` 消费 + functions 广泛消费 —
//! 属机制项, 迁回 (依赖闭包仅 `privileged::config`, 反转后同子树)。
//!
//! functions 侧改 `pub use crate::privileged::proc::types::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。

#![deny(missing_docs)]

/// 进程标识符 (PID) 的底层整数类型。
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
    /// 已创建: PCB 已分配, 资源初始化中 (尚不可运行)
    Created = 0,
    /// 就绪: 除 CPU 外资源就绪, 等待调度
    Ready = 1,
    /// 运行中: 正在 CPU 上执行
    Running = 2,
    /// 阻塞: 等待事件 (I/O / 子进程 / 信号 / 睡眠)
    Blocked = 3,
    /// 僵尸: 已退出但 PCB 保留供父进程回收
    Zombie = 4,
    /// 已终止: PCB 已回收, PID 可重用
    Terminated = 5,
    /// 已冻结: 被挂起 (SIGSTOP / cgroup freezer / 调试断点)
    Frozen = 6,
}

impl ProcessState {
    /// 安全的从 u8 值转换为 `ProcessState`
    #[must_use]
    pub fn from_u8(value: u8) -> Self {
        match value {
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
    #[must_use]
    pub fn from_u32(value: u32) -> Self {
        Self::from_u8(crate::privileged::glue::low_u8_from_u32(value))
    }

    /// 获取状态名称 (用于日志和调试)
    #[must_use]
    pub fn name(self) -> &'static str {
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

    /// ✅ 检查进程是否存活 (未终止或僵尸)
    #[must_use]
    pub fn is_alive(self) -> bool {
        !matches!(self, Self::Zombie | Self::Terminated)
    }

    /// ✅ 检查进程是否可以被唤醒 (从 Frozen 解冻后应转到的状态)
    #[must_use]
    pub fn thaw_target_state(self) -> Option<Self> {
        match self {
            Self::Frozen => Some(Self::Ready), // 默认解冻到 Ready
            _ => None,
        }
    }
}

/// 进程阻塞原因 — 记录线程因何事件进入阻塞态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// 等待 I/O 完成
    WaitingForIo = 0,
    /// 等待子进程状态变化
    WaitingForChild = 1,
    /// 等待信号
    WaitingForSignal = 2,
    /// 定时睡眠
    Sleeping = 3,
    /// 等待 futex 唤醒
    FutexWait = 4,
    /// 未知原因
    Unknown = 255,
}

impl BlockReason {
    /// 由原始 `u8` 值解析阻塞原因 (未知值回落为 `Unknown`)
    #[must_use]
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

/// 进程优先级等级 — 从 Idle 到 `RealTime` 共五档, 供调度器使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPriority {
    /// 空闲级 (最低)
    Idle = 0,
    /// 低优先级
    Low = 1,
    /// 普通优先级
    Normal = 2,
    /// 高优先级
    High = 3,
    /// 实时优先级 (最高)
    RealTime = 4,
}

impl ProcessPriority {
    /// 由原始 `u32` 值解析优先级 (未知值回落为 `Normal`)
    #[must_use]
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Low,
            3 => Self::High,
            4 => Self::RealTime,
            _ => Self::Normal,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    /// 进程标志位 (内核态 / 被跟踪 / 已停止)
    pub struct ProcessFlags: u32 {
        /// 内核线程标志
        const IS_KERNEL = 1 << 0;
        /// 被跟踪 (ptrace 等调试器附加) 标志
        const IS_TRACED = 1 << 1;
        /// 已停止 (收到停止信号) 标志
        const IS_STOPPED = 1 << 2;
    }
}

/// 进程/线程的 CPU 上下文 — 保存切换时需恢复的通用寄存器、RIP/RSP、CR3 与段寄存器。
///
/// `align(16)` 不可省: `switch.asm` 以 `fxsave/fxrstor [ctx + 144]` 读写
/// `fpu_state`, 而 FXSAVE/FXRSTOR 要求 512 字节区域自身 16 字节对齐. 仅靠
/// `fpu_pad` 只能把字段偏移钉在 144 (=16×9), 结构体基址若只保证 8 对齐, 内层
/// 区域仍是 8 mod 16 → #GP. 注意: 不能靠"嵌入位置碰巧 16 对齐"侥幸 —— P-61
/// 乙给 `IrqSpinLock` 补 `repr(C)` 改变相邻布局后, `PerCpuSched.scratch_ctx`
/// 的 16 对齐被打破, x86_64 首次 AP 切换在 `fxsave` 处稳定 #GP.
#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
pub struct ProcessContext {
    /// 通用寄存器 r15
    pub r15: u64,
    /// 通用寄存器 r14
    pub r14: u64,
    /// 通用寄存器 r13
    pub r13: u64,
    /// 通用寄存器 r12
    pub r12: u64,
    /// 通用寄存器 rbx (被调用者保存)
    pub rbx: u64,
    /// 栈帧基址 rbp
    pub rbp: u64,
    /// 通用寄存器 rax (fork 返回值的传递通道)
    pub rax: u64, // ✅ fork 返回值控制
    /// 指令指针 rip
    pub rip: u64,
    /// 栈指针 rsp
    pub rsp: u64,
    /// 标志寄存器 rflags
    pub rflags: u64,
    /// 页表基址 cr3
    pub cr3: u64,
    /// 代码段选择子 cs
    pub cs: u64,
    /// 数据段选择子 ds
    pub ds: u64,
    /// 附加数据段选择子 es
    pub es: u64,
    /// 段选择子 fs
    pub fs: u64,
    /// 段选择子 gs
    pub gs: u64,
    /// 栈段选择子 ss
    pub ss: u64,
    /// 填充字段，确保 `fpu_state` 16 字节对齐
    /// fxsave/fxrstor 要求内存地址 16 字节对齐
    pub fpu_pad: u64,
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
    /// syscall/中断栈上的 `InterruptFrame` (schedule 后 iretq 恢复), 不受影响.
    /// 但**首次被调度的进程** (如 fork 的子进程) 由 `process_switch_asm` 直接 iretq
    /// 进用户态, 无 `InterruptFrame` → 这些寄存器是调度器残留. 若用户代码依赖
    /// fork 返回后 rdi 等保持父进程值 (如 init 的 write 用 fork 前设置的 rdi=1),
    /// 残留值会导致 syscall 参数错乱 (fd=0 失败).
    ///
    /// 由 `proc_save_user_regs` 在每次 syscall 入口保存 (取 `InterruptFrame` 的
    /// rdi/rsi/rdx/rcx/r8/r9/r10/r11), fork/clone 复制 context 时继承, 子进程
    /// 首次切换时由 `process_switch_asm` 恢复. 布局偏移 672 (`fpu_state` 144+512,
    /// fpcr 656, fpsr 664).
    pub extra_regs: [u64; 8],
}

// FXSAVE/FXRSTOR 硬约束 (P-61 乙 暴露): `switch.asm` 以 `[ctx + 144]` 访问
// `fpu_state`, 要求结构体基址 16 对齐且该字段偏移恒为 144; 任一失效即 #GP.
const _: () = {
    assert!(core::mem::align_of::<ProcessContext>() == 16);
    assert!(core::mem::offset_of!(ProcessContext, fpu_state) == 144);
};

impl ProcessContext {
    /// 构造全零上下文
    #[must_use]
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
            fpu_pad: 0,
            fpu_state: [0; 64],
            fpcr: 0,
            fpsr: 0,
            extra_regs: [0; 8],
        }
    }

    /// 把上下文设置为用户态 (x86_64 Ring 3 段选择子与初始 rflags)
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

// 线程级调度词汇 (ThreadPriority / ThreadState) 随 SchedulerEx 并行实现
// 一并在 P-29 route C 切除: 进程级模型使用 ProcessPriority / ProcessState.
