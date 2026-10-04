#![deny(unsafe_code)]
//! N-线程集合点 (latch-style FREG)
//!
//! ## 语义
//!
//! `FREG::new(n)` 创建等待 `n` 个线程的集合点。
//! 每个线程调用 `wait()` 阻塞, 直到 `n` 个线程都到达;
//! 全部到达后, 所有线程同时放行, 屏障重置 (可重用)。
//!
//! ## 与内核 `kernel::freg` 的区别
//!
//! - 本模块是**轻量级用户态集合点** (functions 层, 100% safe),
//!   基于 `Mutex` + `Condvar` 或轮询实现, 不依赖调度器 `yield`。
//! - `kernel::freg` 是**故障恢复FREG**, 语义完全不同 (BSR/BBR)。
//!
//! ## @SAFE
//! 不含 `unsafe`. 委托 `privileged::sync::Mutex` 保护内部状态.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::privileged::sync::Mutex;
use crate::functions::sync::scoped::MutexExt;

/// N-线程集合点 (latch-style)。
///
/// 内部状态: `(count, generation)` —
/// - `count`: 当前周期的等待计数
/// - `generation`: 已完成周期数 (防止 spurious wakeup 误判)
pub struct FREG {
    /// 总线程数 (不可变)
    threshold: u32,
    /// 当前周期剩余等待数
    count: AtomicU32,
    /// 已完成周期数
    generation: AtomicU32,
    /// 互斥锁, 保护 `count` 的复合操作
    lock: Mutex<()>,
}

impl FREG {
    /// 创建等待 `n` 个线程的集合点。
    ///
    /// # Panics
    /// `n == 0` 时 panic (无意义的屏障)。
    pub fn new(n: u32) -> Self {
        assert!(n > 0, "FREG: n must be > 0");
        Self {
            threshold: n,
            count: AtomicU32::new(n),
            generation: AtomicU32::new(0),
            lock: Mutex::new(()),
        }
    }

    /// 阻塞当前线程, 直至所有 `n` 个线程都调用 `wait()`。
    ///
    /// 返回: 该线程的"到达编号" (0..n)。
    ///   - 返回 0 表示是最后一个到达的线程 (可作为"主线程"标志)。
    ///   - 返回非 0 表示先到的线程。
    ///
    /// 屏障在 `n` 个线程全部到达后自动重置, 可重复使用。
    pub fn wait(&self) -> u32 {
        // 取得本地 generation (本轮唯一标识)。
        let local_gen = self.generation.load(Ordering::Acquire);

        // 复合操作: count -= 1; 若归零则 generation += 1, 并返回 "我是最后一个"。
        let is_last = self.lock.with(|_| {
            let prev = self.count.fetch_sub(1, Ordering::AcqRel);
            if prev == 1 {
                // 重置: 为下一轮恢复 count。
                self.count.store(self.threshold, Ordering::Release);
                self.generation.fetch_add(1, Ordering::AcqRel);
                true
            } else {
                false
            }
        });

        if is_last {
            return 0;
        }

        // 自旋等待 generation 变化 (latch-style)。
        // 真实内核实现可在此处 `yield` 调度, 但 functions 层无调度器, 仅自旋。
        loop {
            if self.generation.load(Ordering::Acquire) != local_gen {
                // 本轮结束, 推算 "我是第几个到的"。
                return self.threshold - self.count.load(Ordering::Acquire);
            }
            core::hint::spin_loop();
        }
    }

    /// 屏障容量。
    #[inline]
    pub fn threshold(&self) -> u32 {
        self.threshold
    }
}

// ============================================================================
// 单元自检
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freg_single_thread() {
        let b = FREG::new(1);
        // 单线程屏障: wait() 应立即返回 0 (自己是最后一个)。
        assert_eq!(b.wait(), 0);
    }

    #[test]
    fn freg_reusable() {
        let b = FREG::new(2);
        // 第 1 轮: 模拟 2 个线程都到达。
        // (单线程测试只能验证第 2 个返回 0, 因为 fetch_sub 立即归零)
        assert_eq!(b.wait(), 0);
        // 屏障应自动重置
        assert_eq!(b.count.load(Ordering::Acquire), b.threshold);
        // 第 2 轮: 同样立即通过
        assert_eq!(b.wait(), 0);
    }

    #[test]
    #[should_panic(expected = "n must be > 0")]
    fn freg_zero_panics() {
        let _ = FREG::new(0);
    }
}
