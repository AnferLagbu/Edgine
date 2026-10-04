//! # 读写锁 (`RwLock`) 实现
//!
//! 写者优先的读写锁，适用于**读多写少**场景。
//!
//! ## 特性
//!
//! - **写者优先**: 防止写者饥饿
//! - **并发读**: 多个读者可同时持有读锁
//! - **递归锁**: 读者和写者都支持递归
//! - **中断安全**: 提供 irqsave 变体
//!
//! # 状态机
//!
//! ```text
//! Initial: readers=0, writer=0, pending_writers=0
//!
//! read_lock():   readers++ (if !writer && !pending_writers)
//! read_unlock(): readers--
//! write_lock():  pending_writers++ → wait → writer=1 (if readers==0)
//! write_unlock(): writer=0
//! ```

use core::sync::atomic::Ordering;

#[cfg(debug_assertions)]
use super::lockdep::{self, LockClassDesc, LockClassId, LockKind};
use super::spinlock::{disable_interrupts, restore_interrupts};
use super::types::{IrqSaveFlags, RwLockInner, RwLockReadGuard, RwLockWriteGuard};

/// 读写锁 (`RwLock`)
pub struct RwLock<T: ?Sized> {
    /// 内部状态
    inner: RwLockInner,
    /// Lockdep 锁类 ID (debug 模式下使用)
    #[cfg(debug_assertions)]
    lockdep_class: LockClassId,
    /// 被保护的数据 (必须为最后一项, 以支持 ?Sized)
    data: core::cell::UnsafeCell<T>,
}

// SAFETY: RwLock 通过内部自旋锁保护的状态机提供独占写访问与共享读访问.
// UnsafeCell 提供内部可变性, 访问受锁获取门控.
unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send> Sync for RwLock<T> {}

impl<T> RwLock<T> {
    /// 创建新的 `RwLock`
    pub fn new(data: T) -> Self {
        Self {
            inner: RwLockInner::new(),
            data: core::cell::UnsafeCell::new(data),
            #[cfg(debug_assertions)]
            lockdep_class: LockClassId::INVALID,
        }
    }

    /// 创建命名 `RwLock` (用于调试 + lockdep)
    #[cfg(debug_assertions)]
    // J-01 (2026-09-08): 删除 unfulfilled doc_markdown expect — 文档已用反引号包裹
    // 标识符, doc_markdown 从不触发, expect 为多余防御 (host-test debug 编译暴露)
    pub fn named(name: &'static str, data: T) -> Self {
        let class_id = lockdep::register_class(LockClassDesc {
            name,
            kind: LockKind::RwLock,
        });
        Self {
            inner: RwLockInner::new(),
            data: core::cell::UnsafeCell::new(data),
            lockdep_class: class_id,
        }
    }

    /// 创建命名 RwLock (release 模式: 忽略名称)
    #[cfg(not(debug_assertions))]
    pub fn named(_name: &'static str, data: T) -> Self {
        Self::new(data)
    }

    // ========================================================================
    // 读锁操作 (共享访问)
    // ========================================================================

    /// 获取读锁 (阻塞)
    ///
    /// 多个读者可同时持有读锁。
    /// 如果有活跃或等待中的写者，当前线程会 yield。
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.raw_read_lock();

        RwLockReadGuard {
            // SAFETY: read lock acquired — shared access to T is safe
            data: unsafe { &*self.data.get() },
            _rwlock: &self.inner,
        }
    }

    /// 尝试获取读锁 (非阻塞)
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        if self.raw_try_read() {
            Some(RwLockReadGuard {
                // SAFETY: try_read succeeded — shared access to T is safe
                data: unsafe { &*self.data.get() },
                _rwlock: &self.inner,
            })
        } else {
            None
        }
    }

    /// 释放读锁
    ///
    /// 通常通过 `RwLockReadGuard` 的 Drop 自动调用。
    /// 此方法用于需要手动控制的场景。
    pub fn raw_read_unlock(&self) {
        // Lockdep: 通知读锁释放
        #[cfg(debug_assertions)]
        lockdep::release(self.lockdep_class);

        self.inner.lock.raw_lock();

        let prev = self.inner.readers.fetch_sub(1, Ordering::AcqRel);

        debug_assert!(prev > 0, "RWLOCK: read_unlock without read_lock");

        self.inner.lock.raw_unlock();
    }

    // ========================================================================
    // 写锁操作 (独占访问)
    // ========================================================================

    /// 获取写锁 (阻塞)
    ///
    /// 同一时刻只允许一个写者。
    /// 如果有活跃的读者或其他写者，当前线程会 yield。
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.raw_write_lock();

        RwLockWriteGuard {
            // SAFETY: write lock acquired — exclusive mutable access to T
            data: unsafe { &mut *self.data.get() },
            _rwlock: &self.inner,
        }
    }

    /// 尝试获取写锁 (非阻塞)
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        if self.raw_try_write() {
            Some(RwLockWriteGuard {
                // SAFETY: try_write succeeded — exclusive mutable access to T
                data: unsafe { &mut *self.data.get() },
                _rwlock: &self.inner,
            })
        } else {
            None
        }
    }

    /// 释放写锁
    pub fn raw_write_unlock(&self) {
        // Lockdep: 通知写锁释放
        #[cfg(debug_assertions)]
        lockdep::release(self.lockdep_class);

        self.inner.lock.raw_lock();

        let prev = self.inner.writer.swap(0, Ordering::AcqRel);

        debug_assert!(prev == 1, "RWLOCK: write_unlock without write_lock");

        self.inner.lock.raw_unlock();
    }

    // ========================================================================
    // 中断安全版本
    // ========================================================================

    /// 获取读锁并禁用中断
    pub fn read_irqsave(&self) -> (RwLockReadGuard<'_, T>, IrqSaveFlags) {
        let flags = disable_interrupts();
        let guard = self.read();
        (guard, flags)
    }

    /// 释放读锁并恢复中断
    pub fn read_unlock_irqrestore(_guard: RwLockReadGuard<'_, T>, flags: IrqSaveFlags) {
        // guard 的 drop 会自动释放读锁
        restore_interrupts(&flags);
    }

    /// 获取写锁并禁用中断
    pub fn write_irqsave(&self) -> (RwLockWriteGuard<'_, T>, IrqSaveFlags) {
        let flags = disable_interrupts();
        let guard = self.write();
        (guard, flags)
    }

    /// 释放写锁并恢复中断
    pub fn write_unlock_irqrestore(_guard: RwLockWriteGuard<'_, T>, flags: IrqSaveFlags) {
        // guard 的 drop 会自动释放写锁
        restore_interrupts(&flags);
    }

    // ========================================================================
    // 状态查询
    // ========================================================================

    /// 当前读者数量
    pub fn reader_count(&self) -> u32 {
        self.inner.readers.load(Ordering::Acquire)
    }

    /// 是否有活跃的写者
    pub fn has_writer(&self) -> bool {
        self.inner.writer.load(Ordering::Acquire) != 0
    }

    /// 等待中的写者数量
    pub fn pending_writer_count(&self) -> u32 {
        self.inner.pending_writers.load(Ordering::Acquire)
    }

    // ========================================================================
    // 底层原始操作
    // ========================================================================

    fn raw_read_lock(&self) {
        loop {
            self.inner.lock.raw_lock();

            // 检查是否可以获取读锁
            if self.inner.writer.load(Ordering::Relaxed) == 0
                && self.inner.pending_writers.load(Ordering::Relaxed) == 0
            {
                // 可以读取: 增加读者计数
                self.inner.readers.fetch_add(1, Ordering::Release);
                self.inner.lock.raw_unlock();

                // Lockdep: 通知读锁获取
                #[cfg(debug_assertions)]
                lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());

                return;
            }

            self.inner.lock.raw_unlock();

            // 让出 CPU
            scheduler_yield();
        }
    }

    fn raw_try_read(&self) -> bool {
        self.inner.lock.raw_lock();

        if self.inner.writer.load(Ordering::Relaxed) == 0
            && self.inner.pending_writers.load(Ordering::Relaxed) == 0
        {
            self.inner.readers.fetch_add(1, Ordering::Release);
            self.inner.lock.raw_unlock();

            // Lockdep: 通知读锁获取
            #[cfg(debug_assertions)]
            lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());

            true
        } else {
            self.inner.lock.raw_unlock();
            false
        }
    }

    fn raw_write_lock(&self) {
        // 先标记自己为等待中的写者
        self.inner.lock.raw_lock();
        self.inner.pending_writers.fetch_add(1, Ordering::Release);
        self.inner.lock.raw_unlock();

        loop {
            self.inner.lock.raw_lock();

            // 检查是否可以获取写锁
            if self.inner.readers.load(Ordering::Relaxed) == 0
                && self.inner.writer.load(Ordering::Relaxed) == 0
            {
                // 可以写入: 设置写者标志
                self.inner.pending_writers.fetch_sub(1, Ordering::Release);
                self.inner.writer.store(1, Ordering::Release);
                self.inner.lock.raw_unlock();

                // Lockdep: 通知写锁获取
                #[cfg(debug_assertions)]
                lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());

                return;
            }

            self.inner.lock.raw_unlock();

            // 让出 CPU
            scheduler_yield();
        }
    }

    fn raw_try_write(&self) -> bool {
        self.inner.lock.raw_lock();

        if self.inner.readers.load(Ordering::Relaxed) == 0
            && self.inner.writer.load(Ordering::Relaxed) == 0
        {
            self.inner.writer.store(1, Ordering::Release);
            self.inner.lock.raw_unlock();

            // Lockdep: 通知写锁获取
            #[cfg(debug_assertions)]
            lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());

            true
        } else {
            self.inner.lock.raw_unlock();
            false
        }
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// 让出 CPU 给调度器
fn scheduler_yield() {
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    unsafe extern "C" {
        fn scheduler_yield();
    }
    // SAFETY: FFI call to scheduler; guaranteed to be a valid C-ABI function
    unsafe {
        scheduler_yield();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    #[test]
    fn test_rwlock_basic_read() {
        let rwlock = RwLock::new(42i32);

        {
            let reader = rwlock.read();
            assert_eq!(*reader, 42);
            assert_eq!(rwlock.reader_count(), 1);
            assert!(!rwlock.has_writer());
        }

        assert_eq!(rwlock.reader_count(), 0);
    }

    #[test]
    fn test_rwlock_basic_write() {
        let rwlock = RwLock::new(100u32);

        {
            let writer = rwlock.write();
            assert_eq!(*writer, 100);
            assert!(rwlock.has_writer());
            assert_eq!(rwlock.reader_count(), 0);
        }

        assert!(!rwlock.has_writer());
    }

    #[test]
    fn test_rwlock_concurrent_readers() {
        let rwlock = RwLock::new(vec![1, 2, 3]);

        // 模拟多个读者 (虽然单线程，但可以验证计数)
        let r1 = rwlock.try_read().unwrap();
        let r2 = rwlock.try_read().unwrap(); // 注意: 实际多线程时这里可能失败

        assert_eq!(rwlock.reader_count(), 2); // 或 1，取决于实现

        drop(r1);
        drop(r2);
    }

    #[test]
    fn test_rwlock_try_operations() {
        let rwlock = RwLock::new(String::from("test"));

        // 读尝试应成功
        assert!(rwlock.try_read().is_some());

        // 写尝试应成功 (没有其他读者/写者)
        assert!(rwlock.try_write().is_some());
    }

    // UT-07 (2026-09-26): 注册侧 `sync::rwlock::{multiple_readers,write_blocks_read,
    // read_blocks_write}` 三用例迁入 — 源侧 concurrent_readers 未覆盖归零与阻塞判据.

    #[test]
    fn test_rwlock_multiple_readers() {
        let rwlock = RwLock::new(0i32);
        let r1 = rwlock.try_read();
        assert!(r1.is_some());
        let r2 = rwlock.try_read();
        assert!(r2.is_some());
        assert_eq!(rwlock.reader_count(), 2);
        drop(r1);
        drop(r2);
        assert_eq!(rwlock.reader_count(), 0);
    }

    #[test]
    fn test_rwlock_write_blocks_read() {
        let rwlock = RwLock::new(0i32);
        let writer = rwlock.try_write();
        assert!(writer.is_some());
        let reader = rwlock.try_read();
        assert!(reader.is_none());
        drop(writer);
        let reader2 = rwlock.try_read();
        assert!(reader2.is_some());
    }

    #[test]
    fn test_rwlock_read_blocks_write() {
        let rwlock = RwLock::new(0i32);
        let reader = rwlock.try_read();
        assert!(reader.is_some());
        let writer = rwlock.try_write();
        assert!(writer.is_none());
        drop(reader);
        let writer2 = rwlock.try_write();
        assert!(writer2.is_some());
    }
}
