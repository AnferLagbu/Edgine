//! Frame — 物理页安全抽象 (TCB)
//!
//! 将裸 `PhysAddr` 封装为类型安全句柄，防止 double-free / use-after-free / DMA 竞争。
//!
//! ## 与 Asterinas OSTD `Frame` 的关系
//!
//! 等价于 OSTD 的 `Frame<M>` 概念：句柄可共享 (`Clone` ⇒ 持有者 +1，
//! `Drop` ⇒ 持有者 −1)，持有计数由 PMM 按 pfn 索引维护，**仅归零才归还**物理页。
//! 元数据槽位 (`usize`) 可供 functions 层挂载自定义状态（如 slab 缓存索引、DMA pin 标志）。
//!
//! ## SAFETY 不变量
//!
//! - **归还恰好一次**: 持有计数归零只发生一次，物理归还仅由最后一个句柄的 `Drop` 触发。
//! - **释放前清理**: 释放 Frame 前确保无 DMA 缓冲区 / 页表条目引用。
//! - **对齐**: Frame 地址始终对齐到 `PAGE_SIZE` 边界。
//! - `from_raw()` 是唯一 unsafe 构造路径；functions 层通过 `FrameAlloc::alloc()` 获取。

use core::fmt;

use crate::privileged::mm::PAGE_SIZE;
use crate::privileged::mm::PhysAddr;

/// 一个可共享的物理帧句柄（持有计数由 PMM 维护）。
///
/// # Safety Invariant
/// 每个物理地址在同一时刻被一个或多个 `Frame` 句柄共同持有，
/// 句柄数即 PMM 侧的持有计数；计数归零前后续不得再构造句柄。
#[derive(Debug)]
pub struct Frame {
    phys: PhysAddr,
    order: u8,
    meta: usize,
}

impl Frame {
    /// 从裸物理地址构造 Frame。
    ///
    /// # SAFETY
    /// 调用方保证 `phys` 是有效的可分配物理地址，
    /// 且未被其他 `Frame` 实例持有。
    pub unsafe fn from_raw(phys: PhysAddr, order: u8) -> Self {
        debug_assert!(
            phys.as_u64().is_multiple_of(PAGE_SIZE as u64),
            "Frame must be page-aligned"
        );
        Self {
            phys,
            order,
            meta: 0,
        }
    }

    /// 物理地址
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn phys(&self) -> PhysAddr {
        self.phys
    }

    /// Buddy 阶数 (0 = 4KB, 9 = 2MB)
    #[inline(always)]
    pub fn order(&self) -> u8 {
        self.order
    }

    /// 帧大小 (字节)
    // 有意窄化: 用户内存代理, 指针/长度上下文保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn size(&self) -> usize {
        (PAGE_SIZE as usize) << self.order
    }

    /// 当前持有者数 (PMM 计数面, 0 = 未计数帧)
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn ref_count(&self) -> u32 {
        crate::privileged::mm::api::frame_ref_count(self.phys)
    }

    /// 登记一个额外持有者 (如被页表映射、DMA 缓冲引用)。
    ///
    /// 返回 `false` = 帧不处于计数态 (未计数块 / MMIO 地址), 未登记成功。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn inc_ref(&self) -> bool {
        crate::privileged::mm::api::frame_inc(self.phys)
    }

    /// 自定义元数据（functions 可挂载任意 usize 值）
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn meta(&self) -> usize {
        self.meta
    }

    /// 设置自定义元数据
    #[inline(always)]
    pub fn set_meta(&mut self, val: usize) {
        self.meta = val;
    }

    /// 转换为内核可访问的虚拟地址
    ///
    /// # B03-26 警告
    /// 返回 `*mut u8` 无生命周期绑定, Frame Drop 后指针悬挂。
    /// 优先使用 `as_virt_slice()` 安全 API (生命周期绑定 `&self`)。
    /// 仅在 FFI / DMA 等需要 raw pointer 的场景保留。
    pub fn as_virt_ptr(&self) -> *mut u8 {
        crate::privileged::mm::phys_to_virt(self.phys.as_u64()) as *mut u8
    }

    /// B03-26: 安全 API — 返回生命周期绑定 `&self` 的可变字节切片。
    /// Frame Drop 后切片自动失效, 防止悬挂指针 (I1 安全不变式)。
    pub fn as_virt_slice(&mut self) -> &mut [u8] {
        let ptr = self.as_virt_ptr();
        // SAFETY: Frame 拥有物理页, phys_to_virt 返回唯一内核映射 VA。
        // `&mut self` 保证独占借用, Frame 生命周期内指针有效。
        unsafe { core::slice::from_raw_parts_mut(ptr, crate::privileged::mm::PAGE_SIZE as usize) }
    }

    /// 零填充帧内容
    pub fn zero(&self) {
        let ptr = self.as_virt_ptr();
        // SAFETY: `as_virt_ptr()` 由 `phys_to_virt` 转换物理地址到内核虚拟地址;
        // `Frame` 持有的 `phys` 由 `FrameAlloc::allocate_frame` 返回, 保证:
        //   1. 物理地址是已分配 (free list 中扣除) 的 4K 对齐页
        //   2. 物理地址在 `phys_to_virt` 线性映射范围内 (内核高半区直接映射)
        //   3. `self.size()` 字节全部可写, 写 0 不会破坏其他数据结构
        //   4. 期间无并发写 (Frame 所有权唯一, 由 FrameAlloc 跟踪)
        unsafe {
            core::ptr::write_bytes(ptr, 0, self.size());
        }
    }
}

impl Clone for Frame {
    /// 派生一个共享同一物理帧的句柄 (持有者 +1)。
    ///
    /// 帧不处于计数态 (MMIO / 未计数块 / 计数已归零) 时 **硬失败**：
    /// 静默产生未计数句柄会让后续 `Drop` 把仍被使用的物理帧计入零。
    fn clone(&self) -> Self {
        assert!(
            crate::privileged::mm::api::frame_inc(self.phys),
            "Frame::clone 要求帧处于持有计数态 (分配后未归零)"
        );
        Self {
            phys: self.phys,
            order: self.order,
            meta: self.meta,
        }
    }
}

impl Drop for Frame {
    /// 注销本句柄的持有者 (持有者 −1)；**仅计数归零才归还**物理帧。
    fn drop(&mut self) {
        // host 维: 帧为纯算术载体 (host-tests/src/dma_stream.rs 经 from_raw 构造
        // 伪地址/MMIO 地址), 不触碰 PMM —— 与 sync/spinlock.rs 的
        // "host 无中断语义降为 no-op" 先例同型, 非平行实现.
        #[cfg(not(feature = "host-test"))]
        {
            use crate::privileged::mm::{api, pmm};
            if self.order == 0 {
                // 单页帧: 持有计数归零才归还 (frame_dec 返回 true = 无其他持有者)
                if api::frame_dec(self.phys) {
                    api::pmm_free_page_phys(self.phys);
                }
            } else if self.order <= pmm::MAX_BUDDY_ORDER {
                // 连续多帧块: 块内页不参与计数 (见 pmm::alloc_pages 的 SIMPLIFIED 契约),
                // 整块视为单一持有者, 按阶整块归还
                api::pmm_free_pages_phys(self.phys, 1usize << self.order);
            }
            // SIMPLIFIED: order > MAX_BUDDY_ORDER (1GB 大页, 经 alloc_huge(Size1G) 构造)
            // 无 buddy 阶可表达, 不归还 (fail-closed: 宁可泄漏也不按错阶释放 buddy 状态);
            // 影响面: 仅该形态帧 (当前零调用点); 何时需扩展: 出现 alloc_huge(Size1G)
            // 真实使用点时改走 pmm::free_huge_page(phys, PageSize::Size1G).
        }
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Frame(phys=0x{:x}, order={}, ref={})",
            self.phys.as_u64(),
            self.order,
            self.ref_count()
        )
    }
}

// SAFETY: Frame 是堆分配对象，Send + Sync 来自 PMM 计数面 (内部锁保护) 的安全并发访问。
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}
