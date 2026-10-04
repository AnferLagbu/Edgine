//! IrqSpinLock — TCB 中断安全自旋锁
//!
//! 在 `lock()` 时保存 IF 标志并 `cli` 屏蔽中断, `unlock()` (guard drop) 时恢复 IF。
//!
//! ## 设计动机
//!
//! 防止:
//! - 中断处理程序与线程争用同一锁导致死锁
//! - 持锁状态下被中断打断, 中断处理程序自旋等待
//!
//! ## SAFETY 契约
//!
//! - 不可在中断上下文使用 (会嵌套屏蔽, 丢失中断)
//! - 不可与 `SpinLock` 嵌套使用 (顺序无定义)
//! - `Send`/`Sync`: T: Send 即可, 因中断屏蔽保证临界区原子性
//!
//! ## 与 functions::sync::irq_lock 的关系
//!
//! functions 层的 `IrqSpinLock` 是本类型的类型别名 (保持 API 兼容)。
//! 所有 unsafe 集中在 privileged, functions 零 unsafe。
//!
//! ## 实现
//!
//! - `SpinLock` (raw, 来自 `sync::spinlock`) 提供底层原子锁原语 (`raw_lock`/`raw_unlock`)
//! - 由于 `SpinLock::raw_lock` 需要 `&mut self`, 我们将 `SpinLock` 包装在 `UnsafeCell` 中
//! - `UnsafeCell<T>` 用于保护被守卫的数据
//! - 嵌套深度计数器: 防止 lock 期间再 lock 时错误恢复 IF

use core::cell::UnsafeCell;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

use crate::privileged::sync::{IrqSaveFlags, SpinLock, disable_interrupts, restore_interrupts};
#[cfg(debug_assertions)]
use crate::privileged::sync::{LockClassDesc, LockClassId, LockKind};

/// 中断安全自旋锁 (TCB)。
///
/// 在 `lock()` 时保存 IF 标志并屏蔽中断, `unlock()` (guard drop) 时恢复原始 IF 状态。
///
/// # 示例
///
/// ```ignore
/// let data = IrqSpinLock::new(0u32);
/// data.with_mut(|g| *g += 1);
/// ```
pub struct IrqSpinLock<T> {
    /// 底层自旋锁 (放在 `UnsafeCell` 中以便从 &self 调用 `raw_lock`)
    lock: UnsafeCell<SpinLock>,
    /// 被保护的数据
    data: UnsafeCell<T>,
    /// 嵌套深度 (防止 lock 期间再 lock 时错误恢复 IF)
    depth: UnsafeCell<AtomicU32>,
    /// Lockdep 锁类 ID (debug 模式下使用)
    #[cfg(debug_assertions)]
    lockdep_class: LockClassId,
}

// SAFETY: 中断屏蔽保证临界区原子性, T: Send 即可。
unsafe impl<T: Send> Send for IrqSpinLock<T> {}
// SAFETY: 共享引用跨线程安全, 中断屏蔽保证访问互斥。
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T: fmt::Debug> fmt::Debug for IrqSpinLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            // 拿不到锁时, 用占位符表示 (避免 Debug 死锁)
            None => f.write_str("IrqSpinLock(<locked>)"),
            Some(guard) => f
                .debug_struct("IrqSpinLock")
                .field("data", &*guard)
                .finish(),
        }
    }
}

impl<T> IrqSpinLock<T> {
    /// 创建新的中断安全自旋锁, 包装初始数据。
    pub const fn new(data: T) -> Self {
        Self {
            lock: UnsafeCell::new(SpinLock::new()),
            data: UnsafeCell::new(data),
            depth: UnsafeCell::new(AtomicU32::new(0)),
            #[cfg(debug_assertions)]
            lockdep_class: LockClassId::INVALID,
        }
    }

    /// 创建命名 `IrqSpinLock` (用于调试 + lockdep)
    #[cfg(debug_assertions)]
    // J-01 (2026-09-08): 删除 unfulfilled doc_markdown expect — 文档已用反引号包裹
    // 标识符, doc_markdown 从不触发, expect 为多余防御 (host-test debug 编译暴露)
    pub fn named(name: &'static str, data: T) -> Self {
        let class_id = crate::privileged::sync::register_class(LockClassDesc {
            name,
            kind: LockKind::IrqSpinLock,
        });
        Self {
            lock: UnsafeCell::new(SpinLock::named(name)),
            data: UnsafeCell::new(data),
            depth: UnsafeCell::new(AtomicU32::new(0)),
            lockdep_class: class_id,
        }
    }

    /// 创建命名 IrqSpinLock (release 模式: 忽略名称)
    #[cfg(not(debug_assertions))]
    pub fn named(_name: &'static str, data: T) -> Self {
        Self::new(data)
    }

    /// 获取锁并返回 RAII Guard, 持锁期间屏蔽中断。
    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let prev = disable_interrupts();
        // SAFETY: cli 屏蔽中断后, 此 CPU 上不存在并发访问; 我们独占借用。
        // 同一 IrqSpinLock 上的 lock 调用由 cli + 自旋锁串行化。
        unsafe { &mut *self.lock.get() }.raw_lock();
        // SAFETY: 已持有锁, 任何其他访问者都被锁在外。
        let data_ref = unsafe { &mut *self.data.get() };
        // SAFETY: 仅本线程访问 depth (cli 保证 ISR 不并发)。
        unsafe { &*self.depth.get() }.fetch_add(1, Ordering::Relaxed);

        // Lockdep: 通知锁获取 (IrqSpinLock 始终在中断上下文安全)
        #[cfg(debug_assertions)]
        crate::privileged::sync::acquire(self.lockdep_class, true);

        IrqSpinLockGuard {
            data: data_ref,
            lock_ptr: self.lock.get(),
            depth_ptr: self.depth.get(),
            prev_if: Some(prev),
            #[cfg(debug_assertions)]
            lockdep_class: self.lockdep_class,
        }
    }

    /// 不加锁直接获取内部数据的可变引用 (进程上下文切换专用)。
    ///
    /// # Safety
    ///
    /// 调用方必须保证调用期间对 `self` 无并发访问。用于 `process_switch_asm`
    /// 切换路径: 单核 (smp=off) + `process_switch_asm` 入口 `cli` 保证排他。
    /// 不能使用 `lock()` 获取 Guard 再调 `context_switch`, 因为切换后函数
    /// 永不返回 (iretq 到 next 用户态), Guard 的 Drop 不执行 → 锁永久泄漏 →
    /// next 进程运行时 `p.context.lock()` (如 syscall 入口保存寄存器) 自旋死锁。
    pub unsafe fn get_mut_unchecked(&self) -> &mut T {
        // SAFETY: 调用方保证无并发访问 (见函数文档)
        unsafe { &mut *self.data.get() }
    }

    /// 闭包 API: 获取锁, 持有期间屏蔽中断, 执行闭包后自动释放。
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock();
        f(&mut *guard)
    }

    /// 不可变闭包版本。
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard = self.lock();
        f(&*guard)
    }

    /// 尝试获取锁, 不阻塞。
    ///
    /// 成功时返回 `Some(Guard)`, 失败 (锁已被持有) 时返回 `None`。
    /// 注意: 与 `lock()` 不同, `try_lock()` 在等待期间**不**屏蔽中断,
    /// 因此不应在中断上下文使用。
    pub fn try_lock(&self) -> Option<IrqSpinLockGuard<'_, T>> {
        use crate::privileged::sync::TryLockResult;
        let inner_ptr = self.lock.get();
        // SAFETY: 持 &self 借用, 通过 UnsafeCell 获取 &mut 底层 SpinLock; 自旋锁本身
        // 通过原子操作保证即使两个 &mut 并发也不冲突 (compare_exchange 原子性)。
        let result = unsafe { &mut *inner_ptr }.try_lock();
        if matches!(result, TryLockResult::WouldBlock) {
            return None;
        }
        // 获取成功后才 cli (与 lock() 顺序一致, 保证 guard drop 时的 IF 语义)
        let prev = disable_interrupts();
        let data_ref = unsafe { &mut *self.data.get() };
        // SAFETY: 仅本线程访问 depth (cli 保证 ISR 不并发)。
        unsafe { &*self.depth.get() }.fetch_add(1, Ordering::Relaxed);
        Some(IrqSpinLockGuard {
            data: data_ref,
            lock_ptr: inner_ptr,
            depth_ptr: self.depth.get(),
            prev_if: Some(prev),
            #[cfg(debug_assertions)]
            lockdep_class: self.lockdep_class,
        })
    }

    /// 返回被保护数据的裸指针 (用于 `static` 场景下获取 `&'static mut T`)。
    ///
    /// # Safety
    ///
    /// 调用方必须持有本锁 (通过 `lock()` 或 `try_lock()`)，且不得同时通过
    /// `IrqSpinLockGuard` 的 `DerefMut` 借用同一字段。
    #[inline(always)]
    pub unsafe fn data_ptr(&self) -> *mut T {
        // SAFETY: UnsafeCell::get 返回 *mut T，调用方保证互斥访问。
        self.data.get()
    }

    /// 消费锁, 取出内部数据 (调用方需保证无并发访问)。
    ///
    /// # Safety
    ///
    /// 调用方必须保证当前没有其他线程或中断上下文在访问此锁。
    pub unsafe fn into_inner(self) -> T {
        // SAFETY: 借用规则由调用方契约保证 (见方法文档)。
        self.data.into_inner()
    }
}

/// 中断安全自旋锁的 RAII Guard。
pub struct IrqSpinLockGuard<'a, T> {
    data: &'a mut T,
    /// 指向所属 `IrqSpinLock` 的 `SpinLock` 的裸指针 (cli 保证独占)
    lock_ptr: *mut SpinLock,
    /// 指向所属 `IrqSpinLock` 的深度计数器的裸指针 (cli 保证独占)
    depth_ptr: *mut AtomicU32,
    /// `lock()` 时保存的 IF 标志
    prev_if: Option<IrqSaveFlags>,
    /// Lockdep 锁类 ID (debug 模式下使用)
    #[cfg(debug_assertions)]
    lockdep_class: LockClassId,
}

impl<T> Deref for IrqSpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data
    }
}

impl<T> DerefMut for IrqSpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

impl<T> Drop for IrqSpinLockGuard<'_, T> {
    fn drop(&mut self) {
        // Lockdep: 通知锁释放
        #[cfg(debug_assertions)]
        crate::privileged::sync::release(self.lockdep_class);

        // SAFETY: 持有 lock_ptr 上的锁, 任何其他访问者都被锁在外; cli 屏蔽中断。
        unsafe { &*self.lock_ptr }.raw_unlock();
        // SAFETY: 仅本线程访问 depth (cli 保证 ISR 不并发)。
        let prev = unsafe { &*self.depth_ptr }.fetch_sub(1, Ordering::Relaxed);
        if prev == 1 {
            if let Some(flags) = self.prev_if.take() {
                restore_interrupts(&flags);
            }
        }
    }
}
