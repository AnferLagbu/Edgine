//! Slab 分配器 (Slab Allocator) - Rust 完整实现
//!
//! ## 功能概览
//!
//! 基于**伙伴系统 (Buddy System)** 和**位图管理**的 Slab 分配器。
//! 用于高效的小对象内存分配, 减少内存碎片。
//!
//! `Edgine` 原生 Slab 分配器 — 减少内存碎片, 加速频繁分配/释放
//!
//! **功能复刻 + Rust 增强**:
//! ✅ **类型安全**: `Slab<T>` 泛型替代 `void*`
//! ✅ **自动清理**: RAII (Drop trait) 自动释放 Slab
//! ✅ **错误处理**: `Option` / `Result` 强制显式错误处理
//! ✅ **零成本**: 编译时单态化消除泛型开销
//! ✅ **借用检查**: 防止 use-after-free 和 double-free
//!
//! ## 核心数据结构
//!
//! ```text
//! KmemCache (缓存描述符)
//! ├── object_size: usize        // 对象大小
//! ├── objects_per_slab: u32     // 每个 Slab 的对象数
//! ├── slabs_full: LinkedList    // 完全使用的 Slab 链表
//! ├── slabs_partial: LinkedList // 部分使用的 Slab 链表
//! └── slabs_free: LinkedList    // 完全空闲的 Slab 链表
//!
//! Slab (物理页 + 对象数组 + 位图)
//! ├── header: SlabHeader        // 元数据
//! ├── objects: [T; N]           // 对象数组
//! └── bitmap: [u8; M]          // 分配位图
//! ```

// ============================================================================
// 日志宏 (与 pmm.rs 保持一致)
// ============================================================================

macro_rules! klog_slab {
    ($($arg:tt)*) => {
        $crate::klog_ffi!(klog_ffi_info, $($arg)*)
    };
}

// ============================================================================
// 常量定义
// ============================================================================

/// Slab 配置常量 (统一从 config.rs 引用)
pub use crate::framework::config::{
    SLAB_DEFAULT_SIZE, SLAB_GENERAL_CACHE_NUM, SLAB_MAX_OBJECT_SIZE, SLAB_MIN_OBJECT_SIZE,
};

/// 预定义的通用缓存大小 (bytes)
/// 覆盖常见的小对象分配需求
pub const GENERAL_CACHE_SIZES: [usize; SLAB_GENERAL_CACHE_NUM] =
    [16, 32, 64, 128, 256, 512, 1024, 2048];

// ============================================================================
// 数据结构定义
// ============================================================================

/// Slab 描述符头 (存储在每个物理页的开头)
///
/// 内存布局:
/// ```text
/// [SlabHeader | Objects... | Bitmap]
/// ```
#[derive(Debug)]
#[repr(C)]
pub(crate) struct SlabHeader {
    /// 对象区域起始地址 (相对于页面起始)
    start_addr: *mut u8,

    /// 该 Slab 可容纳的对象总数
    obj_count: u32,

    /// 当前已分配的对象数
    active_count: u32,

    /// 是否已满 (所有对象都已分配)
    is_full: bool,

    /// 双向链表指针 (用于挂在 `KmemCache` 的链表中)
    prev: *mut Self,
    next: *mut Self,
}

impl Default for SlabHeader {
    fn default() -> Self {
        Self {
            start_addr: core::ptr::null_mut(),
            obj_count: 0,
            active_count: 0,
            is_full: false,
            prev: core::ptr::null_mut(),
            next: core::ptr::null_mut(),
        }
    }
}

// === E4: unsafe 集中化 — 裸指针子模块 ===
//
// slab 内部涉及的所有裸指针解引用都封装在这里.
pub(crate) mod raw {
    use super::SlabHeader;

    /// `*mut SlabHeader` 的 safe 包装器.
    ///
    /// SAFETY 不变式: 指针指向 slab 页内合法的 `SlabHeader`,
    /// 且 slab 锁 (或全局锁) 已持有.
    #[derive(Clone, Copy)]
    pub struct SlabRef(*mut SlabHeader);

    impl SlabRef {
        /// # Safety
        /// - `ptr` 必须指向合法的 `SlabHeader`
        /// - 必须持有相应的锁
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub unsafe fn new_unchecked(ptr: *mut SlabHeader) -> Self {
            Self(ptr)
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn as_ptr(self) -> *mut SlabHeader {
            self.0
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn start_addr(&self) -> *mut u8 {
            // SAFETY: caller guarantees valid pointer
            unsafe { (*self.0).start_addr }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_start_addr(&self, val: *mut u8) {
            // SAFETY: caller guarantees valid pointer
            unsafe {
                (*self.0).start_addr = val;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn obj_count(&self) -> u32 {
            // SAFETY: caller guarantees valid pointer
            unsafe { (*self.0).obj_count }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_obj_count(&self, val: u32) {
            // SAFETY: caller guarantees valid pointer
            unsafe {
                (*self.0).obj_count = val;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn active_count(&self) -> u32 {
            // SAFETY: caller guarantees valid pointer
            unsafe { (*self.0).active_count }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_active_count(&self, val: u32) {
            // SAFETY: caller guarantees valid pointer
            unsafe {
                (*self.0).active_count = val;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn is_full(&self) -> bool {
            // SAFETY: caller guarantees valid pointer
            unsafe { (*self.0).is_full }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_is_full(&self, val: bool) {
            // SAFETY: caller guarantees valid pointer
            unsafe {
                (*self.0).is_full = val;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn prev(&self) -> *mut SlabHeader {
            // SAFETY: caller guarantees valid pointer
            unsafe { (*self.0).prev }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_prev(&self, p: *mut SlabHeader) {
            // SAFETY: caller guarantees valid pointer
            unsafe {
                (*self.0).prev = p;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn next(&self) -> *mut SlabHeader {
            // SAFETY: caller guarantees valid pointer
            unsafe { (*self.0).next }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_next(&self, p: *mut SlabHeader) {
            // SAFETY: 调用方保证指针合法
            unsafe {
                (*self.0).next = p;
            }
        }

        /// 在该位置写入默认的 `SlabHeader`.
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn write_default(&self) {
            // SAFETY: 调用方保证指针合法
            unsafe {
                *self.0 = SlabHeader::default();
            }
        }

        /// 获取该 slab 的 bitmap 指针.
        /// Bitmap 起始于 header + 对象区之后.
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn bitmap_ptr(&self, object_size: usize) -> *mut u8 {
            // SAFETY: 调用方保证指针合法且 object_size 正确
            unsafe {
                let obj_area = (*self.0).start_addr;
                obj_area.add((*self.0).obj_count as usize * object_size)
            }
        }

        /// 获取指定索引处的对象指针.
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn object_ptr(&self, idx: u32, object_size: usize) -> *mut u8 {
            // SAFETY: 调用方保证指针合法且 idx < obj_count
            unsafe { (*self.0).start_addr.add(idx as usize * object_size) }
        }
    }

    /// 清零一段内存.
    ///
    /// # Safety
    /// - `ptr` 必须指向 `len` 字节的合法可写区
    #[inline(always)]
    pub unsafe fn zero_memory(ptr: *mut u8, len: usize) {
        unsafe {
            core::ptr::write_bytes(ptr, 0, len);
        }
    }
}

use super::PAGE_SIZE;
use raw::SlabRef;

/// 缓存描述符 (`KmemCache`)
///
/// 管理一组相同大小的对象。
/// 内部维护三个 Slab 链表:
/// - `slabs_full`: 所有对象都已分配
/// - `slabs_partial`: 部分对象已分配
/// - `slabs_free`: 所有对象都空闲
#[derive(Debug)]
pub struct KmemCache {
    /// 缓存名称 (用于调试)
    name: &'static str,

    /// 单个对象的大小 (bytes)
    pub(crate) object_size: usize,

    /// 每个 Slab 可容纳的对象数
    pub(crate) objects_per_slab: u32,

    /// 完全使用的 Slab 链表头
    slabs_full: *mut SlabHeader,

    /// 部分使用的 Slab 链表头
    slabs_partial: *mut SlabHeader,

    /// 完全空闲的 Slab 链表头
    slabs_free: *mut SlabHeader,

    /// 总 Slab 数量
    pub(crate) slab_count: u32,

    /// 统计: 总分配次数
    total_allocs: u64,

    /// 统计: 总释放次数
    total_frees: u64,

    /// 统计: 缓存命中次数 (从已有 Slab 分配)
    cache_hits: u64,

    /// 统计: 缓存未命中次数 (需要新建 Slab)
    cache_misses: u64,
}

// SAFETY: KmemCache 的裸指针 (slabs_full/partial/free) 指向 PMM 分配的页面,
//         在内核生命周期内始终有效; 所有访问由外部锁 (IrqSpinLock) 保护.
unsafe impl Send for KmemCache {}

impl KmemCache {
    /// 创建新的缓存
    ///
    /// # Arguments
    /// * `name` - 缓存名称 (静态字符串, 用于调试)
    /// * `object_size` - 单个对象的大小 (bytes)
    ///
    /// # Returns
    /// * Ok(KmemCache) - 成功创建
    /// * Err(&str) - 错误描述 (大小无效等)
    ///
    /// # Errors
    /// 当 `object_size` 为 0 或超过最大对象大小上限 (`SLAB_MAX_OBJECT_SIZE`) 时,
    /// 返回 `Err`, 错误描述为 "Invalid object size (zero or exceeds maximum)".
    pub fn create(name: &'static str, object_size: usize) -> Result<Self, &'static str> {
        // T2-3: 大小规范化委托给 SlabPolicy
        let effective_size = super::slab_trait::current_slab_policy()
            .normalize_object_size(object_size)
            .ok_or("Invalid object size (zero or exceeds maximum)")?;

        let objects_per_slab = Self::calculate_objects_per_slab(effective_size);

        Ok(Self {
            name,
            object_size: effective_size,
            objects_per_slab,
            slabs_full: core::ptr::null_mut(),
            slabs_partial: core::ptr::null_mut(),
            slabs_free: core::ptr::null_mut(),
            slab_count: 0,
            total_allocs: 0,
            total_frees: 0,
            cache_hits: 0,
            cache_misses: 0,
        })
    }

    /// 计算每个 Slab 可容纳的对象数
    ///
    /// T2-3: 策略已提取到 `slab_trait::SlabPolicy`, 本函数保留为内部快捷路径
    /// (直接调用 `current_slab_policy().calculate_objects_per_slab()`).
    fn calculate_objects_per_slab(object_size: usize) -> u32 {
        super::slab_trait::current_slab_policy().calculate_objects_per_slab(
            SLAB_DEFAULT_SIZE,
            core::mem::size_of::<SlabHeader>(),
            object_size,
        )
    }

    pub fn active_objects(&self) -> u64 {
        self.total_allocs.saturating_sub(self.total_frees)
    }

    /// 从缓存中分配一个对象
    ///
    /// # Returns
    /// * Some(*mut u8) - 成功分配的对象指针
    /// * None - 分配失败 (内存不足)
    pub fn allocate(&mut self) -> Option<*mut u8> {
        self.total_allocs += 1;

        // 优先从 partial 链表分配
        if !self.slabs_partial.is_null() {
            self.cache_hits += 1;
            return self.alloc_from_slab(self.slabs_partial);
        }

        // 其次从 free 链表分配
        if !self.slabs_free.is_null() {
            self.cache_hits += 1;
            let slab_ptr = self.slabs_free; // 复制指针值, 避免借用冲突
            return self.alloc_from_slab(slab_ptr);
        }

        // 最后: 新建一个 Slab
        self.cache_misses += 1;
        let new_slab = self.new_slab()?;

        // 将新 Slab 加入 free 链表
        // SAFETY: new_slab is a valid pointer from new_slab()
        let ns = unsafe { SlabRef::new_unchecked(new_slab) };
        ns.set_next(self.slabs_free);
        if !self.slabs_free.is_null() {
            let old = unsafe { SlabRef::new_unchecked(self.slabs_free) };
            old.set_prev(new_slab);
        }
        self.slabs_free = new_slab;
        self.slab_count += 1;

        self.alloc_from_slab(new_slab)
    }

    /// 从指定 Slab 中分配对象 (内部辅助函数)
    fn alloc_from_slab(&mut self, slab: *mut SlabHeader) -> Option<*mut u8> {
        let free_idx = self.find_free_bit(slab)?;

        self.set_bit(slab, free_idx);

        // SAFETY: slab is a valid pointer from new_slab (PMM-allocated page)
        let sr = unsafe { SlabRef::new_unchecked(slab) };
        sr.set_active_count(sr.active_count() + 1);

        let obj_ptr = sr.object_ptr(free_idx, self.object_size);

        if sr.active_count() >= sr.obj_count() {
            sr.set_is_full(true);
            Self::list_remove(&mut self.slabs_partial, slab);
            Self::list_remove(&mut self.slabs_free, slab);
            Self::list_push_front(&mut self.slabs_full, slab);
        } else if sr.active_count() == 1 {
            Self::list_remove(&mut self.slabs_free, slab);
            Self::list_push_front(&mut self.slabs_partial, slab);
        }

        Some(obj_ptr)
    }

    /// 释放对象回缓存
    ///
    /// # Arguments
    /// * `obj` - 要释放的对象指针 (必须是从此缓存分配的)
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn deallocate(&mut self, obj: *mut u8) {
        if obj.is_null() {
            return;
        }

        self.total_frees += 1;

        // 查找对象所属的 Slab
        let slab = self.find_object_slab(obj);
        if slab.is_null() {
            return; // 对象不属于此缓存
        }

        // SAFETY: slab was found by find_object_slab, guaranteed valid
        let sr = unsafe { SlabRef::new_unchecked(slab) };

        // 计算对象索引
        let obj_addr = obj as usize;
        let start_addr = sr.start_addr() as usize;
        let obj_idx = (obj_addr - start_addr) / self.object_size;

        if obj_idx >= sr.obj_count() as usize {
            return; // 索引越界 (可能是无效指针)
        }

        // 清除位图中的标志位
        self.clear_bit(slab, obj_idx as u32);

        // 更新计数
        sr.set_active_count(sr.active_count() - 1);

        // 移动 Slab 到正确的链表
        if sr.is_full() {
            // 从 full 移动到 partial
            sr.set_is_full(false);

            Self::list_remove(&mut self.slabs_full, slab);
            Self::list_push_front(&mut self.slabs_partial, slab);
        } else if sr.active_count() == 0 {
            // 全部释放 → 从 partial 移动到 free
            Self::list_remove(&mut self.slabs_partial, slab);
            Self::list_push_front(&mut self.slabs_free, slab);
        }
    }

    /// 销毁缓存 (释放所有 Slab)
    pub fn destroy(&mut self) {
        // 释放 full 链表中的所有 Slab
        let mut slab = self.slabs_full;
        while !slab.is_null() {
            // SAFETY: 循环条件 !is_null 保证 slab 指向合法 Slab 头; destroy
            // 方法独占缓存, 无并发别名; SlabRef::new_unchecked 仅包装指针.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            let next = sr.next();
            self.destroy_slab(slab);
            slab = next;
        }

        // 释放 partial 链表中的所有 Slab
        slab = self.slabs_partial;
        while !slab.is_null() {
            // SAFETY: 同上, slab 由 slabs_partial 链表保证是合法 Slab 头.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            let next = sr.next();
            self.destroy_slab(slab);
            slab = next;
        }

        // 释放 free 链表中的所有 Slab
        slab = self.slabs_free;
        while !slab.is_null() {
            // SAFETY: 同上, slab 由 slabs_free 链表保证是合法 Slab 头.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            let next = sr.next();
            self.destroy_slab(slab);
            slab = next;
        }

        // 重置状态
        self.slabs_full = core::ptr::null_mut();
        self.slabs_partial = core::ptr::null_mut();
        self.slabs_free = core::ptr::null_mut();
        self.slab_count = 0;
    }

    /// 获取缓存统计信息
    pub fn get_stats(&self) -> CacheStats {
        let mut total_objects = 0u32;
        let mut active_objects = 0u32;

        // 遍历 full 链表
        let mut slab = self.slabs_full;
        while !slab.is_null() {
            // SAFETY: 循环条件 !is_null 保证 slab 指向合法 Slab 头; get_stats
            // 只读统计, 无并发修改; SlabRef::new_unchecked 仅包装指针.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            total_objects += sr.obj_count();
            active_objects += sr.active_count();
            slab = sr.next();
        }

        // 遍历 partial 链表
        slab = self.slabs_partial;
        while !slab.is_null() {
            // SAFETY: 同上, slab 由 slabs_partial 链表保证是合法 Slab 头.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            total_objects += sr.obj_count();
            active_objects += sr.active_count();
            slab = sr.next();
        }

        CacheStats {
            total_objects,
            active_objects,
            total_slabs: self.slab_count,
            total_allocs: self.total_allocs,
            total_frees: self.total_frees,
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
        }
    }

    // ========================================================================
    // 内部辅助方法 (private)
    // ========================================================================

    /// 创建新的 Slab (分配一页物理内存)
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "page as *mut SlabHeader: pmm_alloc_pages 返回页对齐 *mut u8, 安全 cast"
    )]
    fn new_slab(&self) -> Option<*mut SlabHeader> {
        // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
        unsafe extern "C" {
            fn pmm_alloc_pages(count: u64) -> *mut u8;
        }
        let pages_needed = SLAB_DEFAULT_SIZE.div_ceil(PAGE_SIZE as usize);
        // SAFETY: pmm_alloc_pages 返回空 (失败) 或经 KERNEL_BASE 映射的合法页对齐物理地址
        let page = unsafe { pmm_alloc_pages(pages_needed as u64) };

        if page.is_null() {
            return None;
        }

        let slab = page as *mut SlabHeader;

        // SAFETY: slab points to a PMM-allocated page
        let sr = unsafe { SlabRef::new_unchecked(slab) };
        sr.write_default();
        // SAFETY: page 是 pmm_alloc_pages 分配的页对齐指针; add 偏移在 page
        // 范围内 (page 至少 SLAB_DEFAULT_SIZE 字节, header 后还有 bitmap 空间);
        sr.set_start_addr(unsafe { page.add(core::mem::size_of::<SlabHeader>()) } as *mut u8);
        sr.set_obj_count(self.objects_per_slab);

        let bitmap_bytes = self.objects_per_slab.div_ceil(8);
        let bitmap_start = sr.bitmap_ptr(self.object_size);

        // SAFETY: bitmap_start 由 sr.bitmap_ptr 计算, 落在 page 范围内;
        // bitmap_bytes = obj_count/8 是字节目数, add 偏移在合法范围内;
        // 仅作指针运算, 后续零内存操作前还有 bounds check.
        let bitmap_end = unsafe { bitmap_start.add(bitmap_bytes as usize) };
        // SAFETY: 同上, page 是 PMM 分配指针, add 偏移在 page 范围内.
        let page_end = unsafe { page.add(SLAB_DEFAULT_SIZE) } as *mut u8;

        if bitmap_end > page_end {
            // SAFETY: pmm_free_pages 由 PMM 层提供, 释放 new_slab 中分配的页
            unsafe extern "C" {
                fn pmm_free_pages(addr: *mut u8, count: u64);
            }
            // SAFETY: page was just allocated; freeing on layout overflow
            unsafe {
                pmm_free_pages(page, pages_needed as u64);
            }
            klog_slab!(
                "[SLAB] new_slab: layout overflow (obj_size={}, obj_count={}, bitmap={}B), page={:?}",
                self.object_size,
                self.objects_per_slab,
                bitmap_bytes,
                page
            );
            return None;
        }

        // SAFETY: bitmap_start..bitmap_end is within the page (verified above)
        unsafe {
            raw::zero_memory(bitmap_start, bitmap_bytes as usize);
        }

        Some(slab)
    }

    /// 销毁单个 Slab (释放物理页)
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    fn destroy_slab(&self, slab: *mut SlabHeader) {
        if slab.is_null() {
            return;
        }
        // SAFETY: slab 由 new_slab 中 pmm_alloc_pages 分配,
        // 释放同等数量页. 调用方保证 slab 已不在任何链表中且不持有活动对象.
        unsafe {
            let pages_needed = SLAB_DEFAULT_SIZE.div_ceil(PAGE_SIZE as usize);
            #[expect(
                clippy::items_after_statements,
                reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
            )]
            // SAFETY: pmm_free_pages 由 PMM 层提供, 释放 destroy_slab 持有的 slab 物理页
            unsafe extern "C" {
                fn pmm_free_pages(addr: *mut u8, count: u64);
            }
            pmm_free_pages(slab as *mut u8, pages_needed as u64);
        }
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn find_free_bit(&self, slab: *mut SlabHeader) -> Option<u32> {
        // SAFETY: slab 来自 find_free_bit 调用方, 是合法 Slab 头; lock 持有中.
        let sr = unsafe { SlabRef::new_unchecked(slab) };
        let bitmap_bytes = sr.obj_count().div_ceil(8) as usize;
        let bitmap_start = sr.bitmap_ptr(self.object_size);

        for byte_idx in 0..bitmap_bytes {
            // SAFETY: bitmap region verified during init; byte_idx < bitmap_bytes
            // 保证 add 落在 page 范围内, 读操作不会越界.
            let byte = unsafe { *bitmap_start.add(byte_idx) };
            if byte == 0xFF {
                continue;
            }
            let bit_idx = byte.trailing_ones();
            let global_bit = byte_idx as u32 * 8 + bit_idx;
            if global_bit < sr.obj_count() {
                return Some(global_bit);
            }
        }

        None
    }

    fn set_bit(&self, slab: *mut SlabHeader, bit: u32) {
        // SAFETY: slab 由调用者保证是合法 Slab 头 (来自 new_slab/alloc/free);
        // SlabRef::new_unchecked 仅包装指针, 后续操作在 lock 持有中.
        let sr = unsafe { SlabRef::new_unchecked(slab) };
        let bitmap_start = sr.bitmap_ptr(self.object_size);

        let byte_idx = (bit / 8) as usize;
        let bit_idx = bit % 8;

        // SAFETY: bit < obj_count (ensured by find_free_bit); byte_idx = bit/8
        // 落在 bitmap 范围内 (bitmap_bytes = obj_count.div_ceil(8)).
        unsafe {
            let byte_ptr = bitmap_start.add(byte_idx);
            *byte_ptr |= 1 << bit_idx;
        }
    }

    fn clear_bit(&self, slab: *mut SlabHeader, bit: u32) {
        // SAFETY: 同 set_bit, slab 合法, lock 持有.
        let sr = unsafe { SlabRef::new_unchecked(slab) };
        let bitmap_start = sr.bitmap_ptr(self.object_size);

        let byte_idx = (bit / 8) as usize;
        let bit_idx = bit % 8;

        // SAFETY: bit < obj_count (computed from obj address); byte_idx 落
        // 在 bitmap 范围内, 写操作不会越界.
        unsafe {
            let byte_ptr = bitmap_start.add(byte_idx);
            *byte_ptr &= !(1 << bit_idx);
        }
    }

    /// 查找对象所属的 Slab
    fn find_object_slab(&self, obj: *mut u8) -> *mut SlabHeader {
        let obj_addr = obj as usize;

        let mut slab = self.slabs_partial;
        while !slab.is_null() {
            // SAFETY: 循环条件 !is_null 保证 slab 指向合法 Slab 头 (链表项);
            // 后续只读取 start_addr/obj_count/next 字段, 不修改.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            let start = sr.start_addr() as usize;
            let end = start + sr.obj_count() as usize * self.object_size;
            if obj_addr >= start && obj_addr < end {
                return slab;
            }
            slab = sr.next();
        }

        slab = self.slabs_full;
        while !slab.is_null() {
            // SAFETY: 同上, slab 来自 slabs_full 链表, 是合法 Slab 头.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            let start = sr.start_addr() as usize;
            let end = start + sr.obj_count() as usize * self.object_size;
            if obj_addr >= start && obj_addr < end {
                return slab;
            }
            slab = sr.next();
        }

        slab = self.slabs_free;
        while !slab.is_null() {
            // SAFETY: 同上, slab 来自 slabs_free 链表, 是合法 Slab 头.
            let sr = unsafe { SlabRef::new_unchecked(slab) };
            let start = sr.start_addr() as usize;
            let end = start + sr.obj_count() as usize * self.object_size;
            if obj_addr >= start && obj_addr < end {
                return slab;
            }
            slab = sr.next();
        }

        core::ptr::null_mut()
    }

    /// ✅ 从双向链表中移除节点 (静态方法, 避免借用冲突)
    ///
    /// # Arguments
    /// * `head` - 链表头指针的可变引用 (如 &mut `self.slabs_partial`)
    /// * `slab` - 要移除的节点
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn list_remove(head: &mut *mut SlabHeader, slab: *mut SlabHeader) {
        if head.is_null() || slab.is_null() {
            return;
        }

        // SAFETY: 入口处 !is_null 校验保证 slab 指向合法 Slab 头; 调用方
        // 保证链表状态一致; SlabRef::new_unchecked 仅包装指针.
        let sr = unsafe { SlabRef::new_unchecked(slab) };

        if *head == slab {
            // 移除的是头节点
            *head = sr.next();
            if !head.is_null() {
                // SAFETY: !is_null 分支保证 *head 指向合法 Slab 头 (即原
                // 头节点的 next, 由 sr.next() 获得).
                let new_head = unsafe { SlabRef::new_unchecked(*head) };
                new_head.set_prev(core::ptr::null_mut());
            }
        } else {
            // 移除的是中间或尾节点
            let next = sr.next();
            let prev = sr.prev();
            if !next.is_null() {
                // SAFETY: next 由 sr.next() 获得, 非空即合法 Slab 头指针.
                let n = unsafe { SlabRef::new_unchecked(next) };
                n.set_prev(prev);
            }
            if !prev.is_null() {
                // SAFETY: prev 由 sr.prev() 获得, 非空即合法 Slab 头指针.
                let p = unsafe { SlabRef::new_unchecked(prev) };
                p.set_next(next);
            }
        }

        sr.set_prev(core::ptr::null_mut());
        sr.set_next(core::ptr::null_mut());
    }

    #[inline(always)]
    fn list_push_front(head: &mut *mut SlabHeader, slab: *mut SlabHeader) {
        if slab.is_null() {
            return;
        }

        // SAFETY: 入口处 !is_null 校验保证 slab 合法; SlabRef 仅包装指针.
        let sr = unsafe { SlabRef::new_unchecked(slab) };
        sr.set_next(*head);
        sr.set_prev(core::ptr::null_mut());

        if !head.is_null() {
            // SAFETY: !is_null 分支说明 *head 指向已有 Slab 头, 合法.
            let old_head = unsafe { SlabRef::new_unchecked(*head) };
            old_head.set_prev(slab);
        }

        *head = slab;
    }
}

/// 缓存统计信息
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    /// 总对象数 (所有 Slab 之和)
    pub total_objects: u32,

    /// 已分配对象数
    pub active_objects: u32,

    /// 总 Slab 数
    pub total_slabs: u32,

    /// 总分配次数
    pub total_allocs: u64,

    /// 总释放次数
    pub total_frees: u64,

    /// 缓存命中次数
    pub cache_hits: u64,

    /// 缓存未命中次数
    pub cache_misses: u64,
}

impl CacheStats {
    /// 计算命中率 (千分比, 0..=1000)
    #[inline]
    pub fn hit_rate(&self) -> u64 {
        if self.total_allocs == 0 {
            0
        } else {
            self.cache_hits * 1000 / self.total_allocs
        }
    }

    /// 计算利用率 (千分比, 0..=1000)
    #[inline]
    pub fn utilization(&self) -> u64 {
        if self.total_objects == 0 {
            0
        } else {
            u64::from(self.active_objects) * 1000 / u64::from(self.total_objects)
        }
    }
}

// ============================================================================
// 全局状态 (通用缓存池)
// ============================================================================

/// 通用缓存数组 (预定义 8 个大小的缓存)
static GENERAL_CACHES: crate::framework::sync::IrqSpinLock<
    [Option<KmemCache>; SLAB_GENERAL_CACHE_NUM],
> = crate::framework::sync::IrqSpinLock::new([const { None }; SLAB_GENERAL_CACHE_NUM]);

/// 系统是否已初始化
static SLAB_INITIALIZED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 初始化 Slab 系统 (创建通用缓存池)
///
/// **必须在内核启动早期调用一次**, 在任何 `slab_alloc/slab_free` 之前。
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn slab_system_init() -> i32 {
    klog_slab!("[SLAB] Initializing Slab allocator...");

    let mut caches = GENERAL_CACHES.lock();
    for (i, &cache_size) in GENERAL_CACHE_SIZES.iter().enumerate() {
        if let Ok(cache) = KmemCache::create("", cache_size) {
            caches[i] = Some(cache);
        }
    }
    drop(caches);
    SLAB_INITIALIZED.store(true, core::sync::atomic::Ordering::Release);

    klog_slab!("[SLAB] System initialized with 8 general caches");
    0
}

/// 根据请求大小查找合适的通用缓存索引
///
/// T2-3: 策略已提取到 `slab_trait::SlabPolicy`, 本函数保留为内部快捷路径
/// (直接调用 `current_slab_policy().find_cache_index()`).
pub(crate) fn find_general_cache_index(size: usize) -> Option<usize> {
    super::slab_trait::current_slab_policy().find_cache_index(size, &GENERAL_CACHE_SIZES)
}

/// 单个 slab 缓存的统计快照
#[derive(Debug, Clone, Copy)]
pub struct SlabCacheSnapshot {
    /// 对象大小 (字节)
    pub object_size: u32,
    /// 总对象数
    pub total_objects: u32,
    /// 已用对象数
    pub active_objects: u32,
    /// slab 页数
    pub total_slabs: u32,
}

/// 遍历所有通用缓存, 返回每个缓存的统计快照.
/// `out` 由调用方提供, 最大写入 `out.len()` 项. 返回实际写入数.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub(crate) fn get_all_cache_snapshots(out: &mut [SlabCacheSnapshot]) -> usize {
    let mut count = 0usize;
    let caches = GENERAL_CACHES.lock();
    for cache_opt in caches.iter() {
        if count >= out.len() {
            break;
        }
        if let Some(cache) = cache_opt {
            let stats = cache.get_stats();
            out[count] = SlabCacheSnapshot {
                object_size: cache.object_size as u32,
                total_objects: stats.total_objects,
                active_objects: stats.active_objects,
                total_slabs: stats.total_slabs,
            };
            count += 1;
        }
    }
    count
}

/// 通用分配接口 (FFI 兼容)
///
/// 自动选择合适大小的缓存进行分配。
///
/// # Arguments
/// * `size` - 请求的字节数
///
/// # Returns
/// 成功: 分配的内存指针
/// 失败: NULL
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn slab_alloc(size: usize) -> *mut u8 {
    if size == 0 || size > SLAB_MAX_OBJECT_SIZE {
        return core::ptr::null_mut();
    }

    if !SLAB_INITIALIZED.load(core::sync::atomic::Ordering::Acquire) {
        return core::ptr::null_mut();
    }

    find_general_cache_index(size).map_or(core::ptr::null_mut(), |idx| {
        let mut caches = GENERAL_CACHES.lock();
        caches[idx].as_mut().map_or(core::ptr::null_mut(), |cache| {
            cache.allocate().unwrap_or(core::ptr::null_mut())
        })
    })
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn slab_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    if !SLAB_INITIALIZED.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    let mut caches = GENERAL_CACHES.lock();
    for i in 0..GENERAL_CACHE_SIZES.len() {
        if let Some(ref mut cache) = caches[i] {
            let slab = cache.find_object_slab(ptr);
            if !slab.is_null() {
                cache.deallocate(ptr);
                return;
            }
        }
    }
}

/// 获取系统级统计信息 (FFI 兼容)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn slab_get_system_stats(
    total_memory: *mut u64,
    used_memory: *mut u64,
    total_caches: *mut u32,
) {
    if total_memory.is_null() || used_memory.is_null() || total_caches.is_null() {
        return;
    }

    let mut total = 0u64;
    let mut used = 0u64;
    let mut count = 0u32;
    let caches = GENERAL_CACHES.lock();

    for i in 0..GENERAL_CACHE_SIZES.len() {
        if let Some(ref cache) = caches[i] {
            count += 1;
            total += u64::from(cache.slab_count) * SLAB_DEFAULT_SIZE as u64;
            used += cache.active_objects() * cache.object_size as u64;
        }
    }

    // SAFETY: 调用方保证指针有效
    unsafe {
        *total_memory = total;
        *used_memory = used;
        *total_caches = count;
    }
}

/// 打印所有缓存的状态 (调试用途)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn slab_dump_all_caches() {
    klog_slab!("[SLAB] === Slab Allocator Status ===");

    let caches = GENERAL_CACHES.lock();
    for i in 0..GENERAL_CACHE_SIZES.len() {
        if let Some(ref cache) = caches[i] {
            klog_slab!(
                "[SLAB] Cache '{}': obj_size={} objs_per_slab={} slabs={} active={}",
                cache.name,
                cache.object_size,
                cache.objects_per_slab,
                cache.slab_count,
                cache.active_objects()
            );
        }
    }
}

/// 打印指定缓存的 slab 链表详情 (调试用途)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn slab_dump_cache(name: *const u8) {
    // SAFETY: 调用方保证 name 为合法 C 字符串
    let name_str = if name.is_null() {
        klog_slab!("[SLAB] Cache name is null");
        return;
    } else {
        unsafe {
            let mut len = 0;
            while *name.add(len) != 0 {
                len += 1;
            }
            core::str::from_utf8_unchecked(core::slice::from_raw_parts(name, len))
        }
    };

    let caches = GENERAL_CACHES.lock();
    for i in 0..GENERAL_CACHE_SIZES.len() {
        if let Some(ref cache) = caches[i] {
            if cache.name == name_str {
                klog_slab!(
                    "[SLAB] Cache '{}': obj_size={} slabs={} active={}",
                    cache.name,
                    cache.object_size,
                    cache.slab_count,
                    cache.active_objects()
                );

                // 遍历 partial 链表显示每个 slab 信息
                let mut current = cache.slabs_partial;
                let mut count = 0;
                while !current.is_null() {
                    // SAFETY: current 由 slabs_partial 链表保证是合法 Slab 头指针
                    let sr = unsafe { SlabRef::new_unchecked(current) };
                    let ptr = sr.as_ptr();
                    klog_slab!(
                        "[SLAB]   partial[{}]: obj_count={} active={}",
                        count,
                        unsafe { (*ptr).obj_count },
                        unsafe { (*ptr).active_count }
                    );
                    current = sr.next();
                    count += 1;
                }
                return;
            }
        }
    }
    klog_slab!("[SLAB] Cache '{}' not found", name_str);
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_creation() {
        let cache = KmemCache::create("test_cache", 64);
        assert!(cache.is_ok());

        let cache = cache.unwrap();
        assert_eq!(cache.object_size, 64);
        assert!(cache.objects_per_slab > 0);
        assert!(cache.slab_count == 0);
    }

    #[test]
    fn test_cache_invalid_size() {
        // 大小为 0
        assert!(KmemCache::create("zero", 0).is_err());

        // 超过最大值
        assert!(KmemCache::create("huge", SLAB_MAX_OBJECT_SIZE + 1).is_err());
    }

    #[test]
    fn test_cache_min_size_enforcement() {
        // 小于最小值应被提升到最小值
        let cache = KmemCache::create("tiny", 8).unwrap();
        assert_eq!(cache.object_size, SLAB_MIN_OBJECT_SIZE); // 应被提升到 16
    }

    #[test]
    fn test_general_cache_sizes() {
        assert_eq!(GENERAL_CACHE_SIZES[0], 16);
        assert_eq!(GENERAL_CACHE_SIZES[3], 128);
        assert_eq!(GENERAL_CACHE_SIZES[7], 2048);
    }

    #[test]
    fn test_find_general_cache_index() {
        assert_eq!(find_general_cache_index(16), Some(0));
        assert_eq!(find_general_cache_index(32), Some(1));
        assert_eq!(find_general_cache_index(64), Some(2));
        assert_eq!(find_general_cache_index(2048), Some(7));
        assert_eq!(find_general_cache_index(3000), None); // 超出范围
    }
}
