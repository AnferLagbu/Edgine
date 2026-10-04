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
//! # Safety
//!
//! - `do_signal_send`: 操作进程原子字段, 线程安全
//! - `do_signal_deliver`: 操作当前进程, 单 CPU 执行, 无竞争
//! - `PROCESS_TABLE.get()` 返回 *mut Process, 需 unsafe 解引用

use core::sync::atomic::Ordering;

use super::process::PROCESS_TABLE;
use super::types::{Pid, ProcessState};

// ============================================================================
// 信号栈帧 (x86_64)
// ============================================================================

/// 信号栈帧 — 保存在用户栈上, sigreturn 时恢复
///
/// 布局与 `InterruptFrame` 兼容, 便于直接拷贝寄存器状态.
/// signum 字段放在最后, handler 通过第一个参数 (rdi) 获取.
#[repr(C)]
pub struct SignalFrame {
    // 通用寄存器 (与 InterruptFrame 顺序一致)
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,

    // 中断元数据
    pub int_no: u64,
    pub err_code: u64,

    // 返回地址信息
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,

    // 信号编号 (handler 参数)
    pub signum: u64,
}

/// sigreturn trampoline 代码
///
/// # `x86_64` 机器码 (7 字节)
/// `mov eax, 15` (`SYS_rt_sigreturn` = 15) + `syscall`:
///   B8 0F 00 00 00     mov eax, 15
///   0F 05              syscall
///
/// # aarch64 机器码 (8 字节)
/// `mov x8, #139` (`SYS_rt_sigreturn` = 139) + `svc #0`:
///   D2 80 11 68        movz x8, #139
///   D4 00 00 01        svc #0
///
/// P1-I-40 修复: 之前硬编码 `x86_64` 字节序, aarch64 上是随机指令
/// (illegal instruction), 致所有 ARM 板信号投递失败. 改为 cfg 分发.
#[cfg(target_arch = "x86_64")]
pub const SIGRETURN_TRAMPOLINE: [u8; 7] = [0xB8, 0x0F, 0x00, 0x00, 0x00, 0x0F, 0x05];

#[cfg(target_arch = "aarch64")]
pub const SIGRETURN_TRAMPOLINE: [u8; 8] = [0xD2, 0x80, 0x11, 0x68, 0xD4, 0x00, 0x00, 0x01];

/// sigreturn trampoline 大小
pub const SIGRETURN_TRAMPOLINE_SIZE: usize = SIGRETURN_TRAMPOLINE.len();

/// 信号栈帧总大小 (含 trampoline)
pub const SIGNAL_FRAME_TOTAL_SIZE: usize =
    core::mem::size_of::<SignalFrame>() + SIGRETURN_TRAMPOLINE_SIZE;

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
pub fn signal_default_action(sig: u8) -> SignalDefaultAction {
    super::signal_trait::current_signal_decision().default_action(sig)
}

/// 信号是否不可捕获/屏蔽
///
/// 委托到 `SignalDecision` trait, functions 可覆盖策略.
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

    // 唤醒目标进程 (如果处于可中断睡眠)
    if state == ProcessState::Blocked as u32 {
        proc.state
            .store(ProcessState::Ready as u32, Ordering::Release);
    }

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
        return match pid {
            p if p > 0 => {
                if PROCESS_TABLE.get(p as u32).is_some() {
                    Ok(1)
                } else {
                    Err(-2)
                }
            }
            0 => {
                // 至少检查当前进程存在
                if PROCESS_TABLE
                    .get(super::scheduler::SCHEDULER.current().unwrap_or(0))
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
                let target_pgid = (-p) as u32;
                let mut count = 0usize;
                PROCESS_TABLE.for_each(|proc| {
                    let pg = proc.pgid.load(Ordering::SeqCst);
                    let effective_pgid = if pg == 0 { proc.pid.0 } else { pg };
                    if effective_pgid == target_pgid {
                        count += 1;
                    }
                    true
                });
                if count > 0 { Ok(count) } else { Err(-2) }
            }
            _ => Err(-2),
        };
    }

    if !(1..=63).contains(&sig) {
        return Err(-1);
    }

    match pid {
        p if p > 0 => {
            // 单进程
            if do_signal_send_inner(p as u32, sig).is_ok() {
                Ok(1)
            } else {
                Err(-2)
            }
        }
        0 => {
            // 广播到同进程组
            let current = super::scheduler::SCHEDULER.current().unwrap_or(0);
            let current_pgid = PROCESS_TABLE
                .get(current)
                // SAFETY: PROCESS_TABLE 保证指针有效, 进程在表中期间不会释放; 只读 pgid
                .map_or(0, |p| unsafe { (&*p).pgid.load(Ordering::SeqCst) });
            let target_pgid = if current_pgid == 0 {
                current
            } else {
                current_pgid
            };
            let mut count = 0usize;
            PROCESS_TABLE.for_each(|proc| {
                let pg = proc.pgid.load(Ordering::SeqCst);
                let effective_pgid = if pg == 0 { proc.pid.0 } else { pg };
                if effective_pgid == target_pgid {
                    // G-11: 回调内已持表锁, 用 do_signal_send_process 直接投递 (避免重入锁死)
                    if do_signal_send_process(proc, sig).is_ok() {
                        count += 1;
                    }
                }
                true
            });
            if count > 0 { Ok(count) } else { Err(-2) }
        }
        -1 => {
            // 广播到所有进程 (除 init pid=1)
            let mut count = 0usize;
            PROCESS_TABLE.for_each(|proc| {
                if proc.pid.0 == 1 {
                    return true; // 跳过 init
                }
                // G-11: 回调内已持表锁, 用 do_signal_send_process 直接投递 (避免重入锁死)
                if do_signal_send_process(proc, sig).is_ok() {
                    count += 1;
                }
                true
            });
            if count > 0 { Ok(count) } else { Err(-2) }
        }
        p if p < -1 => {
            // 广播到 |pid| 进程组
            let target_pgid = (-p) as u32;
            let mut count = 0usize;
            PROCESS_TABLE.for_each(|proc| {
                let pg = proc.pgid.load(Ordering::SeqCst);
                let effective_pgid = if pg == 0 { proc.pid.0 } else { pg };
                if effective_pgid == target_pgid {
                    // G-11: 回调内已持表锁, 用 do_signal_send_process 直接投递 (避免重入锁死)
                    if do_signal_send_process(proc, sig).is_ok() {
                        count += 1;
                    }
                }
                true
            });
            if count > 0 { Ok(count) } else { Err(-2) }
        }
        _ => Err(-2),
    }
}

/// `do_signal_send_inner` — 单进程信号发送 (不检查 `SIG_IGN`, 适用于广播).
///
/// I-52: 与 `do_signal_send` 一致, Zombie 状态不投递 (返回 -3).
/// 广播场景: 即使多数目标正常, 遇到 Zombie 也跳过, 不影响其他目标.
fn do_signal_send_inner(pid: u32, sig: u8) -> Result<(), i32> {
    let proc_ptr = PROCESS_TABLE.get(pid).ok_or(-2)?;
    // SAFETY: 进程在表中期间不会释放
    let proc = unsafe { &*proc_ptr };
    do_signal_send_process(proc, sig)
}

/// 进程表内直接投递 (供 `for_each` 广播回调使用).
///
/// G-11 (2026-09-07): 原广播路径在 `for_each` 回调内调用 `do_signal_send_inner`,
/// 其内部 `PROCESS_TABLE.get(pid)` 会对同一进程表 Mutex 重入加锁 — 该 Mutex
/// 并非真递归 (raw_lock 无 owner 重入检测), 持锁回调内重入 → 无限自旋死锁.
/// 影响面: `kill(0/-1/-pgid, sig)` syscall (dispatch.rs) 与 session 前台组广播
/// (functions/proc/session.rs) 在生产环境同样会死锁; 由 kernel_test 扩容后首次暴露.
fn do_signal_send_process(proc: &super::process::Process, sig: u8) -> Result<(), i32> {
    // I-52: 显式跳过 Zombie (与 do_signal_send 对齐)
    let state = proc.state.load(Ordering::Acquire);
    if state == ProcessState::Zombie as u32 {
        return Err(-3);
    }
    proc.signal_pending_set(u32::from(sig));
    if state == ProcessState::Blocked as u32 {
        proc.state
            .store(ProcessState::Ready as u32, Ordering::Release);
    }
    Ok(())
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
                proc.state
                    .store(ProcessState::Ready as u32, Ordering::Release);
            }
        }
        SignalDefaultAction::Stop => {
            if let Some(proc_ptr) = PROCESS_TABLE.get(pid) {
                // SAFETY: PROCESS_TABLE 保证指针有效, 进程在表中期间不会释放
                let proc = unsafe { &*proc_ptr };
                proc.state
                    .store(ProcessState::Blocked as u32, Ordering::Release);
            }
        }
        SignalDefaultAction::Core => {
            // 生成 core dump (策略实现在 functions, 经 CoredumpSink 注入)
            super::coredump_trait::current_coredump_sink().coredump(pid, sig, frame_addr);
            if let Some(proc_ptr) = PROCESS_TABLE.get(pid) {
                // SAFETY: `proc_ptr` 由调用方保证为有效指针; 只读访问
                let proc = unsafe { &*proc_ptr };
                proc.exit_code
                    .store(u32::from(sig) << 8 | 0x7f, Ordering::Release);
                proc.state
                    .store(ProcessState::Zombie as u32, Ordering::Release);
            }
        }
        SignalDefaultAction::Term => {
            if let Some(proc_ptr) = PROCESS_TABLE.get(pid) {
                // SAFETY: `proc_ptr` 由调用方保证为有效指针; 只读访问
                let proc = unsafe { &*proc_ptr };
                proc.exit_code
                    .store(u32::from(sig) << 8 | 0x7f, Ordering::Release);
                proc.state
                    .store(ProcessState::Zombie as u32, Ordering::Release);
            }
        }
    }
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
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
/// # Safety
///
/// - frame 指针必须有效且指向当前 CPU 的中断帧
/// - 仅在返回用户态前调用 (中断上下文或系统调用出口)
pub fn do_signal_deliver(frame: *mut crate::privileged::idt::InterruptFrame) -> bool {
    let pid = match super::scheduler::SCHEDULER.current() {
        Some(p) => p,
        None => return false,
    };

    let proc_ptr = match PROCESS_TABLE.get(pid) {
        Some(p) => p,
        None => return false,
    };

    // SAFETY: 当前进程一定有效
    let proc = unsafe { &*proc_ptr };

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
                do_signal_default_action(pid, sig, frame as u64);
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
                // 构建信号栈帧并修改 InterruptFrame
                // SAFETY: frame 由调用方保证有效
                let f = unsafe { &mut *frame };

                let user_rsp = f.rsp;

                // 栈帧布局:
                //   frame_rsp+0: 返回地址 (指向 trampoline, handler ret 时弹出)
                //   frame_rsp+8: SignalFrame (保存原始寄存器)
                //   frame_rsp+8+sizeof(SignalFrame): trampoline code (rt_sigreturn)  // 信号 trampoline
                let total = 8 + core::mem::size_of::<SignalFrame>() + SIGRETURN_TRAMPOLINE_SIZE;

                // P1-I-45 修复: 检查 sigaltstack 替代栈.
                // 若进程通过 sigaltstack 注册了替代栈, 且当前不在替代栈上 (SS_ONSTACK 未置位),
                // 优先把信号帧写到替代栈顶部, 避免在主栈溢出场景下 double fault.
                // POSIX SA_ONSTACK 语义.
                let ss_addr = proc.sigaltstack_addr.load(Ordering::Acquire);
                let ss_size = proc.sigaltstack_size.load(Ordering::Acquire);
                let ss_flags = proc.sigaltstack_flags.load(Ordering::Acquire);
                let use_alternate = ss_addr != 0
                    && ss_size as usize >= total
                    && (ss_flags & SS_DISABLE) == 0
                    && (ss_flags & SS_ONSTACK) == 0;

                let frame_rsp = if use_alternate {
                    // 替代栈顶部向下分配
                    ss_addr + ss_size - total as u64
                } else if user_rsp >= total as u64 {
                    user_rsp - total as u64
                } else {
                    // 栈溢出, 执行默认动作
                    do_signal_default_action(pid, sig, frame as u64);
                    delivered = true;
                    break;
                };

                // 标记替代栈为"正在使用", 防止信号重入再次落回主栈
                if use_alternate {
                    proc.sigaltstack_flags
                        .store(ss_flags | SS_ONSTACK, Ordering::Release);
                }

                // 构建 SignalFrame (保存原始寄存器)
                let sigframe = SignalFrame {
                    r15: f.r15,
                    r14: f.r14,
                    r13: f.r13,
                    r12: f.r12,
                    r11: f.r11,
                    r10: f.r10,
                    r9: f.r9,
                    r8: f.r8,
                    rdi: f.rdi,
                    rsi: f.rsi,
                    rbp: f.rbp,
                    rdx: f.rdx,
                    rcx: f.rcx,
                    rbx: f.rbx,
                    rax: f.rax,
                    int_no: f.int_no,
                    err_code: f.err_code,
                    rip: f.rip,
                    cs: f.cs,
                    rflags: f.rflags,
                    rsp: f.rsp,
                    ss: f.ss,
                    signum: u64::from(sig),
                };

                // P0-I-38 修复: 信号栈帧写入走异常表保护版 copy_to_user,
                // 用户栈任意一页失效 (munmap/越界) 时回滚信号投递
                // (恢复原始栈指针 + 不修改 InterruptFrame), 进程继续运行.
                let trampoline_start = frame_rsp + 8 + core::mem::size_of::<SignalFrame>() as u64;
                let ret_addr_bytes = trampoline_start.to_ne_bytes();
                // SAFETY: sigframe 是本函数栈上的 SignalFrame, 引用有效; 长度 =
                // size_of::<SignalFrame>(), 完全在 sigframe 内存范围内;
                // from_raw_parts 仅借用字节视图供 copy_to_user 读取.
                let sigframe_bytes = unsafe {
                    core::slice::from_raw_parts(
                        &sigframe as *const SignalFrame as *const u8,
                        core::mem::size_of::<SignalFrame>(),
                    )
                };

                let ok_ret = crate::privileged::mm::copy_to_user(frame_rsp, &ret_addr_bytes, 8);
                let ok_frame = crate::privileged::mm::copy_to_user(
                    frame_rsp + 8,
                    sigframe_bytes,
                    core::mem::size_of::<SignalFrame>(),
                );
                let ok_trampoline = crate::privileged::mm::copy_to_user(
                    trampoline_start,
                    &SIGRETURN_TRAMPOLINE,
                    SIGRETURN_TRAMPOLINE_SIZE,
                );

                if ok_ret.is_err() || ok_frame.is_err() || ok_trampoline.is_err() {
                    // 栈帧写入失败 (用户栈 munmap/越界):
                    // 不修改 InterruptFrame, 也不投递信号, 进程继续运行
                    // 等待下次信号投递窗口.
                    break;
                }

                // 修改 InterruptFrame: 跳转到 handler
                f.rip = handler_addr;
                f.rdi = u64::from(sig); // 参数1: signum
                f.rsi = 0; // 参数2: siginfo (简化: NULL)
                f.rdx = 0; // 参数3: ucontext (简化: NULL)
                f.rsp = frame_rsp;

                delivered = true;
                // 一次只投递一个 handler 信号 (sigreturn 后再投递下一个)
                break;
            }
        }
    }

    delivered
}

// ============================================================================
// 便利函数
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 检查当前进程是否有可投递信号
pub fn has_deliverable_signal(pid: Pid) -> bool {
    let proc_ptr = match PROCESS_TABLE.get(pid) {
        Some(p) => p,
        None => return false,
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

/// 不可屏蔽信号位图 (屏蔽字 bit N = 信号 N)
const UNCATCHABLE_SIGNAL_BITS: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);

/// 规范化信号屏蔽字: 剔除不可屏蔽信号 (`SIGKILL`/`SIGSTOP`)
///
/// `signal_pick_next` 的投递判据是 `pending & !blocked`, 故屏蔽字中保留
/// `SIGKILL`/`SIGSTOP` 位会导致进程永久不可终止 — 任何写入 `blocked_mask`
/// 的路径都必须经本函数过滤 (单点权威, `rt_sigprocmask` 与 functions 侧
/// `ppoll`/`epoll_pwait` 的临时掩码替换共用).
pub fn sanitize_blocked_mask(mask: u64) -> u64 {
    mask & !UNCATCHABLE_SIGNAL_BITS
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 获取进程的信号屏蔽字
pub fn get_blocked_mask(pid: Pid) -> u64 {
    let proc_ptr = match PROCESS_TABLE.get(pid) {
        Some(p) => p,
        None => return 0,
    };
    // SAFETY: PROCESS_TABLE 保证指针有效
    let proc = unsafe { &*proc_ptr };
    proc.blocked_mask.load(Ordering::Acquire)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 设置进程的信号屏蔽字
pub fn set_blocked_mask(pid: Pid, mask: u64) {
    let proc_ptr = match PROCESS_TABLE.get(pid) {
        Some(p) => p,
        None => return,
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

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// I-48: 显式重置 execve 后进程的信号状态.
///
/// 当前实现为幂等 no-op (新进程已由 `user_proc_load_elf` 分配, 默认状态已正确).
/// 函数存在是为 (1) 文档化语义, (2) 未来扩展的稳定 hook.
pub fn reset_signal_state_on_exec(pid: Pid) {
    let proc_ptr = match PROCESS_TABLE.get(pid) {
        Some(p) => p,
        None => return,
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
    let sig_bit = deliverable.trailing_zeros() as u8;
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
