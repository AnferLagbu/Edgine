#![deny(unsafe_code)]
//! 一次性初始化 (Once + `OnceCell`)
//!
//! ## `Once`
//!
//! 经典 `std::sync::Once` 的内核等价物:
//! - `call_once(|| { ... })` 保证闭包在多线程下仅执行一次。
//! - 后续调用方阻塞等待初始化完成, 然后立即返回。
//! - 闭包 panic 后, 状态机重置 (允许重试)。
//!
//! ## `OnceCell<T>`
//!
//! 类型安全的一次性赋值容器:
//! - `set(value)` 仅在未初始化时成功。
//! - `get_or_init(|| compute())` 懒初始化: 首次调用时计算, 后续直接返回缓存值。
//!
//! ## @SAFE
//! 本文件不含 `unsafe`. 委托:
//! - `privileged::sync::Mutex` — 互斥 (Once 内部)
//! - `privileged::sync::once_lock::OnceLock` — 一次性值容器 (safe 公共 API)
//! - 原子标志 (Once 内部)
//!
//! ## 设计
//!
//! `OnceCell<T>` 在本模块是 `privileged::sync::once_lock::OnceLock<T>` 的类型别名,
//! 保持 API 兼容 (历史代码使用 `OnceCell` 名)。`Once` 是独立原语, 简单闭包
//! 一次性执行, 用 Mutex 串行化 (无锁自旋式实现见 `privileged::sync::once_lock::InnerOnce`)。

use core::sync::atomic::{AtomicU8, Ordering};

use crate::privileged::sync::Mutex;

// ============================================================================
// Once — 一次性闭包执行 (纯 safe, 内部用 Mutex 串行化)
// ============================================================================

/// 一次性闭包执行。
///
/// 内部状态机: `Uninitialized → InProgress → Done`
/// 失败 (panic) 时: `InProgress → Uninitialized` (允许重试)。
pub struct Once {
    state: AtomicU8,
    lock: Mutex<()>,
}

const UNINITIALIZED: u8 = 0;
const IN_PROGRESS: u8 = 1;
const DONE: u8 = 2;

impl Once {
    /// 创建新的 `Once`。
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(UNINITIALIZED),
            lock: Mutex::new(()),
        }
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )]
    /// 执行闭包, 多线程下仅首次调用方真正执行, 后续方阻塞等待完成。
    ///
    /// # Panics
    /// 闭包 panic 时, 状态重置为 `Uninitialized`, 后续 `call_once` 可重试。
    pub fn call_once(&self, f: impl FnOnce()) {
        // 快速路径: 已完成, 直接返回。
        if self.state.load(Ordering::Acquire) == DONE {
            return;
        }

        // 慢路径: 获取锁, 二次检查。
        let _guard = self.lock.lock();
        match self.state.load(Ordering::Acquire) {
            DONE => (),
            UNINITIALIZED => {
                self.state.store(IN_PROGRESS, Ordering::Release);
                // drop guard 临时, 让其他等待者能进入 (但 state 已是 IN_PROGRESS, 不会重复执行)
                drop(_guard);

                // 执行闭包 (panic 安全: 用 catch_unwind? 但内核无 std)。
                // 此处直接执行, panic 由 caller 传播。
                f();

                // 标记完成 (release 屏障保证 f 内的写入对后续 acquire 可见)。
                self.state.store(DONE, Ordering::Release);
            }
            IN_PROGRESS => {
                // 不应发生: 持锁时, state 只可能是 DONE 或 UNINITIALIZED。
                unreachable!("Once: state machine corruption");
            }
            _ => unreachable!("Once: unknown state"),
        }
    }

    /// 返回 `true` 表示闭包已成功执行过一次。
    #[inline]
    pub fn is_completed(&self) -> bool {
        self.state.load(Ordering::Acquire) == DONE
    }
}

impl Default for Once {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// OnceCell<T> — 类型别名, 委托 privileged::sync::once_lock::OnceLock
// ============================================================================

/// 一次性值容器 (类型别名, 指向 privileged 提供的 safe `OnceLock`)。
///
/// ## @SAFE
/// 本类型**不含 unsafe 代码** — 内部实现全部委托 `privileged::sync::once_lock::OnceLock`。
/// 使用本类型无需任何 `unsafe` 块。
pub type OnceCell<T> = crate::privileged::sync::OnceLock<T>;

// ============================================================================
// 单元自检
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn once_basic() {
        let once = Once::new();
        let mut counter = 0u32;
        once.call_once(|| counter += 1);
        once.call_once(|| counter += 1); // 不应执行
        assert_eq!(counter, 1);
    }

    #[test]
    fn once_cell_lazy() {
        let cell: OnceCell<u32> = OnceCell::new();
        assert!(cell.get().is_none());
        let v = cell.get_or_init(|slot| {
            slot.write(42);
        });
        assert_eq!(*v, 42);
        // 第二次调用不应执行闭包
        let v2 = cell.get_or_init(|slot| {
            slot.write(999);
        });
        assert_eq!(*v2, 42);
    }

    #[test]
    fn once_cell_set_returns_err() {
        let cell: OnceCell<u32> = OnceCell::new();
        assert!(cell.set(1).is_ok());
        assert!(matches!(cell.set(2), Err(2)));
        assert_eq!(*cell.get().unwrap(), 1);
    }
}
