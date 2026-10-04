use core::sync::atomic::Ordering;

use super::fault_inject::maybe_inject_fault;
use super::undo_log::UndoLog;
use crate::privileged::sync::{IrqSpinLock, IrqSpinLockGuard};

pub trait Snapshot: Copy + Sized {
    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    fn snapshot(&self) -> Self {
        *self
    }
    fn restore(&mut self, snapshot: &Self) {
        *self = *snapshot;
    }
}

pub trait Recoverable {
    fn domain_id(&self) -> u64 {
        0
    }
    fn capture_freg(&self, _undo: &mut UndoLog) {}
    fn rollback(&self) -> bool {
        true
    }
    fn reset(&self) -> bool {
        true
    }
}

pub struct RecoverableMutex<T: Snapshot + 'static> {
    inner: IrqSpinLock<T>,
    domain_id: u64,
}

// SAFETY: RecoverableMutex 包装 IrqSpinLock<T>, 提供互斥且关中断.
// T: Send 是跨线程转移所需; T: Sync 是因为锁获取后 &T 可共享.
// domain_id 为普通 u64 (Copy). 除 IrqSpinLock 提供的保证外, 不引入额外不安全.
unsafe impl<T: Snapshot + Send> Send for RecoverableMutex<T> {}
unsafe impl<T: Snapshot + Sync> Sync for RecoverableMutex<T> {}

impl<T: Snapshot + 'static> RecoverableMutex<T> {
    pub const fn new(val: T, domain_id: u64) -> Self {
        Self {
            inner: IrqSpinLock::new(val),
            domain_id,
        }
    }

    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    #[expect(
        clippy::ptr_cast_constness,
        reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
    )]
    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let guard = self.inner.lock();
        if self.domain_id != 0 {
            if let Some(dom) = super::RECOVERY_MANAGER.lock().find(self.domain_id) {
                let mut undo = dom.undo.lock();
                undo.current_generation = dom.freg_generation.load(Ordering::SeqCst);
                let key_ptr = &*guard as *const T as *mut T;
                let snapshot = guard.snapshot();
                undo.record(key_ptr, snapshot);
            }
        }
        maybe_inject_fault(self.domain_id);
        guard
    }

    pub fn lock_fast(&self) -> IrqSpinLockGuard<'_, T> {
        self.inner.lock()
    }
}
