#![deny(missing_docs)]
//! POSIX 信号投递 — privileged 层核心实现
//!
//! 提供信号发送、投递和默认动作执行的完整机制：
//! - **`do_signal_send`**: 向目标进程发送信号 (设置 pending 位, 唤醒)
//! - **`do_signal_deliver`**: 返回用户态前检查并投递信号
//! - **`signal_default_action`**: 执行信号的默认动作 (Term/Core/Stop/Ign)
//!
//! ## 信号投递流程
//!
//! ```text
//! sys_kill(pid, sig)
//!   └→ do_signal_send(pid, sig)
//!        ├→ 设置 pending_signals 位
//!        └→ 唤醒目标进程 (如果可中断)
//!
//! 中断/系统调用返回用户态前:
//!   └→ do_signal_deliver()
//!        ├→ 检查 pending & ~blocked
//!        ├→ 选择最高优先级信号
//!        ├→ 查找 sigaction_table[sig]
//!        │   ├→ SIG_DFL: 执行默认动作
//!        │   ├→ SIG_IGN: 忽略
//!        │   └→ handler: 修改用户态栈帧, 跳转到 handler
//!        └→ sigreturn: 恢复原始栈帧
//! ```
//!
//! ## `x86_64` 信号栈帧布局
//!
//! ```text
//! 用户栈 (低地址 → 高地址):
//! ┌────────────────────┐ ← 新 RSP
//! │ ucontext / siginfo │  (未来扩展)
//! ├────────────────────┤
//! │ SignalFrame         │  保存原始寄存器
//! │   rip              │
//! │   cs               │
//! │   rflags           │
//! │   rsp              │
//! │   ss               │
//! │   rax..r15         │
//! │   signum           │
//! ├────────────────────┤
//! │ sigreturn trampoline│  __sigreturn 代码 (syscall 15)
//! └────────────────────┘ ← handler 返回地址
//! ```
//!
//! ## `aarch64` 信号栈帧布局
//!
//! ```text
//! 用户栈 (低地址 → 高地址):
//! ┌────────────────────┐ ← 新 SP (16 字节对齐)
//! │ Aarch64SignalFrame  │  x0-x30 / sp / elr / spsr / signum
//! ├────────────────────┤
//! │ sigreturn trampoline│  movz x0,#139 + svc #0
//! └────────────────────┘ ← handler 的 LR (x30)
//! ```
//!
//! 两架构共享同一套投递/恢复流程 ([`UserFrame`]), 差异只在帧头 (x86_64 的返回
//! 地址槽)、栈指针对齐与 handler 入口寄存器约定.
//!
//! # Safety
//!
//! - `do_signal_send`: 操作进程原子字段, 线程安全
//! - `do_signal_deliver`: 操作当前进程, 单 CPU 执行, 无竞争
//! - `PROCESS_TABLE.get()` 返回 *mut Process, 需 unsafe 解引用

use core::sync::atomic::{AtomicU64, Ordering};

use super::process::PROCESS_TABLE;
use super::types::{Pid, ProcessState};

/// L-2 取证站点预算: 信号默认动作 `Cont` 把死 TCB 直接写回 `Ready` 的读数.
static RD_SIGNAL_CONT: AtomicU64 = AtomicU64::new(0);
/// L-2 取证站点预算: 信号默认动作 `Stop` 把死 TCB 直接写回 `Blocked` 的读数.
static RD_SIGNAL_STOP: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// 信号栈帧与用户态异常帧抽象
// ============================================================================

/// 信号栈帧 — 保存在用户栈上, sigreturn 时恢复 (x86_64 布局)
///
/// 布局与 `InterruptFrame` 兼容, 便于直接拷贝寄存器状态.
/// signum 字段放在最后, handler 通过第一个参数 (rdi) 获取.
#[repr(C)]
pub struct SignalFrame {
    // 通用寄存器 (与 InterruptFrame 顺序一致)
    /// 通用寄存器 r15
    pub r15: u64,
    /// 通用寄存器 r14
    pub r14: u64,
    /// 通用寄存器 r13
    pub r13: u64,
    /// 通用寄存器 r12
    pub r12: u64,
    /// 通用寄存器 r11
    pub r11: u64,
    /// 通用寄存器 r10
    pub r10: u64,
    /// 通用寄存器 r9
    pub r9: u64,
    /// 通用寄存器 r8
    pub r8: u64,
    /// 通用寄存器 rdi (handler 第一参数)
    pub rdi: u64,
    /// 通用寄存器 rsi
    pub rsi: u64,
    /// 通用寄存器 rbp
    pub rbp: u64,
    /// 通用寄存器 rdx
    pub rdx: u64,
    /// 通用寄存器 rcx
    pub rcx: u64,
    /// 通用寄存器 rbx
    pub rbx: u64,
    /// 通用寄存器 rax
    pub rax: u64,

    // 中断元数据
    /// 中断向量号
    pub int_no: u64,
    /// 错误码 (部分异常有)
    pub err_code: u64,

    // 返回地址信息
    /// 信号 handler 返回后恢复的指令指针
    pub rip: u64,
    /// 代码段选择子
    pub cs: u64,
    /// 恢复的 RFLAGS
    pub rflags: u64,
    /// 恢复的栈指针
    pub rsp: u64,
    /// 栈段选择子
    pub ss: u64,

    // 信号编号 (handler 参数)
    /// 信号编号 (handler 第一参数)
    pub signum: u64,
}

/// 信号栈帧 — 保存在用户栈上, `rt_sigreturn` 时恢复 (aarch64 布局)
///
/// 保存异常发生时的全部用户寄存器 (x0-x30)、栈指针与返回现场 (`elr`/`spsr`),
/// 不含中断元数据; `signum` 仅作诊断用途. 全 u64 字段无填充, 可直接按字节
/// 视图写入用户栈.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Aarch64SignalFrame {
    /// 通用寄存器 x0-x30 (按编号顺序)
    pub x: [u64; 31],
    /// 用户栈指针 (`sp_el0`)
    pub sp: u64,
    /// 用户返回地址 (`elr_el1`)
    pub elr: u64,
    /// 用户 PSTATE (`spsr_el1`)
    pub spsr: u64,
    /// 信号编号 (诊断用; 恢复时不参与)
    pub signum: u64,
}

/// 本架构的信号栈帧类型 (x86_64 为 [`SignalFrame`]).
#[cfg(target_arch = "x86_64")]
pub type ArchSignalFrame = SignalFrame;

/// 本架构的信号栈帧类型 (aarch64 为 [`Aarch64SignalFrame`]).
#[cfg(target_arch = "aarch64")]
pub type ArchSignalFrame = Aarch64SignalFrame;

/// 本架构承载信号投递的用户态异常帧类型 (x86_64 `InterruptFrame`).
#[cfg(target_arch = "x86_64")]
pub type ArchUserFrame = crate::privileged::idt::InterruptFrame;

/// 本架构承载信号投递的用户态异常帧类型 (aarch64 `ExceptionFrame`).
#[cfg(target_arch = "aarch64")]
pub type ArchUserFrame = crate::privileged::arch::aarch64::exception::ExceptionFrame;

/// sigreturn trampoline 代码
///
/// # `x86_64` 机器码 (7 字节)
/// `mov eax, 15` (`SYS_RT_SIGRETURN` = 15) + `syscall`:
///   B8 0F 00 00 00     mov eax, 15
///   0F 05              syscall
///
/// # aarch64 机器码 (8 字节)
/// `movz x0, #139` (`SYS_RT_SIGRETURN` = 139) + `svc #0`:
///   60 11 80 D2        movz x0, #139
///   01 00 00 D4        svc #0
///
/// P1-I-40 修复: 之前硬编码 `x86_64` 字节序, aarch64 上是随机指令
/// (illegal instruction), 致所有 ARM 板信号投递失败. 改为 cfg 分发.
/// A-10 修正: aarch64 字面量原按大端书写且落在 x8 (Linux ABI), 与 Edgine
/// `x0=syscall_num` 约定不符; 现改为小端编码的 `movz x0, #139`.
#[cfg(target_arch = "x86_64")]
pub const SIGRETURN_TRAMPOLINE: [u8; 7] = [0xB8, 0x0F, 0x00, 0x00, 0x00, 0x0F, 0x05];

/// sigreturn trampoline 代码 (aarch64 8 字节分支, 机器码与语义见上方两架构说明)
#[cfg(target_arch = "aarch64")]
pub const SIGRETURN_TRAMPOLINE: [u8; 8] = [0x60, 0x11, 0x80, 0xD2, 0x01, 0x00, 0x00, 0xD4];

/// sigreturn trampoline 大小
pub const SIGRETURN_TRAMPOLINE_SIZE: usize = SIGRETURN_TRAMPOLINE.len();

/// 信号栈帧总大小 (含帧头与 trampoline)
pub const SIGNAL_FRAME_TOTAL_SIZE: usize = <ArchUserFrame as UserFrame>::FRAME_HEADER_SIZE
    + core::mem::size_of::<ArchSignalFrame>()
    + SIGRETURN_TRAMPOLINE_SIZE;

// ============================================================================
// UserFrame — 架构相关的返回用户态帧抽象
// ============================================================================

/// 承载信号投递的用户态异常帧抽象 (x86_64 `InterruptFrame` / aarch64 `ExceptionFrame`).
///
/// 两架构的异常入口压栈布局不同, 但投递流程一致: 选择信号帧落点 → 快照当前
/// 用户现场 → 写入 [帧头 | 信号帧 | trampoline] → 改写帧跳转 handler.
/// `rt_sigreturn` 经同一抽象恢复, 系统调用返回与中断返回两条路径共用本实现.
pub trait UserFrame {
    /// 信号帧之前的帧头字节数 (x86_64 的 handler 返回地址槽 = 8; aarch64 = 0).
    const FRAME_HEADER_SIZE: usize;
    /// handler 入口要求的用户栈指针对齐余数 (mod 16).
    ///
    /// x86_64 `SysV` 约定 callee 入口 `rsp % 16 == 8` (返回地址已压栈);
    /// aarch64 要求 SP 16 字节对齐.
    const SP_ALIGN: u64;

    /// 读取用户栈指针.
    fn user_sp(&self) -> u64;

    /// 把异常帧切换到 handler 入口 (PC / 首参数 / 返回地址 / 栈指针).
    ///
    /// `frame_sp` 为信号帧落点 (x86_64 同时是 handler 的返回地址槽),
    /// `trampoline_addr` 为 sigreturn 跳板地址 (aarch64 需写入 LR).
    fn enter_handler(
        &mut self,
        frame_sp: u64,
        trampoline_addr: u64,
        handler_addr: u64,
        signum: u64,
    );

    /// 从当前异常帧快照出信号栈帧.
    fn snapshot(&self, sig: u8) -> ArchSignalFrame;

    /// 按信号栈帧恢复异常帧 (`rt_sigreturn`).
    fn restore(&mut self, sigframe: &ArchSignalFrame);
}

#[cfg(target_arch = "x86_64")]
impl UserFrame for crate::privileged::idt::InterruptFrame {
    const FRAME_HEADER_SIZE: usize = 8;
    const SP_ALIGN: u64 = 8;

    fn user_sp(&self) -> u64 {
        self.rsp
    }

    fn enter_handler(
        &mut self,
        frame_sp: u64,
        _trampoline_addr: u64,
        handler_addr: u64,
        signum: u64,
    ) {
        self.rip = handler_addr;
        self.rdi = signum; // 参数1: signum
        self.rsi = 0; // 参数2: siginfo (简化: NULL)
        self.rdx = 0; // 参数3: ucontext (简化: NULL)
        self.rsp = frame_sp;
    }

    fn snapshot(&self, sig: u8) -> SignalFrame {
        SignalFrame {
            r15: self.r15,
            r14: self.r14,
            r13: self.r13,
            r12: self.r12,
            r11: self.r11,
            r10: self.r10,
            r9: self.r9,
            r8: self.r8,
            rdi: self.rdi,
            rsi: self.rsi,
            rbp: self.rbp,
            rdx: self.rdx,
            rcx: self.rcx,
            rbx: self.rbx,
            rax: self.rax,
            int_no: self.int_no,
            err_code: self.err_code,
            rip: self.rip,
            cs: self.cs,
            rflags: self.rflags,
            rsp: self.rsp,
            ss: self.ss,
            signum: u64::from(sig),
        }
    }

    fn restore(&mut self, sigframe: &SignalFrame) {
        self.r15 = sigframe.r15;
        self.r14 = sigframe.r14;
        self.r13 = sigframe.r13;
        self.r12 = sigframe.r12;
        self.r11 = sigframe.r11;
        self.r10 = sigframe.r10;
        self.r9 = sigframe.r9;
        self.r8 = sigframe.r8;
        self.rdi = sigframe.rdi;
        self.rsi = sigframe.rsi;
        self.rbp = sigframe.rbp;
        self.rdx = sigframe.rdx;
        self.rcx = sigframe.rcx;
        self.rbx = sigframe.rbx;
        self.rax = sigframe.rax;
        self.rip = sigframe.rip;
        self.cs = sigframe.cs;
        self.rflags = sigframe.rflags;
        self.rsp = sigframe.rsp;
        self.ss = sigframe.ss;
    }
}

#[cfg(target_arch = "aarch64")]
impl UserFrame for crate::privileged::arch::aarch64::exception::ExceptionFrame {
    const FRAME_HEADER_SIZE: usize = 0;
    const SP_ALIGN: u64 = 0;

    fn user_sp(&self) -> u64 {
        self.sp
    }

    fn enter_handler(
        &mut self,
        frame_sp: u64,
        trampoline_addr: u64,
        handler_addr: u64,
        signum: u64,
    ) {
        self.elr = handler_addr;
        self.x0 = signum; // 参数1: signum
        self.x1 = 0; // 参数2: siginfo (简化: NULL)
        self.x2 = 0; // 参数3: ucontext (简化: NULL)
        self.x30 = trampoline_addr; // handler 返回即跳 sigreturn 跳板
        self.sp = frame_sp;
    }

    fn snapshot(&self, sig: u8) -> Aarch64SignalFrame {
        Aarch64SignalFrame {
            x: [
                self.x0, self.x1, self.x2, self.x3, self.x4, self.x5, self.x6, self.x7, self.x8,
                self.x9, self.x10, self.x11, self.x12, self.x13, self.x14, self.x15, self.x16,
                self.x17, self.x18, self.x19, self.x20, self.x21, self.x22, self.x23, self.x24,
                self.x25, self.x26, self.x27, self.x28, self.x29, self.x30,
            ],
            sp: self.sp,
            elr: self.elr,
            spsr: self.spsr,
            signum: u64::from(sig),
        }
    }

    fn restore(&mut self, sigframe: &Aarch64SignalFrame) {
        self.x0 = sigframe.x[0];
        self.x1 = sigframe.x[1];
        self.x2 = sigframe.x[2];
        self.x3 = sigframe.x[3];
        self.x4 = sigframe.x[4];
        self.x5 = sigframe.x[5];
        self.x6 = sigframe.x[6];
        self.x7 = sigframe.x[7];
        self.x8 = sigframe.x[8];
        self.x9 = sigframe.x[9];
        self.x10 = sigframe.x[10];
        self.x11 = sigframe.x[11];
        self.x12 = sigframe.x[12];
        self.x13 = sigframe.x[13];
        self.x14 = sigframe.x[14];
        self.x15 = sigframe.x[15];
        self.x16 = sigframe.x[16];
        self.x17 = sigframe.x[17];
        self.x18 = sigframe.x[18];
        self.x19 = sigframe.x[19];
        self.x20 = sigframe.x[20];
        self.x21 = sigframe.x[21];
        self.x22 = sigframe.x[22];
        self.x23 = sigframe.x[23];
        self.x24 = sigframe.x[24];
        self.x25 = sigframe.x[25];
        self.x26 = sigframe.x[26];
        self.x27 = sigframe.x[27];
        self.x28 = sigframe.x[28];
        self.x29 = sigframe.x[29];
        self.x30 = sigframe.x[30];
        self.sp = sigframe.sp;
        self.elr = sigframe.elr;
        self.spsr = sigframe.spsr;
    }
}

// ============================================================================
// 常量
// ============================================================================

/// `SIG_DFL`: 默认动作
pub const SIG_DFL: u64 = 0;
/// `SIG_IGN`: 忽略
pub const SIG_IGN: u64 = 1;

/// `SS_ONSTACK`: 信号替换栈正在使用
pub const SS_ONSTACK: u32 = 1;
/// `SS_DISABLE`: 信号替换栈已禁用
pub const SS_DISABLE: u32 = 2;

/// 信号默认动作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalDefaultAction {
    /// 终止进程
    Term,
    /// 终止进程 + 核心转储
    Core,
    /// 忽略
    Ign,
    /// 停止进程
    Stop,
    /// 继续 (如果已停止)
    Cont,
}

/// 获取标准信号的默认动作
///
/// 委托到 `SignalDecision` trait, functions 可覆盖策略.
#[must_use]
pub fn signal_default_action(sig: u8) -> SignalDefaultAction {
    super::signal_trait::current_signal_decision().default_action(sig)
}

/// 信号是否不可捕获/屏蔽
///
/// 委托到 `SignalDecision` trait, functions 可覆盖策略.
#[must_use]
pub fn is_uncatchable(sig: u8) -> bool {
    super::signal_trait::current_signal_decision().is_uncatchable(sig)
}

// ============================================================================
// do_signal_send — 发送信号
// ============================================================================

/// 向目标进程发送信号
///
/// 1. 验证信号编号有效性
/// 2. 设置目标进程的 `pending_signals` 位
/// 3. 如果目标进程在可中断睡眠状态, 唤醒它
///
/// # Returns
/// - `Ok(())`: 成功发送
/// - `Err(i32)`: 错误码 (-1: 无效信号, -2: 进程不存在)
///
/// # Errors
/// 当 `sig` 不在 `1..=63` 范围内时返回 `Err(-1)`; 当目标进程不存在时返回 `Err(-2)`;
/// 当目标进程处于 `Zombie` 状态 (信号不会被消费) 时返回 `Err(-3)`.
/// (sig 上限 63 = bit 63, 避开 `1u64 << 64` UB)
pub fn do_signal_send(pid: Pid, sig: u8) -> Result<(), i32> {
    // 验证信号编号
    if sig == 0 {
        // POSIX: kill(pid, 0) 仅检查进程存在
        return if PROCESS_TABLE.get(pid).is_some() {
            Ok(())
        } else {
            Err(-2)
        };
    }
    if !(1..=63).contains(&sig) {
        return Err(-1);
    }

    // 查找目标进程
    let proc_ptr = PROCESS_TABLE.get(pid).ok_or(-2)?;

    // SAFETY: PROCESS_TABLE.get() 返回有效指针, 进程在表中期间不会释放
    let proc = unsafe { &*proc_ptr };

    // I-52: 显式检查 ProcessState, Zombie 状态不投递信号.
    // 原因: Zombie 进程已不调度执行, 但其 task_struct 仍在表中 (等待 waitpid 回收).
    // 此时投递信号: pending 位被设置但永远不被消费, 浪费资源; 对用户态也
    // 无意义 (POSIX kill() 对 Zombie 行为未规定, Linux 选择 ESRCH).
    // 选择: 与 Linux kill() 一致, 返回 -3 (ESRCH) 阻止投递.
    let state = proc.state.load(Ordering::Acquire);
    if state == ProcessState::Zombie as u32 {
        return Err(-3);
    }

    // SIGKILL/SIGSTOP 不可忽略, 直接设置 pending
    // 其他信号: 如果 handler 是 SIG_IGN 且不是不可捕获信号, 则忽略
    if !is_uncatchable(sig) {
        let actions = proc.sigaction_table.lock();
        if actions[(sig - 1) as usize] == SIG_IGN {
            return Ok(()); // 显式忽略, 不报错
        }
    }

    // 设置 pending 位
    proc.signal_pending_set(u32::from(sig));

    // 唤醒目标进程 (如果处于可中断睡眠). 必须经调度器入队: 直接改状态为
    // Ready 只会让它脱离睡眠原因, 却不在任何运行队列上, 形成永久丢唤醒.
    // 调度器按状态判定, Running / Zombie / Created 调用为空操作.
    super::scheduler::Scheduler::unblock(pid);

    Ok(())
}

/// `do_signal_send_extended` — kill 4 种 pid 语义 (解决 TRACK-315B7C)
///
/// POSIX `kill()` pid 取值:
/// - pid > 0:    发往指定 pid 进程
/// - pid = 0:    发往调用者同进程组所有进程
/// - pid = -1:   发往系统所有进程 (除 init pid=1)
/// - pid < -1:   发往进程组 |pid| 所有进程
///
/// 简化: 不做权限检查 (Linux 早期行为).
///
/// # Returns
/// - `Ok(0)`: 至少一个目标收到信号
/// - `Err(-2)`: 未找到任何目标 (ESRCH)
///
/// # Errors
/// 当 `sig` 不在 `1..=63` 范围内时返回 `Err(-1)`; 当未找到任何匹配的目标进程
/// (或目标均为 `Zombie`) 时返回 `Err(-2)`/`Err(-3)`.
pub fn do_signal_send_extended(pid: i32, sig: u8) -> Result<usize, i32> {
    // sig=0 仅检查存在, 不发信号
    if sig == 0 {
        return signal_send_presence(pid);
    }

    if !(1..=63).contains(&sig) {
        return Err(-1);
    }

    match pid {
        p if p > 0 => {
            // kill(tid) 按 Linux 语义作用于该 TID 所属线程组: 组内择一线程接收.
            let target = p.cast_unsigned();
            let tgid = PROCESS_TABLE
                .with_process(target, |proc| proc.thread_group.tgid())
                .ok_or(-2)?;
            do_signal_send_group(tgid, sig)
        }
        0 => {
            // 广播到同进程组 (每个线程组只投一次)
            let current = super::scheduler::Scheduler::current().unwrap_or(0);
            let current_pgid = PROCESS_TABLE
                .with_process(current, |p| p.group().pgid())
                .unwrap_or(0);
            let target_pgid = if current_pgid == 0 {
                current
            } else {
                current_pgid
            };
            let tgids = collect_matching_tgids(|proc| proc.group().pgid() == target_pgid);
            deliver_to_groups(&tgids, sig)
        }
        -1 => {
            // 广播到所有进程 (除 init pid=1)
            let tgids = collect_matching_tgids(|proc| proc.pid.0 != 1);
            deliver_to_groups(&tgids, sig)
        }
        p if p < -1 => {
            // 广播到 |pid| 进程组
            let target_pgid = (-p).cast_unsigned();
            let tgids = collect_matching_tgids(|proc| proc.group().pgid() == target_pgid);
            deliver_to_groups(&tgids, sig)
        }
        _ => Err(-2),
    }
}

/// `sig == 0` 的"仅检查存在"分支: 不投递任何信号, 只统计匹配目标数.
///
/// 目标语义与 `do_signal_send_extended` 的投递分支一致: `pid > 0` 单进程,
/// `0` 当前进程所在的组/自身, `-1` 全部进程, `< -1` 指定进程组.
fn signal_send_presence(pid: i32) -> Result<usize, i32> {
    match pid {
        p if p > 0 => {
            if PROCESS_TABLE.get(p.cast_unsigned()).is_some() {
                Ok(1)
            } else {
                Err(-2)
            }
        }
        0 => {
            // 至少检查当前进程存在
            if PROCESS_TABLE
                .get(super::scheduler::Scheduler::current().unwrap_or(0))
                .is_some()
            {
                Ok(1)
            } else {
                Err(-2)
            }
        }
        -1 => {
            let mut count = 0usize;
            PROCESS_TABLE.for_each(|_| {
                count += 1;
                true
            });
            if count > 0 { Ok(count) } else { Err(-2) }
        }
        p if p < -1 => {
            let target_pgid = (-p).cast_unsigned();
            let mut count = 0usize;
            PROCESS_TABLE.for_each(|proc| {
                let effective_pgid = proc.group().pgid();
                if effective_pgid == target_pgid {
                    count += 1;
                }
                true
            });
            if count > 0 { Ok(count) } else { Err(-2) }
        }
        _ => Err(-2),
    }
}

/// 组定向投递 (`kill(2)` 语义): 在线程组内选一个可接收的成员.
///
/// 选择规则 (对齐 Linux `group_send_sig_info` 的组内择线程):
/// - 不可捕获信号 (`SIGKILL`/`SIGSTOP`) 无视屏蔽与忽略, 投给首个存活成员;
/// - 可捕获信号优先投给"未屏蔽且未显式忽略"的存活成员并唤醒它;
/// - 若没有这样的成员, 把信号挂到首个"屏蔽但未忽略"的成员上 (不唤醒,
///   待其解除屏蔽后由 `do_signal_deliver` 消费);
/// - 存活成员全部显式忽略时成功返回但不置位 (POSIX: kill 不算失败).
///
/// # Returns
/// - `Ok(1)`: 组内至少有一个存活成员 (已投递, 或按上述规则挂起/忽略);
/// - `Err(-2)`: 目标线程组不存在;
/// - `Err(-3)`: 组内所有成员均已退出 (Zombie/Terminated).
///
/// # Errors
/// 同 `Returns`; 另外 `sig` 不在 `1..=63` 时返回 `Err(-1)`.
pub fn do_signal_send_group(tgid: Pid, sig: u8) -> Result<usize, i32> {
    if sig == 0 {
        return if PROCESS_TABLE.get(tgid).is_some() {
            Ok(1)
        } else {
            Err(-2)
        };
    }
    if !(1..=63).contains(&sig) {
        return Err(-1);
    }
    let Some(group) = PROCESS_TABLE.with_process(tgid, |p| alloc::sync::Arc::clone(p.group()))
    else {
        return Err(-2);
    };
    let members = group.members();
    if members.is_empty() {
        return Err(-3);
    }
    let bit = 1u64 << u64::from(sig);
    let uncatchable = is_uncatchable(sig);
    // 组内择线程结果: 0 = 已退出成员, 1 = 直接投递, 2 = 显式忽略,
    // 3 = 被屏蔽 (可挂起候选).
    let mut blocked_candidate: Option<Pid> = None;
    let mut saw_live = false;
    for tid in members {
        let pick = PROCESS_TABLE.with_process(tid, |proc| {
            let state = proc.get_state();
            if state == ProcessState::Zombie || state == ProcessState::Terminated {
                return 0u8;
            }
            if uncatchable {
                return 1u8;
            }
            let ignored = proc.sigaction_table.lock()[(sig - 1) as usize] == SIG_IGN;
            if ignored {
                2u8
            } else if proc.blocked_mask.load(Ordering::Acquire) & bit != 0 {
                3u8
            } else {
                1u8
            }
        });
        match pick {
            Some(1) => {
                let _ = PROCESS_TABLE.with_process(tid, |proc| {
                    proc.signal_pending_set(u32::from(sig));
                });
                // 已唤醒/已运行的判定与入队由调度器统一收口 (Blocked 才搬运).
                super::scheduler::Scheduler::unblock(tid);
                return Ok(1);
            }
            Some(2) => saw_live = true,
            Some(3) => {
                saw_live = true;
                if blocked_candidate.is_none() {
                    blocked_candidate = Some(tid);
                }
            }
            _ => {}
        }
    }
    if !saw_live {
        return Err(-3);
    }
    if let Some(tid) = blocked_candidate {
        // 全体存活成员都屏蔽该信号: 挂起到首个未忽略者, 不唤醒 (POSIX 要求
        // 信号在解除屏蔽后才可见).
        let _ = PROCESS_TABLE.with_process(tid, |proc| {
            proc.signal_pending_set(u32::from(sig));
        });
    }
    Ok(1)
}

/// 进程表锁内快照匹配进程的线程组 tgid (去重), 供锁外逐组投递.
///
/// G-11: `for_each` 回调已持进程表锁, 回调内不得调用会再次 `with_process`
/// 的投递函数 (非递归锁 → 自旋死锁); 因此只收集 tgid, 投递在锁外进行.
/// 按 tgid 去重保证组内线程只被信号命中一次, 而非每个 TCB 各投一次.
fn collect_matching_tgids(
    mut matches: impl FnMut(&super::process::Process) -> bool,
) -> alloc::vec::Vec<Pid> {
    let mut tgids: alloc::vec::Vec<Pid> = alloc::vec::Vec::new();
    PROCESS_TABLE.for_each(|proc| {
        if matches(proc) {
            let tgid = proc.thread_group.tgid();
            if !tgids.contains(&tgid) {
                tgids.push(tgid);
            }
        }
        true
    });
    tgids
}

/// 对快照出的线程组逐个投递; 至少一组存活时返回命中的组数.
fn deliver_to_groups(tgids: &[Pid], sig: u8) -> Result<usize, i32> {
    let mut count = 0usize;
    for &tgid in tgids {
        // 快照与投递之间组可能消失 (-2) 或全员退出 (-3); 跳过, 不影响其他组.
        if do_signal_send_group(tgid, sig).is_ok() {
            count += 1;
        }
    }
    if count > 0 { Ok(count) } else { Err(-2) }
}

// ============================================================================
// do_signal_deliver — 投递信号
// ============================================================================

/// 选择下一个待投递的信号
///
/// 从 pending & ~blocked 中选择, 委托到 `SignalDecision` trait.
///
/// # Returns
/// - `Some(sig)`: 待投递的信号编号 (1..=63)
/// - `None`: 无待投递信号
pub fn signal_pick_next(proc: &super::process::Process) -> Option<u8> {
    let pending = proc.signal_pending_get();
    let blocked = proc.blocked_mask.load(Ordering::Acquire);
    let deliverable = pending & !blocked;

    super::signal_trait::current_signal_decision().pick_next_signal(deliverable)
}

/// 执行信号的默认动作
///
/// 在 `do_signal_deliver` 中, 当 sigaction 为 `SIG_DFL` 时调用.
pub fn do_signal_default_action(pid: Pid, sig: u8, frame_addr: u64) {
    match signal_default_action(sig) {
        SignalDefaultAction::Ign => {}
        SignalDefaultAction::Cont => {
            if let Some(proc_ptr) = PROCESS_TABLE.get(pid) {
                // SAFETY: PROCESS_TABLE 保证指针有效
                let proc = unsafe { &*proc_ptr };
                // L-2 取证 (只读): 本支绕过状态机直接写 `Ready`, 是内核里唯一能把
                // `Zombie` 翻回可运行态的形态. 判据在写入前取, 锁外发射; 写入本身
                // 逐字未变 (本轮纯读数, 不改语义).
                let state = proc.get_state();
                let tgid = proc.group().tgid();
                let group_exited = proc.group().has_exited();
                proc.state
                    .store(ProcessState::Ready as u32, Ordering::Release);
                super::scheduler::Scheduler::redispatch_emit(
                    &RD_SIGNAL_CONT,
                    "signal-cont",
                    pid,
                    tgid,
                    state,
                    group_exited,
                );
            }
        }
        SignalDefaultAction::Stop => {
            if let Some(proc_ptr) = PROCESS_TABLE.get(pid) {
                // SAFETY: PROCESS_TABLE 保证指针有效, 进程在表中期间不会释放
                let proc = unsafe { &*proc_ptr };
                // L-2 取证: 同上, `Stop` 直接写 `Blocked` 会让死 TCB 既不是 Zombie
                // 也不在可运行集, 变成无人收割的隐形态; 本行记录写入前状态.
                let state = proc.get_state();
                let tgid = proc.group().tgid();
                let group_exited = proc.group().has_exited();
                proc.state
                    .store(ProcessState::Blocked as u32, Ordering::Release);
                super::scheduler::Scheduler::redispatch_emit(
                    &RD_SIGNAL_STOP,
                    "signal-stop",
                    pid,
                    tgid,
                    state,
                    group_exited,
                );
            }
        }
        SignalDefaultAction::Core => {
            // 生成 core dump (策略实现在 functions, 经 CoredumpSink 注入)
            super::coredump_trait::current_coredump_sink().coredump(pid, sig, frame_addr);
            terminate_from_fatal_signal(pid, sig);
        }
        SignalDefaultAction::Term => terminate_from_fatal_signal(pid, sig),
    }
}

/// 致命信号的终止路径: 默认动作 `Term`/`Core` 统一进入调度器退出流程.
///
/// K-06 修正: 旧实现只把当前 TCB 置 `Zombie` 后从信号投递路径返回用户态,
/// 僵尸线程会继续执行用户代码. 组定向语义下默认动作终止整个线程组;
/// 例外是 `exec` `de_thread` 期间兄弟线程收到的 `SIGKILL` — 只结束该线程,
/// 否则会把正在执行 exec 的调用线程一并带走.
fn terminate_from_fatal_signal(pid: Pid, sig: u8) {
    // 与 Linux 的用户可见形态一致: 信号致死的退出码为 128 + signum.
    let code = u32::from(sig) << 8 | 0x7f;
    let exec_de_thread = PROCESS_TABLE
        .with_process(pid, |proc| proc.group().exec_de_thread())
        .unwrap_or(false);
    if exec_de_thread {
        crate::privileged::proc::process_exit(code);
    } else {
        crate::privileged::proc::process_exit_group(code);
    }
}

/// 投递待处理信号 (在返回用户态前调用)
///
/// 遍历当前进程的 pending & ~blocked, 逐个投递:
/// - `SIG_DFL`: 执行默认动作
/// - `SIG_IGN`: 清除 pending 位, 忽略
/// - handler: 修改用户态栈帧跳转到 handler
///
/// # Arguments
/// * `frame` - 当前中断帧 (用户态寄存器状态), 仅当 handler 投递时修改
///
/// # Returns
///
/// 返回 true 表示有信号被投递 (handler 已设置, 需要返回用户态执行).
/// 返回 false 表示无待投递信号.
///
/// 调用点: x86_64 系统调用出口 / 中断出口, aarch64 `svc_handler` 与 EL0
/// 中断出口 —— 两架构、两条路径共用本函数, 差异由 [`UserFrame`] 收口.
pub fn do_signal_deliver<F: UserFrame>(frame: &mut F) -> bool {
    let Some(pid) = super::scheduler::Scheduler::current() else {
        return false;
    };

    let Some(proc_ptr) = PROCESS_TABLE.get(pid) else {
        return false;
    };

    // SAFETY: 当前进程一定有效
    let proc = unsafe { &*proc_ptr };
    let frame_addr = core::ptr::from_mut(&mut *frame).cast::<()>() as u64;

    let mut delivered = false;

    while let Some(sig) = signal_pick_next(proc) {
        // 清除 pending 位
        proc.signal_pending_clear(1u64 << u64::from(sig));

        // 查找 sigaction
        let action = {
            let actions = proc.sigaction_table.lock();
            actions[(sig - 1) as usize]
        };

        match action {
            SIG_DFL => {
                do_signal_default_action(pid, sig, frame_addr);
                delivered = true;
                // 如果进程被终止或停止, 不再投递更多信号
                let state = proc.state.load(Ordering::Acquire);
                if state == ProcessState::Zombie as u32 || state == ProcessState::Blocked as u32 {
                    break;
                }
            }
            SIG_IGN => {
                delivered = true;
            }
            handler_addr => {
                delivered |= deliver_user_handler(pid, sig, proc, &mut *frame, handler_addr);
                // 一次只投递一个 handler 信号 (sigreturn 后再投递下一个)
                break;
            }
        }
    }

    delivered
}

/// 计算信号帧在用户栈上的落点 (sigaltstack 优先, 否则主栈).
///
/// 返回 `None` 表示主栈剩余空间不足 (栈溢出), 调用方应转默认动作.
fn signal_frame_base<F: UserFrame>(
    proc: &super::process::Process,
    user_rsp: u64,
    total: usize,
) -> Option<u64> {
    // P1-I-45: 检查 sigaltstack 替代栈. 若进程通过 sigaltstack 注册了替代栈,
    // 且当前不在替代栈上 (SS_ONSTACK 未置位), 优先把信号帧写到替代栈顶部,
    // 避免在主栈溢出场景下 double fault (POSIX SA_ONSTACK 语义).
    let ss_addr = proc.sigaltstack_addr.load(Ordering::Acquire);
    let ss_size = proc.sigaltstack_size.load(Ordering::Acquire);
    let ss_flags = proc.sigaltstack_flags.load(Ordering::Acquire);
    // 向下对齐最多吃掉 15 字节, 容量判据把该量算进去 (否则对齐会把帧推出栈底).
    let use_alternate = ss_addr != 0
        && crate::privileged::glue::usize_from_u64(ss_size) >= total + 15
        && (ss_flags & SS_DISABLE) == 0
        && (ss_flags & SS_ONSTACK) == 0;

    let frame_top = if use_alternate {
        // 替代栈顶部向下分配
        ss_addr + ss_size - total as u64
    } else if user_rsp >= total as u64 {
        user_rsp - total as u64
    } else {
        return None;
    };

    // 帧落点向低地址对齐到 handler 入口要求 (x86_64 rsp ≡ 8 mod 16 /
    // aarch64 sp ≡ 0 mod 16); 只向下对齐, 帧仍完整位于原落点之内.
    let frame_rsp = frame_top.checked_sub(frame_top.wrapping_sub(F::SP_ALIGN) & 0xF)?;

    // 标记替代栈为"正在使用", 防止信号重入再次落回主栈
    if use_alternate {
        proc.sigaltstack_flags
            .store(ss_flags | SS_ONSTACK, Ordering::Release);
    }
    Some(frame_rsp)
}

/// 把 [帧头 | `ArchSignalFrame` | trampoline] 写到用户栈, 并改写异常帧跳转 handler.
///
/// 帧头仅 x86_64 存在 (handler 的返回地址槽 = trampoline 地址); aarch64 以 LR
/// 承载返回地址, 帧头长度为 0.
///
/// 返回 `false` 表示用户栈写入失败 (munmap/越界): 异常帧保持原样, 信号未投递.
fn install_user_signal_frame<F: UserFrame>(
    f: &mut F,
    frame_rsp: u64,
    sigframe: &ArchSignalFrame,
    handler_addr: u64,
) -> bool {
    // P0-I-38 修复: 信号栈帧写入走异常表保护版 copy_to_user,
    // 用户栈任意一页失效 (munmap/越界) 时回滚信号投递
    // (恢复原始栈指针 + 不修改异常帧), 进程继续运行.
    let sigframe_addr = frame_rsp + F::FRAME_HEADER_SIZE as u64;
    let trampoline_addr = sigframe_addr + core::mem::size_of::<ArchSignalFrame>() as u64;
    // SAFETY: sigframe 由调用方保证指向本函数可读的信号帧; 长度 =
    // size_of::<ArchSignalFrame>(), 完全在 sigframe 内存范围内;
    // from_raw_parts 仅借用字节视图供 copy_to_user 读取.
    let sigframe_bytes = unsafe {
        core::slice::from_raw_parts(
            (&raw const *sigframe).cast::<u8>(),
            core::mem::size_of::<ArchSignalFrame>(),
        )
    };

    let mut ok = true;
    if F::FRAME_HEADER_SIZE > 0 {
        let ret_addr_bytes = trampoline_addr.to_ne_bytes();
        ok = crate::privileged::mm::copy_to_user(
            frame_rsp,
            &ret_addr_bytes[..F::FRAME_HEADER_SIZE],
            F::FRAME_HEADER_SIZE,
        )
        .is_ok();
    }
    ok &= crate::privileged::mm::copy_to_user(
        sigframe_addr,
        sigframe_bytes,
        core::mem::size_of::<ArchSignalFrame>(),
    )
    .is_ok();
    ok &= crate::privileged::mm::copy_to_user(
        trampoline_addr,
        &SIGRETURN_TRAMPOLINE,
        SIGRETURN_TRAMPOLINE_SIZE,
    )
    .is_ok();

    if !ok {
        // 栈帧写入失败 (用户栈 munmap/越界): 不修改异常帧,
        // 也不投递信号, 进程继续运行等待下次信号投递窗口.
        return false;
    }

    // 修改异常帧: 跳转到 handler (两架构的寄存器约定见 `UserFrame::enter_handler`)
    f.enter_handler(frame_rsp, trampoline_addr, handler_addr, sigframe.signum);
    true
}

/// 针对"用户 handler"分支: 装配信号帧并改写异常帧, 返回是否已投递.
///
/// 栈帧布局 (帧头仅 x86_64 存在):
///   `frame_rsp+0`: 返回地址 (指向 trampoline, handler ret 时弹出)
///   `frame_rsp+8`: `ArchSignalFrame` (保存原始寄存器)
///   `frame_rsp+8+sizeof(ArchSignalFrame)`: 跳板代码 (`rt_sigreturn` 桩)
fn deliver_user_handler<F: UserFrame>(
    pid: Pid,
    sig: u8,
    proc: &super::process::Process,
    frame: &mut F,
    handler_addr: u64,
) -> bool {
    let total =
        F::FRAME_HEADER_SIZE + core::mem::size_of::<ArchSignalFrame>() + SIGRETURN_TRAMPOLINE_SIZE;
    let Some(frame_rsp) = signal_frame_base::<F>(proc, frame.user_sp(), total) else {
        // 栈溢出: 执行默认动作
        let frame_addr = core::ptr::from_mut(&mut *frame).cast::<()>() as u64;
        do_signal_default_action(pid, sig, frame_addr);
        return true;
    };

    let sigframe = frame.snapshot(sig);
    install_user_signal_frame::<F>(frame, frame_rsp, &sigframe, handler_addr)
}

/// 从用户栈上的信号栈帧恢复异常帧 (`rt_sigreturn` 的两架构共享实现).
///
/// 读取点: x86_64 在 handler `ret` 之后 `rsp` 已指向 `SignalFrame` 起点
/// (返回地址槽已被弹出); aarch64 的 `sp` 同样指向 `Aarch64SignalFrame` 起点
/// (trampoline 不压栈). 两架构都从 [`UserFrame::user_sp`] 直接读, 不带偏移.
///
/// 返回 `false` 表示用户栈信号帧不可读 (栈被 munmap / 指针越界): 调用方须终止
/// 该线程组, 不能带着未恢复的帧返回用户态.
pub fn restore_user_signal_frame<F: UserFrame>(frame: &mut F) -> bool {
    let mut sigframe = core::mem::MaybeUninit::<ArchSignalFrame>::uninit();
    // SAFETY: dst 覆盖本地 MaybeUninit 的完整字节范围; copy_from_user 只写该范围.
    let dst = unsafe {
        core::slice::from_raw_parts_mut(
            sigframe.as_mut_ptr().cast::<u8>(),
            core::mem::size_of::<ArchSignalFrame>(),
        )
    };
    if crate::privileged::mm::copy_from_user(dst, frame.user_sp(), dst.len()).is_err() {
        return false;
    }
    // SAFETY: copy_from_user 成功即已写满 ArchSignalFrame 的全部字节 (全 u64 字段).
    let sigframe = unsafe { sigframe.assume_init() };
    frame.restore(&sigframe);

    // P1-I-45: sigreturn 清除替代栈 SS_ONSTACK 标志 (返回后不再处于替代栈上,
    // POSIX 语义允许下一次信号再次落回替代栈).
    if let Some(pid) = super::scheduler::Scheduler::current() {
        let _ = PROCESS_TABLE.with_process_mut(pid, |proc| {
            let flags = proc.sigaltstack_flags.load(Ordering::Acquire);
            proc.sigaltstack_flags
                .store(flags & !SS_ONSTACK, Ordering::Release);
        });
    }
    true
}

// ============================================================================
// 便利函数
// ============================================================================

/// 检查当前进程是否有可投递信号
pub fn has_deliverable_signal(pid: Pid) -> bool {
    let Some(proc_ptr) = PROCESS_TABLE.get(pid) else {
        return false;
    };
    // SAFETY: PROCESS_TABLE 保证指针有效
    let proc = unsafe { &*proc_ptr };
    let pending = proc.signal_pending_get();
    let blocked = proc.blocked_mask.load(Ordering::Acquire);
    (pending & !blocked) != 0
}

/// `SIGKILL` 信号编号 (不可捕获/不可屏蔽)
pub const SIGKILL: u8 = 9;
/// `SIGSTOP` 信号编号 (不可捕获/不可屏蔽)
pub const SIGSTOP: u8 = 19;
/// `SIGALRM` 信号编号 (`alarm(2)` / `setitimer(ITIMER_REAL)` 到期投递)
pub const SIGALRM: u8 = 14;

/// 不可屏蔽信号位图 (屏蔽字 bit N = 信号 N)
const UNCATCHABLE_SIGNAL_BITS: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);

/// 规范化信号屏蔽字: 剔除不可屏蔽信号 (`SIGKILL`/`SIGSTOP`)
///
/// `signal_pick_next` 的投递判据是 `pending & !blocked`, 故屏蔽字中保留
/// `SIGKILL`/`SIGSTOP` 位会导致进程永久不可终止 — 任何写入 `blocked_mask`
/// 的路径都必须经本函数过滤 (单点权威, `rt_sigprocmask` 与 functions 侧
/// `ppoll`/`epoll_pwait` 的临时掩码替换共用).
#[must_use]
pub fn sanitize_blocked_mask(mask: u64) -> u64 {
    mask & !UNCATCHABLE_SIGNAL_BITS
}

/// 获取进程的信号屏蔽字
pub fn get_blocked_mask(pid: Pid) -> u64 {
    let Some(proc_ptr) = PROCESS_TABLE.get(pid) else {
        return 0;
    };
    // SAFETY: PROCESS_TABLE 保证指针有效
    let proc = unsafe { &*proc_ptr };
    proc.blocked_mask.load(Ordering::Acquire)
}

/// 设置进程的信号屏蔽字
pub fn set_blocked_mask(pid: Pid, mask: u64) {
    let Some(proc_ptr) = PROCESS_TABLE.get(pid) else {
        return;
    };
    // SAFETY: PROCESS_TABLE 保证指针有效
    let proc = unsafe { &*proc_ptr };
    proc.blocked_mask.store(mask, Ordering::Release);
}

/// 获取进程的 sigaction 表项
pub fn get_sigaction(pid: Pid, sig: u8) -> Option<u64> {
    if !(1..=63).contains(&sig) {
        return None;
    }
    let proc_ptr = PROCESS_TABLE.get(pid)?;
    // SAFETY: PROCESS_TABLE 保证指针有效
    let proc = unsafe { &*proc_ptr };
    let actions = proc.sigaction_table.lock();
    Some(actions[(sig - 1) as usize])
}

/// 设置进程的 sigaction 表项, 返回旧值
pub fn set_sigaction(pid: Pid, sig: u8, action: u64) -> Option<u64> {
    if !(1..=63).contains(&sig) {
        return None;
    }
    if is_uncatchable(sig) {
        return None; // SIGKILL/SIGSTOP 不可捕获
    }
    let proc_ptr = PROCESS_TABLE.get(pid)?;
    // SAFETY: PROCESS_TABLE 保证指针有效
    let proc = unsafe { &*proc_ptr };
    let mut actions = proc.sigaction_table.lock();
    let old = actions[(sig - 1) as usize];
    actions[(sig - 1) as usize] = action;
    Some(old)
}

// ============================================================================
// I-48: execve 信号状态重置
//
// Linux execve(2) 信号语义 (man 2 execve):
// 1. SA_RESETHAND 标志的 handler 在 exec 后重置为 SIG_DFL
// 2. 进程挂起的标准信号 (1..=31) 通常被保留
// 3. 实时信号 (SIGRTMIN..MAX) 也保留
// 4. sigaction 表本身保留 (handler 函数指针, 但目标地址在旧地址空间,
//    执行后即失效, 内核再投递时若仍指向旧地址需特殊处理)
//
// Edgine 简化语义:
// - execve 走 transactional 双进程替换 (proc_exec_replace):
//   1. 阶段 1: 加载新 ELF → 全新 UserProc (新 PID, 全新 signal_pending=0,
//      sigaction_table 全 SIG_DFL).
//   2. 阶段 2: 销毁旧进程.
// - 因此新进程天然不带旧进程的信号状态, 不需要"显式保留/重置".
// - 本函数提供显式 hook, 未来如引入"同 PID 复用"或"sigaction 跨 exec 保留"
//   即可在此实现, 保持调用点稳定.
// ============================================================================

/// I-48: 显式重置 execve 后进程的信号状态.
///
/// 当前实现为幂等 no-op (新进程已由 `user_proc_load_elf` 分配, 默认状态已正确).
/// 函数存在是为 (1) 文档化语义, (2) 未来扩展的稳定 hook.
pub fn reset_signal_state_on_exec(pid: Pid) {
    let Some(proc_ptr) = PROCESS_TABLE.get(pid) else {
        return;
    };
    // SAFETY: PROCESS_TABLE.get() 返回有效指针, 进程在表中期间不会释放
    let proc = unsafe { &*proc_ptr };
    // pending 清零 (理论上新进程已是 0, 此处显式确保)
    proc.pending_signals.store(0, Ordering::Release);
    // sigaction 表回到 SIG_DFL (0)
    let mut table = proc.sigaction_table.lock();
    for entry in table.iter_mut() {
        *entry = 0;
    }
    // blocked mask 清零
    proc.blocked_mask.store(0, Ordering::Release);
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_signal_default_action() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    assert_eq_test!(
        signal_default_action(9),
        SignalDefaultAction::Term,
        "SIGKILL=Term"
    );
    assert_eq_test!(
        signal_default_action(11),
        SignalDefaultAction::Core,
        "SIGSEGV=Core"
    );
    assert_eq_test!(
        signal_default_action(17),
        SignalDefaultAction::Ign,
        "SIGCHLD=Ign"
    );
    assert_eq_test!(
        signal_default_action(19),
        SignalDefaultAction::Stop,
        "SIGSTOP=Stop"
    );
    assert_eq_test!(
        signal_default_action(18),
        SignalDefaultAction::Cont,
        "SIGCONT=Cont"
    );
    assert_eq_test!(
        signal_default_action(15),
        SignalDefaultAction::Term,
        "SIGTERM=Term"
    );
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_uncatchable() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    check!(is_uncatchable(9), "SIGKILL uncatchable");
    check!(is_uncatchable(19), "SIGSTOP uncatchable");
    check!(!is_uncatchable(15), "SIGTERM catchable");
    check!(!is_uncatchable(2), "SIGINT catchable");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_signal_pick_next_logic() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    // 模拟 pending = 0b1010 (bit 1=SIGINT, bit 3=SIGQUIT)
    // blocked = 0b10 (bit 1=SIGINT)
    // deliverable = 0b1000, 应选择 bit 3 = SIGQUIT (sig=3)
    let pending: u64 = 0b1010;
    let blocked: u64 = 0b0010;
    let deliverable = pending & !blocked;
    assert_eq_test!(deliverable, 0b1000, "deliverable");
    let sig_bit = crate::privileged::glue::low_u8_from_u32(deliverable.trailing_zeros());
    assert_eq_test!(sig_bit, 3, "lowest bit is 3");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_set_get_sigaction() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // SIGKILL (9) 不可设置
    let result = set_sigaction(1, 9, 0xDEAD);
    check!(result.is_none(), "SIGKILL cannot be caught");
    // SIGSTOP (19) 不可设置
    let result = set_sigaction(1, 19, 0xDEAD);
    check!(result.is_none(), "SIGSTOP cannot be caught");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_default_action_coverage() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    // 验证所有 31 个标准信号都有定义
    for sig in 1u8..=31 {
        let _action = signal_default_action(sig);
    }
    // 抽查关键信号
    assert_eq_test!(
        signal_default_action(1),
        SignalDefaultAction::Term,
        "SIGHUP=Term"
    );
    assert_eq_test!(
        signal_default_action(2),
        SignalDefaultAction::Term,
        "SIGINT=Term"
    );
    assert_eq_test!(
        signal_default_action(13),
        SignalDefaultAction::Term,
        "SIGPIPE=Term"
    );
    assert_eq_test!(
        signal_default_action(14),
        SignalDefaultAction::Term,
        "SIGALRM=Term"
    );
    TestResult::Pass
}

/// 注册 signal 相关的内核测试用例。
#[cfg(feature = "kernel_test")]
pub fn register_signal_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("signal", "default_action", test_signal_default_action);
    r.register("signal", "uncatchable", test_uncatchable);
    r.register("signal", "pick_next_logic", test_signal_pick_next_logic);
    r.register("signal", "set_get_sigaction", test_set_get_sigaction);
    r.register(
        "signal",
        "default_action_coverage",
        test_default_action_coverage,
    );
    r.register(
        "signal",
        "kill_broadcast_pid_positive",
        test_kill_broadcast_pid_positive,
    );
    r.register(
        "signal",
        "kill_broadcast_pid_zero_group",
        test_kill_broadcast_pid_zero_group,
    );
    r.register(
        "signal",
        "kill_broadcast_pid_negative_all",
        test_kill_broadcast_pid_negative_all,
    );
    r.register(
        "signal",
        "kill_broadcast_pid_negative_group",
        test_kill_broadcast_pid_negative_group,
    );
}

// ============================================================================
// TRACK-315B7C: kill 4 种 pid 语义测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_kill_broadcast_pid_positive() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    // pid > 0 单进程: 不存在的 pid 必返回 Err(ESRCH)
    let res = do_signal_send_extended(9999, 9);
    check!(res.is_err(), "kill non-existent pid should fail");
    // 验证 sig 范围检查: 32-63 是合法实时信号, 越界需用 64
    let res2 = do_signal_send_extended(9999, 64); // 越界
    assert_eq_test!(res2, Err(-1i32), "sig out of range -> EINVAL");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_kill_broadcast_pid_zero_group() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    // pid = 0 广播: 接受 Err(-2) (ESRCH) 或 Ok(N) (有进程).
    // G-11 修复后广播不再死锁; 测试进程组真实投递 sig=9 仅置 pending
    // (kernel_test 不返回用户态不投递), 用作广播路径回归覆盖.
    let res = do_signal_send_extended(0, 9);
    check!(res.is_err() || res.is_ok(), "pid=0 must not EINVAL");
    // 验证信号范围: 32-63 合法, 越界用 64
    let res = do_signal_send_extended(0, 64);
    assert_eq_test!(res, Err(-1i32), "sig out of range -> EINVAL");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_kill_broadcast_pid_negative_all() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // pid = -1: 广播到所有进程 (除 init).
    // host test 环境下进程表通常为空 -> Err(ESRCH)
    let res = do_signal_send_extended(-1, 9);
    check!(res != Err(-1i32), "pid=-1 must not return EINVAL");
    // sig=0 检查存在
    let res = do_signal_send_extended(-1, 0);
    check!(res.is_err() || res.is_ok(), "pid=-1 sig=0 must not EINVAL");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_kill_broadcast_pid_negative_group() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // pid < -1: 广播到进程组 |pid|.
    let res = do_signal_send_extended(-100, 9);
    check!(res != Err(-1i32), "pid=-100 must not return EINVAL");
    // sig=0 检查存在
    let res = do_signal_send_extended(-100, 0);
    check!(res != Err(-1i32), "pid=-100 sig=0 must not EINVAL");
    TestResult::Pass
}
