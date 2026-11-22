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

#![deny(missing_docs)]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering, compiler_fence};

/// 顺序锁 (seqlock): 读多写少场景的无锁读者同步原语.
#[repr(C)]
pub struct SeqLock<T> {
    pub(crate) sequence: AtomicUsize,
    data: UnsafeCell<T>,
}

// SAFETY: `SeqLock<T>` 的读写由 `sequence` 协议串行化 —— 写者经 `write()` 置奇序并独占 `UnsafeCell<T>`, 读者经 `read()`/`is_valid()` 复核; `T: Send` 覆盖跨线程独占转移.
unsafe impl<T: Send> Sync for SeqLock<T> {}

impl<T> SeqLock<T> {
    /// 构造顺序锁, 以 `data` 作为初始受保护值, 序号从 0 开始.
    pub const fn new(data: T) -> Self {
        Self {
            sequence: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    /// 读取当前序号 (Relaxed 语义, 仅供观测; 奇数值表示写者持有锁).
    pub fn current_sequence(&self) -> usize {
        self.sequence.load(Ordering::Relaxed)
    }

    /// 获取读守卫: 自旋等待无写者后返回, 读期间须用 `is_valid` 复检一致性.
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

    /// 获取写守卫: 自旋取得写者独占权 (序号置奇), `Drop` 时递增序号结束临界区.
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

    /// 尝试获取写守卫: 无写者时立即返回 `Some`, 否则返回 `None` 而不阻塞.
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

/// 顺序锁读守卫: 持有时可读取数据, 并用 `is_valid` 校验期间无写入.
pub struct SeqLockReadGuard<'a, T> {
    lock: &'a SeqLock<T>,
    seq1: usize,
}

impl<T> SeqLockReadGuard<'_, T> {
    /// 复检读期间无写者介入 (序号未变即数据保持一致).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        compiler_fence(Ordering::Release);
        let seq2 = self.lock.sequence.load(Ordering::Acquire);
        seq2 == self.seq1
    }

    /// 直接获取受保护数据的共享引用 (不校验一致性, 一致性由 `is_valid` 负责).
    #[must_use]
    pub fn get(&self) -> &T {
        // SAFETY: `self.lock` 是 `SeqLockReadGuard` 持有的 `&SeqLock<T>`; 读者仅在 `seq1` 为偶时入守卫, `&*data.get()` 的共享借用与写者的 `&mut` 由 `is_valid` 复检协议排他 (单写者多读者).
        unsafe { &*self.lock.data.get() }
    }

    /// 校验一致性后返回数据引用; 若读期间发生写入则返回 `None`.
    #[must_use]
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

/// 顺序锁写守卫: `Drop` 时结束写临界区并递增序号.
pub struct SeqLockWriteGuard<'a, T> {
    lock: &'a SeqLock<T>,
}

impl<T> core::ops::Deref for SeqLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        // SAFETY: `self.lock` 是 `SeqLockWriteGuard` 持有的 `&SeqLock<T>`; 写者以奇序独占 `UnsafeCell<T>` 直到 `Drop`, 故此处共享借用不与写者别名.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for SeqLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: `&mut self` 独占写守卫, 且守卫已置奇序 (见 `write`/`try_write`); 由此 `&mut *data.get()` 是唯一的可变借用, 写临界区到 `Drop` 结束.
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
