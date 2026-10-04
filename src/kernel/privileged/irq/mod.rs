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

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use crate::privileged::config::MAX_CPUS;
use crate::privileged::racy_cell::RacyCell;

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

/// 全局 softirq 处理程序表.
///
/// handlers 在 `open_softirq` (启动期单线程) 一次性注册, 运行期只读; 故该表
/// 与具体 CPU 无关, 无需 per-CPU 复制 (原实现为每个 CPU 各存一份, 纯冗余).
static SOFTIRQ_HANDLERS: RacyCell<[Option<SoftirqHandler>; MAX_SOFTIRQS]> =
    RacyCell::new([None; MAX_SOFTIRQS]);

/// 每 CPU softirq 状态 — 独立的 `pending` 位图 + `running` 标志.
///
/// `running` 防止同一 CPU 上的 softirq 重入; `pending` per-CPU 化后各 CPU
/// 独立排程自己的软中断.
struct SoftirqState {
    pending: AtomicU64,
    running: AtomicBool,
}

impl SoftirqState {
    const fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            running: AtomicBool::new(false),
        }
    }
}

/// BSP 的静态 per-CPU softirq 状态 (`SoftirqState::new()` 全零, 落 `.bss`).
static SOFTIRQ_BSP: RacyCell<SoftirqState> = RacyCell::new(SoftirqState::new());

/// AP 的 per-CPU softirq 状态表 (按 `cpu_index` 索引, 槽位 0 保留给 BSP).
///
/// AP 的状态由 [`softirq_alloc_cpu`] 在 AP 上线前按需从页池分配, 以**内核高半区
/// 直接映射别名**存放 —— aarch64 运行期 TTBR0 的每进程 EL1 视图刻意移除 DRAM
/// 块, 低半区物理地址在运行期并非有效可解引用地址, 故必须走高半区别名.
/// 槽位为 `null` 表示尚未分配.
static SOFTIRQ_PER_CPU: [AtomicPtr<SoftirqState>; MAX_CPUS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_CPUS];

#[inline]
fn current_cpu_id() -> usize {
    crate::arch!(cpu_id()) as usize
}

/// 将 CPU 编号映射到 softirq 状态槽位下标.
#[inline]
fn softirq_slot(cpu: u32) -> usize {
    (cpu as usize) % MAX_CPUS
}

/// 获取指定 CPU 的 softirq 状态引用.
///
/// # 不变量
///
/// 调用方保证目标槽位已分配 (启动时序: `softirq_alloc_cpu` 早于 AP 使用 softirq).
/// 若槽位为 `null` (例如真机 LAPIC ID 与顺序 `cpu_index` 不一致导致查错槽位),
/// 回退 BSP 状态以保证内存安全 —— 这是已知的预存问题 (见 `smp::get_current_cpu`
/// 返回 LAPIC ID 的语义).
#[inline]
fn softirq_state(cpu: u32) -> &'static SoftirqState {
    let idx = softirq_slot(cpu);
    if idx == 0 {
        // SAFETY: SOFTIRQ_BSP 为静态实例; 运行期经原子访问.
        return unsafe { SOFTIRQ_BSP.get() };
    }
    let ptr = SOFTIRQ_PER_CPU[idx].load(Ordering::Acquire);
    if ptr.is_null() {
        // SAFETY: 回退 BSP 仅用于避免 null 解引用 (见 doc 的预存问题).
        return unsafe { SOFTIRQ_BSP.get() };
    }
    // SAFETY: 指针由 softirq_alloc_cpu 以 Release 发布, 指向页池分配且已清零的
    // SoftirqState; 高半区直接映射使其运行期可访问; 只读/原子访问.
    unsafe { &*ptr }
}

/// 为指定 `cpu_index` 分配 per-CPU softirq 状态 (幂等).
///
/// 槽位 0 为 BSP 静态实例, 直接返回 `true`. 返回 `false` 表示页池分配失败,
/// 调用方应放弃启动该 AP.
pub fn softirq_alloc_cpu(cpu_index: u32) -> bool {
    let idx = softirq_slot(cpu_index);
    if idx == 0 {
        return true;
    }
    if !SOFTIRQ_PER_CPU[idx].load(Ordering::Acquire).is_null() {
        return true;
    }
    let Some(ptr) = crate::privileged::mm::alloc_zeroed_page_as::<SoftirqState>() else {
        return false;
    };
    // 页已整体清零, SoftirqState 全零即 SoftirqState::new() 语义, 无需再 write.
    SOFTIRQ_PER_CPU[idx].store(ptr, Ordering::Release);
    true
}

pub fn open_softirq(nr: SoftirqVec, handler: SoftirqHandler) {
    // handlers 全局唯一, 启动期单线程注册; get_mut 独占访问 (无并发写者).
    SOFTIRQ_HANDLERS.get_mut()[nr.to_idx()] = Some(handler);
}

#[inline]
pub fn raise_softirq(nr: SoftirqVec) {
    let cpu = current_cpu_id();
    if cpu < MAX_CPUS {
        softirq_state(cpu as u32)
            .pending
            .fetch_or(1u64 << nr.to_idx(), Ordering::Release);
    }
    // CPU id 越界: 静默丢弃 (启动期 cpu_local 尚未初始化)
}

#[inline]
pub fn raise_softirq_mask(mask: u64) {
    let cpu = current_cpu_id();
    if cpu < MAX_CPUS {
        softirq_state(cpu as u32)
            .pending
            .fetch_or(mask, Ordering::Release);
    }
}

pub fn do_softirq() {
    let cpu = current_cpu_id();
    if cpu >= MAX_CPUS {
        return;
    }
    let state = softirq_state(cpu as u32);

    if state
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    // SAFETY: handlers 在启动期已注册 (open_softirq), 运行期只读.
    let handlers = unsafe { SOFTIRQ_HANDLERS.get() };

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
        softirq_state(cpu as u32).running.load(Ordering::Acquire)
    } else {
        false
    }
}

#[inline]
pub fn pending_softirq() -> bool {
    let cpu = current_cpu_id();
    if cpu < MAX_CPUS {
        softirq_state(cpu as u32).pending.load(Ordering::Acquire) != 0
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

use crate::privileged::sync::IrqSpinLock;

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
