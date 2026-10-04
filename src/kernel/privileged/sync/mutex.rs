//! # 互斥锁 (Mutex) 实现
//!
//! 基于自旋锁 + yield 的互斥锁。
//!
//! ## 当前实现说明
//!
//! **注意**: 当前实现并非真正的睡眠锁，而是"自旋 + yield"混合模式：
//! - Fast path: 使用内部自旋锁快速尝试获取
//! - Slow path: 自旋等待 + 偶尔调用 `scheduler_yield()` 让出 CPU
//!
//! 这意味着持锁期间仍会消耗 CPU 周期进行自旋，不是完全的阻塞等待。
//! 未来可改进为基于等待队列的真正睡眠锁，避免 CPU 空转。
//!
//! ## 与 `SpinLock` 的区别
//!
//! | 特性 | `SpinLock` | Mutex |
//! |------|----------|--------|
//! | 等待方式 | 纯忙等待 | 自旋 + yield |
//! | 适用场景 | 极短临界区 | 中等长度临界区 |
//! | 中断上下文 | ✅ 可用 | ❌ 不可用 (会 yield) |
//! | 开销 | 低 (无系统调用) | 中 (涉及调度) |
//!
//! # 设计特性
//!
//! - **递归锁定支持**: 同一线程可多次 lock
//! - **公平性**: 竞争时偶尔让出 CPU，避免饥饿
//! - **调试信息**: 记录持有者 PID 和获取时间

use core::sync::atomic::Ordering;

#[cfg(debug_assertions)]
use super::lockdep::{self, LockClassDesc, LockClassId, LockKind};
use super::types::{MutexGuard, MutexInner};

/// 睡眠锁 (Mutex)
///
/// 当锁被持有时，后续的 `lock()` 调用会让出 CPU，
/// 允许调度器选择其他就绪进程运行。
pub struct Mutex<T: ?Sized> {
    /// 内部状态
    inner: MutexInner,
    /// Lockdep 锁类 ID (debug 模式下使用)
    #[cfg(debug_assertions)]
    lockdep_class: LockClassId,
    /// 被保护的数据 (必须为最后一项, 以支持 ?Sized)
    data: core::cell::UnsafeCell<T>,
}

// G-17 (2026-09-08): 当前进程 PID 获取 — 供 Mutex 重入检测 (raw_lock) 与
// 持有者记录 (acquire_lock_internal) 共用. 原实现内联于 acquire_lock_internal
// (局部 extern 声明 + items_after_statements expect), 重入检测需复用, 提取到模块级.
// SAFETY: process_get_current_pid 是有效的 C ABI 函数; 参数列表与声明一致
unsafe extern "C" {
    fn process_get_current_pid() -> u32;
}

// SAFETY: Mutex 通过内部自旋锁提供互斥.
// UnsafeCell 提供内部可变性; 对 T 的访问受锁获取约束.
// 要求 T: Send 是因为所有权可通过 lock/unlock 在线程间转移.
// T: Sync 自动满足, 因为同一时刻仅一个线程可持有锁, 提供排他访问.
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// 创建新的 Mutex
    pub fn new(data: T) -> Self {
        Self {
            inner: MutexInner::default(),
            data: core::cell::UnsafeCell::new(data),
            #[cfg(debug_assertions)]
            lockdep_class: LockClassId::INVALID,
        }
    }

    /// 创建命名 Mutex (用于调试 + lockdep)
    #[cfg(debug_assertions)]
    pub fn named(name: &'static str, data: T) -> Self {
        let class_id = lockdep::register_class(LockClassDesc {
            name,
            kind: LockKind::Mutex,
        });
        Self {
            inner: MutexInner::default(),
            data: core::cell::UnsafeCell::new(data),
            lockdep_class: class_id,
        }
    }

    /// 创建命名 Mutex (release 模式: 忽略名称)
    #[cfg(not(debug_assertions))]
    pub fn named(_name: &'static str, data: T) -> Self {
        Self::new(data)
    }

    /// 获取锁 (阻塞)
    ///
    /// 如果锁已被持有，当前线程会**让出 CPU**，
    /// 直到锁被释放后重新尝试获取。
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw_lock();

        // 创建守卫 (RAII)
        MutexGuard {
            // SAFETY: `self` 由调用方保证为有效指针; 只读访问
            data: unsafe { &mut *self.data.get() },
            _mutex: &self.inner,
        }
    }

    /// 尝试获取锁 (非阻塞)
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self.raw_trylock() {
            Some(MutexGuard {
                // SAFETY: `self` 由调用方保证为有效指针; 只读访问
                data: unsafe { &mut *self.data.get() },
                _mutex: &self.inner,
            })
        } else {
            None
        }
    }

    // ========================================================================
    // 底层原始操作 (供 FFI 使用)
    // ========================================================================

    /// 原始锁获取 (不返回 Guard)
    fn raw_lock(&self) {
        // Fast path: 尝试立即获取或同线程重入
        {
            // 先获取内部自旋锁
            self.inner.inner_spinlock.raw_lock();

            if !self.is_locked_internal() {
                // 成功获取
                self.acquire_lock_internal();
                self.inner.inner_spinlock.raw_unlock();
                return;
            }

            // G-17 (2026-09-08): 递归锁定 — 同一线程对已持有的 Mutex 再次 lock.
            // 文档承诺"递归锁定支持"但旧实现无 owner 检测, 同一线程二次 lock 在
            // slow path 死等 → 自死锁无限自旋 (G-11 kill 广播 / G-12 signalfd 均为
            // 受害点). owner 字段存持有进程 PID, 重入时 depth++ 直接返回, 由
            // raw_unlock 递减 (depth<=1 才完全释放). 此检测须在 inner_spinlock
            // 内进行, 保证 owner/depth 读改写原子.
            // SAFETY: process_get_current_pid 声明于模块顶部 (C ABI)
            let cur = unsafe { process_get_current_pid() } as i32;
            if self.inner.owner.load(Ordering::Acquire) == cur {
                self.inner.depth.fetch_add(1, Ordering::AcqRel);
                self.inner.inner_spinlock.raw_unlock();
                return;
            }

            #[cfg(feature = "debug_mutex")]
            log::warn!("MUTEX: lock contention detected");

            self.inner.inner_spinlock.raw_unlock();
        }

        // Slow path: 自旋 + yield (仅真竞争到达, 同线程重入已在上方处理)
        loop {
            // 检查是否可用
            self.inner.inner_spinlock.raw_lock();

            if !self.is_locked_internal() {
                self.acquire_lock_internal();
                self.inner.inner_spinlock.raw_unlock();
                return;
            }

            self.inner.inner_spinlock.raw_unlock();

            // 让出 CPU 给其他进程
            scheduler_yield();
        }
    }

    /// 原始尝试获取锁
    fn raw_trylock(&self) -> bool {
        self.inner.inner_spinlock.raw_lock();

        if self.is_locked_internal() {
            self.inner.inner_spinlock.raw_unlock();
            false
        } else {
            self.acquire_lock_internal();
            self.inner.inner_spinlock.raw_unlock();
            true
        }
    }

    /// 原始释放锁
    fn raw_unlock(&self) {
        // Lockdep: 通知锁释放
        #[cfg(debug_assertions)]
        lockdep::release(self.lockdep_class);

        self.inner.inner_spinlock.raw_lock();

        let depth = self.inner.depth.fetch_sub(1, Ordering::AcqRel);

        if depth <= 1 {
            // 完全释放
            self.inner.locked.store(0, Ordering::Release);
            self.inner.owner.store(-1, Ordering::Release);
            self.inner.acquire_time.store(0, Ordering::Release);
        }

        self.inner.inner_spinlock.raw_unlock();
    }

    // ========================================================================
    // 内部辅助函数
    // ========================================================================

    fn is_locked_internal(&self) -> bool {
        self.inner.locked.load(Ordering::Acquire) != 0
    }

    fn acquire_lock_internal(&self) {
        self.inner.locked.store(1, Ordering::Release);

        // 设置持有者 PID (从模块级 extern 获取, 见文件顶部 G-17 说明)
        // SAFETY: process_get_current_pid 声明于模块顶部 (C ABI)
        let pid = unsafe { process_get_current_pid() };
        self.inner.owner.store(pid as i32, Ordering::Release);

        // 重置递归深度
        self.inner.depth.store(1, Ordering::Release);

        // 记录获取时间
        self.inner.acquire_time.store(rdtsc(), Ordering::Release);

        // Lockdep: 通知锁获取
        #[cfg(debug_assertions)]
        lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());
    }

    /// 检查锁是否被持有
    pub fn is_locked(&self) -> bool {
        self.inner.locked.load(Ordering::Acquire) != 0
    }

    /// 获取当前持有者 PID (-1 = 未持有)
    pub fn owner(&self) -> i32 {
        self.inner.owner.load(Ordering::Acquire)
    }

    /// 获取递归深度
    pub fn depth(&self) -> u32 {
        self.inner.depth.load(Ordering::Acquire)
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// ============================================================================
// 条件变量 (CondVar)
// ============================================================================

/// 条件变量
///
/// 用于线程间通知机制，通常配合 Mutex 使用。
///
/// # Example
/// ```rust,ignore
/// let mutex = Mutex::new(false);
/// let cond = CondVar::new();
///
/// // Producer:
/// {
///     let mut guard = mutex.lock();
///     *guard = true;
/// } // 释放锁
/// cond.signal(&mutex);  // 通知一个等待者
///
/// // Consumer:
/// {
///     let mut guard = mutex.lock();
///     while !*guard {
///         cond.wait(&mutex);  // 释放锁并等待
///     }
/// }
/// ```
pub struct CondVar {
    waiters: core::sync::atomic::AtomicU32,
}

impl CondVar {
    pub const fn new() -> Self {
        Self {
            waiters: core::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn wait<T>(&self, mutex: &Mutex<T>) {
        self.waiters.fetch_add(1, Ordering::AcqRel);
        mutex.raw_unlock();
        scheduler_yield();
        mutex.lock();
        self.waiters.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn wait_timeout<T>(&self, mutex: &Mutex<T>, timeout_ms: u32) -> bool {
        self.waiters.fetch_add(1, Ordering::AcqRel);
        mutex.raw_unlock();

        // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        unsafe extern "C" {
            fn timer_sleep_busy(ms: u64);
        }
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            timer_sleep_busy(u64::from(timeout_ms));
        }

        mutex.lock();
        self.waiters.fetch_sub(1, Ordering::AcqRel);
        true
    }

    pub fn signal(&self) {
        if self.waiters.load(Ordering::Acquire) > 0 {
            scheduler_yield();
        }
    }

    pub fn broadcast(&self) {
        let count = self.waiters.load(Ordering::Acquire);
        if count > 0 {
            for _ in 0..count {
                scheduler_yield();
            }
        }
    }
}

impl Default for CondVar {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 读取 TSC 时间戳计数器 (架构无关封装)
fn rdtsc() -> u64 {
    crate::arch!(timestamp())
}

/// 让出 CPU 给调度器
fn scheduler_yield() {
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    unsafe extern "C" {
        fn scheduler_yield();
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        scheduler_yield();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mutex_basic() {
        let mutex = Mutex::new(42i32);
        assert!(!mutex.is_locked());

        {
            let guard = mutex.lock();
            assert!(mutex.is_locked());
            assert_eq!(*guard, 42);
        } // ← 自动 unlock

        assert!(!mutex.is_locked());
    }

    #[test]
    fn test_mutex_trylock() {
        let mutex = Mutex::new(100u32);

        let guard1 = mutex.try_lock().expect("first try should succeed");
        assert!(mutex.is_locked());
        assert_eq!(*guard1, 100);

        // 第二次尝试应失败
        assert!(mutex.try_lock().is_none());

        drop(guard1);
        assert!(!mutex.is_locked());
    }

    // UT-07 (2026-09-26): 注册侧 `sync::mutex::reentrant` 迁入 — G-17 (2026-09-08)
    // 回归: 同一线程对已持有 Mutex 再次 lock. 修复前 raw_lock 无 owner 重入检测,
    // 二次 lock 在 slow path 死等自死锁 (G-11 kill 广播 / G-12 signalfd 受害);
    // 修复后 depth 递增, 逐层 drop 释放.
    #[test]
    fn test_mutex_reentrant() {
        let mutex = Mutex::new(42i32);
        let guard1 = mutex.lock();
        let guard2 = mutex.lock();
        assert!(mutex.is_locked());
        assert_eq!(mutex.depth(), 2);
        assert_eq!(*guard2, 42);
        drop(guard2);
        assert!(mutex.is_locked());
        assert_eq!(mutex.depth(), 1);
        drop(guard1);
        assert!(!mutex.is_locked());
        assert_eq!(mutex.owner(), -1);
    }

    #[test]
    fn test_condvar_creation() {
        let cond = CondVar::new();
        let _ = cond; // 仅验证可以创建
    }
}
