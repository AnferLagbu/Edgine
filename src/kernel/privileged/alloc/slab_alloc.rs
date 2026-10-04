//! `SlabAlloc` — 内核对象分配器 trait (TCB)
//!
//! 策略注入点: functions 层通过此 trait 分配/释放
//! 小对象 (≤ 8KB), 而不直接操作 Slab 缓存。
//!
//! ## 与 Asterinas OSTD `SlabAlloc` 的关系
//!
//! 等价于 OSTD 的 `HeapAlloc` trait (kmalloc 抽象)。
//!
//! ## SAFETY 不变量
//!
//! - 释放的指针必须来自同一分配器的 `alloc()`。
//! - layout 必须与分配时的 layout 一致。
//! - 分配器内部用 spinlock 保护, ISR 安全。

use core::alloc::Layout;
use core::ptr::NonNull;

/// Slab / kmalloc 分配器 trait。
///
/// 当前实现在 `mm/kmalloc.rs` (`KernelHeap` + Slab),
/// 未来可替换为自定义策略。
pub trait SlabAlloc: Send + Sync {
    /// 分配对齐内存块。
    fn alloc(&self, layout: Layout) -> Option<NonNull<u8>>;

    /// 分配零初始化内存块。
    fn alloc_zeroed(&self, layout: Layout) -> Option<NonNull<u8>>;

    /// 释放内存块。
    ///
    /// # SAFETY
    /// `ptr` 必须来自同一 `SlabAlloc` 实例的 `alloc()`。
    /// `layout` 必须与分配时的 layout 一致。
    unsafe fn free(&self, ptr: NonNull<u8>, layout: Layout);
}

// ============================================================================
// 默认实现: 委托给现有 KernelHeap
// ============================================================================

/// `KernelHeap` 实现的 `SlabAlloc`。
pub struct KmallocSlabAlloc;

impl SlabAlloc for KmallocSlabAlloc {
    fn alloc(&self, layout: Layout) -> Option<NonNull<u8>> {
        let heap = crate::privileged::mm::get_kmalloc();
        let ptr = heap.allocate(layout.size());
        let p = ptr?;
        NonNull::new(p)
    }

    fn alloc_zeroed(&self, layout: Layout) -> Option<NonNull<u8>> {
        let ptr = self.alloc(layout)?;
        // SAFETY: ptr is freshly allocated, zeroing is safe.
        unsafe {
            core::ptr::write_bytes(ptr.as_ptr(), 0, layout.size());
        }
        Some(ptr)
    }

    /// 释放 ptr 指向的内存。
    ///
    /// # Safety
    ///
    /// 调用方必须保证 `ptr` 来自本分配器的 `allocate` 返回, 且未被双重 free。
    /// `GlobalAlloc` 契约要求 ptr.layout 与本分配器路由一致。
    unsafe fn free(&self, ptr: NonNull<u8>, _layout: Layout) {
        // SAFETY:
        //   1. `ptr` 是 `NonNull<u8>`, 由 `allocate` 返回, 保证非空
        //   2. 调用方 (按 `GlobalAlloc::dealloc` 契约) 必须保证 ptr 来自本分配器
        //   3. 同一 ptr 未被双重 free (GlobalAlloc 自身要求, 不在 SAFETY 内)
        // `get_kmalloc()` 返回全局 kmalloc 堆, `deallocate` 内部按 size class
        // 归还到对应 slab; `_layout` 在当前实现中未使用 (slab 按 size class 路由)
        let heap = crate::privileged::mm::get_kmalloc();
        heap.deallocate(ptr.as_ptr());
    }
}
