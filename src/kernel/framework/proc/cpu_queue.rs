//! Per-CPU `RunQueue` — SMP 调度基础
//!
//! 在现有全局 MLFQ 之上添加 per-CPU 状态追踪：
//! - 每个 CPU 跟踪自己的 `current` PID
//! - per-CPU `need_reschedule` 标志
//! - 跨 CPU 重新调度 IPI (通过 `SOftirq::Sched`)
//!
//! ## 架构
//!
//! ```text
//! CPU 0                     CPU 1
//! schedule()               schedule()
//!   ├─ CpuQueue`[0]`          ├─ CpuQueue`[1]`
//!   │  current/need_resched  │  current/need_resched
//!   └─ SCHEDULER (global)   └─ SCHEDULER (global)
//!                                ↑
//!                          resched_ipi() → raise_softirq(Sched)
//! ```

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use super::types::Pid;
use crate::framework::racy_cell::RacyCell;

pub struct CpuQueue {
    pub current: AtomicU32,
    pub need_reschedule: AtomicBool,
    pub idle_pid: AtomicU32,
    pub online: AtomicBool,
}

// 所有字段 (AtomicU32, AtomicBool) 自动实现 Send + Sync.

impl CpuQueue {
    pub const fn new() -> Self {
        Self {
            current: AtomicU32::new(0),
            need_reschedule: AtomicBool::new(false),
            idle_pid: AtomicU32::new(0),
            online: AtomicBool::new(false),
        }
    }

    #[inline]
    pub fn get_current(&self) -> Option<Pid> {
        let pid = self.current.load(Ordering::Acquire);
        if pid == 0 { None } else { Some(pid) }
    }

    #[inline]
    pub fn set_current(&self, pid: Pid) {
        self.current.store(pid, Ordering::Release);
    }

    #[inline]
    pub fn set_need_reschedule(&self) {
        self.need_reschedule.store(true, Ordering::Release);
    }

    #[inline]
    pub fn take_need_reschedule(&self) -> bool {
        self.need_reschedule.swap(false, Ordering::AcqRel)
    }
}

/// 将 CPU 编号映射到 `CpuQueue` 槽位下标.
///
/// 槽位容量与 `MAX_CPUS` 一致; 取模仅为防御越界 (越界时退化为槽位别名而非 UB).
#[inline]
fn cpu_queue_slot(cpu_id: u32) -> usize {
    cpu_id as usize % crate::framework::config::MAX_CPUS
}

/// BSP 的静态 `CpuQueue` 实例 (`CpuQueue::new()` 全零, 落 `.bss`).
static CPU_QUEUE_BSP: RacyCell<CpuQueue> = RacyCell::new(CpuQueue::new());

/// AP 的 `CpuQueue` 表 (按 `cpu_index` 索引, 槽位 0 保留给 BSP).
///
/// AP 的队列由 [`init_cpu_queue`] 在 AP 上线前按需从页池分配, 以**内核高半区
/// 直接映射别名**存放 —— aarch64 运行期 TTBR0 的每进程 EL1 视图刻意移除 DRAM
/// 块, 低半区物理地址在运行期并非有效可解引用地址, 故必须走高半区别名.
/// 槽位为 `null` 表示尚未分配.
static CPU_QUEUES: [AtomicPtr<CpuQueue>; crate::framework::config::MAX_CPUS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; crate::framework::config::MAX_CPUS];

/// 为指定 `cpu_id` 分配 `CpuQueue` (幂等). 槽位 0 为 BSP 静态实例.
///
/// 返回 `false` 表示页池分配失败.
fn cpu_queue_alloc(cpu_id: u32) -> bool {
    let idx = cpu_queue_slot(cpu_id);
    if idx == 0 {
        return true;
    }
    if !CPU_QUEUES[idx].load(Ordering::Acquire).is_null() {
        return true;
    }
    let Some(ptr) = crate::framework::mm::alloc_zeroed_page_as::<CpuQueue>() else {
        return false;
    };
    // 页已整体清零, CpuQueue 全零即 CpuQueue::new() 语义, 无需再 write.
    CPU_QUEUES[idx].store(ptr, Ordering::Release);
    true
}

/// 获取指定 CPU 的 `CpuQueue` 引用.
///
/// # 不变量
///
/// 调用方保证目标槽位已分配 (启动时序: `init_cpu_queue` 早于该 CPU 使用队列).
/// 若槽位为 `null` (例如真机 LAPIC ID 与顺序 `cpu_index` 不一致导致查错槽位),
/// 回退 BSP 以保证内存安全 —— 这是已知的预存问题 (见 `smp::get_current_cpu`
/// 返回 LAPIC ID 的语义).
pub fn cpu_queue(cpu_id: u32) -> &'static CpuQueue {
    let idx = cpu_queue_slot(cpu_id);
    if idx == 0 {
        // SAFETY: CPU_QUEUE_BSP 为静态实例; 运行期经原子访问.
        return unsafe { CPU_QUEUE_BSP.get() };
    }
    let ptr = CPU_QUEUES[idx].load(Ordering::Acquire);
    if ptr.is_null() {
        // SAFETY: 回退 BSP 仅用于避免 null 解引用 (见 doc 的预存问题).
        return unsafe { CPU_QUEUE_BSP.get() };
    }
    // SAFETY: 指针由 cpu_queue_alloc 以 Release 发布, 指向页池分配且已清零的
    // CpuQueue; 高半区直接映射使其运行期可访问; 只读/原子访问.
    unsafe { &*ptr }
}

pub fn current_cpu_queue() -> &'static CpuQueue {
    let cpu_id = crate::framework::smp::get_current_cpu();
    cpu_queue(cpu_id)
}

/// 初始化指定 CPU 的 `CpuQueue` (上线前一次性, 幂等).
///
/// 返回 `false` 表示页池分配失败 —— 此时直接返回, 绝不回退写 BSP 队列,
/// 由调用方放弃启动该 AP.
pub fn init_cpu_queue(cpu_id: u32, idle_pid: Pid) -> bool {
    if !cpu_queue_alloc(cpu_id) {
        return false;
    }
    let q = cpu_queue(cpu_id);
    q.idle_pid.store(idle_pid, Ordering::Release);
    q.current.store(idle_pid, Ordering::Release);
    q.online.store(true, Ordering::Release);
    true
}

/// 向目标 CPU 发送重新调度 IPI
pub fn resched_cpu(target_cpu: u32) {
    let current = crate::framework::smp::get_current_cpu();
    if target_cpu == current {
        current_cpu_queue().set_need_reschedule();
        return;
    }

    cpu_queue(target_cpu).set_need_reschedule();

    let target_apic_id = crate::framework::smp::get_apic_id(target_cpu);
    if target_apic_id != 0xFFFF {
        crate::arch!(send_ipi(target_apic_id, 0xFE));
    }
}

/// IPI 重新调度入口 (由 IPI handler 调用，在目标 CPU 上执行)
/// 通过 softirq 延迟执行 `schedule()`
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn resched_ipi_handler() {
    crate::framework::irq::raise_softirq(crate::framework::irq::SoftirqVec::Sched);
}

/// 注册 softirq Sched handler (在 scheduler init 时调用)
pub fn register_sched_softirq() {
    crate::framework::irq::open_softirq(
        crate::framework::irq::SoftirqVec::Sched,
        sched_softirq_handler,
    );
}

fn sched_softirq_handler() {
    let q = current_cpu_queue();
    if q.take_need_reschedule() {
        super::scheduler::SCHEDULER.schedule();
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn cpq_init(cpu_id: u32, idle_pid: Pid) {
    init_cpu_queue(cpu_id, idle_pid);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn cpq_resched_cpu(target_cpu: u32) {
    resched_cpu(target_cpu);
}
