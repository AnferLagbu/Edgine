//! DMA 引擎 API 层
//!
//! 一致性 DMA 内存管理、ioremap MMIO 映射、流式 DMA、散射聚集列表的统一入口。
//!
//! ## 调用方契约
//! - `driver::storage::nvme` —— `NVMe` 命令队列的 DMA 缓冲区
//! - `driver::storage::ahci` —— AHCI PRDT 表的 DMA 映射
//! - `driver::net::e1000` —— E1000 收发描述符的 DMA 映射
//! - `driver::virtio::blk` —— `VirtIO` 队列的 DMA 映射
//!   (批次 Z ④: virtio-net 权威已迁 services, 其 DMA 路径经 framework
//!   `DmaBuffer` 机制, 见 services/driver/virtio/net.rs)
//! - `fs::unkfs` —— `UNKFS` 页缓存直接 I/O (通过 DMA 绕过 CPU)
//!
//! ## 内部接口
//! - `mod.rs` —— 公开类型: `DmaMapping`, `DmaTransfer`, `DmaScatterList`
//! - `engine.rs` —— `DmaEngine` 实现
//!
//! ## 安全约束
//! - DmaTransfer.callback 函数指针在 ISR 上下文调用, 不得持有锁或 sleep
//! - `DmaMapping.cpu_addr` 和 `dma_addr` 必须保持一致 (x86: 自动; aarch64: 需 cache flush)
//! - MMIO 映射虚拟地址范围: [0xFFFF900000000000, ...)
//!
//! ## 性能特征
//! - 映射查找: O(1) 哈希 (`MAX_MAPPINGS` = 256)
//! - 散射聚集: 最多 64 条目
//! - 统计: lock-free `AtomicU64`, ISR 安全

use crate::framework::mm::PhysAddr;

// ============================================================================
// 契约常量
// ============================================================================

pub const DMA_MAX_MAPPINGS: usize = 256;
pub const DMA_MAX_SCATTER_ENTRIES: usize = 64;

// ============================================================================
// 契约类型
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaDirection {
    ToDevice,
    FromDevice,
    Bidirectional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaCachePolicy {
    None,
    Writeback,
    Writethrough,
}

/// DMA 映射描述符 (契约视角: 只暴露驱动需要的信息)
#[derive(Debug)]
pub struct DmaMapping {
    pub dma_addr: PhysAddr,
    pub size: usize,
    pub direction: DmaDirection,
}

/// 散射聚集列表条目
#[derive(Clone, Copy, Debug)]
pub struct DmaScatterEntry {
    pub phys_addr: u64,
    pub length: usize,
}

// ============================================================================
// 契约 trait: DmaEngine — guideline §4.1 明确要求
// ============================================================================

/// DMA 引擎抽象。
///
/// `Edgine` 当前只有一个 `DmaEngine` 实例, trait 化是为了
/// 未来架构差异 (x86 自动一致 vs aarch64 需显式 flush) 的策略注入。
pub trait DmaEngine: Send + Sync {
    /// 分配一致性 DMA 缓冲区, 返回 (`cpu_vaddr`, `dma_phys`)
    fn alloc_coherent(&self, size: usize) -> Option<(*mut u8, PhysAddr)>;

    /// 释放一致性 DMA 缓冲区
    fn free_coherent(&self, cpu_vaddr: *mut u8, dma_phys: PhysAddr, size: usize);

    /// 创建流式 DMA 映射
    fn map_stream(&self, buf: &[u8], dir: DmaDirection) -> Option<DmaMapping>;

    /// 解除流式 DMA 映射
    fn unmap_stream(&self, mapping: &DmaMapping);

    /// 内存屏障: 确保 DMA 写入对 CPU 可见
    fn fence(&self);

    /// 获取统计快照
    fn stats(&self) -> DmaPoolStats;
}

/// DMA 池统计 (契约视角: 只读快照)
#[derive(Debug, Clone, Default)]
pub struct DmaPoolStats {
    pub total_allocations: u64,
    pub total_frees: u64,
    pub current_in_use: u64,
    pub total_bytes_allocated: u64,
}
