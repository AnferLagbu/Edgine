#![deny(unsafe_code)]
//! 闭包作用域 API (functions 层)
//!
//! 提供 `with` / `with_mut` / `try_with` 闭包形式的锁使用方式,
//! 杜绝 RAII Guard 泄漏或跨作用域误用。
//!
//! ## 为什么需要闭包 API
//!
//! 标准 RAII 模式 `let g = lock.lock(); ... drop(g);` 有以下隐患:
//! 1. `?` / `return` 路径仍安全, 但 `panic` 路径需 `catch_unwind` 配合
//! 2. 显式 drop 增加代码噪音
//! 3. 难以静态分析"guard 一定在临界区内 drop"
//!
//! 闭包 API 通过类型系统保证 guard 不离开闭包作用域:
//! ```ignore
//! mutex.with_mut(|g| g.value = 42);  // 闭包返回后, 锁已释放
//! ```
//!
//! ## @SAFE
//! 不含 `unsafe`. 委托 `privileged::sync` 的 Guard 实现.

use core::ops::{Deref, DerefMut};

use crate::privileged::sync::{
    mutex::{Mutex, MutexGuard},
    rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard},
    spinlock::{SpinLock, SpinLockGuard},
};

// ============================================================================
// Mutex 扩展
// ============================================================================

/// `Mutex<T>` 的闭包作用域扩展。
///
/// 通过 trait 扩展添加 `with` / `with_mut` 方法, 不修改 `privileged::sync` 的 API。
pub trait MutexExt<T> {
    /// 不可变闭包: 获取读锁, 执行 `f(&T)`, 自动释放。
    fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R;

    /// 可变闭包: 获取写锁, 执行 `f(&mut T)`, 自动释放。
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;

    /// 尝试获取锁, 失败返回 `None`, 成功执行 `f`。
    fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard: MutexGuard<'_, T> = self.lock();
        f(guard.deref())
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard: MutexGuard<'_, T> = self.lock();
        f(guard.deref_mut())
    }

    fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut guard = self.try_lock()?;
        Some(f(guard.deref_mut()))
    }
}

// ============================================================================
// RwLock 扩展
// ============================================================================

/// `RwLock<T>` 的闭包作用域扩展。
pub trait RwLockExt<T> {
    /// 读闭包: 共享访问, 多个 reader 可并发。
    fn read_with<R>(&self, f: impl FnOnce(&T) -> R) -> R;

    /// 写闭包: 独占可变访问, 与所有其他访问互斥。
    fn write_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard: RwLockReadGuard<'_, T> = self.read();
        f(guard.deref())
    }

    fn write_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard: RwLockWriteGuard<'_, T> = self.write();
        f(guard.deref_mut())
    }
}

// ============================================================================
// SpinLock 扩展
// ============================================================================

/// `SpinLock<T>` 的闭包作用域扩展。
///
/// SpinLock 没有 `Read` Guard (非公平), 仅提供 `with` / `with_mut`。
pub trait SpinLockExt<T> {
    /// 不可变闭包 (与 `with_mut` 等价, SpinLock 总是独占访问)。
    fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R;

    /// 可变闭包。
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;

    /// 尝试获取 (非阻塞)。
    fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R>;
}

impl<T> SpinLockExt<T> for SpinLock<T> {
    fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard: SpinLockGuard<'_, T> = self.lock();
        f(guard.deref())
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard: SpinLockGuard<'_, T> = self.lock();
        f(guard.deref_mut())
    }

    fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut guard = self.try_lock()?;
        Some(f(guard.deref_mut()))
    }
}

// ============================================================================
// 单元自检
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutex_with() {
        let m = Mutex::new(0u32);
        m.with_mut(|g| *g += 5);
        m.with(|g| assert_eq!(*g, 5));
    }

    #[test]
    fn rwlock_read_write() {
        let rw = RwLock::new(100u32);
        let v = rw.read_with(|g| *g);
        assert_eq!(v, 100);
        rw.write_with(|g| *g = 200);
        assert_eq!(rw.read_with(|g| *g), 200);
    }

    #[test]
    fn spinlock_try_with() {
        let l = SpinLock::new(0u32);
        l.try_with(|g| *g = 7).unwrap();
        assert_eq!(l.with(|g| *g), 7);
    }
}
