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
    #[must_use]
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

// === unsafe 依据单源 (下面七个 `atomic_*` 导出函数共用) ===
//
// 类别 A「i32 槽位重解释为 AtomicI32」: 七个函数的 body 都是
// `&*(ptr as *const AtomicI32)` 再调一个原子方法. 该重解释成立的前提两条:
// ① 布局等价 —— core 保证原子整数与其底层整数同尺寸、同位有效性, 且原子类型的
//    对齐恒等于其尺寸; `AtomicI32` 尺寸 4 ⇒ 对齐 4, 与本仓两个目标架构
//    (x86_64 / aarch64) 上 `i32` 的对齐 4 一致, 故重解释不抬高对齐要求.
// ② 调用方契约 —— 各函数 `# Safety` 段要求 `ptr` 有效、正确对齐且调用期间持续
//    有效, 即该槽位确实是被并发访问的原子变量, 而非只被单线程读写的普通 i32.
// 逐处注释只引「类别 A」并补该处特有的运算 / 内存序语义, 不重复上述两条.
// 七个函数一律 `SeqCst`: 这些导出面供 C 侧与 Rust 侧混用, 不承担 Release/Acquire
// 配对推理, 取最强序以免调用方误判可见性.

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
    // SAFETY: 类别 A (见本节单源); fetch_add(1) 是单槽位 RMW, 返回加一前的旧值.
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
    // SAFETY: 类别 A; fetch_sub(1) 溢出按 i32 补码回绕 (原子运算不 panic), 返回旧值.
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
    // SAFETY: 类别 A; 成功与失败两序均 SeqCst, 比较失败时不写槽位, 只返回 is_ok().
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
    // SAFETY: 类别 A; `val` 是任意 i32 实参, `AtomicI32::fetch_add` 按补码回绕并返回旧值, 无范围前提.
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
    // SAFETY: 类别 A; 与 atomic_add 互为逆运算, 同样不校验 val 范围、按补码回绕.
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
    // SAFETY: 类别 A; store 只发布新值不读旧值, 故不是 RMW, 无返回值可丢.
    unsafe {
        let atomic = &*(ptr as *const core::sync::atomic::AtomicI32);
        atomic.store(val, Ordering::SeqCst);
    }
}

/// 原子读取 (Atomic load)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// `ptr` 是指向 `i32` 的有效且正确对齐的指针, 在调用期间持续有效.
pub unsafe extern "C" fn atomic_read(ptr: *const i32) -> i32 {
    // SAFETY: 类别 A; 入参为 *const i32, cast 后只取共享引用做 load, 不写槽位.
    unsafe {
        let atomic = &*ptr.cast::<core::sync::atomic::AtomicI32>();
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

        // SAFETY: `ptr` 指向本测试栈上独占的 `val: i32`, 生命周期覆盖整个块; 五个原子操作串行作用于同一地址.
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
