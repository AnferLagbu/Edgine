//! # 同步原语 (TCB) — Framekernel `sync/` 统一入口
//!
//! v2.22 终极合并产物: 11 个 TCB 同步原语子模块统一在本目录.
//! 历史: v2.21 分为新 API (OnceLock/OnceCell/IrqSpinLock) + 原 TCB 内部实现,
//!       v2.22 已将后者全量吸收合并, 11 个子模块物理上统一在 `privileged/sync/`.
//!
//! ## 依赖声明
//!
//! privileged 内部依赖: arch, syscall (仅 lockdep)
//! functions 依赖: `functions::sync` (安全代理)
//!
//! ## 架构定位
//!
//! ```text
//! privileged/sync/    (TCB 唯一, 唯一允许 unsafe 的同步原语层)
//! ├── types.rs        核心数据结构 (与 C 版本布局兼容)
//! ├── atomic.rs       原子操作封装
//! ├── spinlock.rs     自旋锁 (基于 xchg/原子操作)
//! ├── mutex.rs        睡眠锁 (基于调度器 yield)
//! ├── rwlock.rs       读写锁 (写者优先策略)
//! ├── seqlock.rs      顺序锁
//! ├── rcu.rs          读-拷贝-更新
//! ├── arch.rs         架构内存屏障
//! ├── once_lock.rs    现代 TCB OnceLock (新原语, safe API)
//! ├── once_cell.rs    现代 TCB OnceCell (新原语, safe API)
//! ├── irq_spinlock.rs 现代 TCB IrqSpinLock (新原语, safe API)
//! └── mod.rs          模块导出 + FFI 接口层
//! ```
//!
//! ## 服务层契约
//!
//! - `functions/sync/` 通过 `pub use` 重新导出本模块的安全包装 (SpinLockGuard/MutexGuard/...)
//! - `functions/sync/` 顶部 `#![deny(unsafe_code)]`, 禁止任何 unsafe
//! - 所有裸指针解引用 (`unsafe { (*ptr).field }`) 集中在本模块的 `raw` 子模块
//!
//! ## SAFETY 契约
//!
//! - 本模块是 TCB 一部分, `unsafe` 块必须附 `// SAFETY:` 注释
//! - 所有 FFI 桥接函数 `*const T` / `*mut T` 参数假定调用方已做 `is_null()` 检查
//! - `raw` 子模块封装所有裸指针解引用, 业务 FFI 函数不直接使用 unsafe

// ============================================================================
// 1. 传统 TCB 同步原语 (8 个模块, 来自原 sync/)
// ============================================================================

pub mod arch;
pub mod atomic;
pub mod mutex;
pub mod rcu;
pub mod rwlock;
pub mod seqlock;
pub mod spinlock;
pub mod types;

/// 优先级继承互斥锁 (P1 #3, DECISION-009/010/011)
pub mod pi_mutex;

// ============================================================================
// 2. 现代 TCB 同步原语 (3 个 模块, 新原语, safe API 为主)
// ============================================================================

/// `OnceLock<T>` — 一次性初始化, 线程安全的全局单次设置
pub mod once_lock;

/// `OnceCell<T>` — 一次性单元格, 单线程初始化 + 共享只读
pub mod once_cell;

/// `IrqSpinLock` — 中断安全自旋锁, 锁内自动禁用中断
pub mod irq_spinlock;

/// Lockdep — 运行时锁依赖检测器 (P1)
pub mod lockdep;

// ============================================================================
// 3. 内部 use (本模块 FFI 桥接层使用)
// ============================================================================

use core::sync::atomic::Ordering;

/// 重新导出类型
pub use types::{MutexInner, RwLockInner, SpinLockInner};

// ============================================================================
// 公共 API 导出 (便捷访问)
// ============================================================================
pub use atomic::{
    AtomicBool, atomic_add, atomic_cmpxchg, atomic_dec, atomic_inc, atomic_read, atomic_set,
    atomic_sub,
};
pub use irq_spinlock::IrqSpinLock;
pub use irq_spinlock::IrqSpinLockGuard;
pub use mutex::Mutex;
pub use once_lock::OnceLock;
pub use rcu::{
    call_rcu, rcu_alloc_cpu, rcu_assign_pointer, rcu_dereference, rcu_read_lock, rcu_read_unlock,
    synchronize_rcu,
};
pub use rwlock::RwLock;
pub use seqlock::SeqLock;
pub use spinlock::{SpinLock, disable_interrupts, restore_interrupts, smp_mb, smp_rmb, smp_wmb};
pub use types::{
    IrqSaveFlags, MutexGuard, RwLockReadGuard, RwLockWriteGuard, SpinLockGuard, TryLockResult,
};

// lockdep 公共接口 re-export — 避免跨子系统直接访问 sync::lockdep 内部
pub use lockdep::{
    LockClassDesc, LockClassId, LockKind, acquire, deadlock_detected, dump_state, held_depth,
    in_irq_context, irq_enter, irq_exit, num_classes, num_violations, register_class, release,
};

// ============================================================================
// FFI 接口层 (C ↔ Rust 桥接)
// ============================================================================

/// 初始化自旋锁 (FFI 导出)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_init(lock: *mut SpinLockInner) {
    if !lock.is_null() {
        raw::spin_locked_mut(lock).store(0, Ordering::Relaxed);
    }
}

/// 获取自旋锁 (原始版本)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_lock_raw(lock: *const SpinLockInner) {
    if !lock.is_null() {
        // Fast path: 尝试立即获取
        let acquired = raw::spin_locked(lock)
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();

        if acquired {
            return;
        }

        // Slow path: 自旋等待
        loop {
            let result =
                raw::spin_locked(lock).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed);

            if result.is_ok() {
                break;
            }

            core::hint::spin_loop();
        }
    }
}

/// 释放自旋锁
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_unlock(lock: *const SpinLockInner) {
    if !lock.is_null() {
        core::sync::atomic::fence(Ordering::SeqCst);
        raw::spin_locked(lock).store(0, Ordering::Release);
    }
}

/// 尝试获取自旋锁 (非阻塞)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_trylock(lock: *const SpinLockInner) -> i32 {
    if lock.is_null() {
        return 0; // 失败
    }

    match raw::spin_locked(lock).compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed) {
        Ok(_) => 1,  // 成功
        Err(_) => 0, // 已被锁定
    }
}

/// 检查自旋锁是否被持有
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_is_locked(lock: *const SpinLockInner) -> i32 {
    if lock.is_null() {
        return 0;
    }

    raw::spin_locked(lock).load(Ordering::Acquire) as i32
}

/// 初始化互斥锁
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
pub extern "C" fn mutex_init(m: *mut MutexInner) {
    if !m.is_null() {
        raw::mutex_locked(m as *const MutexInner).store(0, Ordering::Relaxed);
        raw::mutex_owner(m as *const MutexInner).store(-1i32, Ordering::Relaxed); // -1 表示无主
        raw::mutex_depth(m as *const MutexInner).store(0, Ordering::Relaxed);
        raw::mutex_acquire_time(m as *const MutexInner).store(0, Ordering::Relaxed);

        // 初始化内部自旋锁
        spin_init(raw::mutex_inner_spinlock_mut(m));
    }
}

/// 获取互斥锁 (阻塞)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn mutex_lock(m: *const MutexInner) {
    if m.is_null() {
        return;
    }

    let inner_lock = raw::mutex_inner_spinlock(m);

    // Fast path: 尝试立即获取
    {
        spin_lock_raw(inner_lock);

        let is_locked = raw::mutex_locked(m).load(Ordering::Acquire) != 0;

        if !is_locked {
            // 成功获取
            raw::mutex_locked(m).store(1, Ordering::Release);
            raw::mutex_owner(m).store(raw::current_pid() as i32, Ordering::Release);
            raw::mutex_depth(m).store(1, Ordering::Release);
            spin_unlock(inner_lock);
            return;
        }

        spin_unlock(inner_lock);
    }

    // Slow path: 自旋 + yield (简化版)
    loop {
        spin_lock_raw(inner_lock);

        let is_locked = raw::mutex_locked(m).load(Ordering::Acquire) != 0;

        if !is_locked {
            raw::mutex_locked(m).store(1, Ordering::Release);
            raw::mutex_owner(m).store(raw::current_pid() as i32, Ordering::Release);
            raw::mutex_depth(m).store(1, Ordering::Release);
            spin_unlock(inner_lock);
            return;
        }

        spin_unlock(inner_lock);

        // 让出 CPU
        raw::scheduler_yield();
    }
}

/// 释放互斥锁
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn mutex_unlock(m: *const MutexInner) {
    if m.is_null() {
        return;
    }

    let inner_lock = raw::mutex_inner_spinlock(m);
    spin_lock_raw(inner_lock);

    let depth = raw::mutex_depth(m).load(Ordering::Acquire);

    if depth > 1 {
        // 嵌套锁，减少深度
        raw::mutex_depth(m).store(depth - 1, Ordering::Release);
    } else {
        // 完全释放
        raw::mutex_locked(m).store(0, Ordering::Release);
        raw::mutex_owner(m).store(0, Ordering::Release);
        raw::mutex_depth(m).store(0, Ordering::Release);
        raw::mutex_acquire_time(m).store(0, Ordering::Release);
    }

    spin_unlock(inner_lock);
}

/// 尝试获取互斥锁 (非阻塞)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn mutex_trylock(m: *const MutexInner) -> i32 {
    if m.is_null() {
        return 0;
    }

    let inner_lock = raw::mutex_inner_spinlock(m);
    spin_lock_raw(inner_lock);

    let result = if raw::mutex_locked(m).load(Ordering::Acquire) == 0 {
        raw::mutex_locked(m).store(1, Ordering::Release);
        raw::mutex_owner(m).store(raw::current_pid() as i32, Ordering::Release);
        raw::mutex_depth(m).store(1, Ordering::Release);
        1 // 成功
    } else {
        0 // 已被锁定
    };

    spin_unlock(inner_lock);

    result
}

/// 检查互斥锁是否被持有
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn mutex_is_locked(m: *const MutexInner) -> i32 {
    if m.is_null() {
        return 0;
    }

    raw::mutex_locked(m).load(Ordering::Acquire) as i32
}

/// 初始化读写锁
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
pub extern "C" fn rwlock_init(rw: *mut RwLockInner) {
    if !rw.is_null() {
        raw::rwlock_readers(rw as *const RwLockInner).store(0, Ordering::Relaxed);
        raw::rwlock_writer(rw as *const RwLockInner).store(0, Ordering::Relaxed);
        raw::rwlock_pending_writers(rw as *const RwLockInner).store(0, Ordering::Relaxed);

        // 初始化内部自旋锁 (获取可写指针)
        let inner_lock_ptr = raw::rwlock_inner_lock_mut(rw) as *mut SpinLockInner;
        spin_init(inner_lock_ptr);
    }
}

/// 获取读锁 (阻塞)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn read_lock(rw: *const RwLockInner) {
    if rw.is_null() {
        return;
    }

    let inner_lock = raw::rwlock_inner_lock(rw);

    loop {
        spin_lock_raw(inner_lock);

        // 检查是否有写者或等待中的写者
        let has_writer = raw::rwlock_writer(rw).load(Ordering::Relaxed) != 0;
        let pending_writers = raw::rwlock_pending_writers(rw).load(Ordering::Relaxed) > 0;

        if !has_writer && !pending_writers {
            // 可以读取: 增加读者计数
            let readers = raw::rwlock_readers(rw).fetch_add(1, Ordering::AcqRel);
            spin_unlock(inner_lock);

            if readers == 0xFFFF {
                // 溢出保护
                raw::rwlock_readers(rw).fetch_sub(1, Ordering::AcqRel);
                continue;
            }

            return;
        }

        spin_unlock(inner_lock);

        // 让出 CPU
        raw::scheduler_yield();
    }
}

/// 释放读锁
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn read_unlock(rw: *const RwLockInner) {
    if !rw.is_null() {
        raw::rwlock_readers(rw).fetch_sub(1, Ordering::AcqRel);
    }
}

/// 获取写锁 (阻塞)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn write_lock(rw: *const RwLockInner) {
    if rw.is_null() {
        return;
    }

    let inner_lock = raw::rwlock_inner_lock(rw);

    // 先标记自己为等待中的写者
    spin_lock_raw(inner_lock);
    raw::rwlock_pending_writers(rw).fetch_add(1, Ordering::Release);
    spin_unlock(inner_lock);

    loop {
        spin_lock_raw(inner_lock);

        // 检查是否可以获取写锁
        let readers = raw::rwlock_readers(rw).load(Ordering::Relaxed);
        let writer = raw::rwlock_writer(rw).load(Ordering::Relaxed);

        if readers == 0 && writer == 0 {
            // 可以写入: 设置写者标志
            raw::rwlock_pending_writers(rw).fetch_sub(1, Ordering::Release);
            raw::rwlock_writer(rw).store(1, Ordering::Release);
            spin_unlock(inner_lock);
            return;
        }

        spin_unlock(inner_lock);

        // 让出 CPU
        raw::scheduler_yield();
    }
}

/// 释放写锁
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn write_unlock(rw: *const RwLockInner) {
    if !rw.is_null() {
        raw::rwlock_writer(rw).store(0, Ordering::Release);
    }
}

// ============================================================================
// 中断安全锁操作 (补充缺失的函数)
// ============================================================================

/// 获取自旋锁并禁用中断 (返回中断标志)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_lock_irqsave_raw(lock: *const SpinLockInner) -> IrqSaveFlags {
    let flags = disable_interrupts();
    spin_lock_raw(lock);
    flags
}

/// 释放自旋锁并恢复中断
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_unlock_irqrestore(lock: *const SpinLockInner, flags: &IrqSaveFlags) {
    spin_unlock(lock);
    restore_interrupts(flags);
}

/// 获取自旋锁并禁用中断 (不保存标志)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_lock_irq(lock: *const SpinLockInner) {
    disable_interrupts();
    spin_lock_raw(lock);
}

/// 释放自旋锁并启用中断
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn spin_unlock_irq(lock: *const SpinLockInner) {
    spin_unlock(lock);
    crate::arch!(interrupt_enable());
}

// ============================================================================
// 读写锁扩展操作 (补充缺失的函数)
// ============================================================================

/// 尝试获取读锁 (非阻塞)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn read_trylock(rw: *const RwLockInner) -> i32 {
    if rw.is_null() {
        return 0; // 失败
    }

    let inner_lock = raw::rwlock_inner_lock(rw);
    spin_lock_raw(inner_lock);

    let has_writer = raw::rwlock_writer(rw).load(Ordering::Relaxed) != 0;
    let pending_writers = raw::rwlock_pending_writers(rw).load(Ordering::Relaxed) > 0;

    if !has_writer && !pending_writers {
        raw::rwlock_readers(rw).fetch_add(1, Ordering::AcqRel);
        spin_unlock(inner_lock);
        return 1; // 成功
    }

    spin_unlock(inner_lock);
    0 // 失败
}

/// 获取读锁并禁用中断
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn read_lock_irqsave(rw: *const RwLockInner) -> IrqSaveFlags {
    let flags = disable_interrupts();
    read_lock(rw);
    flags
}

/// 释放读锁并恢复中断
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn read_unlock_irqrestore(rw: *const RwLockInner, flags: &IrqSaveFlags) {
    read_unlock(rw);
    restore_interrupts(flags);
}

/// 获取写锁并禁用中断
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn write_lock_irqsave(rw: *const RwLockInner) -> IrqSaveFlags {
    let flags = disable_interrupts();
    write_lock(rw);
    flags
}

/// 释放写锁并恢复中断
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn write_unlock_irqrestore(rw: *const RwLockInner, flags: &IrqSaveFlags) {
    write_unlock(rw);
    restore_interrupts(flags);
}

/// 尝试获取写锁 (非阻塞)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn write_trylock(rw: *const RwLockInner) -> i32 {
    if rw.is_null() {
        return 0; // 失败
    }

    let inner_lock = raw::rwlock_inner_lock(rw);
    spin_lock_raw(inner_lock);

    let readers = raw::rwlock_readers(rw).load(Ordering::Relaxed);
    let writer = raw::rwlock_writer(rw).load(Ordering::Relaxed);

    if readers == 0 && writer == 0 {
        raw::rwlock_pending_writers(rw).fetch_sub(1, Ordering::Release);
        raw::rwlock_writer(rw).store(1, Ordering::Release);
        spin_unlock(inner_lock);
        return 1; // 成功
    }

    spin_unlock(inner_lock);
    0 // 失败
}

// ============================================================================
// 互斥锁扩展操作 (补充缺失的函数)
// ============================================================================

/// 获取互斥锁持有者 PID
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn mutex_owner(m: *const MutexInner) -> i32 {
    if m.is_null() {
        return -1;
    }

    raw::mutex_owner(m).load(Ordering::Acquire)
}

// ============================================================================
// 条件变量 re-export (P0-1 修复)
// ============================================================================
//
// 历史: 此前模块自带 `CondVar` 占位结构 (#[repr(C)] 64 字节 padding) + 3 个
// extern "C" stub (cond_init/cond_signal/cond_broadcast), 无任何调用方,
// 与 `mutex::CondVar` (带 new() 实现) 冲突.
//
// 修复: 删除 stub 结构与 stub 函数, re-export `mutex::CondVar` 作为正式实现.
// 这样 `sync::CondVar` 解析到真实 Rust 实现, 测试 `CondVar::new()` 可用.

pub use mutex::CondVar;

// ============================================================================
// 辅助函数声明
// ============================================================================

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    fn process_get_current_pid() -> u32;
    fn scheduler_yield();
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中裸指针访问
// ============================================================================
//
// 所有 FFI 桥接函数接收 `*const T` / `*mut T` 参数, 内部对指针解引用
// (`unsafe { (*ptr).field }`) 是 unsafe 的主要来源。
// 本子模块通过 `AddrOf` 系列安全方法封装指针解引用, 业务 FFI 函数
// 不再直接使用 unsafe, 仅调用 raw 模块的安全包装。
//
// SAFETY 契约: 所有 raw 方法假定传入的指针非空且指向有效对象。
//             调用方 (FFI 函数) 已做 `is_null()` 检查。

pub(crate) mod raw {
    use super::{MutexInner, RwLockInner, SpinLockInner};

    // ============ SpinLockInner ============

    /// 获取 SpinLockInner.locked 字段引用
    pub fn spin_locked<'a>(ptr: *const SpinLockInner) -> &'a core::sync::atomic::AtomicU32 {
        // SAFETY: ptr 假定非空, 指向有效 SpinLockInner。
        // 返回的引用生命周期由调用方保证 (ptr 在使用期间有效)。
        unsafe { &(*ptr).locked }
    }

    pub fn spin_locked_mut<'a>(ptr: *mut SpinLockInner) -> &'a mut core::sync::atomic::AtomicU32 {
        // SAFETY: ptr 假定非空且唯一借用, 指向有效 SpinLockInner。
        unsafe { &mut (*ptr).locked }
    }

    // ============ MutexInner ============

    pub fn mutex_locked<'a>(ptr: *const MutexInner) -> &'a core::sync::atomic::AtomicU32 {
        // SAFETY: 同上。
        unsafe { &(*ptr).locked }
    }

    pub fn mutex_owner<'a>(ptr: *const MutexInner) -> &'a core::sync::atomic::AtomicI32 {
        // SAFETY: 同上。
        unsafe { &(*ptr).owner }
    }

    pub fn mutex_depth<'a>(ptr: *const MutexInner) -> &'a core::sync::atomic::AtomicU32 {
        // SAFETY: 同上。
        unsafe { &(*ptr).depth }
    }

    pub fn mutex_acquire_time<'a>(ptr: *const MutexInner) -> &'a core::sync::atomic::AtomicU64 {
        // SAFETY: 同上。
        unsafe { &(*ptr).acquire_time }
    }

    pub fn mutex_inner_spinlock<'a>(ptr: *const MutexInner) -> &'a SpinLockInner {
        // SAFETY: 同上。
        unsafe { &(*ptr).inner_spinlock }
    }

    pub fn mutex_inner_spinlock_mut<'a>(ptr: *mut MutexInner) -> &'a mut SpinLockInner {
        // SAFETY: 同上。
        unsafe { &mut (*ptr).inner_spinlock }
    }

    // ============ RwLockInner ============

    pub fn rwlock_readers<'a>(ptr: *const RwLockInner) -> &'a core::sync::atomic::AtomicU32 {
        // SAFETY: 同上。
        unsafe { &(*ptr).readers }
    }

    pub fn rwlock_writer<'a>(ptr: *const RwLockInner) -> &'a core::sync::atomic::AtomicU32 {
        // SAFETY: 同上。
        unsafe { &(*ptr).writer }
    }

    pub fn rwlock_pending_writers<'a>(
        ptr: *const RwLockInner,
    ) -> &'a core::sync::atomic::AtomicU32 {
        // SAFETY: 同上。
        unsafe { &(*ptr).pending_writers }
    }

    pub fn rwlock_inner_lock<'a>(ptr: *const RwLockInner) -> &'a SpinLockInner {
        // SAFETY: 同上。
        unsafe { &(*ptr).lock }
    }

    pub fn rwlock_inner_lock_mut<'a>(ptr: *mut RwLockInner) -> &'a mut SpinLockInner {
        // SAFETY: 同上。
        unsafe { &mut (*ptr).lock }
    }

    /// 调度器让出 CPU (FFI 包装)
    pub fn scheduler_yield() {
        // SAFETY: 调度器 yield 是 C 端实现的纯函数, 无内存不安全。
        unsafe { super::scheduler_yield() }
    }

    /// 获取当前进程 PID (FFI 包装)
    pub fn current_pid() -> u32 {
        // SAFETY: 同上。
        unsafe { super::process_get_current_pid() }
    }
}
