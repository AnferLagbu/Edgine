//! 内核堆分配器 (Kmalloc)
//!
//! 为内核提供基于 first-fit 算法的动态内存分配.
//! 特性:
//! - First-fit 空闲链表分配
//! - 块合并, 减少碎片
//! - 通过 VMM/PMM 自动扩展堆
//! - 早期引导阶段堆支持
//! - 内存统计与调试

/// 串口打印宏 (占位)
macro_rules! serial_println {
    ($($arg:tt)*) => {};
}

use super::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr, get_pmm, get_vmm};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
// P1-I-28 修复: kmalloc 自旋锁在中断上下文会死锁 (同 CPU ISR 持锁 + 主线 spin)
// 仿 pmm.rs 模式, acquire_lock 时 disable_interrupts, release_lock 时 restore.
// 导入 privileged/sync/spinlock 的 arch 无关原语, 避免直接 crate::arch!().
use crate::privileged::sync::{IrqSaveFlags, disable_interrupts, restore_interrupts};

/// 堆头校验魔数
const HEAP_MAGIC: u32 = 0xDEADBEEF;

/// 最小块大小 (容纳头部 + 少量数据)
const MIN_BLOCK_SIZE: u64 = 64;

/// 分配对齐 (必须为 2 的幂)
const ALIGNMENT: u64 = 16;

/// 早期分配最大追踪数
const MAX_EARLY_ALLOCS: usize = 128;

/// 早期分配条目
#[derive(Clone, Copy)]
struct EarlyHeapAlloc {
    ptr: *mut u8,
    size: usize,
}

impl EarlyHeapAlloc {
    pub const fn const_default() -> Self {
        Self {
            ptr: core::ptr::null_mut(),
            size: 0,
        }
    }
}

/// 堆头结构 (与 C 布局一致)
///
/// 该结构位于每个分配块起始, 已分配块和空闲块均含此结构.
/// 空闲块中, 它还是双向链表的一部分.
#[repr(C)]
pub struct HeapHeader {
    /// 本块大小 (含头部)
    size: u64,

    /// 本块是否空闲 (1=空闲, 0=已分配)
    free: bool,

    /// 校验用魔数
    magic: u32,

    /// 下一个空闲块指针 (仅在 free 时有效)
    next: *mut Self,

    /// 上一个空闲块指针 (仅在 free 时有效)
    prev: *mut Self,
}

impl HeapHeader {
    pub fn new(size: u64, is_free: bool) -> Self {
        Self {
            size,
            free: is_free,
            magic: HEAP_MAGIC,
            next: core::ptr::null_mut(),
            prev: core::ptr::null_mut(),
        }
    }

    #[expect(
        clippy::ref_as_ptr,
        reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
    )]
    /// 获取本头部之后的数据区指针
    pub fn data_ptr(&self) -> *mut u8 {
        // SAFETY: self 是对 HeapHeader 的有效引用; 加上 size_of::<Self>()
        // 仍在同一分配块内 (头部紧跟数据).
        unsafe { (self as *const Self as *mut u8).add(core::mem::size_of::<Self>()) }
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 由数据指针取回头部
    ///
    /// # Safety
    /// 调用方须保证 `data` 由某次合法分配返回
    pub unsafe fn from_data_ptr(data: *mut u8) -> *mut Self {
        unsafe { data.sub(core::mem::size_of::<Self>()) as *mut Self }
    }
}

// === E4: unsafe 集中 — 原始子模块 ===
//
// 所有针对堆头内部的裸指针解引用都在此处封装.
// 外层 `KernelHeap` 方法只调用 safe 包装, 分配算法本身保持纯 safe Rust.
pub(crate) mod raw {
    use super::{HeapHeader, UnsafeCell, VirtAddr};

    /// 对 `*mut HeapHeader` 的 safe 包装.
    ///
    /// SAFETY 不变式: 指针指向堆内合法 `HeapHeader`, 且堆锁被持有.
    #[derive(Clone, Copy)]
    pub struct HeaderRef(*mut HeapHeader);

    impl HeaderRef {
        /// # Safety
        /// - `ptr` 必须指向堆中合法的 `HeapHeader`
        /// - 使用期间必须持有堆锁
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub unsafe fn new_unchecked(ptr: *mut HeapHeader) -> Self {
            Self(ptr)
        }

        /// 从 allocate 返回的数据指针构造.
        ///
        /// # Safety
        /// - `data` 必须由先前的分配返回
        /// - 必须持有堆锁
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub unsafe fn from_data_ptr(data: *mut u8) -> Self {
            unsafe { Self(HeapHeader::from_data_ptr(data)) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn as_ptr(self) -> *mut HeapHeader {
            self.0
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn size(&self) -> u64 {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe { (*self.0).size }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_size(&self, val: u64) {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe {
                (*self.0).size = val;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn is_free(&self) -> bool {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe { (*self.0).free }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_free(&self, val: bool) {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe {
                (*self.0).free = val;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn magic(&self) -> u32 {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe { (*self.0).magic }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn next(&self) -> *mut HeapHeader {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe { (*self.0).next }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_next(&self, p: *mut HeapHeader) {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe {
                (*self.0).next = p;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn prev(&self) -> *mut HeapHeader {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe { (*self.0).prev }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_prev(&self, p: *mut HeapHeader) {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe {
                (*self.0).prev = p;
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn data_ptr(&self) -> *mut u8 {
            // SAFETY: caller guarantees valid pointer under heap lock
            unsafe { (*self.0).data_ptr() }
        }

        /// 在本地址写入新的 `HeapHeader` 值.
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn write(&self, val: HeapHeader) {
            // SAFETY: 调用方保证指针合法, 且持锁
            unsafe {
                *self.0 = val;
            }
        }

        /// 取得本头部的字节地址.
        #[inline(always)]
        #[expect(
            clippy::ptr_as_ptr,
            reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
        )]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn byte_ptr(self) -> *mut u8 {
            self.0 as *mut u8
        }

        /// 通过字节偏移计算相邻的下一个头部.
        #[inline(always)]
        #[expect(
            clippy::ptr_as_ptr,
            reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
        )]
        #[expect(
            clippy::cast_ptr_alignment,
            reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
        )]
        pub fn adjacent_next(&self, offset: usize) -> Self {
            // SAFETY: 调用方保证偏移仍在堆区范围内
            unsafe { Self::new_unchecked(self.byte_ptr().add(offset) as *mut HeapHeader) }
        }
    }

    /// `free_list_head` 访问的 safe 包装.
    ///
    /// 2026-07-02: 改用 raw pointer + volatile, 防 LTO 字段错位.
    /// 调用方通过 `addr_of!(self.free_list_head)` 传入真实字段地址.
    pub struct FreeListHeadRef {
        ptr: *const UnsafeCell<*mut HeapHeader>,
    }

    impl FreeListHeadRef {
        pub fn new(ptr: *const UnsafeCell<*mut HeapHeader>) -> Self {
            Self { ptr }
        }

        #[expect(
            clippy::ptr_as_ptr,
            reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
        )]
        pub fn get(&self) -> *mut HeapHeader {
            // SAFETY: 持有堆锁; UnsafeCell 是 repr(transparent).
            // 用 read_volatile 强制 LTO 不可 cache/错位.
            unsafe { core::ptr::read_volatile(self.ptr as *const *mut HeapHeader) }
        }

        pub fn set(&self, val: *mut HeapHeader) {
            // SAFETY: 持有堆锁; 用 write_volatile 强制 LTO 不可错位.
            unsafe { core::ptr::write_volatile(self.ptr as *mut *mut HeapHeader, val) }
        }
    }

    /// `heap_end` 访问的 safe 包装.
    ///
    /// 与 `FreeListHeadRef` 同类 LTO 字段错位防御:
    /// 通过 `addr_of!(self.heap_end)` 传入真实字段地址,
    /// 用 `read_volatile`/`write_volatile` 强制 LTO 不可 cache/错位.
    pub struct HeapEndRef {
        ptr: *const UnsafeCell<VirtAddr>,
    }

    impl HeapEndRef {
        pub fn new(ptr: *const UnsafeCell<VirtAddr>) -> Self {
            Self { ptr }
        }

        #[expect(
            clippy::ptr_as_ptr,
            reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
        )]
        pub fn get(&self) -> VirtAddr {
            // SAFETY: 持有堆锁; UnsafeCell 是 repr(transparent).
            // 用 read_volatile 强制 LTO 不可 cache/错位.
            unsafe { core::ptr::read_volatile(self.ptr as *const VirtAddr) }
        }

        pub fn set(&self, val: VirtAddr) {
            // SAFETY: 持有堆锁; 用 write_volatile 强制 LTO 不可错位.
            unsafe { core::ptr::write_volatile(self.ptr as *mut VirtAddr, val) }
        }
    }

    /// 将一段内存清零.
    ///
    /// # Safety
    /// - `ptr` 必须指向 `len` 字节的合法可写区域
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub unsafe fn zero_memory(ptr: *mut u8, len: usize) {
        unsafe {
            core::ptr::write_bytes(ptr, 0, len);
        }
    }

    /// 不重叠内存复制.
    ///
    /// # Safety
    /// - src 必须可读 `len` 字节
    /// - dst 必须可写 `len` 字节
    /// - 两区不得重叠
    #[inline(always)]
    pub unsafe fn copy_nonoverlapping(src: *const u8, dst: *mut u8, len: usize) {
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, len);
        }
    }
}

use raw::{FreeListHeadRef, HeaderRef, HeapEndRef};

/// 内核堆分配器状态
///
/// 2026-07-02: 加 `#[repr(C)]` 防止 LTO 字段重排. 本次会话诊断发现
/// LTO 在 release 模式错位 `free_list_head` 字段写入, 虽有 `addr_of`!
/// 修复, repr(C) 提供额外防御层.
#[repr(C)]
pub struct KernelHeap {
    /// 堆起始地址 (虚拟地址)
    heap_start: VirtAddr,

    /// 堆当前尾地址 (虚拟地址)
    /// SAFETY: 通过 `UnsafeCell` 包装, 允许在 &self 方法 (`expand_heap`) 中更新.
    /// 所有写入都在堆锁 (self.lock) 保护下进行, 保证独占访问.
    heap_end: UnsafeCell<VirtAddr>,

    /// 空闲链表头
    free_list_head: UnsafeCell<*mut HeapHeader>,

    /// 线程安全锁
    lock: AtomicBool,

    /// 堆是否已初始化?
    initialized: AtomicBool,

    /// 正式初始化前的早期分配
    early_allocs: [EarlyHeapAlloc; MAX_EARLY_ALLOCS],

    /// 早期分配计数
    early_count: AtomicUsize,

    /// 早期堆缓冲区 (初始化前用于分配的静态缓冲区)
    early_buffer: [u8; EARLY_BUFFER_SIZE],

    /// 早期缓冲区当前分配位置
    early_pos: AtomicUsize,

    /// 统计
    total_allocated: AtomicU64,
    total_freed: AtomicU64,
    current_usage: AtomicU64,
    peak_usage: AtomicU64,
    alloc_count: AtomicU64,
    free_count: AtomicU64,
    failed_allocs: AtomicU64,
}

// SAFETY: KernelHeap 的 UnsafeCell/裸指针 在内核生命周期内有效;
//         所有访问由 IrqSpinLock + 内部原子锁保护.
unsafe impl Send for KernelHeap {}

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
const EARLY_BUFFER_SIZE: usize = PAGE_SIZE as usize;

impl KernelHeap {
    pub const fn new() -> Self {
        Self {
            heap_start: VirtAddr(0),
            heap_end: UnsafeCell::new(VirtAddr(0)),
            free_list_head: UnsafeCell::new(core::ptr::null_mut()),
            lock: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            early_allocs: [EarlyHeapAlloc::const_default(); MAX_EARLY_ALLOCS],
            early_count: AtomicUsize::new(0),
            early_buffer: [0u8; EARLY_BUFFER_SIZE],
            early_pos: AtomicUsize::new(0),
            total_allocated: AtomicU64::new(0),
            total_freed: AtomicU64::new(0),
            current_usage: AtomicU64::new(0),
            peak_usage: AtomicU64::new(0),
            alloc_count: AtomicU64::new(0),
            free_count: AtomicU64::new(0),
            failed_allocs: AtomicU64::new(0),
        }
    }

    /// 初始化内核堆.
    pub fn init(&mut self, start: VirtAddr, initial_size: u64) {
        self.heap_start = start;
        // SAFETY: init 独占调用, 无并发访问
        HeapEndRef::new(core::ptr::addr_of!(self.heap_end)).set(VirtAddr(start.0 + initial_size));

        // SAFETY: 调用方 (kmem_init) 提供的堆区已映射, 长度 >= sizeof(HeapHeader);
        // 起始地址页对齐, 且在持锁状态下独占访问.
        let header = unsafe { HeaderRef::new_unchecked(start.0 as *mut HeapHeader) };
        header.write(HeapHeader::new(initial_size, true));

        let head = FreeListHeadRef::new(core::ptr::addr_of!(self.free_list_head));
        head.set(header.as_ptr());

        self.initialized.store(true, Ordering::Release);

        serial_println!(
            "[Kmalloc] Initialized: start=0x{:X}, size={} KB",
            start.0,
            initial_size / 1024
        );

        self.process_early_allocations();
    }

    /// 从内核堆分配内存
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn allocate(&self, size: usize) -> Option<*mut u8> {
        if size == 0 {
            return None;
        }

        let aligned_size = align_up(size as u64, ALIGNMENT);
        let total_size = aligned_size + core::mem::size_of::<HeapHeader>() as u64;
        let actual_size = total_size.max(MIN_BLOCK_SIZE);

        let flags = self.acquire_lock();

        let mut result = if self.initialized.load(Ordering::Acquire) {
            self.allocate_first_fit(actual_size)
        } else {
            self.early_allocate(actual_size as usize)
        };

        result = result.inspect(|ptr| {
            self.alloc_count.fetch_add(1, Ordering::Relaxed);
            self.total_allocated
                .fetch_add(actual_size, Ordering::Relaxed);
            let usage = self.current_usage.fetch_add(actual_size, Ordering::Relaxed) + actual_size;

            let mut peak = self.peak_usage.load(Ordering::Relaxed);
            while usage > peak {
                match self.peak_usage.compare_exchange_weak(
                    peak,
                    usage,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(p) => peak = p,
                }
            }
            let _ = ptr;
        });
        if result.is_none() {
            self.failed_allocs.fetch_add(1, Ordering::Relaxed);
        }
        self.release_lock(&flags);
        result
    }

    /// 释放 `k_malloc` 之前分配的内存
    pub fn deallocate(&self, ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }

        let flags = self.acquire_lock();

        if !self.initialized.load(Ordering::Acquire) {
            serial_println!("[Kmalloc] Warning: Cannot free before initialization");
            self.release_lock(&flags);
            return;
        }

        // SAFETY: caller guarantees ptr was returned by a valid allocation
        let header = unsafe { HeaderRef::from_data_ptr(ptr) };

        if header.magic() != HEAP_MAGIC {
            serial_println!(
                "[Kmalloc] Error: Invalid magic (got 0x{:X}, expected 0x{:X})",
                header.magic(),
                HEAP_MAGIC
            );
            self.release_lock(&flags);
            return;
        }

        if header.is_free() {
            serial_println!("[Kmalloc] Warning: Double free detected");
            self.release_lock(&flags);
            return;
        }

        header.set_free(true);

        let freed_size = header.size();
        self.free_count.fetch_add(1, Ordering::Relaxed);
        self.total_freed.fetch_add(freed_size, Ordering::Relaxed);
        self.current_usage.fetch_sub(freed_size, Ordering::Relaxed);

        let effective = self.coalesce(header);
        self.add_to_free_list(effective);

        self.release_lock(&flags);
    }

    /// 重新分配内存块
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn reallocate(&self, ptr: *mut u8, size: usize) -> Option<*mut u8> {
        if size == 0 {
            self.deallocate(ptr);
            return None;
        }

        if ptr.is_null() {
            return self.allocate(size);
        }

        let flags = self.acquire_lock();

        // SAFETY: ptr was returned by kmalloc; magic/size validated below
        let header = unsafe { HeaderRef::from_data_ptr(ptr) };
        if header.magic() != HEAP_MAGIC || header.is_free() {
            self.release_lock(&flags);
            return None;
        }

        let old_data_size = (header.size() - core::mem::size_of::<HeapHeader>() as u64) as usize;
        let new_aligned = align_up(size as u64, ALIGNMENT) as usize;

        if new_aligned <= old_data_size {
            self.release_lock(&flags);
            return Some(ptr);
        }

        // 在持锁状态下分配新块, 防止 ptr 在复制完成前被释放
        let actual_size =
            (new_aligned as u64 + core::mem::size_of::<HeapHeader>() as u64).max(MIN_BLOCK_SIZE);
        let new_ptr = if let Some(p) = self.allocate_first_fit(actual_size) {
            self.alloc_count.fetch_add(1, Ordering::Relaxed);
            self.total_allocated
                .fetch_add(actual_size, Ordering::Relaxed);
            let usage = self.current_usage.fetch_add(actual_size, Ordering::Relaxed) + actual_size;
            let mut peak = self.peak_usage.load(Ordering::Relaxed);
            while usage > peak {
                match self.peak_usage.compare_exchange_weak(
                    peak,
                    usage,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(p) => peak = p,
                }
            }
            p
        } else {
            self.release_lock(&flags);
            return None;
        };

        // SAFETY: ptr 是指向 old_data_size 字节的合法指针; new_ptr 是不重叠的
        // 独立分配; 两区不可能重叠.
        unsafe {
            raw::copy_nonoverlapping(ptr, new_ptr, old_data_size);
        }

        // 在持锁状态下原地释放旧块
        header.set_free(true);
        let freed_size = header.size();
        self.free_count.fetch_add(1, Ordering::Relaxed);
        self.total_freed.fetch_add(freed_size, Ordering::Relaxed);
        self.current_usage.fetch_sub(freed_size, Ordering::Relaxed);
        let effective = self.coalesce(header);
        self.add_to_free_list(effective);

        self.release_lock(&flags);
        Some(new_ptr)
    }

    /// 返回堆使用统计.
    pub fn get_stats(&self) -> HeapStats {
        // SAFETY: 读取 heap_end, 堆锁未持有时可能读到稍旧的值,
        // 但对统计信息而言可接受 (best-effort 一致性)
        let end = HeapEndRef::new(core::ptr::addr_of!(self.heap_end)).get();
        HeapStats {
            heap_start: self.heap_start,
            heap_end: end,
            total_allocated: self.total_allocated.load(Ordering::Relaxed),
            total_freed: self.total_freed.load(Ordering::Relaxed),
            current_usage: self.current_usage.load(Ordering::Relaxed),
            peak_usage: self.peak_usage.load(Ordering::Relaxed),
            alloc_count: self.alloc_count.load(Ordering::Relaxed),
            free_count: self.free_count.load(Ordering::Relaxed),
            failed_allocs: self.failed_allocs.load(Ordering::Relaxed),
        }
    }

    /// 打印堆统计
    pub fn dump_stats(&self) {
        let stats = self.get_stats();
        // B03-02 修复: 改用 crate::klog_info! 走真实 klog 路径, 替代空宏 serial_println!.
        // 调用方: mm/api.rs::kmalloc_dump_stats (FFI 入口) + mm/mechanism.rs re-export.
        crate::klog_info!(
            Memory,
            "=== Kmalloc Statistics === Heap Range: 0x{:X} - 0x{:X} \
             Total Allocated: {} KB, Total Freed: {} KB, Current Usage: {} KB, \
             Peak Usage: {} KB, Alloc Count: {}, Free Count: {}, Failed Allocs: {} \
             =========================",
            stats.heap_start.0,
            stats.heap_end.0,
            stats.total_allocated / 1024,
            stats.total_freed / 1024,
            stats.current_usage / 1024,
            stats.peak_usage / 1024,
            stats.alloc_count,
            stats.free_count,
            stats.failed_allocs
        );
    }

    /// 校验堆完整性 (调试用)
    pub fn validate(&self) -> bool {
        if !self.initialized.load(Ordering::Acquire) {
            return true;
        }

        let flags = self.acquire_lock();

        let mut count = 0usize;
        let head = FreeListHeadRef::new(core::ptr::addr_of!(self.free_list_head));
        let mut current = head.get();

        while !current.is_null() {
            // SAFETY: current is a non-null *mut HeapHeader in the free list
            let cur = unsafe { HeaderRef::new_unchecked(current) };

            if cur.magic() != HEAP_MAGIC {
                serial_println!("[Kmalloc] Validate: Bad magic at {:p}", cur.as_ptr());
                self.release_lock(&flags);
                return false;
            }

            if !cur.is_free() {
                serial_println!("[Kmalloc] Validate: Non-free block in free list");
                self.release_lock(&flags);
                return false;
            }

            let next_ptr = cur.next();
            if !next_ptr.is_null() {
                // SAFETY: 上方 !is_null 检查保证 next_ptr 来自已分配堆块; 堆锁
                // 持有中 (validate 方法开头 lock); HeaderRef::new_unchecked 仅
                // 包装指针, 不访问内存, 后续 .prev() 由 HeaderRef API 保证安全.
                let next = unsafe { HeaderRef::new_unchecked(next_ptr) };
                let next_prev = next.prev();
                if !next_prev.is_null() && next_prev != current {
                    serial_println!("[Kmalloc] Validate: Broken backward link");
                    self.release_lock(&flags);
                    return false;
                }
            }

            count += 1;
            current = next_ptr;

            if count > 10000 {
                serial_println!("[Kmalloc] Validate: Too many nodes (possible cycle)");
                self.release_lock(&flags);
                return false;
            }
        }

        self.release_lock(&flags);
        true
    }

    // ==================== 私有方法 ====================

    /// First-fit 分配算法
    fn allocate_first_fit(&self, size: u64) -> Option<*mut u8> {
        let head = FreeListHeadRef::new(core::ptr::addr_of!(self.free_list_head));
        let mut current = head.get();

        while !current.is_null() {
            // SAFETY: current is a non-null *mut HeapHeader in the free list
            let cur = unsafe { HeaderRef::new_unchecked(current) };
            let block_size = cur.size();

            if block_size >= size {
                if block_size >= size + MIN_BLOCK_SIZE + core::mem::size_of::<HeapHeader>() as u64 {
                    self.split_block(cur, size);
                }

                cur.set_free(false);
                self.remove_from_free_list(cur);

                return Some(cur.data_ptr());
            }

            current = cur.next();
        }

        self.expand_heap(size)
    }

    /// 将一个块拆分为两个
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    fn split_block(&self, header: HeaderRef, size: u64) {
        let original_size = header.size();
        let remaining = original_size - size;

        let second_part = header.adjacent_next(size as usize);
        second_part.write(HeapHeader::new(remaining, true));

        header.set_size(size);

        let header_next = header.next();
        if !header_next.is_null() {
            // SAFETY: header_next 由上方 header.next() 获得, header 是合法堆块
            // 头 (由 split_block 调用者保证), header_next 非空说明其指向已分配
            // 堆块, HeaderRef::new_unchecked 仅包装指针.
            let hn = unsafe { HeaderRef::new_unchecked(header_next) };
            hn.set_prev(second_part.as_ptr());
        }
        second_part.set_next(header_next);
        second_part.set_prev(header.as_ptr());
        header.set_next(second_part.as_ptr());
    }

    /// 合并相邻的空闲块
    fn coalesce(&self, header: HeaderRef) -> HeaderRef {
        self.coalesce_forward(header);
        self.coalesce_backward(header)
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn coalesce_forward(&self, header: HeaderRef) {
        let next_addr = header.adjacent_next(header.size() as usize);
        // SAFETY: 读取 heap_end, 持有堆锁, 独占访问
        let heap_end = HeapEndRef::new(core::ptr::addr_of!(self.heap_end)).get();
        let heap_end_ptr = heap_end.0 as *mut u8;

        if next_addr.byte_ptr() < heap_end_ptr {
            if next_addr.magic() == HEAP_MAGIC && next_addr.is_free() {
                self.remove_from_free_list(next_addr);
                header.set_size(header.size() + next_addr.size());
            }
        }
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn coalesce_backward(&self, header: HeaderRef) -> HeaderRef {
        let head = FreeListHeadRef::new(core::ptr::addr_of!(self.free_list_head));
        let mut current = head.get();

        while !current.is_null() {
            // SAFETY: current is a valid node in the free list
            let candidate = unsafe { HeaderRef::new_unchecked(current) };
            let candidate_end = candidate.byte_ptr() as usize + candidate.size() as usize;

            if candidate_end == header.byte_ptr() as usize {
                self.remove_from_free_list(candidate);
                candidate.set_size(candidate.size() + header.size());
                candidate.set_free(true);
                return candidate;
            }

            current = candidate.next();
        }

        header
    }

    /// 将块加入空闲链表
    fn add_to_free_list(&self, header: HeaderRef) {
        let head = FreeListHeadRef::new(core::ptr::addr_of!(self.free_list_head));
        let head_ptr = head.get();

        if head_ptr.is_null() {
            header.set_next(core::ptr::null_mut());
        } else {
            // SAFETY: head_ptr 由 head.get() 返回, !is_null 分支说明 free_list_head
            // 指向当前 free list 的头部 (合法堆块, 由 add_to_free_list 调用者保证).
            let old_head = unsafe { HeaderRef::new_unchecked(head_ptr) };
            header.set_next(old_head.as_ptr());
            old_head.set_prev(header.as_ptr());
        }
        header.set_prev(core::ptr::null_mut());
        head.set(header.as_ptr());
    }

    /// 从空闲链表中移除块
    fn remove_from_free_list(&self, header: HeaderRef) {
        let prev = header.prev();
        let next = header.next();
        let head = FreeListHeadRef::new(core::ptr::addr_of!(self.free_list_head));

        if prev.is_null() {
            head.set(next);
        } else {
            // SAFETY: prev 由 header.prev() 返回, !is_null 分支说明 prev 指向
            // free list 上的合法堆块 (remove_from_free_list 调用方持有堆锁).
            let p = unsafe { HeaderRef::new_unchecked(prev) };
            p.set_next(next);
        }

        if !next.is_null() {
            // SAFETY: 同上, next 来自 header.next(), 非空即合法堆块指针.
            let n = unsafe { HeaderRef::new_unchecked(next) };
            n.set_prev(prev);
        }

        header.set_next(core::ptr::null_mut());
        header.set_prev(core::ptr::null_mut());
    }

    /// 通过 VMM/PMM 申请更多页来扩展堆
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn expand_heap(&self, size: u64) -> Option<*mut u8> {
        let pages_needed = size.div_ceil(PAGE_SIZE);
        let expand_by = pages_needed * PAGE_SIZE;

        let vmm = get_vmm();
        let pmm = get_pmm();

        let phys = pmm.alloc_pages(pages_needed as usize)?;

        // SAFETY: 读取 heap_end, 当前持有堆锁, 独占访问
        let current_end = HeapEndRef::new(core::ptr::addr_of!(self.heap_end)).get();
        let new_start = current_end;
        let new_end = VirtAddr(current_end.0 + expand_by);

        for i in 0..pages_needed {
            let page_phys = PhysAddr(phys.as_u64() + i * PAGE_SIZE);
            let page_virt = VirtAddr(new_start.0 + i * PAGE_SIZE);

            if vmm
                .map_page(
                    page_virt,
                    page_phys,
                    PageFlags::PRESENT | PageFlags::WRITABLE,
                )
                .is_err()
            {
                for j in 0..i {
                    let unmap_virt = VirtAddr(new_start.0 + j * PAGE_SIZE);
                    vmm.unmap_page(unmap_virt);
                }
                pmm.free_pages(phys, pages_needed as usize);
                return None;
            }
        }

        // SAFETY: 上面 vmm.map_page 已将新页映射为可写;
        // new_start 是已映射区的起始; 持锁状态下独占访问.
        let new_block = unsafe { HeaderRef::new_unchecked(new_start.0 as *mut HeapHeader) };
        new_block.write(HeapHeader::new(expand_by, true));

        // SAFETY: 持有堆锁, 独占访问 heap_end; 更新为扩展后的尾地址
        HeapEndRef::new(core::ptr::addr_of!(self.heap_end)).set(new_end);

        self.add_to_free_list(new_block);

        self.allocate_first_fit(size)
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::ptr_cast_constness,
        reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
    )]
    /// 早期分配 (堆初始化前)
    fn early_allocate(&self, size: usize) -> Option<*mut u8> {
        let current = self.early_pos.fetch_add(size, Ordering::Relaxed);

        if current + size > self.early_buffer.len() {
            serial_println!("[Kmalloc] Error: Early allocation out of space!");
            self.early_pos.fetch_sub(size, Ordering::Relaxed);
            return None;
        }

        // SAFETY: current is checked above to be within early_buffer bounds
        let ptr = unsafe { self.early_buffer.as_ptr().add(current) as *mut u8 };

        let idx = self.early_count.fetch_add(1, Ordering::Relaxed);
        if idx < MAX_EARLY_ALLOCS {
            // SAFETY: idx < MAX_EARLY_ALLOCS guards the bounds
            unsafe {
                let alloc_ptr = self.early_allocs.as_ptr().add(idx) as *mut EarlyHeapAlloc;
                raw::zero_memory(alloc_ptr as *mut u8, core::mem::size_of::<EarlyHeapAlloc>());
                (*alloc_ptr).ptr = ptr;
                (*alloc_ptr).size = size;
            }
        }

        Some(ptr)
    }

    /// 在正式初始化后处理早期分配
    fn process_early_allocations(&self) {
        let count = self.early_count.load(Ordering::Acquire);

        for i in 0..count.min(MAX_EARLY_ALLOCS) {
            let alloc = self.early_allocs[i];

            if let Some(new_ptr) = self.allocate_first_fit(alloc.size as u64) {
                // SAFETY: early alloc ptr is valid, new_ptr is a distinct allocation
                unsafe {
                    raw::copy_nonoverlapping(alloc.ptr, new_ptr, alloc.size);
                }
            }
        }
    }

    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn acquire_lock(&self) -> IrqSaveFlags {
        let flags = disable_interrupts();
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        flags
    }

    #[inline(always)]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    fn release_lock(&self, flags: &IrqSaveFlags) {
        self.lock.store(false, Ordering::Release);
        restore_interrupts(flags);
    }
}

/// 堆统计结构
#[derive(Debug, Clone, Copy)]
pub struct HeapStats {
    pub heap_start: VirtAddr,
    pub heap_end: VirtAddr,
    pub total_allocated: u64,
    pub total_freed: u64,
    pub current_usage: u64,
    pub peak_usage: u64,
    pub alloc_count: u64,
    pub free_count: u64,
    pub failed_allocs: u64,
}

// 全局 Kmalloc 实例
static GLOBAL_KMALLOC: crate::privileged::sync::IrqSpinLock<KernelHeap> =
    crate::privileged::sync::IrqSpinLock::new(KernelHeap::new());

/// 获取全局 Kmalloc 实例的锁 guard
pub fn get_kmalloc() -> crate::privileged::sync::IrqSpinLockGuard<'static, KernelHeap> {
    GLOBAL_KMALLOC.lock()
}

/// 获取全局 Kmalloc 实例的可变 guard (用于初始化)
pub fn get_kmalloc_mut() -> crate::privileged::sync::IrqSpinLockGuard<'static, KernelHeap> {
    GLOBAL_KMALLOC.lock()
}

// ==================== 辅助函数 ====================

/// 将值向上对齐到指定对齐 (必须为 2 的幂)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub const fn align_up(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}

/// 将值向下对齐到指定对齐 (必须为 2 的幂)
#[inline(always)]
pub const fn align_down(value: u64, alignment: u64) -> u64 {
    value & !(alignment - 1)
}
