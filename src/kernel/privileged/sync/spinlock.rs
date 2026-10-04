//! # 自旋锁 (`SpinLock`) 实现
//!
//! 高效的忙等待锁，基于原子交换指令 (xchg)。
//!
//! ## 特性
//!
//! - **零开销**: 无系统调用，纯用户态自旋
//! - **公平性**: 使用 pause 指令降低功耗
//! - **中断安全**: 提供 irqsave/irqrestore 变体
//! - **调试支持**: 可选的锁持有者跟踪
//!
//! # 适用场景
//!
//! ✅ 临界区极短 (< 1μs)
//! ✅ 中断上下文 / 不能睡眠的场景
//! ✅ 实时性要求高的代码路径
//!
//! ❌ 不适合长时间持有 (浪费 CPU)

use core::sync::atomic::{Ordering, fence};

#[cfg(debug_assertions)]
use super::lockdep::{self, LockClassDesc, LockClassId, LockKind};
pub use super::types::IrqSaveFlags;
use super::types::{SpinLockGuard, SpinLockInner, TryLockResult};

/// 自旋锁 (`SpinLock`)
///
/// 基于 `x86_64` 的 `xchg` 指令实现原子锁定。
/// 在自旋循环中插入 `pause` 指令以优化性能。
pub struct SpinLock {
    /// 内部状态
    inner: SpinLockInner,
    /// Lockdep 锁类 ID (debug 模式下使用)
    #[cfg(debug_assertions)]
    lockdep_class: LockClassId,
}

impl SpinLock {
    /// 创建新的自旋锁
    pub const fn new() -> Self {
        Self {
            inner: SpinLockInner::new(),
            #[cfg(debug_assertions)]
            lockdep_class: LockClassId::INVALID,
        }
    }

    /// 创建命名自旋锁 (用于调试 + lockdep)
    #[cfg(debug_assertions)]
    pub fn named(name: &'static str) -> Self {
        let class_id = lockdep::register_class(LockClassDesc {
            name,
            kind: LockKind::SpinLock,
        });
        let mut lock = Self {
            inner: SpinLockInner::new(),
            lockdep_class: class_id,
        };
        lock.inner.name = name;
        lock
    }

    /// 创建命名自旋锁 (release 模式: 忽略名称)
    #[cfg(not(debug_assertions))]
    pub fn named(_name: &'static str) -> Self {
        Self::new()
    }

    /// 获取锁 (阻塞直到成功)
    ///
    /// # Safety
    /// 调用者必须确保在适当的时候调用 unlock，
    /// 否则会导致死锁。推荐使用 `lock()` 方法返回 Guard。
    pub fn raw_lock(&mut self) {
        // Fast path: 尝试立即获取
        if self
            .inner
            .locked
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            #[cfg(debug_assertions)]
            self.debug_acquire();
            #[cfg(debug_assertions)]
            lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());
            return; // 成功获取
        }

        // Slow path: 自旋等待
        while self
            .inner
            .locked
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // 提示 CPU 我们在自旋等待 (pause / yield)
            core::hint::spin_loop();
        }

        #[cfg(debug_assertions)]
        self.debug_acquire();
        #[cfg(debug_assertions)]
        lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());
    }

    /// ✅ P1-8 修复: 带超时的锁获取 (防止死锁)
    ///
    /// # Arguments
    /// * `max_spins` - 最大自旋次数 (建议: `10_000_000` ≈ 数秒 @ GHz CPU)
    ///
    /// # Returns
    /// - `TryLockResult::Acquired`: 成功获取
    /// - `TryLockResult::WouldBlock`: 超时未获取到
    ///
    /// # Usage
    /// ```rust,ignore
    /// match lock.raw_lock_with_timeout(1_000_000) {
    ///     TryLockResult::Acquired => { /* 临界区 */ lock.raw_unlock(); }
    ///     TryLockResult::WouldBlock => { /* 处理超时 */ }
    /// }
    /// ```
    pub fn raw_lock_with_timeout(&mut self, max_spins: usize) -> TryLockResult {
        // Fast path: 尝试立即获取
        if self
            .inner
            .locked
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            #[cfg(debug_assertions)]
            self.debug_acquire();
            return TryLockResult::Acquired;
        }

        // Slow path: 带计数的自旋等待
        for _ in 0..max_spins {
            if self
                .inner
                .locked
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                #[cfg(debug_assertions)]
                self.debug_acquire();
                return TryLockResult::Acquired;
            }

            core::hint::spin_loop();
        }

        // 超时: 记录警告 (调试模式) — 已禁用: no_std 环境
        // #[cfg(debug_assertions)]
        // eprintln!("[SPINLOCK] 警告: 锁 '{}' 自旋 {} 次后超时",
        //           self.inner.name.unwrap_or("未命名"), max_spins);

        TryLockResult::WouldBlock
    }

    /// 释放锁
    ///
    /// # Safety
    /// 调用者必须持有该锁
    pub fn raw_unlock(&self) {
        #[cfg(debug_assertions)]
        lockdep::release(self.lockdep_class);

        // 内存屏障: 确保所有写操作对其他 CPU 可见
        fence(Ordering::SeqCst);

        // 清除锁定标志
        self.inner.locked.store(0, Ordering::Release);
    }

    /// 尝试获取锁 (非阻塞)
    ///
    /// # Returns
    /// - `TryLockResult::Acquired`: 成功获取
    /// - `TryLockResult::WouldBlock`: 锁已被持有
    pub fn try_lock(&mut self) -> TryLockResult {
        match self
            .inner
            .locked
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => {
                #[cfg(debug_assertions)]
                self.debug_acquire();
                #[cfg(debug_assertions)]
                lockdep::acquire(self.lockdep_class, lockdep::in_irq_context());
                TryLockResult::Acquired
            }
            Err(_) => TryLockResult::WouldBlock,
        }
    }

    /// 检查锁是否被持有
    pub fn is_locked(&self) -> bool {
        self.inner.locked.load(Ordering::Acquire) != 0
    }

    /// 获取锁并返回守卫 (RAII)
    ///
    /// 推荐使用此方法而非 `raw_lock/raw_unlock`，
    /// 因为 Guard 在离开作用域时自动释放锁。
    ///
    /// # Example
    /// ```rust,ignore
    /// let lock = SpinLock::new();
    /// let data = Mutex::new(42i32);
    ///
    /// {
    ///     let guard = lock.lock(&data);
    ///     println!("Protected: {}", *guard);
    /// } // ← 自动 unlock
    /// 获取锁并返回守卫 (RAII)
    pub fn lock<'a, T>(&'a mut self, data: &'a core::cell::UnsafeCell<T>) -> SpinLockGuard<'a, T>
    where
        T: Sized,
    {
        self.raw_lock();

        // 安全: 我们已持有锁，可以创建可变引用
        // SAFETY: `data` 由调用方保证为有效指针; 只读访问
        let data_ref = unsafe { &mut *data.get() };

        SpinLockGuard {
            data: data_ref,
            _lock: &self.inner,
        }
    }

    // ========================================================================
    // 中断安全版本
    // ========================================================================

    /// 获取锁并禁用中断 (保存中断标志)
    ///
    /// 用于需要在中断上下文中保护的临界区。
    ///
    /// # Returns
    /// 之前的中断状态 (用于 restore)
    pub fn lock_irqsave(&mut self) -> IrqSaveFlags {
        let flags = disable_interrupts();
        self.raw_lock();
        flags
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 释放锁并恢复中断标志
    ///
    /// # Arguments
    /// * `flags` - 之前保存的中断标志
    pub fn unlock_irqrestore(&mut self, flags: &IrqSaveFlags) {
        self.raw_unlock();
        restore_interrupts(flags);
    }

    /// 获取锁并禁用中断 (不保存标志)
    pub fn lock_irq(&mut self) {
        disable_interrupts();
        self.raw_lock();
    }

    // ========================================================================
    // 调试支持
    // ========================================================================

    #[cfg(debug_assertions)]
    #[cfg(target_arch = "x86_64")]
    fn debug_acquire(&mut self) {
        let rsp: u64;
        // SAFETY: 内联汇编的寄存器约束与变量类型一致; 无内存副作用; 输出 reg 通过 out(reg) 绑定
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem)) };
        self.inner.owner = rsp as *const ();
        self.inner.acquire_time = crate::arch!(timestamp());
    }

    #[cfg(debug_assertions)]
    #[cfg(target_arch = "aarch64")]
    fn debug_acquire(&mut self) {
        let sp: u64;
        // SAFETY: 内联汇编的寄存器约束与变量类型一致; 无内存副作用; 输出 reg 通过 out(reg) 绑定
        unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nostack, nomem)) };
        self.inner.owner = sp as *const ();
        self.inner.acquire_time = crate::arch!(timestamp());
    }

    /// 断言当前线程持有锁 (仅 debug 模式)
    ///
    /// # Panics
    /// 在 `debug_assertions` 构建下, 当锁未被持有 (数据竞争风险) 时触发 `assert!` panic,
    /// 错误信息为 "SPINLOCK ASSERTION FAILED: lock not held".
    #[cfg(debug_assertions)]
    pub fn assert_held(&self) {
        // 不可恢复: 自旋锁断言失败意味着代码逻辑错误, 继续执行会导致数据竞争
        assert!(self.is_locked(), "SPINLOCK ASSERTION FAILED: lock not held");
    }
}

impl Default for SpinLock {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 中断控制辅助函数
// ============================================================================

/// 禁用中断并返回当前中断标志
///
/// 通过 Arch trait 的 `interrupt_disable` 实现，架构无关。
#[cfg(not(feature = "host-test"))]
pub fn disable_interrupts() -> IrqSaveFlags {
    IrqSaveFlags(crate::arch!(interrupt_disable()) as u64)
}

// B08-14 前置: host-test (std) 下中断禁用为 no-op — host 无中断语义,
// 且执行 cli 特权指令在用户态会 SIGSEGV. IrqSpinLock/SpinLock 的原子自旋
// 在 host 多线程下仍正确互斥. 裸机 (非 host-test) 行为不变.
#[cfg(feature = "host-test")]
pub fn disable_interrupts() -> IrqSaveFlags {
    IrqSaveFlags(0)
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
/// 恢复中断标志
///
/// 通过 Arch trait 的 `interrupt_restore` 实现，架构无关。
#[cfg(not(feature = "host-test"))]
pub fn restore_interrupts(flags: &IrqSaveFlags) {
    crate::arch!(interrupt_restore(flags.0 as usize));
}

// B08-14 前置: host-test no-op (与 disable_interrupts host 变体对应).
// J-01 (2026-09-08): 与裸机变体一致的 trivially_copy_pass_by_ref expect —
// host-test 编译下该变体同样传引用 (API 签名一致性), lint 与裸机对齐处理.
#[cfg(feature = "host-test")]
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (与裸机变体签名一致); 当前优先 expect"
)]
pub fn restore_interrupts(_flags: &IrqSaveFlags) {}

// ============================================================================
// 内存屏障辅助函数
// ============================================================================

/// 写内存屏障 (Store freg)
/// 确保所有写操作对其他 CPU 可见
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn smp_wmb() {
    fence(Ordering::Release);
}

/// 读内存屏障 (Load freg)
/// 确保读取到最新值
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn smp_rmb() {
    fence(Ordering::Acquire);
}

/// 全局内存屏障
#[inline(always)]
pub fn smp_mb() {
    fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinlock_basic() {
        // raw_lock/raw_unlock 取 &mut self (底层原语不提供共享访问).
        let mut lock = SpinLock::new();
        assert!(!lock.is_locked());

        lock.raw_lock();
        assert!(lock.is_locked());

        lock.raw_unlock();
        assert!(!lock.is_locked());
    }

    #[test]
    fn test_spinlock_trylock() {
        let mut lock = SpinLock::new();

        assert_eq!(lock.try_lock(), TryLockResult::Acquired);
        assert!(lock.is_locked());

        // 再次尝试应失败
        assert_eq!(lock.try_lock(), TryLockResult::WouldBlock);

        lock.raw_unlock();
    }

    #[test]
    fn test_spinlock_irqsave() {
        let mut lock = SpinLock::new();

        let flags = lock.lock_irqsave();
        assert!(lock.is_locked());

        lock.unlock_irqrestore(&flags);
        assert!(!lock.is_locked());
    }

    #[test]
    fn test_spinlock_debug_assert() {
        let lock = SpinLock::new();

        // 未持有时断言应失败
        #[cfg(debug_assertions)]
        std::panic::catch_unwind(|| {
            lock.assert_held();
        })
        .expect_err("assert_held should panic when not holding lock");
    }
}
