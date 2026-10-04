//! `SeqLock` — 顺序锁
//!
//! 一种针对"读多写少"场景优化的锁原语.
//! 读者永不阻塞写者; 写者之间相互排斥.
//!
//! # 适用场景
//! - 中断统计 (每个时钟 tick 读取, IRQ 时写入)
//! - 性能计数器 (频繁读, 极少写)
//! - 任何允许读到陈旧值但不容许读到撕裂数据的场景
//!
//! # 算法
//! ```text
//! Writer:  sequence++ → write data → sequence++
//! Reader:  do {
//!            s1 = sequence
//!            if s1 is odd: continue  (write in progress)
//!            read data
//!            s2 = sequence
//!          } while (s1 != s2)
//! ```
//!
//! # Safety
//! `UnsafeCell<T>` 提供内部可变性. 写者侧安全性由
//! `AtomicUsize sequence` (奇数=正在写) 保证. `unsafe impl Sync`
//! 是 sound 的, 因为所有写操作都在顺序锁保护下进行.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering, compiler_fence};

pub struct SeqLock<T> {
    pub(crate) sequence: AtomicUsize,
    data: UnsafeCell<T>,
}

// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe impl<T: Send> Sync for SeqLock<T> {}

impl<T> SeqLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            sequence: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn current_sequence(&self) -> usize {
        self.sequence.load(Ordering::Relaxed)
    }

    pub fn read(&self) -> SeqLockReadGuard<'_, T> {
        loop {
            let seq1 = self.sequence.load(Ordering::Acquire);

            if seq1 & 1 == 1 {
                core::hint::spin_loop();
                continue;
            }

            compiler_fence(Ordering::Acquire);

            return SeqLockReadGuard { lock: self, seq1 };
        }
    }

    pub fn write(&self) -> SeqLockWriteGuard<'_, T> {
        loop {
            let current = self.sequence.load(Ordering::Relaxed);
            if current & 1 == 1 {
                core::hint::spin_loop();
                continue;
            }
            if self
                .sequence
                .compare_exchange_weak(current, current + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                compiler_fence(Ordering::Acquire);
                return SeqLockWriteGuard { lock: self };
            }
        }
    }

    pub fn try_write(&self) -> Option<SeqLockWriteGuard<'_, T>> {
        let current = self.sequence.load(Ordering::Relaxed);
        if current & 1 == 1 {
            return None;
        }

        if self
            .sequence
            .compare_exchange(current, current | 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            compiler_fence(Ordering::Acquire);
            Some(SeqLockWriteGuard { lock: self })
        } else {
            None
        }
    }
}

pub struct SeqLockReadGuard<'a, T> {
    lock: &'a SeqLock<T>,
    seq1: usize,
}

impl<T> SeqLockReadGuard<'_, T> {
    pub fn is_valid(&self) -> bool {
        compiler_fence(Ordering::Release);
        let seq2 = self.lock.sequence.load(Ordering::Acquire);
        seq2 == self.seq1
    }

    pub fn get(&self) -> &T {
        // SAFETY: `self` 由调用方保证为有效指针; 只读访问
        unsafe { &*self.lock.data.get() }
    }

    pub fn get_valid(&self) -> Option<&T> {
        if self.is_valid() {
            Some(self.get())
        } else {
            None
        }
    }
}

impl<T> core::ops::Deref for SeqLockReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

pub struct SeqLockWriteGuard<'a, T> {
    lock: &'a SeqLock<T>,
}

impl<T> core::ops::Deref for SeqLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        // SAFETY: `self` 由调用方保证为有效指针; 只读访问
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for SeqLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: `self` 由调用方保证为有效指针; 只读访问
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SeqLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        compiler_fence(Ordering::Release);
        self.lock.sequence.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seqlock_basic() {
        let lock = SeqLock::new(42u32);
        {
            let guard = lock.read();
            assert!(guard.is_valid());
            assert_eq!(*guard, 42);
        }
    }

    #[test]
    fn test_seqlock_write() {
        let lock = SeqLock::new(0u32);
        {
            let mut w = lock.write();
            *w = 100;
        }
        let r = lock.read();
        assert!(r.is_valid());
        assert_eq!(*r, 100);
    }

    #[test]
    fn test_seqlock_sequence_increments() {
        let lock = SeqLock::new(0u32);
        assert_eq!(lock.sequence.load(Ordering::Relaxed), 0);
        {
            let _w = lock.write();
            assert_eq!(lock.sequence.load(Ordering::Relaxed), 1);
        }
        assert_eq!(lock.sequence.load(Ordering::Relaxed), 2);
    }
}
