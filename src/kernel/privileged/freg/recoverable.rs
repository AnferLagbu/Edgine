//! FREG 可恢复对象接口与带撤销日志的互斥锁

#![deny(missing_docs)]

use core::sync::atomic::Ordering;

use super::fault_inject::maybe_inject_fault;
use super::undo_log::UndoLog;
use crate::privileged::sync::{IrqSpinLock, IrqSpinLockGuard};

/// 可快照/恢复的值接口
pub trait Snapshot: Copy + Sized {
    /// 返回该值的快照副本 (默认返回自身拷贝)。
    #[must_use]
    fn snapshot(&self) -> Self {
        *self
    }
    /// 用给定快照覆盖当前值。
    fn restore(&mut self, snapshot: &Self) {
        *self = *snapshot;
    }
}

/// 可恢复对象接口: 提供域标识、快照捕获与回滚/重置。
pub trait Recoverable {
    /// 返回所属恢复域 ID (默认 0 表示不属于任何域)。
    fn domain_id(&self) -> u64 {
        0
    }
    /// 将当前状态记入撤销日志 (默认空实现)。
    fn capture_freg(&self, _undo: &mut UndoLog) {}
    /// 回滚到快照状态, 成功返回 true (默认成功)。
    fn rollback(&self) -> bool {
        true
    }
    /// 重置对象状态, 成功返回 true (默认成功)。
    fn reset(&self) -> bool {
        true
    }
}

/// 带 FREG 撤销日志的互斥锁: 加锁时自动记录字段快照。
pub struct RecoverableMutex<T: Snapshot + 'static> {
    inner: IrqSpinLock<T>,
    domain_id: u64,
}

// SAFETY: RecoverableMutex 包装 IrqSpinLock<T>, 提供互斥且关中断.
// 跨线程转移只需 T: Send (值随锁一起移动); T: Sync 不是 Send 的前提,
// 故本 impl 的约束与内层 `IrqSpinLock<T>: Send iff T: Send` 一致.
unsafe impl<T: Snapshot + Send> Send for RecoverableMutex<T> {}
// SAFETY: `RecoverableMutex<T>` 的可变面全在 `inner: IrqSpinLock<T>` 内 (该锁自带
// 互斥 + 关中断), `domain_id` 是普通 u64 (Copy, 构造后只读) ⇒ &Self 跨核共享安全。
// 约束取 `T: Send` 而非 `T: Sync`: 与内层 `IrqSpinLock<T>` 的 Sync impl 同口径
// (sync/irq_spinlock.rs), 且 Mutex 语义下经锁取得的是 `&mut T`, 不要求 `T: Sync`.
unsafe impl<T: Snapshot + Send> Sync for RecoverableMutex<T> {}

impl<T: Snapshot + 'static> RecoverableMutex<T> {
    /// 构造带撤销日志的互斥锁
    pub const fn new(val: T, domain_id: u64) -> Self {
        Self {
            inner: IrqSpinLock::new(val),
            domain_id,
        }
    }

    /// 获取锁并记录字段快照到撤销日志
    pub fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let guard = self.inner.lock();
        if self.domain_id != 0
            && let Some(dom) = super::RECOVERY_MANAGER.lock().find(self.domain_id)
        {
            let mut undo = dom.undo.lock();
            undo.current_generation = dom.freg_generation.load(Ordering::SeqCst);
            let key_ptr = (&raw const *guard).cast_mut();
            let snapshot = guard.snapshot();
            undo.record(key_ptr, snapshot);
        }
        maybe_inject_fault(self.domain_id);
        guard
    }

    /// 快速加锁 (不记录撤销日志)
    pub fn lock_fast(&self) -> IrqSpinLockGuard<'_, T> {
        self.inner.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sync<T: Sync>() {}
    fn assert_send<T: Send>() {}

    /// `Copy + Send + !Sync` 的探针载荷: `PhantomData` 继承 `Cell<u32>` 的 `!Sync`,
    /// 同时保持 `Send` 与 `Copy` (满足 `Snapshot: Copy` 的前置).
    #[derive(Clone, Copy)]
    struct SendNotSync(core::marker::PhantomData<core::cell::Cell<u32>>);

    impl Snapshot for SendNotSync {}

    /// ㊻ 约束面: 外层 `Sync` 只要求 `T: Send` (与内层 `IrqSpinLock<T>` 同口径)。
    ///
    /// 修复前外层写 `T: Sync`, 本条对 `SendNotSync` 不成立 ⇒ 编译失败; 修复后通过。
    #[test]
    fn sync_bound_follows_inner_spinlock_send_requirement() {
        assert_sync::<RecoverableMutex<SendNotSync>>();
        assert_send::<RecoverableMutex<SendNotSync>>();
    }
}
