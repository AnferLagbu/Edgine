//! 同步原语数据类型定义 — privileged 机制类型
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原类型定义 (锁状态/守卫/统计) 于 T6-9 (2026-06-16) 迁至 `functions::sync::types`,
//! 本文件仅 re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：
//! `SpinLockInner`/`MutexInner`/`RwLockInner` 及 RAII 守卫被 privileged sync 机制
//! (spinlock/rwlock/mutex FFI 层) 直接消费, 且与 C 版本布局兼容 (`#[repr(C)]`)
//! — 属机制类型, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::sync::types::*` 保持 API 兼容。
//! 本文件 0 unsafe (纯类型 + 原子操作).

// ============================================================================
// 同步原语数据类型定义
// ============================================================================

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// 简单的 AtomicI32 (如果标准库没有)
use core::sync::atomic::AtomicI32;

/// 锁状态枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    /// 未锁定
    Unlocked,
    /// 已锁定
    Locked,
}

/// 锁获取结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryLockResult {
    /// 成功获取锁
    Acquired,
    /// 锁已被其他线程持有
    WouldBlock,
}

/// 自旋锁内部状态 (与 C 版本 `spinlock_t` 兼容)
///
/// # Safety
/// 此结构的布局必须与 C 版本保持一致 (用于 FFI)
#[repr(C)]
pub struct SpinLockInner {
    /// 锁定状态 (0=unlocked, 1=locked)
    pub locked: AtomicU32,

    #[cfg(debug_assertions)]
    pub owner: *const (), // 调试: 持有者 RSP
    #[cfg(debug_assertions)]
    pub acquire_time: u64, // debug: 获取时间戳 (TSC)
    #[cfg(debug_assertions)]
    pub name: &'static str, // debug: 锁名称
}

impl Default for SpinLockInner {
    fn default() -> Self {
        Self {
            locked: AtomicU32::new(0),
            #[cfg(debug_assertions)]
            owner: core::ptr::null(),
            #[cfg(debug_assertions)]
            acquire_time: 0,
            #[cfg(debug_assertions)]
            name: "(unnamed)",
        }
    }
}

impl SpinLockInner {
    /// 创建新的自旋锁内部状态 (const, 可用于 static)
    pub const fn new() -> Self {
        Self {
            locked: AtomicU32::new(0),
            #[cfg(debug_assertions)]
            owner: core::ptr::null(),
            #[cfg(debug_assertions)]
            acquire_time: 0,
            #[cfg(debug_assertions)]
            name: "(unnamed)",
        }
    }

    /// 原始锁获取 (用于内部实现)
    pub fn raw_lock(&self) {
        // Fast path: 尝试立即获取
        if self
            .locked
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }

        // Slow path: 自旋等待
        loop {
            if self
                .locked
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }

            // pause 指令提示 CPU 我们在自旋等待
            core::hint::spin_loop();
        }
    }

    /// 原始锁释放 (用于内部实现)
    pub fn raw_unlock(&self) {
        core::sync::atomic::fence(Ordering::SeqCst);
        self.locked.store(0, Ordering::Release);
    }

    /// 尝试获取锁 (非阻塞)
    ///
    /// # Returns
    /// - `true`: 成功获取
    /// - `false`: 锁已被持有
    pub fn try_lock(&self) -> bool {
        self.locked
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

/// 睡眠锁 (Mutex) 内部状态 (与 C 版本 `mutex_t` 兼容)
///
/// 基于等待队列和调度器，适用于长时间持有临界区的场景。
///
/// # 特性
/// - 支持递归锁定 (depth 计数)
/// - 锁竞争时让出 CPU (`scheduler_yield`)
/// - 记录持有者 PID 和获取时间
#[repr(C)]
pub struct MutexInner {
    /// 是否已锁定 (0/1)
    pub locked: AtomicU32,
    /// 持有者 PID (-1 = 未持有)
    pub owner: AtomicI32,
    /// 递归深度 (支持同一线程多次 lock)
    pub depth: AtomicU32,
    /// 获取时间戳 (TSC)
    pub acquire_time: AtomicU64,
    /// 内部自旋锁 (保护状态字段)
    pub inner_spinlock: SpinLockInner,
}

impl Default for MutexInner {
    fn default() -> Self {
        Self {
            locked: AtomicU32::new(0),
            owner: AtomicI32::new(-1),
            depth: AtomicU32::new(0),
            acquire_time: AtomicU64::new(0),
            inner_spinlock: SpinLockInner::default(),
        }
    }
}

impl MutexInner {
    /// 创建新的互斥锁内部状态
    pub fn new() -> Self {
        Self::default()
    }
}

/// 读写锁 (`RwLock`) 内部状态 (与 C 版本 `rwlock_t` 兼容)
///
/// 实现写者优先策略，防止写者饥饿。
///
/// # 状态机
/// ```text
/// Initial: readers=0, writer=0, pending_writers=0
///
/// read_lock():   readers++ (if !writer && !pending_writers)
/// read_unlock(): readers--
/// write_lock():  pending_writers++ → wait → writer=1 (if readers==0)
/// write_unlock(): writer=0
/// ```
#[repr(C)]
pub struct RwLockInner {
    /// 内部自旋锁 (保护所有字段)
    pub lock: SpinLockInner,
    /// 当前活跃的读者数量
    pub readers: AtomicU32,
    /// 是否有活跃的写者 (0/1)
    pub writer: AtomicU32,
    /// 等待中的写者数量 (用于公平性)
    pub pending_writers: AtomicU32,
}

impl Default for RwLockInner {
    fn default() -> Self {
        Self {
            lock: SpinLockInner::default(),
            readers: AtomicU32::new(0),
            writer: AtomicU32::new(0),
            pending_writers: AtomicU32::new(0),
        }
    }
}

impl RwLockInner {
    /// 创建新的读写锁内部状态
    pub fn new() -> Self {
        Self::default()
    }
}

/// 条件变量 (`CondVar`) 内部状态
///
/// 用于线程间通知机制，通常配合 Mutex 使用。
#[repr(C)]
pub struct CondVarInner {
    /// 关联的互斥锁引用 (不拥有)
    // 注意: 这里不能存储 &MutexInner，因为生命周期问题
    // 实际使用时通过 FFI 传入
    _private: [u8; 0],
}

/// 中断标志保存 (用于 irqsave 版本的锁操作)
///
/// # Layout
/// 必须能存储完整的 RFLAGS 寄存器值
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct IrqSaveFlags(pub u64);

impl IrqSaveFlags {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 检查中断是否启用 (IF bit = bit 9)
    pub fn interrupts_enabled(&self) -> bool {
        (self.0 & (1 << 9)) != 0
    }
}

/// 锁守卫 (RAII wrapper for `SpinLock`)
///
/// 当 Guard 被 drop 时自动释放锁，
/// **确保不会忘记解锁**。
///
/// # Example
/// ```rust,ignore
/// let data = Mutex::new(42i32);
/// {
///     let guard = data.lock();
///     println!("Protected: {}", *guard);
/// } // ← 自动 drop, 锁释放
/// ```
pub struct SpinLockGuard<'a, T> {
    /// 被保护的数据引用
    pub data: &'a mut T,
    /// 锁的引用 (用于 unlock)
    pub _lock: &'a SpinLockInner,
}

impl<T> core::ops::Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> core::ops::DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    fn drop(&mut self) {
        core::sync::atomic::fence(Ordering::SeqCst);
        self._lock.locked.store(0, Ordering::Release);
    }
}

/// Mutex 守卫 (RAII wrapper for Mutex)
///
/// 支持**递归锁定**：同一线程可多次 lock，
/// 但必须配对相同次数的 unlock。
pub struct MutexGuard<'a, T> {
    pub data: &'a mut T,
    pub _mutex: &'a MutexInner,
}

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    fn drop(&mut self) {
        self._mutex.inner_spinlock.raw_lock();

        let depth = self._mutex.depth.fetch_sub(1, Ordering::AcqRel);
        if depth <= 1 {
            self._mutex.locked.store(0, Ordering::Release);
            self._mutex.owner.store(-1, Ordering::Release);
            self._mutex.acquire_time.store(0, Ordering::Release);
        }

        self._mutex.inner_spinlock.raw_unlock();
    }
}

/// 读锁守卫 (RAII for `RwLock` read mode)
pub struct RwLockReadGuard<'a, T> {
    pub data: &'a T,
    pub _rwlock: &'a RwLockInner,
}

impl<T> core::ops::Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> Drop for RwLockReadGuard<'_, T> {
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    fn drop(&mut self) {
        let prev_readers = self._rwlock.readers.fetch_sub(1, Ordering::AcqRel);

        // 边界检查 (debug 模式)
        debug_assert!(prev_readers > 0, "RwLock: read_unlock without read_lock");
    }
}

/// 写锁守卫 (RAII for `RwLock` write mode)
pub struct RwLockWriteGuard<'a, T> {
    pub data: &'a mut T,
    pub _rwlock: &'a RwLockInner,
}

impl<T> core::ops::Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> core::ops::DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}

impl<T> Drop for RwLockWriteGuard<'_, T> {
    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    fn drop(&mut self) {
        core::sync::atomic::fence(Ordering::SeqCst);
        self._rwlock.writer.store(0, Ordering::Release);
        self._rwlock.lock.locked.store(0, Ordering::Release);
    }
}

// ============================================================================
// 编译时验证
// ============================================================================

// 确保 SpinLockInner 大小合理 (放宽到 64 bytes 以适应原子操作)
const _: () = assert!(core::mem::size_of::<SpinLockInner>() <= 64);

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spin_lock_inner_default() {
        let lock = SpinLockInner::default();
        assert_eq!(lock.locked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_mutex_inner_default() {
        let m = MutexInner::default();
        assert_eq!(m.locked.load(Ordering::Relaxed), 0);
        assert_eq!(m.owner.load(Ordering::Relaxed), -1);
        assert_eq!(m.depth.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_rwlock_inner_default() {
        let rw = RwLockInner::default();
        assert_eq!(rw.readers.load(Ordering::Relaxed), 0);
        assert_eq!(rw.writer.load(Ordering::Relaxed), 0);
        assert_eq!(rw.pending_writers.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_irq_save_flags() {
        let flags = IrqSaveFlags(0x202); // IF=1 (interrupts enabled)
        assert!(flags.interrupts_enabled());

        // RFLAGS bit1 恒为 1, IF 是 bit9 (0x200); 故 IF=0 的样本是 0x002
        // (原用例误用 0x200 作为"禁用"样本; UT-06 实测修正)
        let flags_disabled = IrqSaveFlags(0x002); // IF=0 (disabled)
        assert!(!flags_disabled.interrupts_enabled());
    }

    #[test]
    fn test_try_lock_result_variants() {
        let acquired = TryLockResult::Acquired;
        let would_block = TryLockResult::WouldBlock;

        assert_eq!(acquired, TryLockResult::Acquired);
        assert_ne!(acquired, would_block);
    }
}
