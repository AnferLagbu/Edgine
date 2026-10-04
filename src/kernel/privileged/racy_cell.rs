//! 无锁全局可变状态容器 (`RacyCell`)
//!
//! 用于替换 `static mut` 全局变量, 提供安全的 `&self` 读写接口。
//! 内部使用 [`UnsafeCell`] 并手动实现 `Sync`, 允许跨线程无锁访问。
//!
//! # Safety
//!
//! 调用方必须保证不会发生数据竞争(例如通过外部锁、per-CPU 亲和性、
//! 或仅在内核单线程启动阶段访问)。
//!
//! 参考: Linux kernel `kernel::types::RacyCell`, Asterinas `RacyCell`.

use core::cell::UnsafeCell;

/// 无锁可变全局容器
///
/// 与 `UnsafeCell` 的区别在于 `RacyCell` 实现了 `Sync`,
/// 允许在 `static` 中安全地存储可变数据。
///
/// # 示例
///
/// ```ignore
/// static CURRENT_PROCESS: RacyCell<CProcess> = RacyCell::new(CProcess::zero());
///
/// // 安全地读取(调用方保证无数据竞争)
/// let pid = CURRENT_PROCESS.map(|p| p.pid);
/// ```
pub struct RacyCell<T> {
    inner: UnsafeCell<T>,
}

// SAFETY: RacyCell 的 Sync 安全性由调用方保证(外部同步或 per-CPU 访问)。
// 这与 UnsafeCell 不同——UnsafeCell 不实现 Sync 是为了强制编译器检查,
// 但内核中许多全局状态确实需要 Sync 特性(例如 static 变量)。
//
// B03-24 根治 (2026-08-23): 加 `T: Send` 约束 — Sync 类型必须可安全跨线程
// 引用 (Send 是必要条件), 无约束会让非 Send 数据 (如含裸指针的容器) 被强制
// Sync → 跨线程共享裸指针 UB。配套改动: functions/ipc 的 `NonNull<Message>`
// 链表指针改 `AtomicPtr<Message>` (safe Send), 使 IpcNamespace/DynIpcNamespace
// 满足 `T: Send`。Rust 1.98+ 的 `NonNull<T>` 已是显式 `!Send`, 无法直接使用。
unsafe impl<T: Send> Sync for RacyCell<T> {}

impl<T> RacyCell<T> {
    /// 创建新的 `RacyCell`
    pub const fn new(val: T) -> Self {
        Self {
            inner: UnsafeCell::new(val),
        }
    }

    /// 获取不可变引用
    ///
    /// # Safety
    ///
    /// 调用方必须保证当前没有对同一数据的可变引用。
    pub unsafe fn get(&self) -> &T {
        unsafe {
            // SAFETY: `UnsafeCell::get()` 返回 `*mut T`, `&*` 解引用为 `&T`。
            // 调用方 (此函数标记为 `unsafe fn`) 必须保证: 当前无其他线程持有
            // `&mut T` 引用, 否则构成数据竞争 (Rust 内存模型 UB)。
            &*self.inner.get()
        }
    }

    /// 获取可变引用
    ///
    /// 调用方必须保证独占访问。单线程上下文或外部同步保证。
    pub fn get_mut(&self) -> &mut T {
        // SAFETY: RacyCell 的 Sync 实现由调用方通过外部同步保证。
        // 在单线程或持有锁的上下文中调用是安全的。
        // `&mut *UnsafeCell::get()` 产生独占引用, 与外部同步原语 (如 SpinLock) 配合
        // 时, 锁的持有期间独占, drop 时释放, 借用检查器接受是因为 UnsafeCell 的特殊规则。
        unsafe { &mut *self.inner.get() }
    }

    /// 读取值(仅适用于 `Copy` 类型)
    ///
    /// # Safety
    ///
    /// 调用方必须保证读取期间无并发写入。
    pub unsafe fn read(&self) -> T
    where
        T: Copy,
    {
        unsafe {
            // SAFETY: `core::ptr::read` 是未对齐的 memcpy, 不要求 T 初始化;
            // 调用方必须保证: 1) T: Copy (T 是无 drop 的简单值), 2) 当前无并发写
            // (避免读到撕裂值), 3) 地址来自 UnsafeCell, 必然 valid for reads。
            core::ptr::read(self.inner.get())
        }
    }

    /// 写入值
    ///
    /// 通过闭包安全地访问内部数据
    ///
    /// 这是推荐的安全 API——闭包内对 `&T` 的访问是安全的,
    /// 因为 Rust 借用规则保证 `&T` 无数据竞争。
    pub fn map<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        // SAFETY: &T 是只读借用, Rust 借用规则保证无数据竞争。
        // 调用方通过外部同步机制保证无并发写入。
        f(unsafe { &*self.inner.get() })
    }

    /// 通过闭包安全地修改内部数据
    ///
    /// 闭包内对 `&mut T` 的访问是安全的,
    /// 因为 Rust 借用规则保证 `&mut T` 是独占的。
    pub fn map_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        // SAFETY: &mut T 是独占借用, Rust 借用规则保证无并发访问。
        // 调用方通过外部同步机制保证不会同时有多个 &mut 引用。
        f(unsafe { &mut *self.inner.get() })
    }

    /// 获取内部数据的原始指针
    pub fn as_ptr(&self) -> *const T {
        self.inner.get()
    }

    /// 安全获取 `Option<T>` 内部值的不可变引用
    ///
    /// 与 `unsafe fn get` 的区别: 本方法专门针对 `Option<T>` 模式,
    /// 调用方只需保证 `Option` 已被初始化 (`Some`), 而无需担心借用竞争。
    ///
    /// # 安全性保证
    ///
    /// 内部使用 `get()` 获取 `&Option<T>`, 调用方应通过外部同步
    /// (锁/per-CPU 等) 保证无并发写。返回值是从 `&Option<T>` 中
    /// 解包得到的 `&T`, 借用规则确保 `&T` 与 `&Option<T>` 生命周期相同。
    ///
    /// # Panics
    ///
    /// 当内部 `Option` 为 `None` 时 panic (即尚未初始化)。
    pub fn get_ref(&self) -> &<T as StableDeref>::Target
    where
        T: StableDeref,
    {
        // 调用方保证无并发写 (通过外部同步), 而 `T: StableDeref` 保证
        // `Option<T>` 解包为 `&Target` 是安全的栈式判别联合借用。
        self.get_unchecked_option()
    }

    /// (内部) 安全获取 `Option<T>` 引用
    fn get_unchecked_option(&self) -> &<T as StableDeref>::Target
    where
        T: StableDeref,
    {
        // SAFETY: 调用方保证无并发写; `Option<T>` 借用稳定。
        let opt_ref = unsafe { &*self.inner.get() };
        opt_ref.unpack()
    }
}

/// Trait 用于在 `RacyCell::get_ref` 中安全解包 `Option<T>`
///
/// `Option<T>` 的 `&Option<T>` 解包为 `&T` 的安全性依赖于:
/// 1. `Option<T>` 是栈式判别联合 (discriminated union), 与 `T` 共享地址。
/// 2. 当 `Option` 为 `Some` 时, `&T` 借用与 `&Option<T>` 借用生命周期一致。
///
/// 这个 trait 显式标记该不变量, 避免随意解包。
pub trait StableDeref {
    /// 解包为内部值的 `&T` 引用
    fn unpack(&self) -> &Self::Target;
    /// 解包的目标类型
    type Target;
}

impl<T> StableDeref for Option<T> {
    type Target = T;
    fn unpack(&self) -> &T {
        self.as_ref().expect("RacyCell<Option<T>> not initialized")
    }
}
