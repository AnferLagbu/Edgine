//! 中断底部半 (Bottom-Half / Softirq) 机制
//!
//! 在硬中断退出时延迟执行非关键处理，减少中断禁用时间。
//! 参考 Linux softirq + tasklet 设计，但保持极简。
//!
//! ## 架构
//!
//! ```text
//! Hardware IRQ
//!   → hardirq_handler()       (快速路径: ACK/EOI, 关键数据搬运)
//!   → raise_softirq()         (标记 pending bit)
//!   → send_eoi()
//!   → do_softirq()            (开中断执行延后处理)
//!       ├── Softirq::Timer    → 定时器账本更新
//!       ├── Softirq::NetRx    → 网络包提交上层
//!       ├── Softirq::NetTx    → 网络发送完成回收
//!       ├── Softirq::Block    → 块设备 IO 完成
//!       └── Softirq::Tasklet  → 通用 tasklet
//! ```
//!
//! ## 安全性
//!
//! - `do_softirq()` 在开中断环境下运行，可被硬中断抢占
//! - `running` 标志防止重入
//! - handlers 在 `open_softirq()` 时一次性注册，运行时只读

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::framework::config::MAX_CPUS;

const MAX_SOFTIRQS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SoftirqVec {
    High = 0,
    Timer = 1,
    NetRx = 2,
    NetTx = 3,
    Block = 4,
    Tasklet = 5,
    Sched = 6,
    /// Kswapd: 内存回收/页面换出 (B3 完整实现)
    Kswapd = 7,
    /// Hotplug: PCIe/USB 热插拔槽位轮询 (由 scheduler tick 周期驱动)
    Hotplug = 8,
    Count = 9,
}

impl SoftirqVec {
    #[inline]
    pub const fn to_idx(self) -> usize {
        self as usize
    }

    #[inline]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::High),
            1 => Some(Self::Timer),
            2 => Some(Self::NetRx),
            3 => Some(Self::NetTx),
            4 => Some(Self::Block),
            5 => Some(Self::Tasklet),
            6 => Some(Self::Sched),
            7 => Some(Self::Kswapd),
            8 => Some(Self::Hotplug),
            _ => None,
        }
    }
}

pub type SoftirqHandler = fn();

struct SoftirqState {
    pending: AtomicU64,
    handlers: UnsafeCell<[Option<SoftirqHandler>; MAX_SOFTIRQS]>,
    running: AtomicBool,
}

// SAFETY: SoftirqState 的 pending/running 使用 AtomicU64/AtomicBool.
// handlers (UnsafeCell) 仅在注册期 (启动期) 修改,
// 在 softirq 处理期 (中断上下文, 单线程) 读取.
unsafe impl Sync for SoftirqState {}

/// B03-11: 每 CPU 独立的 softirq 状态 — 让多核并行处理 softirq 而非全局锁。
/// 每个 CPU 有独立的 pending 位图 + handlers + running 标记。
/// `running` 防止同一 CPU 上的 softirq 重入（仍需保留）。
/// `pending` per-CPU 化后, 各 CPU 独立排程自己的软中断。
static SOFTIRQ: [SoftirqState; MAX_CPUS] = {
    #[expect(
        clippy::declare_interior_mutable_const,
        reason = "declare_interior_mutable_const: 常量含 Atomic/UnsafeCell 字段; 作为每 CPU 状态模板复制到 static 数组, 保持 const 语义"
    )]
    const INIT: SoftirqState = SoftirqState {
        pending: AtomicU64::new(0),
        handlers: UnsafeCell::new([None; MAX_SOFTIRQS]),
        running: AtomicBool::new(false),
    };
    [INIT; MAX_CPUS]
};

#[inline]
fn current_cpu_id() -> usize {
    crate::arch!(cpu_id()) as usize
}

pub fn open_softirq(nr: SoftirqVec, handler: SoftirqHandler) {
    // B03-11: handlers 在所有 CPU 槽位都注册 (启动期单线程)。
    for state in &SOFTIRQ {
        // SAFETY: 启动期单线程, 无竞争访问同一槽位
        let handlers = unsafe { &mut *state.handlers.get() };
        handlers[nr.to_idx()] = Some(handler);
    }
}

#[inline]
pub fn raise_softirq(nr: SoftirqVec) {
    let cpu = current_cpu_id();
    if cpu < MAX_CPUS {
        SOFTIRQ[cpu]
            .pending
            .fetch_or(1u64 << nr.to_idx(), Ordering::Release);
    }
    // CPU id 越界: 静默丢弃 (启动期 cpu_local 尚未初始化)
}

#[inline]
pub fn raise_softirq_mask(mask: u64) {
    let cpu = current_cpu_id();
    if cpu < MAX_CPUS {
        SOFTIRQ[cpu].pending.fetch_or(mask, Ordering::Release);
    }
}

pub fn do_softirq() {
    let cpu = current_cpu_id();
    if cpu >= MAX_CPUS {
        return;
    }
    let state = &SOFTIRQ[cpu];

    if state
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    // SAFETY: handlers 在启动期已注册 (open_softirq 同步到所有 CPU), 运行时只读
    let handlers = unsafe { &*state.handlers.get() };

    loop {
        let pending = state.pending.swap(0, Ordering::AcqRel);
        if pending == 0 {
            break;
        }

        crate::arch!(interrupt_enable());

        for i in 0..MAX_SOFTIRQS {
            let bit = 1u64 << i;
            if pending & bit != 0 {
                if let Some(handler) = handlers[i] {
                    handler();
                }
            }
        }

        crate::arch!(interrupt_disable());
    }

    state.running.store(false, Ordering::Release);
}

#[inline]
pub fn in_softirq() -> bool {
    let cpu = current_cpu_id();
    if cpu < MAX_CPUS {
        SOFTIRQ[cpu].running.load(Ordering::Acquire)
    } else {
        false
    }
}

#[inline]
pub fn pending_softirq() -> bool {
    let cpu = current_cpu_id();
    if cpu < MAX_CPUS {
        SOFTIRQ[cpu].pending.load(Ordering::Acquire) != 0
    } else {
        false
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn softirq_init() {
    // 注册 Tasklet softirq 处理程序
    open_softirq(SoftirqVec::Tasklet, tasklet_softirq_handler);
    // 默认: Timer/NetRx/NetTx/Block 由各子系统在初始化时注册.
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn softirq_do() {
    do_softirq();
}

// ============================================================================
// Tasklet 框架 — 轻量级延迟工作执行 (参考 Linux tasklet)
// ============================================================================

use crate::framework::sync::IrqSpinLock;

/// Tasklet 回调函数类型
pub type TaskletFn = fn();

/// Tasklet 条目
struct TaskletEntry {
    func: Option<TaskletFn>,
    scheduled: AtomicBool,
}

impl TaskletEntry {
    const fn new() -> Self {
        Self {
            func: None,
            scheduled: AtomicBool::new(false),
        }
    }
}

/// 最大 tasklet 数量
const MAX_TASKLETS: usize = 32;

/// Tasklet 注册表
static TASKLETS: IrqSpinLock<[TaskletEntry; MAX_TASKLETS]> =
    IrqSpinLock::new([const { TaskletEntry::new() }; MAX_TASKLETS]);

/// 注册一个 tasklet 回调, 返回 tasklet ID
///
/// # Safety
/// `func` 必须是有效的函数指针, 且在 softirq 上下文中安全执行.
pub fn register_tasklet(func: TaskletFn) -> Option<usize> {
    let mut table = TASKLETS.lock();
    for (i, entry) in table.iter_mut().enumerate() {
        if entry.func.is_none() {
            entry.func = Some(func);
            return Some(i);
        }
    }
    None
}

/// 调度一个 tasklet (标记为 pending, 由 softirq 执行)
pub fn schedule_tasklet(id: usize) {
    let table = TASKLETS.lock();
    if let Some(entry) = table.get(id) {
        entry.scheduled.store(true, Ordering::Release);
    }
    drop(table);
    raise_softirq(SoftirqVec::Tasklet);
}

/// Tasklet softirq 处理程序 — 遍历所有已注册 tasklet, 执行已调度的
fn tasklet_softirq_handler() {
    let table = TASKLETS.lock();
    for entry in table.iter() {
        if entry.scheduled.load(Ordering::Acquire) {
            entry.scheduled.store(false, Ordering::Release);
            if let Some(func) = entry.func {
                drop(table);
                func();
                return; // 每次 softirq 轮次只执行一个 tasklet, 避免长时间占用
            }
        }
    }
}
