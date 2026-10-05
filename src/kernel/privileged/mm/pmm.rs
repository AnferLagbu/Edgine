//! 物理内存管理器 (PMM) — Buddy 分配器
//!
//! 使用 buddy 分配器管理物理内存页, 提供 O(1) 空闲链表操作
//! 以及 O(log n) 释放合并.
//!
//! # 设计
//! - 阶数 0–9 (4 KB – 2 MB 连续块).
//! - Buddy 元数据建立前使用早期 (线性) 分配器.
//! - 保留位图用于 reserved 页跟踪和统计.
//! - 双向链表的索引式空闲链表 (prev/next 存独立 FREE_LINKS 数组,
//!   16 字节/页, 哨兵 = u64::MAX; 链表关系与物理页内容解耦, host 可测).
//! - Buddy 合并使用按页的阶数元数据, 实现 O(1) 伙伴检查.
//!
//! # 安全
//! 所有修改都在内部 `AtomicBool` 自旋锁下进行.

macro_rules! klog_pmm {
    ($($arg:tt)*) => {
        $crate::klog_ffi!(klog_ffi_info, $($arg)*)
    };
}

use super::{MemoryInfo, NonNull, PAGE_SIZE, PageSize, PhysAddr, phys_to_virt};
use crate::privileged::sync::{IrqSaveFlags, disable_interrupts, restore_interrupts};
use core::cell::{Cell, UnsafeCell};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::privileged::sync::IrqSpinLock;

use crate::privileged::sync::OnceLock;
const MAX_EARLY_ALLOCS: usize = 256;

/// 最大 buddy 阶数: 2^9 × 4 KB = 2 MB
///
/// `pub(crate)`: `Frame` 句柄析构需按同一界判定整块归还 (见 `frame.rs`),
/// 避免该界在 privileged 内出现第二份硬编码.
pub(crate) const MAX_BUDDY_ORDER: u8 = 9;
/// `buddy_meta` 中的哨兵值: 页面已分配 / 不是空闲链表头
const BUDDY_ALLOCATED: u8 = 0xFF;
/// 索引式空闲链表哨兵值: 表示链表头/尾 (无前驱或后继).
/// 不能用 0 — pfn 0 是合法物理页号.
const SENTINEL: u64 = u64::MAX;

/// 分配失败注入的"未武装"标记 (见 `PhysicalMemoryManager::alloc_fail_countdown`).
#[cfg(any(test, feature = "kernel_test"))]
const ALLOC_FAIL_DISARMED: u32 = u32::MAX;

/// 物理 RAM 基地址
/// `x86_64`: 0 (multiboot 给出的物理内存从 0 开始)
/// aarch64: 0x40000000 (QEMU virt 机器 RAM 基址)
#[cfg(target_arch = "x86_64")]
const RAM_BASE: u64 = 0;
#[cfg(target_arch = "aarch64")]
const RAM_BASE: u64 = 0x40000000;

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
fn phys_to_page(phys: u64) -> u64 {
    (phys - RAM_BASE) / PAGE_SIZE
}

#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
fn page_to_phys(page: u64) -> u64 {
    RAM_BASE + page * PAGE_SIZE
}

/// 将页数向上取整到 2 的幂 → 对应 buddy 阶数
///
/// T2-2: 策略已提取到 `pmm_trait::PmmPolicy`, 本函数保留为内部快捷路径
/// (直接调用 `current_pmm_policy().count_to_order()`).
#[inline]
fn count_to_order(count: usize) -> u8 {
    super::pmm_trait::current_pmm_policy().count_to_order(count, MAX_BUDDY_ORDER)
}

#[derive(Clone, Copy)]
struct EarlyAlloc {
    addr: u64,
    size: u64,
}

impl EarlyAlloc {
    pub const fn const_default() -> Self {
        Self { addr: 0, size: 0 }
    }
}

// ---- 索引式双向空闲链表节点, 存于独立 FREE_LINKS 数组 (16 字节/项) ----
// H-01 (2026-09-06): 由侵入式 (FreeNode 存物理页内) 改为索引式,
// prev/next 存相邻空闲块头 pfn, 链表关系与物理页内容解耦, host 可测.
#[repr(C)]
pub(crate) struct FreeIndex {
    prev: u64,
    next: u64,
}

// === E3: unsafe 集中化 — 裸指针子模块 ===
//
// buddy 分配器内部涉及的所有裸指针解引用都
// 封装在这里.  外层 `PhysicalMemoryManager` 方法只调用
// safe 包装器, 使 buddy 分配算法本身保持 safe Rust.
pub(crate) mod raw {
    /// 清零一段内存.
    ///
    /// # Safety
    /// - `ptr` 必须指向 `len` 字节的合法可写区
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

    /// 用指定字节值填充一段内存.
    ///
    /// # Safety
    /// - `ptr` 必须指向 `len` 字节的合法可写区
    #[inline(always)]
    pub unsafe fn fill_memory(ptr: *mut u8, val: u8, len: usize) {
        unsafe {
            core::ptr::write_bytes(ptr, val, len);
        }
    }
}

// ============================================================================
// H-04 (2026-09-09): MetaStore — 内存元数据载体统一访问接口
//
// buddy 分配器对 3 类外部内存载体 (bitmap / buddy_meta / FREE_LINKS) 与
// 链表头 (buddy_heads) 的统一抽象访问. 生产实现 `RawMetaStore` 基于
// `phys + KERNEL_BASE` 裸指针 (行为与 H-04 改造前一致); host 测试实现
// `VecMetaStore` 基于 `Vec<u8>`/`Vec<u64>` 堆载体 (构造注入), 使
// init_bitmap 与全部 buddy 算法仅一份代码, 无测试/生产分叉
// (B08-12 路线 C 核心).
//
// `buddy_heads` 是 `PhysicalMemoryManager` 结构体内部字段: 生产 `RawMetaStore`
// 构造时接收该字段指针 (init_bitmap 单线程启动期创建, self 不移动,
// buddy 就绪后仅在 PMM 锁下访问, 故指针稳定); host 测试 `VecMetaStore`
// 以 `Vec<u64>` 惰性模拟 (初始 SENTINEL, 与生产初始态一致).
// ============================================================================

/// 内存元数据载体接口 — buddy 分配器对 bitmap / buddy_meta / FREE_LINKS 的统一访问
///
/// # 实现
/// - [`RawMetaStore`]: 生产实现, 基于 `phys + KERNEL_BASE` 裸指针 (行为不变)
/// - [`VecMetaStore`]: host 测试实现, 基于 `Vec<u8>` 堆载体 (构造注入)
///
/// # 安全
/// 所有读写方法都要求调用方持有 PMM 锁 (buddy 算法持锁执行);
/// `setup_*` 仅在 init_bitmap 单线程启动期调用.
///
/// # Send/Sync
/// 本 trait 不声明 `Send + Sync` 上界: 载体实现 (含 RefCell 的
/// `VecMetaStore`) 的并发安全性由 `PhysicalMemoryManager` 的
/// `unsafe impl Sync` (PMM 锁互斥保证) 承担.
pub trait MetaStore {
    /// 预置 bitmap 载体: 清零 `bytes` 字节并记录区段 (init_bitmap 调用)
    fn setup_bitmap(&mut self, phys: u64, bytes: usize);
    /// 预置 buddy_meta 载体: 填充 `BUDDY_ALLOCATED` (0xFF) 并记录区段
    fn setup_meta(&mut self, phys: u64, bytes: usize);
    /// 预置 FREE_LINKS 载体: 填充 0xFF (=SENTINEL) 并记录区段
    fn setup_links(&mut self, phys: u64, bytes: usize);
    /// 预置帧持有计数载体: 清零 `bytes` 字节并记录区段 (init_bitmap 调用)
    fn setup_counts(&mut self, phys: u64, bytes: usize);

    /// 置位 bitmap 第 `bit` 位 (载体未就绪/越界时静默跳过)
    fn bitmap_set(&self, bit: usize);
    /// 清位 bitmap 第 `bit` 位 (载体未就绪/越界时静默跳过)
    fn bitmap_clear(&self, bit: usize);
    /// 测试 bitmap 第 `bit` 位 (越界返回 false)
    fn bitmap_test(&self, bit: usize) -> bool;
    /// 统计 bitmap 空闲位数 (清零位个数)
    fn bitmap_count_free(&self) -> u64;

    /// 读 buddy_meta[idx] (0xFF=已分配, 0..=MAX_BUDDY_ORDER=空闲块头阶数)
    fn meta_read(&self, idx: usize) -> u8;
    /// 写 buddy_meta[idx]
    fn meta_write(&self, idx: usize, val: u8);

    /// 读帧持有计数 (按 pfn 索引; 0 = 未计数帧, 见 `frame_inc`/`frame_dec` 契约)
    fn counts_read(&self, idx: usize) -> u8;
    /// 写帧持有计数 (载体未就绪/越界时静默跳过)
    fn counts_write(&self, idx: usize, val: u8);

    /// 读 FREE_LINKS[idx].prev (存前驱块头 pfn, SENTINEL=无前驱)
    fn links_read_prev(&self, idx: usize) -> u64;
    /// 读 FREE_LINKS[idx].next (存后继块头 pfn, SENTINEL=无后继)
    fn links_read_next(&self, idx: usize) -> u64;
    /// 写 FREE_LINKS[idx].prev
    fn links_set_prev(&self, idx: usize, val: u64);
    /// 写 FREE_LINKS[idx].next
    fn links_set_next(&self, idx: usize, val: u64);

    /// 读第 `order` 阶空闲链表头 (存块头 pfn, SENTINEL=空链表)
    fn heads_get(&self, order: u8) -> u64;
    /// 写第 `order` 阶空闲链表头 (存块头 pfn)
    fn heads_set(&self, order: u8, pfn: u64);
}

/// 生产 MetaStore 实现 — 基于 `phys + KERNEL_BASE` 裸指针
///
/// 与 H-04 改造前的行为完全一致: bitmap 走 AtomicU32 位操作,
/// buddy_meta / FREE_LINKS 走裸指针读写. 仅在 init_bitmap 中构造
/// (buddy 就绪后指针固定, 之后仅在 PMM 锁下访问).
pub struct RawMetaStore {
    /// bitmap 段虚拟地址 (u32 word 数组, 原子位操作)
    bitmap: Option<NonNull<u32>>,
    /// bitmap 长度 (u32 word 数), 越界位操作静默跳过
    bitmap_words: usize,
    /// buddy_meta 段虚拟地址 (按页 1 字节)
    meta: Option<NonNull<u8>>,
    /// 帧持有计数段虚拟地址 (按 pfn 1 字节)
    counts: Option<NonNull<u8>>,
    /// FREE_LINKS 段虚拟地址 (FreeIndex = prev/next 各 u64)
    links: Option<NonNull<FreeIndex>>,
    /// 宿主 `PhysicalMemoryManager.buddy_heads` 字段地址
    /// (init_bitmap 单线程创建时传入, self 不移动, 锁保护下访问)
    heads: *mut [u64; MAX_BUDDY_ORDER as usize + 1],
}

impl RawMetaStore {
    pub const fn new(heads: *mut [u64; MAX_BUDDY_ORDER as usize + 1]) -> Self {
        Self {
            bitmap: None,
            bitmap_words: 0,
            meta: None,
            counts: None,
            links: None,
            heads,
        }
    }
}

impl MetaStore for RawMetaStore {
    fn setup_bitmap(&mut self, phys: u64, bytes: usize) {
        let virt = phys_to_virt(phys) as *mut u8;
        // SAFETY: virt 由 init_bitmap 计算 (phys + KERNEL_BASE 内核映射区),
        // bytes 为该区段长度, 区段已按页对齐且未他用.
        unsafe { raw::zero_memory(virt, bytes) };
        self.bitmap = NonNull::new(virt.cast::<u32>());
        self.bitmap_words = bytes / 4;
    }

    fn setup_meta(&mut self, phys: u64, bytes: usize) {
        let virt = phys_to_virt(phys) as *mut u8;
        // SAFETY: 同上; 0xFF 预置 = BUDDY_ALLOCATED (与 H-04 之前行为一致)
        unsafe { raw::fill_memory(virt, BUDDY_ALLOCATED, bytes) };
        self.meta = NonNull::new(virt);
    }

    fn setup_links(&mut self, phys: u64, bytes: usize) {
        let virt = phys_to_virt(phys) as *mut u8;
        // SAFETY: 同上; 0xFF 字节填充 → 每个 u64 字段 = u64::MAX = SENTINEL (链表空态)
        unsafe { raw::fill_memory(virt, 0xFF, bytes) };
        self.links = NonNull::new(virt.cast::<FreeIndex>());
    }

    fn setup_counts(&mut self, phys: u64, bytes: usize) {
        let virt = phys_to_virt(phys) as *mut u8;
        // SAFETY: virt 由 init_bitmap 计算 (phys + KERNEL_BASE 内核映射区),
        // bytes 为该区段长度, 区段已按页对齐且未他用; 清零 = 全部帧"未计数"
        unsafe { raw::zero_memory(virt, bytes) };
        self.counts = NonNull::new(virt);
    }

    fn bitmap_set(&self, bit: usize) {
        let Some(bmp) = self.bitmap else { return };
        let word = bit / 32;
        if word < self.bitmap_words {
            // SAFETY: word < bitmap_words 保证访问有效; bmp 在 setup_bitmap 中建立
            unsafe {
                let p = bmp.as_ptr().add(word) as *const AtomicU32;
                (*p).fetch_or(1u32 << (bit % 32), Ordering::Relaxed);
            }
        }
    }

    fn bitmap_clear(&self, bit: usize) {
        let Some(bmp) = self.bitmap else { return };
        let word = bit / 32;
        if word < self.bitmap_words {
            // SAFETY: word < bitmap_words 保证访问有效; bmp 在 setup_bitmap 中建立
            unsafe {
                let p = bmp.as_ptr().add(word) as *const AtomicU32;
                (*p).fetch_and(!(1u32 << (bit % 32)), Ordering::Relaxed);
            }
        }
    }

    fn bitmap_test(&self, bit: usize) -> bool {
        let Some(bmp) = self.bitmap else { return false };
        let word = bit / 32;
        if word < self.bitmap_words {
            // SAFETY: word < bitmap_words 保证访问有效; bmp 在 setup_bitmap 中建立
            unsafe {
                let p = bmp.as_ptr().add(word) as *const AtomicU32;
                (*p).load(Ordering::Relaxed) & (1u32 << (bit % 32)) != 0
            }
        } else {
            false
        }
    }

    fn bitmap_count_free(&self) -> u64 {
        let Some(bmp) = self.bitmap else { return 0 };
        let mut free: u64 = 0;
        for w in 0..self.bitmap_words {
            // SAFETY: w < bitmap_words 保证访问有效; bmp 在 setup_bitmap 中建立
            unsafe {
                let p = bmp.as_ptr().add(w) as *const AtomicU32;
                free += u64::from((!(*p).load(Ordering::Relaxed)).count_ones());
            }
        }
        free
    }

    fn meta_read(&self, idx: usize) -> u8 {
        let Some(meta) = self.meta else {
            // 载体未就绪: 返回已分配态 (保守, 阻止误合并)
            return BUDDY_ALLOCATED;
        };
        // SAFETY: 调用方保证 idx < total_pages; meta 在 setup_meta 中建立 (buddy 就绪后)
        unsafe { *meta.as_ptr().add(idx) }
    }

    fn meta_write(&self, idx: usize, val: u8) {
        let Some(meta) = self.meta else { return };
        // SAFETY: 调用方保证 idx < total_pages; meta 在 setup_meta 中建立 (buddy 就绪后)
        unsafe {
            *meta.as_ptr().add(idx) = val;
        }
    }

    fn counts_read(&self, idx: usize) -> u8 {
        let Some(counts) = self.counts else {
            // 载体未就绪 (bitmap 就绪前): 视为未计数 (0), 归还走传统路径
            return 0;
        };
        // SAFETY: 调用方保证 idx < total_pages; counts 在 setup_counts 中建立
        unsafe { *counts.as_ptr().add(idx) }
    }

    fn counts_write(&self, idx: usize, val: u8) {
        let Some(counts) = self.counts else { return };
        // SAFETY: 调用方保证 idx < total_pages; counts 在 setup_counts 中建立
        unsafe {
            *counts.as_ptr().add(idx) = val;
        }
    }

    fn links_read_prev(&self, idx: usize) -> u64 {
        let Some(links) = self.links else {
            return SENTINEL;
        };
        // SAFETY: 调用方保证 idx < total_pages; links 在 setup_links 中建立 (buddy 就绪后)
        unsafe { (*links.as_ptr().add(idx)).prev }
    }

    fn links_read_next(&self, idx: usize) -> u64 {
        let Some(links) = self.links else {
            return SENTINEL;
        };
        // SAFETY: 调用方保证 idx < total_pages; links 在 setup_links 中建立 (buddy 就绪后)
        unsafe { (*links.as_ptr().add(idx)).next }
    }

    fn links_set_prev(&self, idx: usize, val: u64) {
        let Some(links) = self.links else { return };
        // SAFETY: 调用方保证 idx < total_pages; links 在 setup_links 中建立 (buddy 就绪后)
        unsafe {
            (*links.as_ptr().add(idx)).prev = val;
        }
    }

    fn links_set_next(&self, idx: usize, val: u64) {
        let Some(links) = self.links else { return };
        // SAFETY: 调用方保证 idx < total_pages; links 在 setup_links 中建立 (buddy 就绪后)
        unsafe {
            (*links.as_ptr().add(idx)).next = val;
        }
    }

    fn heads_get(&self, order: u8) -> u64 {
        // SAFETY: heads 指针在构造时指向宿主 buddy_heads 字段 (init_bitmap
        // 单线程创建, self 不移动); order <= MAX_BUDDY_ORDER; 调用方持锁.
        unsafe { (*self.heads)[order as usize] }
    }

    fn heads_set(&self, order: u8, pfn: u64) {
        // SAFETY: 同上, 锁保护下写宿主 buddy_heads 字段.
        unsafe {
            (*self.heads)[order as usize] = pfn;
        }
    }
}

/// host 测试 MetaStore 实现 — 基于 `Vec<u8>` 堆载体 (构造注入)
///
/// bitmap / buddy_meta / FREE_LINKS 分别用独立 Vec 模拟, `setup_*` 时创建
/// (与生产载体等长), 读写为纯内存操作, 单线程测试语义.
/// 仅 `host-test` / `test` 配置下编译, 生产二进制不含 (避免死代码).
#[cfg(any(test, feature = "host-test"))]
use core::cell::RefCell;

#[cfg(any(test, feature = "host-test"))]
/// host 测试用 MetaStore: 以堆 `Vec<u8>` 模拟 bitmap / buddy 元数据 / 空闲链等载体.
pub struct VecMetaStore {
    /// bitmap 载体 (字节数组模拟 u32 word, 小端)
    bitmap: RefCell<Option<Vec<u8>>>,
    /// bitmap 长度 (u32 word 数), 越界位操作静默跳过
    bitmap_words: usize,
    /// buddy_meta 载体 (按页 1 字节)
    meta: RefCell<Option<Vec<u8>>>,
    /// 帧持有计数载体 (按 pfn 1 字节)
    counts: RefCell<Option<Vec<u8>>>,
    /// FREE_LINKS 载体 (每项 16 字节 = prev/next 各 u64)
    links: RefCell<Option<Vec<u8>>>,
    /// buddy_heads 载体 (每阶一个 u64 pfn; 惰性扩容, 未写阶 = SENTINEL)
    heads: RefCell<Vec<u64>>,
}

#[cfg(any(test, feature = "host-test"))]
impl VecMetaStore {
    pub const fn new() -> Self {
        Self {
            bitmap: RefCell::new(None),
            bitmap_words: 0,
            meta: RefCell::new(None),
            counts: RefCell::new(None),
            links: RefCell::new(None),
            heads: RefCell::new(Vec::new()),
        }
    }
}

#[cfg(any(test, feature = "host-test"))]
impl MetaStore for VecMetaStore {
    fn setup_bitmap(&mut self, _phys: u64, bytes: usize) {
        *self.bitmap.borrow_mut() = Some(vec![0u8; bytes]);
        self.bitmap_words = bytes / 4;
    }

    fn setup_meta(&mut self, _phys: u64, bytes: usize) {
        *self.meta.borrow_mut() = Some(vec![BUDDY_ALLOCATED; bytes]);
    }

    fn setup_links(&mut self, _phys: u64, bytes: usize) {
        // 0xFF 字节填充 → 每 u64 = u64::MAX = SENTINEL (链表空态, 与生产一致)
        *self.links.borrow_mut() = Some(vec![0xFFu8; bytes]);
    }

    fn bitmap_set(&self, bit: usize) {
        let mut bmp = self.bitmap.borrow_mut();
        let Some(b) = bmp.as_mut() else { return };
        let word = bit / 32;
        if word < self.bitmap_words {
            let byte = word * 4 + (bit % 32) / 8;
            b[byte] |= 1u8 << ((bit % 32) % 8);
        }
    }

    fn bitmap_clear(&self, bit: usize) {
        let mut bmp = self.bitmap.borrow_mut();
        let Some(b) = bmp.as_mut() else { return };
        let word = bit / 32;
        if word < self.bitmap_words {
            let byte = word * 4 + (bit % 32) / 8;
            b[byte] &= !(1u8 << ((bit % 32) % 8));
        }
    }

    fn bitmap_test(&self, bit: usize) -> bool {
        let bmp = self.bitmap.borrow();
        let Some(b) = bmp.as_ref() else { return false };
        let word = bit / 32;
        if word < self.bitmap_words {
            let byte = word * 4 + (bit % 32) / 8;
            b[byte] & (1u8 << ((bit % 32) % 8)) != 0
        } else {
            false
        }
    }

    fn bitmap_count_free(&self) -> u64 {
        let bmp = self.bitmap.borrow();
        let Some(b) = bmp.as_ref() else { return 0 };
        b.iter().map(|&x| u64::from((!x).count_ones())).sum()
    }

    fn meta_read(&self, idx: usize) -> u8 {
        let meta = self.meta.borrow();
        meta.as_ref()
            .and_then(|m| m.get(idx))
            .copied()
            .unwrap_or(BUDDY_ALLOCATED)
    }

    fn meta_write(&self, idx: usize, val: u8) {
        let mut meta = self.meta.borrow_mut();
        if let Some(m) = meta.as_mut() {
            if let Some(slot) = m.get_mut(idx) {
                *slot = val;
            }
        }
    }

    fn setup_counts(&mut self, _phys: u64, bytes: usize) {
        *self.counts.borrow_mut() = Some(vec![0u8; bytes]);
    }

    fn counts_read(&self, idx: usize) -> u8 {
        let counts = self.counts.borrow();
        counts
            .as_ref()
            .and_then(|c| c.get(idx))
            .copied()
            .unwrap_or(0)
    }

    fn counts_write(&self, idx: usize, val: u8) {
        let mut counts = self.counts.borrow_mut();
        if let Some(c) = counts.as_mut() {
            if let Some(slot) = c.get_mut(idx) {
                *slot = val;
            }
        }
    }

    fn links_read_prev(&self, idx: usize) -> u64 {
        let links = self.links.borrow();
        let Some(l) = links.as_ref() else {
            return SENTINEL;
        };
        let off = idx * 16;
        if off + 8 <= l.len() {
            u64::from_le_bytes(l[off..off + 8].try_into().expect("links prev"))
        } else {
            SENTINEL
        }
    }

    fn links_read_next(&self, idx: usize) -> u64 {
        let links = self.links.borrow();
        let Some(l) = links.as_ref() else {
            return SENTINEL;
        };
        let off = idx * 16 + 8;
        if off + 8 <= l.len() {
            u64::from_le_bytes(l[off..off + 8].try_into().expect("links next"))
        } else {
            SENTINEL
        }
    }

    fn links_set_prev(&self, idx: usize, val: u64) {
        let mut links = self.links.borrow_mut();
        let Some(l) = links.as_mut() else { return };
        let off = idx * 16;
        if off + 8 <= l.len() {
            l[off..off + 8].copy_from_slice(&val.to_le_bytes());
        }
    }

    fn links_set_next(&self, idx: usize, val: u64) {
        let mut links = self.links.borrow_mut();
        let Some(l) = links.as_mut() else { return };
        let off = idx * 16 + 8;
        if off + 8 <= l.len() {
            l[off..off + 8].copy_from_slice(&val.to_le_bytes());
        }
    }

    fn heads_get(&self, order: u8) -> u64 {
        self.heads
            .borrow()
            .get(order as usize)
            .copied()
            .unwrap_or(SENTINEL)
    }

    fn heads_set(&self, order: u8, pfn: u64) {
        let mut h = self.heads.borrow_mut();
        let idx = order as usize;
        if h.len() <= idx {
            // 惰性扩容: 未写过的阶保持 SENTINEL (与生产构造初始态一致)
            h.resize(idx + 1, SENTINEL);
        }
        h[idx] = pfn;
    }
}

// H-04 优化项方案 B (2026-09-09): 编译期选择载体, 消除 `Box<dyn MetaStore>` 的
// vtable 间接调用 (buddy 热路径 alloc/free 每次 ~10-20 次 MetaStore 调用) 与
// 载体堆分配. 物理内存模型在镜像编译时定死, 从不运行时切换 (与 Linux
// CONFIG_FLATMEM/SPARSEMEM 编译期选择一致); PMM 为全局单例, "运行时替换载体"
// 语义无意义, 故 type alias 无损. 未来失效场景 (内存故障注入/热插拔模拟感知)
// 当前与可预见未来均不存在, 按 AGENTS.md §12.3 不预留扩展点.
/// 实际载体类型 — 生产 `RawMetaStore` (裸指针), host 测试 `VecMetaStore` (Vec 注入).
/// cfg 仅此一处, `PhysicalMemoryManager.store` 字段经此编译期定死.
#[cfg(not(any(test, feature = "host-test")))]
pub(crate) type MetaStoreImpl = RawMetaStore;
#[cfg(any(test, feature = "host-test"))]
pub(crate) type MetaStoreImpl = VecMetaStore;

/// 物理内存管理器 — Buddy 分配器
///
/// 2026-07-02: 加 `#[repr(C)]` 防止 LTO 字段重排. 本次会话诊断发现
/// LTO 在 release 模式错位多个字段 (`bitmap_size`, `buddy_meta`, `buddy_heads`),
/// 虽有 `addr_of`! 修复, repr(C) 提供额外防御层.
#[repr(C)]
pub struct PhysicalMemoryManager {
    // ---- Bitmap (reserved 跟踪 + 统计) ----
    /// bitmap 长度 (u32 word 数), count_free_pages 的尾部余位修正使用
    bitmap_size: Cell<usize>,
    mem_size: Cell<u64>,
    kernel_end: Cell<u64>,
    info: Cell<MemoryInfo>,
    // ---- 锁与生命周期 ----
    lock: AtomicBool,
    initialized: AtomicBool,
    buddy_ready: AtomicBool,
    // ---- 早期 (线性) 分配器 ----
    early_allocs: UnsafeCell<[EarlyAlloc; MAX_EARLY_ALLOCS]>,
    early_count: AtomicUsize,
    early_current: AtomicU64,
    // ---- 统计 ----
    total_allocs: AtomicU64,
    total_frees: AtomicU64,
    failed_allocs: AtomicU64,
    /// 分配失败注入剩余次数: `ALLOC_FAIL_DISARMED` 表示未武装, 否则为"还需成功分配
    /// 的次数", 归零后下一次 `alloc_page` 及之后一律返回 `None`.
    ///
    /// 仅 `test` / `kernel_test` 配置编译, 生产二进制不含 (避免死代码, F9).
    /// 用途: 构造"COW 克隆中途分配失败"的确定性状态, 供回滚用例判别
    /// (见 docs/plan/cr3-lifetime-ownership.md §5.2 注入 2).
    #[cfg(any(test, feature = "kernel_test"))]
    alloc_fail_countdown: AtomicU32,
    // ---- Buddy 分配器 ----
    /// H-04 (2026-09-09): 内存元数据载体 — 生产为 RawMetaStore (裸指针),
    /// host 测试经 inject_meta_store 注入 VecMetaStore (Vec<u8> 堆载体).
    /// 载体类型经 type alias `MetaStoreImpl` 编译期定死 (无 dyn/vtable).
    /// init_bitmap 单线程启动期设置; buddy 就绪后仅在 PMM 锁下访问.
    store: UnsafeCell<Option<MetaStoreImpl>>,
    /// 索引式双向链表空闲块头 (存块头 pfn), 每个阶数一个; 空链表头 = SENTINEL
    buddy_heads: UnsafeCell<[u64; MAX_BUDDY_ORDER as usize + 1]>,
    /// B05-55: reserve 摘除块的暂存 (待位图置位后压回, 防止合并吞掉 reserve 区)
    buddy_reserve_deferred: UnsafeCell<alloc::vec::Vec<(u64, u64, u64, u64)>>,
}

// SAFETY: PhysicalMemoryManager 使用 Cell/UnsafeCell 实现内部可变性.
// 所有公开修改都通过 pmm_alloc_pages/pmm_free_pages 进行, 它们
// 获取内部锁 (AtomicBool 自旋锁). 锁保证互斥, 多线程并发访问安全.
// buddy_heads/store 仅在持锁时访问; bitmap_size 仅在初始化时设置,
// SAFETY: PhysicalMemoryManager 含 UnsafeCell, 但初始化完成后只读.
unsafe impl Sync for PhysicalMemoryManager {}
// SAFETY: 同上, 初始化后只读, 无并发写风险.
unsafe impl Send for PhysicalMemoryManager {}

impl PhysicalMemoryManager {
    pub const fn new() -> Self {
        Self {
            bitmap_size: Cell::new(0),
            mem_size: Cell::new(0),
            kernel_end: Cell::new(0),
            info: Cell::new(MemoryInfo::const_default()),
            lock: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            buddy_ready: AtomicBool::new(false),
            early_allocs: UnsafeCell::new([EarlyAlloc::const_default(); MAX_EARLY_ALLOCS]),
            early_count: AtomicUsize::new(0),
            early_current: AtomicU64::new(0),
            total_allocs: AtomicU64::new(0),
            total_frees: AtomicU64::new(0),
            failed_allocs: AtomicU64::new(0),
            #[cfg(any(test, feature = "kernel_test"))]
            alloc_fail_countdown: AtomicU32::new(ALLOC_FAIL_DISARMED),
            store: UnsafeCell::new(None),
            buddy_heads: UnsafeCell::new([SENTINEL; MAX_BUDDY_ORDER as usize + 1]),
            buddy_reserve_deferred: UnsafeCell::new(alloc::vec::Vec::new()),
        }
    }

    // ==================== 公开 API (不变) ====================

    /// 注入 host 测试内存元数据载体 (`VecMetaStore`)
    ///
    /// 仅 `host-test` / `test` 配置编译, 生产二进制不含 (避免死代码, F9).
    /// 必须在 `init_bitmap` 之前调用 — 之后 `init_bitmap` 将经该载体建立
    /// bitmap / buddy_meta / FREE_LINKS 三区, 使 buddy 完整生命周期
    /// (init_bitmap → alloc/free → 合并) 在 host 侧以同一份算法代码运行,
    /// 无测试/生产分叉 (B08-12 路线 C 核心).
    ///
    /// # 安全
    /// `store` 为单写者字段: 生产路径由 `init_bitmap` 创建 `RawMetaStore`,
    /// host 测试经本方法注入 `VecMetaStore`; 二者只能取其一且只写入一次.
    #[cfg(any(test, feature = "host-test"))]
    pub fn inject_meta_store(&self, store: VecMetaStore) {
        // SAFETY: 调用方保证在 init_bitmap 之前注入且仅注入一次;
        // buddy 就绪后 store 为只读路径且均在 PMM 锁下访问 (见 meta_store).
        // cfg(any(test, host-test)) 下 MetaStoreImpl = VecMetaStore, 直接赋值.
        unsafe { *self.store.get() = Some(store) };
    }

    pub fn init(&self, mem_size: u64, kernel_end: u64) {
        self.mem_size.set(mem_size);
        self.kernel_end.set(kernel_end);

        let total_pages = mem_size / PAGE_SIZE;

        let start = (kernel_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        self.early_current.store(start, Ordering::Relaxed);

        let mut info = self.info.get();
        info.total_pages = total_pages;
        info.kernel_end = kernel_end;
        self.info.set(info);

        klog_pmm!(
            "[PMM] Init: {} MB, {} pages, kernel ends at 0x{:X}",
            mem_size / (1024 * 1024),
            total_pages,
            kernel_end
        );
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    /// 初始化 buddy 元数据 (bitmap / buddy_meta / FREE_LINKS) 并建立空闲链表
    ///
    /// `reserved_after_kernel`: 内核镜像末尾之后额外预留的字节数 (向上取整到页),
    /// 与内核镜像页一起标记为已用, 不可被分配.
    ///
    /// # Panics
    /// 元数据载体 (`MetaStore`) 缺失时 panic — 生产路径在载体自动创建失败时可达
    /// (理论上不可达, 见 init 前置); host 测试必须先 `inject_meta_store`.
    pub fn init_bitmap(&self, reserved_after_kernel: u64) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }

        let reserved_aligned = (reserved_after_kernel + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        self.early_current
            .fetch_add(reserved_aligned, Ordering::Relaxed);

        let info = self.info.get();
        let total_pages = info.total_pages as usize;
        let total_bits = total_pages;
        let bitmap_words = total_bits.div_ceil(32);
        let bitmap_bytes = bitmap_words * 4;

        // ---- Bitmap placement ----
        let bitmap_phys = self
            .early_current
            .fetch_add(bitmap_bytes as u64, Ordering::Relaxed);
        let bitmap_aligned = (bitmap_phys + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        // ---- Buddy 元数据布局 (位于 bitmap 之后, 页对齐) ----
        let buddy_meta_bytes = total_pages;
        let buddy_meta_phys =
            (bitmap_aligned + bitmap_bytes as u64 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let buddy_meta_pages = buddy_meta_bytes.div_ceil(PAGE_SIZE as usize) as u64;

        // 将 early_current 推过 buddy 元数据
        self.early_current.store(
            buddy_meta_phys + buddy_meta_pages * PAGE_SIZE + PAGE_SIZE,
            Ordering::Relaxed,
        );

        // ---- FREE_LINKS 索引式链表布局 (位于 buddy 元数据之后, 页对齐) ----
        // H-02 (2026-09-06): 每个空闲块头项 16 字节 (prev/next 各 8 字节, 存 pfn),
        // 长度 = total_pages, 与 buddy_meta 同法从 early 区分配.
        let free_links_bytes = total_pages * 16;
        let free_links_phys =
            (buddy_meta_phys + buddy_meta_pages * PAGE_SIZE + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let free_links_pages = free_links_bytes.div_ceil(PAGE_SIZE as usize) as u64;

        // 将 early_current 推过 FREE_LINKS
        self.early_current.store(
            free_links_phys + free_links_pages * PAGE_SIZE + PAGE_SIZE,
            Ordering::Relaxed,
        );

        // ---- 帧持有计数布局 (位于 FREE_LINKS 之后, 页对齐) ----
        // 按 pfn 索引, 每页 1 字节: 0 = 未计数帧, n >= 1 = n 个持有者.
        // 语义与契约见 `frame_inc` / `frame_dec`; 不复用 buddy_meta (该字节已被
        // buddy 阶数语义占用), 故独立成区.
        let counts_bytes = total_pages;
        let counts_phys =
            (free_links_phys + free_links_pages * PAGE_SIZE + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let counts_pages = counts_bytes.div_ceil(PAGE_SIZE as usize) as u64;

        // 将 early_current 推过计数区
        self.early_current.store(
            counts_phys + counts_pages * PAGE_SIZE + PAGE_SIZE,
            Ordering::Relaxed,
        );

        // H-04 (2026-09-09): 三区内存预置统一经 MetaStore 载体 —
        // 生产自动创建 RawMetaStore (基于 phys + KERNEL_BASE 裸指针, 行为不变),
        // host 测试已在 init_bitmap 前经 inject_meta_store 注入 VecMetaStore.
        // SAFETY: store 为单写者字段 (UnsafeCell), 本处是唯一生产写入点
        // (init_bitmap 单线程启动期; host 测试经 inject_meta_store 注入).
        // 方案 B (2026-09-09): MetaStoreImpl 为具体类型, 静态分发可内联 —
        // 不用 `dyn` 强转 trait object, init 路径同样零 vtable (观察项修复).
        let store: &mut MetaStoreImpl = unsafe {
            let slot = &mut *self.store.get();
            #[cfg(not(any(test, feature = "host-test")))]
            if slot.is_none() {
                // SAFETY: 生产路径单线程启动期创建 RawMetaStore; heads 指针经
                // UnsafeCell::get 指向宿主 buddy_heads 字段 (self 不移动, init_bitmap
                // 后稳定, buddy 就绪后仅在 PMM 锁下访问).
                // 方案 B: 载体直接存储于 store 字段, 无 Box 堆分配.
                *slot = Some(RawMetaStore::new(self.buddy_heads.get()));
            }
            // 注入或新建必然成功 (slot 刚保证非 None); 原实现 bitmap_virt=0 的
            // FATAL 分支在真实内核不可达 (KERNEL_BASE 映射地址恒非零).
            // host-test 下 slot 由 inject_meta_store 预置 (若未注入则 panic, 属错误用法).
            slot.as_mut().expect("[PMM] meta store missing")
        };
        store.setup_bitmap(bitmap_aligned, bitmap_bytes);
        store.setup_meta(buddy_meta_phys, buddy_meta_bytes);
        store.setup_links(free_links_phys, free_links_bytes);
        store.setup_counts(counts_phys, counts_bytes);
        self.bitmap_size.set(bitmap_words);
        klog_pmm!(
            "[PMM] Buddy meta: {} B at 0x{:X}",
            buddy_meta_bytes,
            phys_to_virt(buddy_meta_phys)
        );
        klog_pmm!(
            "[PMM] FREE_LINKS: {} B at 0x{:X} ({} pages)",
            free_links_bytes,
            phys_to_virt(free_links_phys),
            free_links_pages
        );
        klog_pmm!(
            "[PMM] FRAME_COUNTS: {} B at 0x{:X} ({} pages)",
            counts_bytes,
            phys_to_virt(counts_phys),
            counts_pages
        );

        // ---- 在 bitmap 中标记 reserved 区 ----
        let kernel_end_val = self.kernel_end.get();
        let kernel_pages = phys_to_page(kernel_end_val + PAGE_SIZE - 1) as usize;
        let reserved_pages = (reserved_aligned / PAGE_SIZE) as usize;
        let total_reserved = kernel_pages + reserved_pages;
        for i in 0..total_reserved.min(total_pages) {
            self.set_bit(i);
        }
        if total_pages > 0 {
            self.set_bit(0); // page 0 永远不能被分配出去
        }

        // 标记 bitmap 页已用
        let bmp_start_page = phys_to_page(bitmap_aligned) as usize;
        let bmp_pages = (bitmap_bytes as u64).div_ceil(PAGE_SIZE) as usize;
        for i in bmp_start_page..(bmp_start_page + bmp_pages).min(total_pages) {
            self.set_bit(i);
        }

        // 标记 buddy-meta 页已用
        let bm_start_page = phys_to_page(buddy_meta_phys) as usize;
        for i in bm_start_page..(bm_start_page + buddy_meta_pages as usize).min(total_pages) {
            self.set_bit(i);
        }

        // 标记 FREE_LINKS 页已用 (H-02)
        let fl_start_page = phys_to_page(free_links_phys) as usize;
        for i in fl_start_page..(fl_start_page + free_links_pages as usize).min(total_pages) {
            self.set_bit(i);
        }

        // 标记帧持有计数页已用
        let fc_start_page = phys_to_page(counts_phys) as usize;
        for i in fc_start_page..(fc_start_page + counts_pages as usize).min(total_pages) {
            self.set_bit(i);
        }

        // ---- 从空闲 bitmap 页构建 buddy 空闲链表 ----
        self.buddy_init_free_lists(total_pages);

        self.buddy_ready.store(true, Ordering::Release);
        self.initialized.store(true, Ordering::Release);

        let free = self.count_free_pages();
        klog_pmm!(
            "[PMM] Buddy ready: {} total, {} free ({} MB), reserved {} pages",
            total_pages,
            free,
            free * 4 / 1024,
            total_reserved
        );

        self.update_stats();
    }

    /// 武装"分配失败注入": 接下来的 `skip` 次 `alloc_page` 照常成功, 其后一律返回 `None`.
    ///
    /// 仅 `test` / `kernel_test` 配置编译 (调用方为回滚用例). 生产二进制不含本入口,
    /// 故不可作为运行期降级开关使用.
    #[cfg(any(test, feature = "kernel_test"))]
    pub fn arm_alloc_failure(&self, skip: u32) {
        self.alloc_fail_countdown.store(skip, Ordering::SeqCst);
    }

    /// 解除"分配失败注入", 恢复常态分配.
    ///
    /// 用例必须在断言前调用 (含失败分支), 否则注入会泄漏到后续用例.
    #[cfg(any(test, feature = "kernel_test"))]
    pub fn disarm_alloc_failure(&self) {
        self.alloc_fail_countdown
            .store(ALLOC_FAIL_DISARMED, Ordering::SeqCst);
    }

    /// 消费一次注入计数: 返回 `true` 表示本次分配应失败 (计数已归零).
    ///
    /// 未武装时恒 `false` 且不触碰计数; 武装后每次调用递减一次, 归零即失败.
    #[cfg(any(test, feature = "kernel_test"))]
    fn alloc_fail_should_fail(&self) -> bool {
        let remaining = self.alloc_fail_countdown.load(Ordering::SeqCst);
        if remaining == ALLOC_FAIL_DISARMED {
            return false;
        }
        if remaining == 0 {
            return true;
        }
        self.alloc_fail_countdown
            .store(remaining - 1, Ordering::SeqCst);
        false
    }

    pub fn alloc_page(&self) -> Option<PhysAddr> {
        #[cfg(any(test, feature = "kernel_test"))]
        if self.alloc_fail_should_fail() {
            self.failed_allocs.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let flags = self.acquire_lock();
        let result = self.do_alloc(0);
        match result {
            Some(addr) => {
                // 单页帧: 分配者即初始持有者, 持有计数置 1 (见 frame_inc/frame_dec)
                self.frame_counts_write(phys_to_page(addr.0) as usize, 1);
                self.total_allocs.fetch_add(1, Ordering::Relaxed);
            }
            None => {
                self.failed_allocs.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.release_lock(&flags);
        result
    }

    pub fn free_page(&self, addr: PhysAddr) {
        if addr.0 == 0 {
            return;
        }
        let flags = self.acquire_lock();
        // 丢弃一个持有者; 仅当无人持有时才真正归还 PMM (未计数帧保持既有归还语义)
        if self.frame_counts_release(phys_to_page(addr.0) as usize) {
            self.do_free(addr, 0);
            self.total_frees.fetch_add(1, Ordering::Relaxed);
        }
        self.release_lock(&flags);
    }

    /// 登记一个帧持有者 (共享方使用).
    ///
    /// # 契约 (§3.2 大页计数语义)
    /// - 只接受**单页帧** (由 `alloc_page` / `alloc_pages(1)` 产生, 计数 >= 1);
    ///   连续多帧块 (order > 0) 内页未计数, 传入时返回 `false` (拒绝登记).
    /// - **pfn 越界 fail-closed**: 非 PMM 分配的物理地址 (设备/MMIO 映射) 不在计数区
    ///   范围内, 拒绝登记 (否则会越界读写计数区).
    /// - 返回 `true` 表示计数已 +1; `false` 表示帧未处于计数态 (契约违反, 计数不变).
    pub fn frame_inc(&self, addr: PhysAddr) -> bool {
        if addr.0 == 0 {
            return false;
        }
        let flags = self.acquire_lock();
        let ok = match self.frame_counts_pfn(addr) {
            Some(pfn) => {
                let cur = self.frame_counts_read(pfn);
                if cur == 0 || cur == u8::MAX {
                    false
                } else {
                    self.frame_counts_write(pfn, cur + 1);
                    true
                }
            }
            None => false,
        };
        self.release_lock(&flags);
        ok
    }

    /// 注销一个帧持有者.
    ///
    /// # 契约
    /// - 返回 `true` = 该帧已无持有者 (计数归零), 调用方据此销毁/归还.
    /// - 返回 `false` = 仍有持有者, **或**该帧不处于计数态 (未计数帧无计数可减,
    ///   fail-closed: 调用方不得据此销毁). 归零恰好发生一次.
    /// - **pfn 越界 fail-closed**: 同 `frame_inc`, 未计数区地址一律返回 `false`.
    pub fn frame_dec(&self, addr: PhysAddr) -> bool {
        if addr.0 == 0 {
            return false;
        }
        let flags = self.acquire_lock();
        let zero = match self.frame_counts_pfn(addr) {
            Some(pfn) => {
                let cur = self.frame_counts_read(pfn);
                if cur == 0 {
                    false
                } else if cur == 1 {
                    self.frame_counts_write(pfn, 0);
                    true
                } else {
                    self.frame_counts_write(pfn, cur - 1);
                    false
                }
            }
            None => false,
        };
        self.release_lock(&flags);
        zero
    }

    /// 观测单页帧当前持有者数 (0 = 未计数帧; 仅用于断言/审计)
    pub fn frame_ref_count(&self, addr: PhysAddr) -> u8 {
        if addr.0 == 0 {
            return 0;
        }
        let flags = self.acquire_lock();
        let count = self
            .frame_counts_pfn(addr)
            .map_or(0, |pfn| self.frame_counts_read(pfn));
        self.release_lock(&flags);
        count
    }

    /// 帧计数索引前置校验: 返回 pfn; 越界 (非 PMM 分配的物理地址, 如 MMIO 设备
    /// 映射) 返回 `None` ⇒ 调用方 fail-closed (不计数, 也不报告归零).
    #[inline]
    fn frame_counts_pfn(&self, addr: PhysAddr) -> Option<usize> {
        let pfn = phys_to_page(addr.0) as usize;
        (pfn < self.get_total_pages() as usize).then_some(pfn)
    }

    /// 读帧持有计数 (载体未就绪时返回 0 = 未计数)
    #[inline]
    fn frame_counts_read(&self, pfn: usize) -> u8 {
        self.meta_store().map_or(0, |store| store.counts_read(pfn))
    }

    /// 写帧持有计数 (载体未就绪时静默跳过)
    #[inline]
    fn frame_counts_write(&self, pfn: usize, val: u8) {
        if let Some(store) = self.meta_store() {
            store.counts_write(pfn, val);
        }
    }

    /// 归还前的计数处理: 丢弃一个持有者.
    ///
    /// 返回 `true` = 可归还 PMM:
    /// - 计数 >= 2: 递减后返回 `false` (仍有持有者, 不归还);
    /// - 计数 == 1: 递减到 0 并返回 `true`;
    /// - 计数 == 0: 未计数帧 (连续多帧块内页 / reserved 页), 保持既有"直接归还"语义.
    #[inline]
    fn frame_counts_release(&self, pfn: usize) -> bool {
        let cur = self.frame_counts_read(pfn);
        if cur <= 1 {
            self.frame_counts_write(pfn, 0);
            true
        } else {
            self.frame_counts_write(pfn, cur - 1);
            false
        }
    }

    pub fn alloc_pages(&self, count: usize) -> Option<PhysAddr> {
        if count == 0 {
            return None;
        }
        let order = count_to_order(count);
        let npages = 1usize << order as usize;

        // T-02: 分配前策略决策
        // buddy 就绪后使用缓存的 free_pages 统计值, 避免每次分配都遍历 bitmap;
        // 这既提升性能, 又避免 count_free_pages 遍历 + klog 格式化导致的栈溢出风险.
        // 统计值在 do_alloc/do_free 的持锁路径中通过 update_stats 更新.
        let free = if self.buddy_ready.load(Ordering::Relaxed) {
            self.info.get().free_pages
        } else {
            self.count_free_pages()
        };
        let ctx = super::alloc_trait::AllocContext {
            requested_pages: npages,
            free_pages: free,
            total_pages: self.info.get().total_pages as u64,
            pressure_level: 0,
            preferred_node: None,
        };
        match super::alloc_trait::current_alloc_decision().decide_alloc(ctx) {
            super::alloc_trait::AllocDecision::Allow => {}
            super::alloc_trait::AllocDecision::Deny => {
                klog_pmm!("[PMM] alloc denied (policy)");
                self.failed_allocs.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            super::alloc_trait::AllocDecision::RetryAfterReclaim => {
                // 策略建议回收后重试, 但 PMM 不执行回收, 直接失败
                // functions 层的 OOMD 会在上层处理回收逻辑
                klog_pmm!("[PMM] alloc retry-after-reclaim");
                self.failed_allocs.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        }
        let flags = self.acquire_lock();
        let result = self.do_alloc(order);
        match result {
            Some(addr) => {
                // 单页帧 (order == 0) 与 alloc_page 同语义: 分配者持有计数 1.
                // 连续多帧块不逐页计数 (§3.2: 块视为单一 holder, 归还走 free_pages).
                // SIMPLIFIED: 块首计数未实装; 影响面: order > 0 的块内页计数恒为 0,
                // 其归还/共享不经计数面 (与既有语义一致); 何时需扩展: 出现大页共享
                // (如透明大页拆分共享) 时按逐页计数扩展, 契约入口 frame_inc/frame_dec 不变.
                if order == 0 {
                    self.frame_counts_write(phys_to_page(addr.0) as usize, 1);
                }
                self.total_allocs
                    .fetch_add(npages as u64, Ordering::Relaxed);
            }
            None => {
                self.failed_allocs.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.release_lock(&flags);
        result
    }

    pub fn free_pages(&self, addr: PhysAddr, count: usize) {
        if addr.0 == 0 || count == 0 {
            return;
        }
        let order = count_to_order(count);
        let npages = 1usize << order as usize;
        let flags = self.acquire_lock();
        if order == 0 {
            // 单页路径与 free_page 同语义: 计数归零才归还
            if self.frame_counts_release(phys_to_page(addr.0) as usize) {
                self.do_free(addr, 0);
                self.total_frees.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            // 连续多帧块: 不逐页计数, 整块归还为单一 holder 语义
            self.do_free(addr, order);
            self.total_frees.fetch_add(npages as u64, Ordering::Relaxed);
        }
        self.release_lock(&flags);
    }

    /// B03-04: 标记物理地址范围为已用 (反向声明) — 用于 swap/persistent buffer 等
    /// 子系统在初始化后声明其占有的物理内存, 防止被 PMM 二次分配。
    ///
    /// 与 buddy 空闲链表严格同步: 会从空闲链表摘除所有与范围重叠的块,
    /// 将不重叠部分重新压回, 避免含已预留页的块滞留在链表中被二次分配。
    ///
    /// # 调用契约
    /// - `base` 必须对齐到 `PAGE_SIZE` 边界
    /// - `size` 必须 > 0 且为 `PAGE_SIZE` 整数倍
    /// - 范围 `[base, base+size)` 必须全部在 PMM 管理的物理 RAM 内
    /// - 调用方负责不与 kernel reserved / bitmap / buddy-meta 区重叠
    ///   (启动期 init_bitmap 已预留; 运行时新增子系统的预留需自行避让)
    /// - 不可对已分配/已预留的页调本函数 (会破坏 PMM 簿记; 若需共享,
    ///   由调用方加互斥而非依赖本函数)
    ///
    /// # Errors
    /// `base` 未对齐 / `size` 为 0 或非页对齐 / 范围越界 / 页已分配时返回 Err。
    pub fn reserve_range(&self, base: PhysAddr, size: usize) -> Result<(), &'static str> {
        if size == 0 {
            return Err("PMM reserve_range: zero size");
        }
        if !base.as_u64().is_multiple_of(PAGE_SIZE as u64) {
            return Err("PMM reserve_range: base not page-aligned");
        }
        if !size.is_multiple_of(PAGE_SIZE as usize) {
            return Err("PMM reserve_range: size not page-aligned");
        }

        let start_pfn = phys_to_page(base.as_u64()) as usize;
        let npages = size / PAGE_SIZE as usize;
        let end_pfn = start_pfn
            .checked_add(npages)
            .ok_or("PMM reserve_range: overflow")?;
        let total_pages = self.info.get().total_pages as usize;
        if end_pfn > total_pages {
            return Err("PMM reserve_range: range exceeds PMM size");
        }

        let flags = self.acquire_lock();
        // 拒绝范围与已分配页重叠 (避免 PMM 簿记破坏)
        for i in start_pfn..end_pfn {
            if self.test_bit(i) {
                self.release_lock(&flags);
                return Err("PMM reserve_range: range overlaps allocated/reserved page");
            }
        }
        // 摘除空闲链表中重叠的块 + 置位位图 + 更新统计
        self.buddy_reserve_pfn_range(start_pfn as u64, npages as u64);
        self.release_lock(&flags);

        klog_pmm!(
            "[PMM] Reserved range: base=0x{:X} size={} ({} pages, {} KB)",
            base.as_u64(),
            size,
            npages,
            (npages * PAGE_SIZE as usize) / 1024
        );
        Ok(())
    }

    /// B03-03 + DECISION-050: 撤销 `reserve_range` 的簿记, 释放预留范围回 PMM 池。
    ///
    /// # 调用契约
    /// - `base`/`size` 必须与之前的 `reserve_range` 调用严格对应
    /// - 仅对 **reserved** 簿记的页可调 (即 reserve_range 而非 alloc_page 拿的页)
    /// - 调用方负责确保该范围不再被任何子系统使用 (语义同步)
    /// - 不可撤销 `alloc_page` 拿的页 (会破坏 PMM 簿记, 那是 `free_page` 的范畴)
    ///
    /// # Errors
    /// `base` 未对齐 / `size` 非页对齐 / 范围越界 / 页未处于 reserved 状态时返回 Err。
    pub fn unreserve_range(&self, base: PhysAddr, size: usize) -> Result<(), &'static str> {
        if size == 0 {
            return Err("PMM unreserve_range: zero size");
        }
        if !base.as_u64().is_multiple_of(PAGE_SIZE as u64) {
            return Err("PMM unreserve_range: base not page-aligned");
        }
        if !size.is_multiple_of(PAGE_SIZE as usize) {
            return Err("PMM unreserve_range: size not page-aligned");
        }

        let start_pfn = phys_to_page(base.as_u64()) as usize;
        let npages = size / PAGE_SIZE as usize;
        let end_pfn = start_pfn
            .checked_add(npages)
            .ok_or("PMM unreserve_range: overflow")?;
        let total_pages = self.info.get().total_pages as usize;
        if end_pfn > total_pages {
            return Err("PMM unreserve_range: range exceeds PMM size");
        }

        let flags = self.acquire_lock();
        // 校验范围全部处于 reserved 簿记 (set_bit == 1); 若有 free 页, 拒绝
        for i in start_pfn..end_pfn {
            if !self.test_bit(i) {
                self.release_lock(&flags);
                return Err("PMM unreserve_range: range contains non-reserved page");
            }
        }
        // 撤销簿记
        for i in start_pfn..end_pfn {
            self.clear_bit(i);
        }
        // 压回 buddy 空闲链表 (并尝试合并), 保持链表与位图同步
        self.buddy_free_insert_range(start_pfn as u64, npages as u64);
        self.stats_free(npages as u64);
        self.release_lock(&flags);

        klog_pmm!(
            "[PMM] Unreserved range: base=0x{:X} size={} ({} pages, {} KB)",
            base.as_u64(),
            size,
            npages,
            (npages * PAGE_SIZE as usize) / 1024
        );
        Ok(())
    }

    /// B03-03: 扫描 bitmap 找连续 `size` 字节的物理范围, 返回对齐基址。
    ///
    /// 用于 swap / 持久化 buffer 等需要**大块连续物理内存**的子系统.
    /// 不调 buddy allocator (buddy 上限 2MB 不满足 16MB+ 需求), 直接扫描 bitmap.
    ///
    /// # 调用契约
    /// - `size` 必须 > 0 且为 `PAGE_SIZE` 整数倍
    /// - 调用方拿到基址后应立即 `reserve_range(base, size)` 声明 reserved,
    ///   否则后续 alloc 可能踩用
    /// - 返回的基址**未**标记为 allocated/reserved (仅查找), 由调用方负责簿记
    ///
    /// # Returns
    /// `Some(PhysAddr)` 找到连续范围, 基址对齐 PAGE_SIZE;
    /// `None` 无足够连续内存.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn find_contig_range(&self, size: usize) -> Option<PhysAddr> {
        if size == 0 || !size.is_multiple_of(PAGE_SIZE as usize) {
            return None;
        }

        let flags = self.acquire_lock();
        let result = (|| -> Option<PhysAddr> {
            let total_pages = self.info.get().total_pages as usize;
            let npages = size / PAGE_SIZE as usize;

            let mut pfn = 0usize;
            while pfn + npages <= total_pages {
                // 跳过已分配页
                if self.test_bit(pfn) {
                    pfn += 1;
                    continue;
                }

                // 寻找连续 npages 空闲段起点
                let run_start = pfn;
                let mut run_len = 0usize;
                while pfn < total_pages && !self.test_bit(pfn) && run_len < npages {
                    pfn += 1;
                    run_len += 1;
                }

                if run_len >= npages {
                    return Some(PhysAddr(page_to_phys(run_start as u64)));
                }
            }
            None
        })();
        self.release_lock(&flags);
        result
    }

    pub fn alloc_huge_page(&self, size_type: PageSize) -> Option<PhysAddr> {
        match size_type {
            PageSize::Size4K => self.alloc_page(),
            PageSize::Size2M => self.alloc_pages(512),
            PageSize::Size1G => {
                let np = (size_type.size() / PAGE_SIZE) as usize;
                let flags = self.acquire_lock();
                let result = self.buddy_direct_alloc_aligned(np, size_type.size());
                if result.is_some() {
                    self.total_allocs.fetch_add(np as u64, Ordering::Relaxed);
                } else {
                    self.failed_allocs.fetch_add(1, Ordering::Relaxed);
                }
                self.release_lock(&flags);
                result
            }
        }
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn free_huge_page(&self, addr: PhysAddr, size_type: PageSize) {
        match size_type {
            PageSize::Size4K => self.free_page(addr),
            PageSize::Size2M => self.free_pages(addr, 512),
            PageSize::Size1G => {
                let np = (size_type.size() / PAGE_SIZE) as usize;
                let flags = self.acquire_lock();
                let start = phys_to_page(addr.0) as usize;
                for i in 0..np {
                    self.clear_bit(start + i);
                }
                // 压回 buddy 空闲链表 (并尝试合并), 保持链表与位图同步
                self.buddy_free_insert_range(start as u64, np as u64);
                self.total_frees.fetch_add(np as u64, Ordering::Relaxed);
                self.release_lock(&flags);
            }
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn is_aligned_for_huge(&self, addr: PhysAddr, size_type: PageSize) -> bool {
        size_type.is_aligned(addr.0)
    }

    pub fn get_free_pages(&self) -> u64 {
        self.info.get().free_pages
    }
    pub fn get_total_pages(&self) -> u64 {
        self.info.get().total_pages
    }
    pub fn get_used_pages(&self) -> u64 {
        self.info.get().used_pages
    }
    pub fn get_info(&self) -> MemoryInfo {
        self.info.get()
    }

    pub fn dump_stats(&self) {
        let info = self.info.get();
        klog_pmm!("=== PMM (Buddy) ===");
        klog_pmm!(
            "Total: {} MB  Pages: {} total / {} free / {} used",
            self.mem_size.get() / (1024 * 1024),
            info.total_pages,
            info.free_pages,
            info.used_pages
        );
        klog_pmm!("Kernel End: 0x{:X}", info.kernel_end);
        klog_pmm!(
            "Allocs: {}  Frees: {}  Failed: {}",
            self.total_allocs.load(Ordering::Relaxed),
            self.total_frees.load(Ordering::Relaxed),
            self.failed_allocs.load(Ordering::Relaxed)
        );
        klog_pmm!("===================");
    }

    // ==================== 锁辅助函数 ====================

    /// 获取 PMM 锁, 同时禁用中断 (SMP 安全).
    ///
    /// 禁用中断是为了避免当运行在同一 CPU 的中断处理程序
    /// 尝试分配内存时形成死锁.
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

    // ==================== Bitmap 辅助函数 (统计 + reserved) ====================

    /// 访问内存元数据载体 (buddy 就绪后必有; 未 init 时为 None)
    ///
    /// # 安全
    /// store 仅在 init_bitmap (或 host 测试 inject_meta_store) 中写入一次,
    /// 之后为只读路径, 且全部在 PMM 锁保护下访问.
    #[inline]
    fn meta_store(&self) -> Option<&MetaStoreImpl> {
        // SAFETY: store 单写者 (init_bitmap/inject), 读路径持有 PMM 锁.
        // 方案 B: 载体直接存于字段 (非 Box), 具体类型引用, 方法调用静态解析.
        unsafe { (*self.store.get()).as_ref() }
    }

    // B03-05 背景: 2026-07-01 test 110 hang 修复 (LTO 字段错位) —
    // 原实现从 self.bitmap_size 读取 word 数, LTO 在 inline 时错位到
    // failed_allocs 字段导致越界写. H-04 后 bitmap word 数由载体内部
    // 持有 (setup_bitmap 记录), 该 LTO 错位面整体消除.
    fn set_bit(&self, bit: usize) {
        if let Some(store) = self.meta_store() {
            store.bitmap_set(bit);
        }
    }

    fn clear_bit(&self, bit: usize) {
        if let Some(store) = self.meta_store() {
            store.bitmap_clear(bit);
        }
    }

    fn test_bit(&self, bit: usize) -> bool {
        self.meta_store()
            .map_or(false, |store| store.bitmap_test(bit))
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn count_free_pages(&self) -> u64 {
        let total = self.info.get().total_pages as usize;
        let free = self.meta_store().map_or(0, MetaStore::bitmap_count_free);
        // 截断到 total (bitmap 在 total_pages 之外可能还有剩余位)
        let extra = (self.bitmap_size.get() * 32).saturating_sub(total) as u32;
        if extra > 0 {
            free.saturating_sub(u64::from(extra))
        } else {
            free
        }
    }

    fn update_stats(&self) {
        let free = self.count_free_pages();
        let mut info = self.info.get();
        info.free_pages = free;
        info.used_pages = info.total_pages - free;
        self.info.set(info);
    }

    /// 轻量统计增量: 分配 npages 页后更新 free/used 计数.
    /// 调用方必须持有 PMM 锁.
    #[inline]
    fn stats_alloc(&self, npages: u64) {
        let mut info = self.info.get();
        info.free_pages = info.free_pages.saturating_sub(npages);
        info.used_pages = info.used_pages.saturating_add(npages);
        self.info.set(info);
    }

    /// 轻量统计增量: 释放 npages 页后更新 free/used 计数.
    /// 调用方必须持有 PMM 锁.
    #[inline]
    fn stats_free(&self, npages: u64) {
        let mut info = self.info.get();
        info.free_pages = info.free_pages.saturating_add(npages);
        info.used_pages = info.used_pages.saturating_sub(npages);
        self.info.set(info);
    }

    // ==================== Buddy 分配器核心 ====================

    /// 尝试将 `order` 处释放的 `pfn` 与其上方的 buddy 合并.
    /// 返回 (`merged_pfn`, `final_order`).
    ///
    /// `limit_pfn`: 合并块的上界 (不越过该页号). 用于 reserve 压回时防止
    /// 合并吞掉已置位的 reserve 区 (B05-55); do_free 等路径传 `total_pages`.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    fn buddy_try_merge(&self, mut pfn: u64, mut order: u8, limit_pfn: u64) -> (u64, u8) {
        let store = match self.meta_store() {
            Some(s) => s,
            None => return (pfn, order),
        };
        let total = core::cmp::min(self.info.get().total_pages, limit_pfn);

        while order < MAX_BUDDY_ORDER {
            let buddy_pfn = pfn ^ (1u64 << order);
            if buddy_pfn >= total {
                break;
            }
            // 合并块不得越过 limit_pfn: 若 buddy 越界, 停止合并
            // (buddy_pfn 是更高阶块的起始, 若合并则覆盖 [pfn, pfn+2^(order+1)))
            if pfn + (1u64 << (order + 1)) > limit_pfn {
                break;
            }

            // M2: 显式验证伙伴块的 order
            let buddy_state = store.meta_read(buddy_pfn as usize);

            // 检查 buddy_state 是否为有效的 order 值 (0..=MAX_BUDDY_ORDER)
            // 如果 buddy_state == BUDDY_ALLOCATED (0xFF)，说明已分配，不能合并
            // 如果 buddy_state > MAX_BUDDY_ORDER 且 != BUDDY_ALLOCATED，说明元数据损坏
            if buddy_state > MAX_BUDDY_ORDER {
                // 已分配或元数据损坏，停止合并
                break;
            }

            // M2: 验证伙伴块的 order 必须等于当前 order 才能合并
            // 这防止跨阶合并 (例如 order=3 的块与 order=5 的块合并)
            if buddy_state != order {
                break;
            }

            // 从空闲链表中移除 buddy
            self.buddy_list_remove(buddy_pfn, order);
            store.meta_write(buddy_pfn as usize, BUDDY_ALLOCATED);

            pfn = core::cmp::min(pfn, buddy_pfn);
            order += 1;
        }

        store.meta_write(pfn as usize, order);
        (pfn, order)
    }

    /// 从双向链表中移除一个空闲块.
    ///
    /// H-01 (2026-09-06): 索引式 — FREE_LINKS[pfn] 读写 prev/next (存 pfn),
    /// 不再解引用物理页内节点, 不依赖 KERNEL_BASE/物理地址换算.
    fn buddy_list_remove(&self, pfn: u64, order: u8) {
        let Some(store) = self.meta_store() else {
            return;
        };
        // 前置断言: pfn 越界 = FREE_LINKS 越界访问
        debug_assert!(pfn < self.info.get().total_pages);
        let prev = store.links_read_prev(pfn as usize);
        let next = store.links_read_next(pfn as usize);
        if prev == SENTINEL {
            store.heads_set(order, next);
        } else {
            store.links_set_next(prev as usize, next);
        }
        if next != SENTINEL {
            store.links_set_prev(next as usize, prev);
        }
    }

    /// 将一个块压入空闲链表头.
    ///
    /// # 链表/位图同步 (B05-55)
    /// 压入前强制将块内所有页的位图清为 0 (空闲).
    /// 原因: `do_free`/`buddy_free_insert_range` 的 `buddy_try_merge` 向上合并
    /// 伙伴时只清除原块位图, 伙伴页位图可能残留 =1 (历史分配未同步),
    /// 若不清除则链表含"在位页" → 二次分配 → 页表/内核数据被覆盖.
    /// 伙伴必须满足 meta=order (空闲) 才会被合并, 故 push 块不含 reserve 区.
    ///
    /// H-01 (2026-09-06): 索引式 — 写 FREE_LINKS[pfn].prev/next (存 pfn).
    fn buddy_list_push(&self, pfn: u64, order: u8) {
        // 强制位图同步: 链表是空闲权威, push 即声明这些页 free
        let npages = 1u64 << u64::from(order);
        for i in 0..(npages as usize) {
            self.clear_bit(pfn as usize + i);
        }
        let Some(store) = self.meta_store() else {
            return;
        };
        // 前置断言: pfn 越界 = FREE_LINKS 越界访问
        debug_assert!(pfn < self.info.get().total_pages);
        let old_head = store.heads_get(order);
        store.links_set_prev(pfn as usize, SENTINEL);
        store.links_set_next(pfn as usize, old_head);
        if old_head != SENTINEL {
            store.links_set_prev(old_head as usize, pfn);
        }
        store.heads_set(order, pfn);
    }

    /// 从空闲链表头弹出一个块, 返回 pfn.
    ///
    /// H-01 (2026-09-06): 索引式 — 读 FREE_LINKS[head].next (存 pfn).
    fn buddy_list_pop(&self, order: u8) -> Option<u64> {
        let Some(store) = self.meta_store() else {
            return None;
        };
        let pfn = store.heads_get(order);
        if pfn == SENTINEL {
            return None;
        }
        // 前置断言: 索引式链表要求 pfn < total_pages (越界 = OOB 访问)
        debug_assert!(pfn < self.info.get().total_pages);
        let next = store.links_read_next(pfn as usize);
        store.heads_set(order, next);
        if next != SENTINEL {
            store.links_set_prev(next as usize, SENTINEL);
        }
        Some(pfn)
    }

    /// 将 [start_pfn, start_pfn+npages) 范围内已清位图 (free) 的页压回 buddy 空闲链表,
    /// 并尝试与相邻空闲块合并.
    ///
    /// 用于 `unreserve_range` / `free_huge_page` 等把先前摘出 buddy 的页归还回池,
    /// 保证空闲链表与位图严格同步.
    /// 调用方必须持有 PMM 锁; 保证范围内各页位图均为 0 (free) 且当前不在空闲链表中.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn buddy_free_insert_range(&self, start_pfn: u64, npages: u64) {
        // buddy 元数据未就绪 (早期/未初始化阶段) 时, 不操作空闲链表
        if self.meta_store().is_none() {
            return;
        }
        let end_pfn = start_pfn + npages;
        let mut cur = start_pfn;
        while cur < end_pfn {
            let remaining = end_pfn - cur;
            // 找 cur 自然对齐且 ≤ remaining 的最大 2 的幂块
            let mut order =
                (u64::BITS - 1 - remaining.leading_zeros()).min(u32::from(MAX_BUDDY_ORDER));
            while order > 0 {
                let size = 1u64 << order;
                if cur.is_multiple_of(size) && size <= remaining {
                    break;
                }
                order -= 1;
            }

            // 与相邻空闲伙伴合并 (伙伴不满足同阶空闲则停), 然后压入空闲链表
            // limit_pfn = end_pfn: 合并不得越过本范围, 防止吞掉相邻 reserve 区
            let (merged_pfn, merged_order) = self.buddy_try_merge(cur, order as u8, end_pfn);
            self.buddy_list_push(merged_pfn, merged_order);

            // 关键修复 (B05-55): 压回的合并块可能比原块大 (向上合并了伙伴).
            // 必须推进到合并块的末尾, 否则合并块内的页面会被后续迭代再次压入
            // → 同一物理页在空闲链表中出现两次 → 二次分配 → 页表/内核数据被覆盖.
            // 原实现 cur += block_size (原阶大小), 在非对齐范围 (如 swap 16MB
            // reserve 于非 2 的幂对齐基址) 时触发重复压入.
            cur = merged_pfn + (1u64 << merged_order);
        }
    }

    /// 从 buddy 空闲链表摘除与 [start_pfn, start_pfn+npages) 重叠的所有块,
    /// 将块中不重叠的部分重新压回空闲链表, 然后置位该范围的位图并更新统计.
    ///
    /// 用于 `reserve_range` / `buddy_direct_alloc_aligned` 等"位图式"预留:
    /// 保证空闲链表与位图严格同步, 避免含已分配/预留页的块滞留在空闲链表中,
    /// 否则 buddy_alloc 分裂时会把已占用页 push 回空闲链表, 写坏其内容.
    /// 调用方必须持有 PMM 锁; 保证 [start_pfn, ...) 各页位图当前均为 0 (free).
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn buddy_reserve_pfn_range(&self, start_pfn: u64, npages: u64) {
        let end_pfn = start_pfn + npages;
        // H-03 (2026-09-06): 索引式遍历 — FREE_LINKS 已由 init_bitmap 分配,
        // 直接以 pfn 读写链表关系, 不再做物理地址校验 / 解引用物理页.
        let Some(store) = self.meta_store() else {
            return;
        };

        // SAFETY: buddy_reserve_deferred 仅在持有 PMM 锁时访问 (本函数内独占)
        let deferred: &mut alloc::vec::Vec<(u64, u64, u64, u64)> =
            unsafe { &mut *self.buddy_reserve_deferred.get() };
        deferred.clear();

        // 逐阶遍历空闲链表, 摘除与预留范围重叠的块
        for order in 0..=MAX_BUDDY_ORDER {
            let mut cur = store.heads_get(order);
            while cur != SENTINEL {
                // H-03: 先存 next 再可能 remove (remove 会改写链表关系)
                let next = store.links_read_next(cur as usize);
                let block_size = 1u64 << order;
                if cur < end_pfn && cur + block_size > start_pfn {
                    // 重叠: 整块摘除, 元数据整块标记为已分配 (防止后续错误合并)
                    self.buddy_list_remove(cur, order);
                    for i in 0..block_size {
                        store.meta_write((cur + i) as usize, BUDDY_ALLOCATED);
                    }
                    // 不重叠部分暂不压回: 待位图置位后再压回,
                    // 使 buddy_free_insert_range 的合并不会吞掉 reserve 区
                    // (否则合并块覆盖 [start,end) → 压回后置位图 → 链表含在位页 → 二次分配)
                    deferred.push((cur, start_pfn, end_pfn, block_size));
                }
                cur = next;
            }
        }

        // 置位位图并更新统计
        for i in start_pfn as usize..end_pfn as usize {
            self.set_bit(i);
        }
        self.stats_alloc(npages);

        // 位图置位后, 压回不重叠部分 (buddy_try_merge 遇位图=1 的伙伴会停止合并)
        while let Some((block_pfn, s, e, size)) = deferred.pop() {
            if block_pfn < s {
                self.buddy_free_insert_range(block_pfn, s - block_pfn);
            }
            let block_end = block_pfn + size;
            if block_end > e {
                let right_start = core::cmp::max(block_pfn, e);
                self.buddy_free_insert_range(right_start, block_end - right_start);
            }
        }
    }

    /// 指定阶数执行核心分配.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    fn buddy_alloc(&self, order: u8) -> Option<(u64, u8)> {
        if order > MAX_BUDDY_ORDER {
            return None;
        }
        let store = match self.meta_store() {
            Some(s) => s,
            None => return None,
        };

        // 寻找 >= 请求阶数的最小可用阶
        let mut avail_order: Option<u8> = None;
        for o in order..=MAX_BUDDY_ORDER {
            let h = store.heads_get(o);
            if h != SENTINEL {
                avail_order = Some(o);
                break;
            }
        }
        let alloc_order = avail_order?;

        let pfn = self.buddy_list_pop(alloc_order)?;

        store.meta_write(pfn as usize, BUDDY_ALLOCATED);

        // 向下分裂, 直至达到请求阶数
        let cur_pfn = pfn;
        let mut cur_order = alloc_order;
        while cur_order > order {
            cur_order -= 1;
            let buddy_pfn = cur_pfn + (1u64 << cur_order);
            self.buddy_list_push(buddy_pfn, cur_order);
            store.meta_write(buddy_pfn as usize, cur_order);
        }
        store.meta_write(cur_pfn as usize, BUDDY_ALLOCATED);

        Some((cur_pfn, order))
    }

    /// 主 `do_alloc`: 处理早期分配与 buddy 分配.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn do_alloc(&self, order: u8) -> Option<PhysAddr> {
        if !self.initialized.load(Ordering::Acquire) {
            return if order == 0 {
                self.early_alloc_single()
            } else {
                self.early_alloc_multiple(1u64 << u64::from(order))
            };
        }

        if !self.buddy_ready.load(Ordering::Acquire) {
            // init 完成但 buddy 还未就绪: 回退到 bitmap 扫描
            let count = 1usize << order as usize;
            return self.alloc_from_bitmap_fallback(count);
        }

        let (pfn, _) = self.buddy_alloc(order)?;
        let addr = page_to_phys(pfn);
        let npages = 1u64 << u64::from(order);
        for i in 0..(npages as usize) {
            self.set_bit((pfn as usize) + i);
        }
        self.stats_alloc(npages);
        Some(PhysAddr(addr))
    }

    /// 主 `do_free`: 处理 buddy 或 bitmap 释放.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn do_free(&self, addr: PhysAddr, order: u8) {
        if !self.initialized.load(Ordering::Acquire) {
            klog_pmm!("[PMM] Warn: free before bitmap init at 0x{:X}", addr.0);
            return;
        }

        let info = self.info.get();
        let pfn = phys_to_page(addr.0);
        if pfn >= info.total_pages {
            klog_pmm!("[PMM] Error: invalid page 0x{:X}", addr.0);
            return;
        }

        // M1 修复: 在 buddy_ready 前后都检测 double-free
        // 位图约定: 1 = 已分配, 0 = 空闲
        if !self.test_bit(pfn as usize) {
            klog_pmm!(
                "[PMM] Warn: double free at pfn {} (addr=0x{:X})",
                pfn,
                addr.0
            );
            return;
        }

        if !self.buddy_ready.load(Ordering::Acquire) {
            let npages = 1u64 << u64::from(order);
            for i in 0..(npages as usize) {
                self.clear_bit(pfn as usize + i);
            }
            self.stats_free(npages);
            return;
        }

        // Clear bitmap
        let npages = 1u64 << u64::from(order);
        for i in 0..(npages as usize) {
            self.clear_bit(pfn as usize + i);
        }

        // 合并并压入空闲链表 (do_free 无范围限制, limit = total_pages)
        let (merged_pfn, merged_order) =
            self.buddy_try_merge(pfn, order, self.info.get().total_pages);
        self.buddy_list_push(merged_pfn, merged_order);
        self.stats_free(npages);
    }

    /// 扫描所有空闲页 (位未置位), 合并为最大阶的 buddy 块.
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    fn buddy_init_free_lists(&self, total_pages: usize) {
        let store = match self.meta_store() {
            Some(s) => s,
            None => return,
        };

        let mut pfn = 0usize;
        while pfn < total_pages {
            if self.test_bit(pfn) {
                pfn += 1;
                continue;
            }

            // 寻找连续空闲段
            let run_start = pfn;
            while pfn < total_pages && !self.test_bit(pfn) {
                pfn += 1;
            }
            let run_len = pfn - run_start;

            // 合并为最大阶 buddy 块
            let mut cur = run_start as u64;
            let mut remaining = run_len;
            while remaining > 0 {
                // ≤ remaining 的最大 2 的幂, 对齐到自身大小
                // H-04 (2026-09-09): 原 `(remaining - 1).leading_zeros()` 在
                // remaining == 1 时 `0.leading_zeros()` = 64 → `64-1-64` 下溢
                // (host debug 暴露; release 下 wrap-around 为 UB 碰巧工作).
                // checked_ilog2: remaining==1 → None → 0 (order-0 单页, 正确语义);
                // 其余与 `BITS-1-leading_zeros` 恒等.
                let max_order = (remaining - 1)
                    .checked_ilog2()
                    .unwrap_or(0)
                    .min(u32::from(MAX_BUDDY_ORDER)) as u8;
                // 寻找 cur 自然对齐 且 2^order ≤ remaining 的最大阶
                let mut order = max_order;
                while order > 0 {
                    let size = 1usize << order as usize;
                    if (cur as usize).is_multiple_of(size) && size <= remaining {
                        break;
                    }
                    order -= 1;
                }
                let block_size = 1usize << order as usize;

                store.meta_write(cur as usize, order);
                self.buddy_list_push(cur, order);

                cur += block_size as u64;
                remaining -= block_size;
            }
        }
    }

    /// 1GB 页直接对齐分配 (超出 buddy 范围).
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn buddy_direct_alloc_aligned(&self, count: usize, alignment: u64) -> Option<PhysAddr> {
        let total = self.info.get().total_pages as usize;
        let align_pages = (alignment / PAGE_SIZE) as usize;
        let mut i = align_pages; // 跳过 page 0
        while i + count <= total {
            if i.is_multiple_of(align_pages) {
                let mut ok = true;
                for j in 0..count {
                    if self.test_bit(i + j) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    // 摘除空闲链表中重叠的块 + 置位位图 + 更新统计
                    self.buddy_reserve_pfn_range(i as u64, count as u64);
                    return Some(PhysAddr(page_to_phys(i as u64)));
                }
            }
            i += align_pages;
        }
        None
    }

    /// 回退 bitmap 扫描 (在 init 完成但 buddy 还未就绪, 或 buddy 关闭时使用).
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn alloc_from_bitmap_fallback(&self, count: usize) -> Option<PhysAddr> {
        let total = self.info.get().total_pages as usize;
        for i in 0..total {
            if self.test_bit(i) {
                continue;
            }
            if i + count > total {
                return None;
            }
            let mut ok = true;
            for j in 1..count {
                if self.test_bit(i + j) {
                    ok = false;
                    break;
                }
            }
            if ok {
                for j in 0..count {
                    self.set_bit(i + j);
                }
                self.stats_alloc(count as u64);
                return Some(PhysAddr(page_to_phys(i as u64)));
            }
        }
        None
    }

    // ==================== 早期分配器 (bitmap 之前) ====================

    fn early_alloc_single(&self) -> Option<PhysAddr> {
        let current = self.early_current.fetch_add(PAGE_SIZE, Ordering::Relaxed);
        let aligned = (current + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        self.early_current
            .store(aligned + PAGE_SIZE, Ordering::Relaxed);

        let idx = self.early_count.fetch_add(1, Ordering::Relaxed);
        if idx < MAX_EARLY_ALLOCS {
            // SAFETY: idx < MAX_EARLY_ALLOCS 上界检查保证 early_allocs.add(idx) 不越界;
            // early_allocs 由构造时 OnceCell 初始化为定长数组, 类型为 EarlyAlloc.
            unsafe {
                let a = (*self.early_allocs.get()).as_mut_ptr().add(idx);
                (*a).addr = aligned;
                (*a).size = PAGE_SIZE;
            }
        }
        if aligned >= RAM_BASE + self.mem_size.get() {
            klog_pmm!("[PMM] Error: early alloc OOM");
            return None;
        }
        Some(PhysAddr(aligned))
    }

    fn early_alloc_multiple(&self, count: u64) -> Option<PhysAddr> {
        let size = count * PAGE_SIZE;
        let current = self.early_current.fetch_add(size, Ordering::Relaxed);
        let aligned = (current + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        self.early_current.store(aligned + size, Ordering::Relaxed);

        let idx = self.early_count.fetch_add(1, Ordering::Relaxed);
        if idx < MAX_EARLY_ALLOCS {
            // SAFETY: idx < MAX_EARLY_ALLOCS 守护, size = count*PAGE_SIZE, 记录多页范围.
            unsafe {
                let a = (*self.early_allocs.get()).as_mut_ptr().add(idx);
                (*a).addr = aligned;
                (*a).size = size;
            }
        }
        if aligned + size > RAM_BASE + self.mem_size.get() {
            klog_pmm!("[PMM] Error: early multi alloc OOM");
            return None;
        }
        Some(PhysAddr(aligned))
    }
}

// ==================== 全局单例与初始化 ====================

static GLOBAL_PMM: OnceLock<PhysicalMemoryManager> = OnceLock::new();

/// 初始化全局 PMM 单例 (幂等: 已初始化则复用既有实例).
pub fn pmm_init(mem_size: u64, kernel_end: u64) -> &'static PhysicalMemoryManager {
    GLOBAL_PMM.get_or_init(|slot| {
        let pmm = PhysicalMemoryManager::new();
        pmm.init(mem_size, kernel_end);
        slot.write(pmm);
    })
}

/// 初始化物理内存位图。
/// # Panics
/// 在 `pmm_init` 之前调用时 panic。
pub fn pmm_init_bitmap(reserved_after_kernel: u64) {
    let pmm = GLOBAL_PMM
        .get()
        .expect("[PMM] pmm_init_bitmap before pmm_init");
    pmm.init_bitmap(reserved_after_kernel);
}

/// 获取全局 PMM 单例 (未初始化则 panic).
pub fn get_pmm() -> &'static PhysicalMemoryManager {
    GLOBAL_PMM.get_or_panic("PMM")
}

// ==================== 屏障与回滚 ====================

#[derive(Clone, Copy)]
struct PmmSnapshot {
    total_allocs: u64,
    total_frees: u64,
    failed_allocs: u64,
    info: super::MemoryInfo,
}

static PMM_SNAPSHOT: IrqSpinLock<Option<PmmSnapshot>> = IrqSpinLock::new(None);

/// 捕获 PMM 统计快照 (供 FREG 故障恢复回滚).
pub fn pmm_freg_capture() {
    let pmm = get_pmm();
    let mut snap = PMM_SNAPSHOT.lock();
    *snap = Some(PmmSnapshot {
        total_allocs: pmm.total_allocs.load(Ordering::Relaxed),
        total_frees: pmm.total_frees.load(Ordering::Relaxed),
        failed_allocs: pmm.failed_allocs.load(Ordering::Relaxed),
        info: pmm.info.get(),
    });
}

/// 将 PMM 统计回滚到最近一次 `pmm_freg_capture` 捕获的快照.
pub fn pmm_freg_rollback() -> bool {
    let pmm = get_pmm();
    let snap = PMM_SNAPSHOT.lock();
    if let Some(ref s) = *snap {
        pmm.total_allocs.store(s.total_allocs, Ordering::Relaxed);
        pmm.total_frees.store(s.total_frees, Ordering::Relaxed);
        pmm.failed_allocs.store(s.failed_allocs, Ordering::Relaxed);
        pmm.info.set(s.info);
    }
    true
}

fn pmm_freg_capture_cb() {
    pmm_freg_capture();
}
fn pmm_freg_rollback_cb() -> bool {
    pmm_freg_rollback()
}

/// 将 PMM 注册为 FREG 恢复域 (域号 3), 并绑定捕获 / 回滚回调.
pub fn pmm_register_freg_domain() {
    crate::privileged::freg::recovery_domain_register(3);
    if let Some(dom) = crate::privileged::freg::RECOVERY_MANAGER.lock().find(3) {
        *dom.capture_cb.lock() = Some(pmm_freg_capture_cb);
        *dom.rollback_cb.lock() = Some(pmm_freg_rollback_cb);
    }
}
