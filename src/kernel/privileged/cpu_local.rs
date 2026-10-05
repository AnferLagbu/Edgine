//! `CpuLocal` — Per-CPU 变量安全抽象 (TCB)
//!
//! 提供类型安全的 per-CPU 数据访问，内部通过
//! `smp::current_cpu_index()` 索引静态槽位数组。
//!
//! ## 与 Asterinas OSTD `CpuLocal` 的关系
//!
//! 等价于 OSTD 的 `cpu_local!()` 宏 + `PerCpu<T>` 类型。
//!
//! ## SAFETY 不变量
//!
//! - 运行时 CPU 数 ≤ `MAX_CPUS`。
//! - `smp::current_cpu_index()` 返回值为 [0, `MAX_CPUS`) 范围内的有效索引。
//! - per-CPU 数据仅在所属 CPU 上访问。

use core::cell::UnsafeCell;

use crate::privileged::config::MAX_CPUS;

/// Per-CPU 数据容器。
///
/// 初始化通过 `init_this_cpu()` 方法，不使用 `const fn new()`。
/// 槽位用 `Option<T>` 管理生命周期：初始化 → 使用 → 释放。
pub struct CpuLocal<T: 'static> {
    slots: [UnsafeCell<Option<T>>; MAX_CPUS],
}

impl<T> CpuLocal<T> {
    /// 创建空的 Per-CPU 数组（所有槽位初始为 None）。
    pub fn new() -> Self {
        // SAFETY: `UnsafeCell<Option<T>>` 的内部表示是单个 `Option<T>` (含判别符 + 数据)。
        // `core::mem::zeroed()` 把所有字节置 0, 即每个槽位都是 `None`, 这对 `Option<T>`
        // 始终是合法状态 (None 变体不要求 T 初始化)。`MAX_CPUS` 在编译期已知常量,
        // 数组大小匹配, 不会越界。
        let slots: [UnsafeCell<Option<T>>; MAX_CPUS] = unsafe { core::mem::zeroed() };
        Self { slots }
    }

    /// 初始化当前 CPU 的数据。
    ///
    /// # Panics
    /// 如果当前 CPU 的槽位已被初始化。
    pub fn init_this_cpu(&self, val: T) {
        let cpu = crate::privileged::smp::current_cpu_index() as usize;
        assert!(cpu < MAX_CPUS, "CPU id {cpu} exceeds MAX_CPUS");
        // SAFETY:
        //   1. `cpu < MAX_CPUS` 已由上一行 assert 保证, 索引安全
        //   2. `init_this_cpu` 仅在启动单线程阶段调用, 无跨 CPU 竞争
        //   3. 每个 CPU 槽位只被该 CPU 访问, 借用检查器无法证明的"无别名"由
        //      类型系统层级保证 (CpuLocal 的 `Sync` impl 限定 T: Send)
        //   4. `slot.is_none()` 检查 + `*slot = Some(val)` 写入构成初始化序列
        unsafe {
            let slot = &mut *self.slots[cpu].get();
            assert!(slot.is_none(), "CpuLocal slot {cpu} already initialized");
            *slot = Some(val);
        }
    }

    /// 获取当前 CPU 数据的只读引用。
    ///
    /// # Panics
    /// 如果当前 CPU 的槽位未初始化。
    pub fn get(&self) -> &T {
        let cpu = crate::privileged::smp::current_cpu_index() as usize;
        assert!(cpu < MAX_CPUS);
        // SAFETY: 同 `init_this_cpu` 的 1-3 条款; 此外 `init_this_cpu` 已把 Some 写入,
        // 此处的 `expect("slot not initialized")` 是在违反调用契约时 panic (而非 UB)。
        unsafe {
            let slot = &*self.slots[cpu].get();
            slot.as_ref().expect("CpuLocal: slot not initialized")
        }
    }

    /// 获取当前 CPU 数据的可变引用。
    ///
    /// # Panics
    /// 如果当前 CPU 的槽位未初始化。
    pub fn get_mut(&self) -> &mut T {
        let cpu = crate::privileged::smp::current_cpu_index() as usize;
        assert!(cpu < MAX_CPUS);
        // SAFETY: 同 `init_this_cpu` 的 1-3 条款 + `get` 的初始化保证。
        // `&mut *UnsafeCell::get()` 产生独占 `&mut Option<T>`, Rust 借用检查器
        // 接受是因为 UnsafeCell 的特殊规则 (内部可变性 + 单线程独占)。
        unsafe {
            let slot = &mut *self.slots[cpu].get();
            slot.as_mut().expect("CpuLocal: slot not initialized")
        }
    }

    /// 释放当前 CPU 的槽位，返回数据。
    /// # Panics
    /// CPU 编号超出最大 CPU 数时 panic。
    pub fn take(&self) -> Option<T> {
        let cpu = crate::privileged::smp::current_cpu_index() as usize;
        assert!(cpu < MAX_CPUS);
        // SAFETY: 同 `init_this_cpu` 的 1-3 条款。`Option::take` 自身是 safe 操作,
        // 这里需要 unsafe 仅是为了访问 UnsafeCell 的内部值; `&mut` 借用与
        // 上面 get_mut 相同的"单线程独占"保证。
        unsafe {
            let slot = &mut *self.slots[cpu].get();
            slot.take()
        }
    }
}

// SAFETY: 每个 CPU 只访问自己的槽位，无跨 CPU 竞争。
unsafe impl<T: Send> Sync for CpuLocal<T> {}
