//! Scheduler — 调度器 trait + Task 抽象 (TCB)
//!
//! 策略注入点: functions 层通过 `Scheduler` trait 管理任务队列,
//! 通过 `Task` 句柄安全操作进程/线程属性。
//!
//! ## 与 Asterinas OSTD 的关系
//!
//! 等价于 OSTD 的 `Scheduler` trait + `Task` 抽象。
//!
//! ## SAFETY 不变量
//!
//! - `schedule()` 仅在中断/异常返回路径调用。
//! - `Task` 内部持有 Process 指针, 生命周期由 `PROCESS_TABLE` 保证。
//! - 状态修改通过 Atomic 操作, 无锁安全。

use core::fmt;
use core::sync::atomic::Ordering;

use crate::privileged::proc::Process;
use crate::privileged::proc::{BlockReason, Pid};

// ============================================================================
// Task 抽象 — 进程/线程控制块安全句柄
// ============================================================================

/// 进程/线程控制块安全句柄。
///
/// 包装 raw `*const Process`, 提供类型安全的属性读写。
/// 所有字段访问通过 `Process` 的 Atomic 成员或 Mutex。
pub struct Task {
    proc_ptr: *const Process,
    pid: Pid,
}

impl Task {
    /// 从现有 Process 创建 Task。
    ///
    /// # SAFETY
    /// `proc_ptr` 必须指向有效的 Process 实例, 且生命周期覆盖 Task。
    pub unsafe fn from_raw(pid: Pid, proc_ptr: *const Process) -> Self {
        Self { proc_ptr, pid }
    }

    /// PID
    #[inline(always)]
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// 进程名称 (克隆)。
    pub fn name(&self) -> alloc::string::String {
        // SAFETY: Process::name is Mutex<String>.
        unsafe { (*self.proc_ptr).name.lock().clone() }
    }

    /// 进程状态 (`AtomicU32`)。
    pub fn state(&self) -> u32 {
        // SAFETY: `Task` 包装了 `*const Process`, 由 `Task::new`/`from_pid` 等构造路径
        // 保证该指针指向有效的 `Process` 实例, 且通过 `&self` 借用保证存活。
        // 字段 `state` 是 `AtomicU32`, 任何对齐的 load 总是 safe (仅要求指针有效)。
        // `Acquire` ordering 保证与并发写入同步。
        unsafe { (*self.proc_ptr).state.load(Ordering::Acquire) }
    }

    /// 优先级 (`AtomicU32`)。
    pub fn priority(&self) -> u32 {
        // SAFETY: 同 `state` 的契约; 字段是 `AtomicU32`。
        unsafe { (*self.proc_ptr).priority.load(Ordering::Acquire) }
    }

    /// 是否为内核线程。
    pub fn is_kernel(&self) -> bool {
        // SAFETY: 同 `state` 的契约; `is_kernel()` 是 `Process` 的 safe 方法, 仅访问
        // 内部 `is_kernel_thread: bool` 字段。
        unsafe { (*self.proc_ptr).is_kernel() }
    }

    /// PWM 安全上下文。
    pub fn pwm(&self) -> u64 {
        // SAFETY: 同 `state` 的契约; `get_pwm()` 是 `Process` 的 safe 方法, 读取
        // capability 上下文的 u64 标识符。
        unsafe { (*self.proc_ptr).get_pwm() }
    }

    /// 退出码。
    pub fn exit_code(&self) -> u32 {
        // SAFETY: 同 `state` 的契约; 字段是 `AtomicU32`。
        unsafe { (*self.proc_ptr).exit_code.load(Ordering::Acquire) }
    }

    /// 累计 CPU 时间 (ticks)。
    pub fn cpu_time_ticks(&self) -> u64 {
        // SAFETY: 同 `state` 的契约; 字段是 `AtomicU64`。
        unsafe { (*self.proc_ptr).cpu_time.load(Ordering::Acquire) }
    }

    /// CR3 页表根。
    pub fn cr3(&self) -> u64 {
        // SAFETY: 同 `state` 的契约; 字段是 `AtomicU64` (页表基址, 物理地址)。
        unsafe { (*self.proc_ptr).cr3.load(Ordering::Acquire) }
    }

    /// 信号待处理掩码。
    pub fn pending_signals(&self) -> u64 {
        // SAFETY: 同 `state` 的契约; `signal_pending_get()` 是 `Process` 的 safe 方法,
        // 返回 `signal_pending: AtomicU64` 的当前值。
        unsafe { (*self.proc_ptr).signal_pending_get() }
    }
}

impl fmt::Display for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Task(pid={}, state={})", self.pid, self.state())
    }
}

// SAFETY: Task 持有 Process 的只读指针, Process 自身 Send+Sync。
unsafe impl Send for Task {}
unsafe impl Sync for Task {}

// ============================================================================
// Scheduler trait — 调度策略注入点
// ============================================================================

/// 调度器策略 trait。
///
/// functions 层通过此 trait 将任务入队/出队,
/// 而不直接操作 CFS/RT 的具体实现。
pub trait Scheduler: Send + Sync {
    /// 将任务加入就绪队列。
    fn enqueue(&self, pid: Pid);

    /// 设置 nice 值 (-20..19)。
    fn set_nice(&self, pid: Pid, nice: i8);

    /// 核心调度决策: 选择下一个任务并上下文切换。
    fn schedule(&self) -> Option<Pid>;

    /// 当前运行中任务的 PID。
    fn current(&self) -> Option<Pid>;

    /// 阻塞当前任务。
    fn block_current(&self, reason: BlockReason);

    /// 唤醒指定 PID。
    fn unblock(&self, pid: Pid);

    /// 当前任务退出。
    fn exit_current(&self, exit_code: u32);

    /// 主动让出 CPU。
    fn yield_current(&self);

    /// 标记需要重新调度。
    fn set_need_reschedule(&self);

    /// 是否有可运行任务。
    fn has_runnable(&self) -> bool;
}

// ============================================================================
// 默认实现: 委托给 proc::scheduler::SCHEDULER 全局单例
// ============================================================================

/// `Edgine` 默认调度器 (MLFQ + RT + CFS)。
///
/// 委托给 `proc::scheduler::SCHEDULER`。
pub struct EdgineScheduler;

impl Scheduler for EdgineScheduler {
    fn enqueue(&self, pid: Pid) {
        crate::privileged::proc::SCHEDULER.add(pid);
    }

    fn set_nice(&self, pid: Pid, nice: i8) {
        crate::privileged::proc::SCHEDULER.set_nice(pid, nice);
    }

    fn schedule(&self) -> Option<Pid> {
        crate::privileged::proc::SCHEDULER.schedule()
    }

    fn current(&self) -> Option<Pid> {
        crate::privileged::proc::SCHEDULER.current()
    }

    fn block_current(&self, reason: BlockReason) {
        crate::privileged::proc::SCHEDULER.block(reason);
    }

    fn unblock(&self, pid: Pid) {
        crate::privileged::proc::SCHEDULER.unblock(pid);
    }

    fn exit_current(&self, exit_code: u32) {
        crate::privileged::proc::SCHEDULER.exit(exit_code);
    }

    fn yield_current(&self) {
        crate::privileged::proc::SCHEDULER.yield_current();
    }

    fn set_need_reschedule(&self) {
        crate::privileged::proc::SCHEDULER.set_need_reschedule();
    }

    fn has_runnable(&self) -> bool {
        crate::privileged::proc::SCHEDULER.has_any_runnable()
    }
}
