//! OnceCellStorage — TCB 原语: 包装 `UnsafeCell<MaybeUninit<T>>`
//!
//! 这是框内核中**唯一**保留 `UnsafeCell<MaybeUninit<T>>` 类型细节的位置。
//! functions 层只能通过 `OnceCellStorage` 的 unsafe 方法操作内部 uninit 数据,
//! 但调用方必须自行保证:
//! - 同一时刻只有一个写入者 (通常配合 `Once` 串行化)
//! - 在读取前数据已被初始化
//!
//! ## 设计动机
//!
//! `std::sync::OnceLock` 等价物在 `no_std` 内核中无法直接用 std 实现。
//! 我们的方案:
//! 1. `Once` (在 functions 层, 纯 safe) 负责 "执行一次" 的串行化
//! 2. `OnceCellStorage<T>` (本模块, TCB) 提供底层 uninit 存储
//! 3. functions 层的 `OnceCell<T>` 组合 1 + 2, 对外只暴露 safe API
//!
//! ## SAFETY 契约
//!
//! - `write()`: 调用方必须保证当前是唯一的写者, 且该 cell 此后被视为"已初始化"。
//! - `get_ref()`: 调用方必须保证该 cell 已被 `write()` 初始化。
//! - `drop_in_place()`: 调用方必须保证该 cell 已被 `write()` 初始化, 且之后不再访问。
//! - `Send`/`Sync`: 与 `T: Send`/`T: Send + Sync` 一致 — 因为 `Once` 串行化保证
//!   所有访问都是原子的, 类型系统的 send/sync 推断已正确。

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

/// 一次性值存储 (TCB 原语)。
///
/// 仅作为 `OnceCell` 的内部表示使用 — functions 层应使用
/// `functions::sync::once::OnceCell`, 它组合 `Once` 和本类型, 提供纯 safe API。
pub struct OnceCellStorage<T> {
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: 访问完全由 `Once` 串行化; 内部 `UnsafeCell<MaybeUninit<T>>` 是
// Rust 标准库在 `OnceLock` 中使用的同一模式, 等价于 `T: Send` 时可跨线程移动。
unsafe impl<T: Send> Send for OnceCellStorage<T> {}
// SAFETY: `&Self` 可跨线程共享引用, 实际访问在 Once 互斥下进行。
unsafe impl<T: Send + Sync> Sync for OnceCellStorage<T> {}

impl<T> OnceCellStorage<T> {
    /// 构造未初始化的存储。
    pub const fn new() -> Self {
        Self {
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// 写入值 (假设尚未初始化)。
    ///
    /// # SAFETY
    /// - 调用方必须确保本 cell **此前未被初始化** (写独占)。
    /// - 调用方必须确保此后只有读访问, 或正确调用 `drop_in_place`。
    pub unsafe fn write(&self, val: T) {
        // SAFETY: 由调用方保证写独占 (无其他线程可观察到 uninit 状态)。
        unsafe { (*self.value.get()).write(val) };
    }

    /// 读取已初始化的值 (返回 `&T`)。
    ///
    /// # SAFETY
    /// 调用方必须确保本 cell 已被 `write()` 初始化 (即 `Once::is_completed()` 为真)。
    pub unsafe fn assume_init_ref(&self) -> &T {
        // SAFETY: 由调用方保证 cell 已初始化。
        unsafe { (*self.value.get()).assume_init_ref() }
    }

    /// 在原位 drop 已初始化的值。
    ///
    /// # SAFETY
    /// - 调用方必须确保本 cell 已被 `write()` 初始化。
    /// - 调用方必须确保 drop 后不再访问 cell (在所有权终止时调用一次即可)。
    pub unsafe fn assume_init_drop(&self) {
        // SAFETY: 由调用方保证 cell 已初始化。
        unsafe { (*self.value.get()).assume_init_drop() };
    }
}
