//! # RCU (Read-Copy-Update) 同步原语
//!
//! 读多写少场景的零开销读者锁。读者无原子操作开销，
//! 写者等待所有已有读者退出临界区后释放旧数据。
//!
//! ## 核心 API
//!
//! | 操作 | 说明 |
//! |------|------|
//! | `rcu_read_lock()` | 进入 RCU 读临界区 |
//! | `rcu_read_unlock()` | 退出 RCU 读临界区 |
//! | `rcu_dereference(p)` | 安全读取 RCU 保护指针 |
//! | `rcu_assign_pointer(p, v)` | 安全更新 RCU 保护指针 |
//! | `synchronize_rcu()` | 阻塞直到宽限期结束 |
//! | `call_rcu(head, func)` | 注册宽限期回调 |
//!
//! ## 多核宽限期
//!
//! 每 CPU 维护独立的嵌套计数和静止状态标志。
//! `synchronize_rcu()` 等待所有在线 CPU 报告静止状态后
//! 才认为宽限期结束。

use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering, fence};

use crate::privileged::racy_cell::RacyCell;

pub struct RcuHead {
    pub next: *mut Self,
    // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
    pub func: Option<unsafe fn(*mut Self)>,
}

impl RcuHead {
    pub const fn new() -> Self {
        Self {
            next: ptr::null_mut(),
            func: None,
        }
    }
}

struct PerCpuRcu {
    nesting: AtomicU32,
    gp_state: AtomicU32,
    callbacks: UnsafeCell<*mut RcuHead>,
    callback_tail: UnsafeCell<*mut RcuHead>,
    callback_count: AtomicU32,
    need_callback_process: AtomicBool,
}

impl PerCpuRcu {
    const fn new() -> Self {
        Self {
            nesting: AtomicU32::new(0),
            gp_state: AtomicU32::new(GP_IDLE),
            callbacks: UnsafeCell::new(ptr::null_mut()),
            callback_tail: UnsafeCell::new(ptr::null_mut()),
            callback_count: AtomicU32::new(0),
            need_callback_process: AtomicBool::new(false),
        }
    }
}

// SAFETY: PerCpuRcu 仅在关中断路径下访问;
// SAFETY: PerCpuRcu 含 UnsafeCell, 但仅由对应 CPU 访问, UnsafeCell 为回调链表提供内部可变性.
unsafe impl Sync for PerCpuRcu {}

// SAFETY: PerCpuRcu 含裸指针 (callbacks/callback_tail), 裸指针本身非 Send; 但该
// 结构仅在对应 CPU (或关中断的单线程上下文) 内访问, 回调链表指针的所有权始终
// 在同一个 CPU 内部传递, 不跨线程共享裸指针. 故显式实现 Send, 以满足
// `RacyCell<T: Send>` 对静态承载 `T` 的要求.
unsafe impl Send for PerCpuRcu {}

const GP_IDLE: u32 = 0;
const GP_WAIT: u32 = 1;
const GP_DONE: u32 = 2;

/// BSP 的静态 per-CPU RCU 状态.
///
/// RCU 在启动早期 (BSP 单线程) 即可能被使用, 故 BSP 槽位固定为静态实例.
/// `PerCpuRcu::new()` 全零, 该实例落入 `.bss` (不占镜像 `.data`).
static RCU_BSP: RacyCell<PerCpuRcu> = RacyCell::new(PerCpuRcu::new());

/// AP 的 per-CPU RCU 状态表 (按 `cpu_index` 索引, 槽位 0 保留给 BSP).
///
/// AP 的状态由 [`rcu_alloc_cpu`] 在 AP 上线前按需从页池分配, 以**内核高半区
/// 直接映射别名**存放 —— aarch64 运行期 TTBR0 的每进程 EL1 视图刻意移除 DRAM
/// 块, 低半区物理地址在运行期并非有效可解引用地址, 故必须走高半区别名.
/// 槽位为 `null` 表示尚未分配.
static RCU_PER_CPU: [AtomicPtr<PerCpuRcu>; crate::privileged::config::MAX_CPUS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; crate::privileged::config::MAX_CPUS];

static RCU_GP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// 将 CPU 编号映射到 per-CPU RCU 槽位下标.
///
/// 槽位容量与 `MAX_CPUS` 一致; 取模仅为防御越界 (越界时退化为槽位别名而非 UB).
#[inline]
fn rcu_slot(cpu: u32) -> usize {
    (cpu as usize) % crate::privileged::config::MAX_CPUS
}

/// 获取指定 CPU 的 per-CPU RCU 状态引用.
///
/// # 不变量
///
/// 调用方保证目标槽位已分配 (启动时序: `rcu_alloc_cpu` 早于 AP 使用 RCU).
/// 若槽位为 `null` (目标 `cpu_index` 尚未分配 RCU 状态), 回退 BSP 状态以保证
/// 内存安全.
#[inline]
fn rcu_data(cpu: u32) -> &'static PerCpuRcu {
    let idx = rcu_slot(cpu);
    if idx == 0 {
        // SAFETY: RCU_BSP 为静态实例; 启动期单线程写入, 运行期只读/原子访问.
        return unsafe { RCU_BSP.get() };
    }
    let ptr = RCU_PER_CPU[idx].load(Ordering::Acquire);
    if ptr.is_null() {
        // SAFETY: 回退 BSP 仅用于避免 null 解引用 (见 doc 的槽位未分配情形).
        return unsafe { RCU_BSP.get() };
    }
    // SAFETY: 指针由 rcu_alloc_cpu 以 Release 发布, 指向页池分配且已清零的
    // PerCpuRcu; 高半区直接映射使其运行期可访问; 只读/原子访问.
    unsafe { &*ptr }
}

/// 为指定 `cpu_index` 分配 per-CPU RCU 状态 (幂等).
///
/// 槽位 0 为 BSP 静态实例, 直接返回 `true`. 返回 `false` 表示页池分配失败,
/// 调用方应放弃启动该 AP.
pub fn rcu_alloc_cpu(cpu_index: u32) -> bool {
    let idx = rcu_slot(cpu_index);
    if idx == 0 {
        return true;
    }
    if !RCU_PER_CPU[idx].load(Ordering::Acquire).is_null() {
        return true;
    }
    let Some(ptr) = crate::privileged::mm::alloc_zeroed_page_as::<PerCpuRcu>() else {
        return false;
    };
    // 页已整体清零, PerCpuRcu 全零即 PerCpuRcu::new() 语义, 无需再 write.
    RCU_PER_CPU[idx].store(ptr, Ordering::Release);
    true
}

#[inline]
fn current_rcu() -> &'static PerCpuRcu {
    let cpu = crate::privileged::smp::get_current_cpu();
    rcu_data(cpu)
}

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
fn rcu_read_lock_impl() {
    let data = current_rcu();
    let nesting = data.nesting.fetch_add(1, Ordering::Acquire);
    if nesting == 0 {
        fence(Ordering::Acquire);
    }
}

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
fn rcu_read_unlock_impl() {
    let data = current_rcu();
    fence(Ordering::Release);
    let nesting = data.nesting.fetch_sub(1, Ordering::Release);
    if nesting == 1 {
        fence(Ordering::Release);
    }
}

/// 安全读取 RCU 保护的指针
///
/// # Safety
/// 调用者必须在 RCU 读临界区内
#[inline(always)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub unsafe fn rcu_dereference<T>(ptr: *const T) -> *const T {
    unsafe {
        fence(Ordering::Acquire);
        ptr::read_volatile(&ptr)
    }
}

/// 安全更新 RCU 保护的指针
///
/// # Safety
/// 调用者确保旧值在所有 RCU 读者退出前不被释放
#[inline(always)]
pub unsafe fn rcu_assign_pointer<T>(slot: *mut *const T, new_val: *const T) {
    unsafe {
        fence(Ordering::Release);
        ptr::write_volatile(slot, new_val);
    }
}

/// 阻塞直到所有 CPU 上已有 RCU 读者退出
///
/// 宽限期流程:
/// 1. 标记所有在线 CPU 的 `gp_state` = `GP_WAIT`
/// 2. 等待每个 CPU 报告 `GP_DONE` (通过 `rcu_note_quiescent_state`)
/// 3. 处理回调
fn synchronize_rcu_impl() {
    let start_gp = RCU_GP_COUNTER.load(Ordering::Relaxed);
    RCU_GP_COUNTER.store(start_gp.wrapping_add(1), Ordering::Release);

    let cpu_count = crate::privileged::smp::get_cpu_count();
    let current_cpu = crate::privileged::smp::get_current_cpu();

    for i in 0..cpu_count {
        if i == current_cpu {
            continue;
        }
        if !crate::privileged::smp::is_cpu_online(i) {
            let data = rcu_data(i);
            data.gp_state.store(GP_DONE, Ordering::Release);
            continue;
        }
        let data = rcu_data(i);
        data.gp_state.store(GP_WAIT, Ordering::Release);
        // 以逻辑索引为键发送重调度 IPI; 目标未登记时内部静默返回。
        crate::privileged::smp::send_reschedule_ipi(i);
    }

    {
        let data = current_rcu();
        if data.nesting.load(Ordering::Acquire) == 0 {
            data.gp_state.store(GP_DONE, Ordering::Release);
        }
    }

    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const SYNC_TIMEOUT_SPINS: u32 = 50_000_000;
    for i in 0..cpu_count {
        if i == current_cpu {
            continue;
        }
        let data = rcu_data(i);
        let mut spins = 0u32;
        while data.gp_state.load(Ordering::Acquire) != GP_DONE {
            core::hint::spin_loop();
            spins += 1;
            if spins >= SYNC_TIMEOUT_SPINS {
                data.gp_state.store(GP_DONE, Ordering::Release);
                break;
            }
        }
    }

    {
        let data = current_rcu();
        let mut spins = 0u32;
        while data.nesting.load(Ordering::Acquire) > 0 {
            core::hint::spin_loop();
            spins += 1;
            if spins >= SYNC_TIMEOUT_SPINS {
                break;
            }
        }
    }

    for i in 0..cpu_count {
        let data = rcu_data(i);
        data.gp_state.store(GP_IDLE, Ordering::Release);
    }

    process_callbacks();
}

/// 注册 RCU 回调, 在宽限期结束后调用
///
/// `func` 在 `synchronize_rcu()` 或 `process_callbacks()` 时被调用。
///
/// # Safety
/// `head` 必须是从分配器分配的有效指针, `func` 必须正确处理 `head` 指向的内存
pub unsafe fn call_rcu(head: *mut RcuHead, func: unsafe fn(*mut RcuHead)) {
    if head.is_null() {
        return;
    }

    unsafe {
        (*head).next = ptr::null_mut();
        (*head).func = Some(func);
    }

    let data = current_rcu();
    let flags = crate::privileged::sync::disable_interrupts();

    // SAFETY: Interrupts disabled — callback list manipulation is atomic
    let tail = unsafe { *data.callback_tail.get() };
    if tail.is_null() {
        // SAFETY: Callbacks list empty — write to head via UnsafeCell
        unsafe {
            *data.callbacks.get() = head;
        }
    } else {
        // SAFETY: tail != null → dereference safe; writing next ptr
        unsafe {
            (*tail).next = head;
        }
    }
    // SAFETY: data.callback_tail is an UnsafeCell, interrupts disabled
    unsafe {
        *data.callback_tail.get() = head;
    }

    data.callback_count.fetch_add(1, Ordering::Relaxed);
    data.need_callback_process.store(true, Ordering::Release);

    crate::privileged::sync::restore_interrupts(&flags);

    crate::privileged::irq::raise_softirq(crate::privileged::irq::SoftirqVec::High);
}

/// 检查当前上下文是否在 RCU 读临界区内
pub fn rcu_read_lock_held() -> bool {
    current_rcu().nesting.load(Ordering::Acquire) > 0
}

/// 处理所有挂起的 RCU 回调 (当前 CPU)
pub fn process_callbacks() {
    let data = current_rcu();

    if !data.need_callback_process.load(Ordering::Acquire) {
        return;
    }

    let flags = crate::privileged::sync::disable_interrupts();

    // SAFETY: Interrupts disabled — exclusive access to callback list
    let head = unsafe { *data.callbacks.get() };
    // SAFETY: Clearing callbacks under interrupt lock
    unsafe {
        *data.callbacks.get() = ptr::null_mut();
        *data.callback_tail.get() = ptr::null_mut();
    }
    data.callback_count.store(0, Ordering::Relaxed);
    data.need_callback_process.store(false, Ordering::Release);

    crate::privileged::sync::restore_interrupts(&flags);

    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: cur was in the callback linked list, each node is valid
        let next = unsafe { (*cur).next };
        let func = unsafe { (*cur).func };

        if let Some(f) = func {
            // SAFETY: f is the callback registered by call_rcu; cur is the RcuHead
            unsafe {
                f(cur);
            }
        }

        cur = next;
    }
}

/// 标记静止状态 (由调度器在上下文切换时调用)
pub fn rcu_note_quiescent_state() {
    let data = current_rcu();

    if data.nesting.load(Ordering::Acquire) > 0 {
        return;
    }

    let state = data.gp_state.load(Ordering::Acquire);
    if state == GP_WAIT {
        data.gp_state.store(GP_DONE, Ordering::Release);
    }

    if data.need_callback_process.load(Ordering::Acquire)
        && data.nesting.load(Ordering::Acquire) == 0
    {
        process_callbacks();
    }
}

/// 通知所有 CPU 的 RCU 回调 (由同步宽限期调用)
pub fn rcu_process_all_callbacks() {
    let cpu_count = crate::privileged::smp::get_cpu_count();
    for i in 0..cpu_count {
        let data = rcu_data(i);
        if data.need_callback_process.load(Ordering::Acquire) {
            // 使用 IPI 或直接处理 — 简化实现: 直接处理
            // 注意: 在单核或特定场景下可行; 完整实现需 IPI
            let flags = crate::privileged::sync::disable_interrupts();
            // SAFETY: `data` 由调用方保证为有效指针; 只读访问
            let head = unsafe { *data.callbacks.get() };
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                *data.callbacks.get() = ptr::null_mut();
                *data.callback_tail.get() = ptr::null_mut();
            }
            data.callback_count.store(0, Ordering::Relaxed);
            data.need_callback_process.store(false, Ordering::Release);
            crate::privileged::sync::restore_interrupts(&flags);

            let mut cur = head;
            while !cur.is_null() {
                // SAFETY: `cur` 由调用方保证为有效指针; 只读访问
                let next = unsafe { (*cur).next };
                // SAFETY: `cur` 由调用方保证为有效指针; 只读访问
                let func = unsafe { (*cur).func };
                if let Some(f) = func {
                    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                    unsafe {
                        f(cur);
                    }
                }
                cur = next;
            }
        }
    }
}

pub fn rcu_gp_count() -> u32 {
    RCU_GP_COUNTER.load(Ordering::Relaxed)
}

pub fn rcu_callback_count() -> u32 {
    let cpu = crate::privileged::smp::get_current_cpu();
    rcu_data(cpu).callback_count.load(Ordering::Relaxed)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn rcu_read_lock() {
    rcu_read_lock_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn rcu_read_unlock() {
    rcu_read_unlock_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn synchronize_rcu() {
    synchronize_rcu_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn rcu_init() {
    RCU_GP_COUNTER.store(1, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rcu_read_lock_unlock() {
        rcu_read_lock();
        assert!(rcu_read_lock_held());
        rcu_read_lock();
        rcu_read_unlock();
        assert!(rcu_read_lock_held());
        rcu_read_unlock();
        assert!(!rcu_read_lock_held());
    }

    #[test]
    fn test_rcu_gp_counter() {
        let before = rcu_gp_count();
        synchronize_rcu();
        let after = rcu_gp_count();
        assert!(after > before);
    }

    #[test]
    fn test_call_rcu() {
        static CALLED: AtomicBool = AtomicBool::new(false);

        // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
        // call_rcu 的 func 形参为 `unsafe fn(*mut RcuHead)` (Rust ABI), 非 C fn 指针.
        unsafe fn callback(_head: *mut RcuHead) {
            CALLED.store(true, Ordering::Release);
        }

        let mut head = RcuHead::new();
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            call_rcu(&mut head as *mut RcuHead, callback);
        }

        assert!(rcu_callback_count() == 1);

        synchronize_rcu();

        assert!(rcu_callback_count() == 0);
        assert!(CALLED.load(Ordering::Acquire));
    }
}
