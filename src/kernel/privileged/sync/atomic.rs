//! # 原子操作封装
//!
//! 提供类型安全的原子操作，替代 C 版本的 inline asm。
//!
//! ## 特性
//!
//! - **类型安全**: 泛型 `Atomic<T>` 避免类型转换错误
//! - **内存顺序**: 支持 Relaxed/Acquire/Release/SeqCst
//! - **编译期检查**: 防止对非原子类型的误用

use core::sync::atomic::{AtomicU32, Ordering};

/// 原子整数 (32位有符号) - 类型别名
pub type AtomicI32Alias = core::sync::atomic::AtomicI32;

/// 原子整数 (32位无符号) - 类型别名
pub type AtomicU32Alias = core::sync::atomic::AtomicU32;

/// 原子布尔值
#[derive(Debug)]
pub struct AtomicBool(AtomicU32);

impl AtomicBool {
    /// 创建新的原子布尔值
    pub const fn new(v: bool) -> Self {
        Self(AtomicU32::new(if v { 1 } else { 0 }))
    }

    /// 读取当前值
    pub fn load(&self, order: Ordering) -> bool {
        self.0.load(order) != 0
    }

    /// 设置新值，返回旧值
    pub fn swap(&self, val: bool, order: Ordering) -> bool {
        self.0.swap(u32::from(val), order) != 0
    }

    /// 如果当前值为 expected，则设置为 val
    ///
    /// # Returns
    /// - `true`: 成功交换
    /// - `false`: 当前值不是 expected
    pub fn compare_exchange(
        &self,
        expected: bool,
        val: bool,
        success: Ordering,
        failure: Ordering,
    ) -> bool {
        self.0
            .compare_exchange(u32::from(expected), u32::from(val), success, failure)
            .is_ok()
    }
}

impl Default for AtomicBool {
    fn default() -> Self {
        Self::new(false)
    }
}

// ============================================================================
// 原子操作辅助函数 (与 C 版本兼容的 FFI 接口)
// ============================================================================

/// 原子加一 (Atomic increment)
///
/// # Arguments
/// * `ptr` - 目标原子变量指针
///
/// # Returns
/// 操作前的旧值
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_inc(ptr: *mut i32) -> i32 {
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic.fetch_add(1, Ordering::SeqCst)
    }
}

/// 原子减一 (Atomic decrement)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_dec(ptr: *mut i32) -> i32 {
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic.fetch_sub(1, Ordering::SeqCst)
    }
}

/// 原子比较并交换 (Compare and Swap)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_cmpxchg(ptr: *mut i32, oldval: i32, newval: i32) -> bool {
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic
            .compare_exchange(oldval, newval, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// 原子加法 (Atomic add)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_add(ptr: *mut i32, val: i32) -> i32 {
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic.fetch_add(val, Ordering::SeqCst)
    }
}

/// 原子减法 (Atomic subtract)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_sub(ptr: *mut i32, val: i32) -> i32 {
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic.fetch_sub(val, Ordering::SeqCst)
    }
}

/// 原子设置 (Atomic store)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_set(ptr: *mut i32, val: i32) {
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic.store(val, Ordering::SeqCst);
    }
}

/// 原子读取 (Atomic load)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_read(ptr: *const i32) -> i32 {
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atomic_bool_basic() {
        let b = AtomicBool::new(false);

        assert!(!b.load(Ordering::Relaxed));

        b.swap(true, Ordering::Relaxed);
        assert!(b.load(Ordering::Relaxed));

        // compare_exchange
        assert!(b.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst));
        assert!(!b.load(Ordering::Relaxed));

        // 失败的 compare_exchange
        assert!(!b.compare_exchange(true, true, Ordering::SeqCst, Ordering::SeqCst));
    }

    #[test]
    fn test_atomic_operations() {
        let mut val: i32 = 10;
        let ptr = &mut val as *mut i32;

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // inc
            assert_eq!(atomic_inc(ptr), 10);
            assert_eq!(*ptr, 11);

            // dec
            assert_eq!(atomic_dec(ptr), 11);
            assert_eq!(*ptr, 10);

            // add
            assert_eq!(atomic_add(ptr, 5), 10);
            assert_eq!(*ptr, 15);

            // sub
            assert_eq!(atomic_sub(ptr, 3), 15);
            assert_eq!(*ptr, 12);

            // set
            atomic_set(ptr, 42);
            assert_eq!(*ptr, 42);

            // read
            assert_eq!(atomic_read(ptr), 42);

            // cmpxchg success
            assert!(atomic_cmpxchg(ptr, 42, 100));
            assert_eq!(*ptr, 100);

            // cmpxchg fail
            assert!(!atomic_cmpxchg(ptr, 99, 200));
            assert_eq!(*ptr, 100); // 值不变
        }
    }
}
