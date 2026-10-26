//! `FrameAlloc` — 物理页分配器 trait (TCB)
//!
//! 策略注入点: functions 层通过此 trait 分配/释放物理帧,
//! 而不直接操作 Buddy 分配器或位图。
//!
//! ## 与 Asterinas OSTD `FrameAlloc` 的关系
//!
//! 等价于 OSTD 的 `FrameAlloc` trait。
//!
//! ## SAFETY 不变量
//!
//! - `alloc()` 返回的 Frame 必须唯一 (同一 phys 不被多次分配)。
//! - 调用方负责在释放前解除所有映射 (页表 / DMA)。
//! - 分配器内部用 spinlock 保护, ISR 安全。

use crate::privileged::mm::PageSize;

use super::super::frame::Frame;

/// 物理帧分配器 trait。
///
/// 当前实现在 `mm/pmm.rs` (Buddy 分配器),
/// 未来可替换为自定义策略。
pub trait FrameAlloc: Send + Sync {
    /// 分配一个物理帧 (order 4K or 2M)。
    fn alloc(&self, order: u8) -> Option<Frame>;

    /// 分配连续多个 4K 帧 (用于 DMA 缓冲区等)。
    ///
    /// 返回块按 2 的幂页数向上取整 (`Frame::size()` 可能大于 `count × 4KB`)。
    fn alloc_pages(&self, count: usize) -> Option<Frame>;

    /// 分配大页 (2MB / 1GB)。
    fn alloc_huge(&self, size: PageSize) -> Option<Frame>;

    /// 释放帧 (RAII 语义, 等价 `drop(frame)`; 计数归零才真正归还 PMM)。
    fn free(&self, frame: Frame);

    /// 剩余空闲页数。
    fn free_pages(&self) -> u64;

    /// 总页数。
    fn total_pages(&self) -> u64;
}

// ============================================================================
// 默认实现: 委托给现有 PMM
// ============================================================================

/// Buddy 分配器实现的 `FrameAlloc`。
pub struct BuddyFrameAlloc;

impl FrameAlloc for BuddyFrameAlloc {
    fn alloc(&self, order: u8) -> Option<Frame> {
        use crate::privileged::mm::api;
        if order == 0 {
            let phys = api::pmm_alloc_page_phys()?;
            // SAFETY: pmm_alloc_page_phys() guarantees unique ownership.
            unsafe { Some(Frame::from_raw(phys, 0)) }
        } else {
            let count = 1 << order;
            self.alloc_pages(count)
        }
    }

    fn alloc_pages(&self, count: usize) -> Option<Frame> {
        use crate::privileged::mm::api;
        if count == 0 {
            return None;
        }
        // 请求页数向上取整到 2 的幂: PMM 侧 `free_pages(count)` 亦经 `count_to_order`
        // 按 2 的幂推阶, 两者同值才能保证 `Frame.order` 与 buddy 实际返回块的阶一致
        // (否则 Frame 析构会按错阶归还块). 上限 512 页 = 2MB, 与 buddy
        // `MAX_BUDDY_ORDER = 9` 同界 (既有上限判据).
        let npages = count.checked_next_power_of_two().filter(|&n| n <= 512)?;
        let order = u8::try_from(npages.trailing_zeros()).ok()?;
        let phys = api::pmm_alloc_pages_phys(npages)?;
        // SAFETY: pmm_alloc_pages_phys() guarantees unique ownership.
        unsafe { Some(Frame::from_raw(phys, order)) }
    }

    #[expect(
        clippy::match_wildcard_for_single_variants,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    fn alloc_huge(&self, size: PageSize) -> Option<Frame> {
        use crate::privileged::mm::api;
        let phys = api::pmm_alloc_huge_page_phys(size)?;
        let order = match size {
            PageSize::Size2M => 9u8,
            PageSize::Size1G => 18u8,
            _ => 0u8,
        };
        // SAFETY: pmm_alloc_huge_page_phys() guarantees unique ownership.
        unsafe { Some(Frame::from_raw(phys, order)) }
    }

    /// 释放帧 (RAII 语义: 句柄析构即归还, 由 `Frame::drop` 承担;
    /// 计数归零才真正归还 PMM)
    fn free(&self, frame: Frame) {
        drop(frame);
    }

    fn free_pages(&self) -> u64 {
        crate::privileged::mm::pmm_get_free_pages()
    }

    fn total_pages(&self) -> u64 {
        crate::privileged::mm::pmm_get_total_pages()
    }
}
