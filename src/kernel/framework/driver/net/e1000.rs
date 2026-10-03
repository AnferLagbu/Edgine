#[cfg(test)]
use crate::framework::mm::KERNEL_BASE;
use crate::framework::mm::virt_to_phys;
use alloc::vec::Vec;

// ============================================================================
// B04-19: 描述符结构 + 常量已从 services 上移至 framework::driver::net::dma_ring.
// 反向依赖解除 (框架不再 use services).
// ============================================================================

pub use crate::framework::driver::net::dma_ring::{
    E1000_RX_BUFFER_SIZE, E1000_RX_RING_SIZE, E1000_RXD_ERR_CE, E1000_RXD_ERR_RXE,
    E1000_RXD_ERR_SE, E1000_RXD_ERR_SEQ, E1000_RXD_STAT_DD, E1000_TX_RING_SIZE, E1000_TXD_CMD_EOP,
    E1000_TXD_CMD_IFCS, E1000_TXD_CMD_RS, E1000_TXD_STAT_DD, E1000RxDesc, E1000TxDesc,
};

// ============================================================================
// 虚拟地址 → 物理地址转换
// ============================================================================
//
// 复用 mm::virt_to_phys (基于 KERNEL_BASE 常量, 自动适配架构:
// - x86_64: KERNEL_BASE=0xFFFF800000000000, 减去得物理地址
// - aarch64: KERNEL_BASE=0 (恒等映射), VA==PA, 减 0 无变化)
//
// 本文件不重复定义 virt_to_phys, 避免 I-53 架构互斥 cfg 检查失败.

// ============================================================================
// DMA 描述符环安全包装 (framework 层, 封装 unsafe 指针操作)
// ============================================================================

/// TX 描述符环安全包装
///
/// 封装 E1000 TX DMA 描述符环的 unsafe 指针操作, 提供安全公共 API。
/// 内部管理描述符内存分配、物理地址转换、DD 状态检查。
pub struct TxRing {
    ptr: *mut E1000TxDesc,
    phys: u64,
    count: usize,
    tail: usize,
}

impl TxRing {
    /// 分配并初始化 TX 描述符环
    ///
    /// 通过 `DmaEngine::alloc_coherent` 分配物理连续、4KiB 对齐的 DMA 一致内存,
    /// 该内存已自动清零, 且 cache 已 flush, 满足 E1000 硬件 DMA 访问要求。
    #[cfg(not(feature = "kernel_test"))]
    pub fn alloc(count: usize) -> Option<Self> {
        // B04-17: count == 0 时分配大小为 0, 后续解引用越过分配边界 → 任意内存读/写。
        if count == 0 {
            return None;
        }
        let size = core::mem::size_of::<E1000TxDesc>() * count;
        // alloc_coherent 返回 (虚拟地址, 物理地址); 内存已清零, 仅需将 DD 置位
        // 表示描述符初始可用 (硬件发送完成后同样置 DD)。
        let (virt, phys) = crate::framework::dma::get_dma().alloc_coherent(size)?;
        let desc_ptr = virt.0 as *mut E1000TxDesc;
        for i in 0..count {
            // SAFETY: desc_ptr 由 alloc_coherent 分配, 大小为 size;
            // i < count 保证索引在分配范围内。
            unsafe {
                (*desc_ptr.add(i)).status = E1000_TXD_STAT_DD;
            }
        }
        Some(Self {
            ptr: desc_ptr,
            phys: phys.0,
            count,
            tail: 0,
        })
    }

    /// TX 描述符环物理地址 (用于硬件 TDBAL/TDBAH)
    pub fn phys_addr(&self) -> u64 {
        self.phys
    }

    /// TX 描述符环字节长度 (用于硬件 TDLEN)
    pub fn len_bytes(&self) -> usize {
        self.count * core::mem::size_of::<E1000TxDesc>()
    }

    /// 当前 tail 索引
    pub fn tail(&self) -> usize {
        self.tail
    }

    /// 准备一个描述符用于发送 (物理地址版本)
    ///
    /// 设置 buffer 物理地址、长度、命令字, 清除 DD 状态。
    /// 调用方需先确认当前 tail 位置的描述符已完成 (DD=1)。
    pub fn prepare(&mut self, buf_phys: u64, buf_len: u16) {
        // SAFETY: tail 在 0..count 范围内; ptr 由 alloc_coherent 分配且大小足够。
        let desc = unsafe { &mut *self.ptr.add(self.tail) };
        desc.addr = buf_phys;
        desc.length = buf_len;
        desc.cmd = E1000_TXD_CMD_EOP | E1000_TXD_CMD_IFCS | E1000_TXD_CMD_RS;
        desc.status = 0;
    }

    /// 准备一个描述符用于发送 (虚拟地址版本, 内部转换物理地址)
    pub fn prepare_from_virt(&mut self, buf_virt: u64, buf_len: u16) {
        let buf_phys = virt_to_phys(buf_virt);
        self.prepare(buf_phys, buf_len);
    }

    /// 检查指定索引的描述符是否完成 (DD bit)
    pub fn is_done(&self, idx: usize) -> bool {
        // SAFETY: idx 在 0..count 范围内; ptr 已分配。
        let desc = unsafe { &*self.ptr.add(idx) };
        desc.status & E1000_TXD_STAT_DD != 0
    }

    /// 推进 tail 指针到下一个描述符
    pub fn advance_tail(&mut self) {
        self.tail = (self.tail + 1) % self.count;
    }
}

/// RX 描述符环安全包装
///
/// 封装 E1000 RX DMA 描述符环的 unsafe 指针操作, 提供安全公共 API。
/// 内部管理描述符内存分配、接收缓冲区分配、物理地址转换、DD 状态检查。
pub struct RxRing {
    ptr: *mut E1000RxDesc,
    phys: u64,
    count: usize,
    bufs: Vec<*mut u8>,
    tail: usize,
    buf_size: usize,
}

impl RxRing {
    /// 分配并初始化 RX 描述符环及接收缓冲区
    ///
    /// 描述符环与每个接收缓冲区均通过 `DmaEngine::alloc_coherent` 分配物理连续、
    /// 4KiB 对齐的 DMA 一致内存 (自动清零 + cache flush), 满足 E1000 硬件要求。
    #[cfg(not(feature = "kernel_test"))]
    pub fn alloc(count: usize, buf_size: usize) -> Option<Self> {
        // B04-17: count == 0 或 buf_size == 0 时分配大小为 0,
        // 后续解引用越过分配边界 → 任意内存读/写。
        if count == 0 || buf_size == 0 {
            return None;
        }
        let size = core::mem::size_of::<E1000RxDesc>() * count;
        let (virt, phys) = crate::framework::dma::get_dma().alloc_coherent(size)?;
        let desc_ptr = virt.0 as *mut E1000RxDesc;
        let mut bufs = Vec::new();
        for i in 0..count {
            // alloc_coherent 分配物理连续的接收缓冲区 (已清零)。
            let (buf_virt, buf_phys) = crate::framework::dma::get_dma().alloc_coherent(buf_size)?;
            bufs.push(buf_virt.0 as *mut u8);
            // SAFETY: desc_ptr 已分配; i < count。
            unsafe {
                (*desc_ptr.add(i)).addr = buf_phys.0;
            }
        }
        Some(Self {
            ptr: desc_ptr,
            phys: phys.0,
            count,
            bufs,
            tail: 0,
            buf_size,
        })
    }

    /// RX 描述符环物理地址 (用于硬件 RDBAL/RDBAH)
    pub fn phys_addr(&self) -> u64 {
        self.phys
    }

    /// RX 描述符环字节长度 (用于硬件 RDLEN)
    pub fn len_bytes(&self) -> usize {
        self.count * core::mem::size_of::<E1000RxDesc>()
    }

    /// 当前 tail 索引
    pub fn tail(&self) -> usize {
        self.tail
    }

    /// 环大小 (描述符个数)
    pub fn count(&self) -> usize {
        self.count
    }

    /// 检查指定索引的描述符是否包含就绪数据包 (DD bit)
    pub fn is_done(&self, idx: usize) -> bool {
        // SAFETY: idx 在 0..count 范围内; ptr 已分配。
        let desc = unsafe { &*self.ptr.add(idx) };
        desc.status & E1000_RXD_STAT_DD != 0
    }

    /// 检查描述符是否有接收错误
    pub fn has_errors(&self, idx: usize) -> bool {
        // SAFETY: idx 在 0..count 范围内; ptr 已分配。
        let desc = unsafe { &*self.ptr.add(idx) };
        desc.errors & (E1000_RXD_ERR_CE | E1000_RXD_ERR_SE | E1000_RXD_ERR_SEQ | E1000_RXD_ERR_RXE)
            != 0
    }

    /// 获取描述符的接收长度
    pub fn packet_length(&self, idx: usize) -> usize {
        // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
        let desc = unsafe { &*self.ptr.add(idx) };
        desc.length as usize
    }

    /// 获取描述符的错误码
    pub fn errors(&self, idx: usize) -> u8 {
        // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
        let desc = unsafe { &*self.ptr.add(idx) };
        desc.errors
    }

    /// 从指定索引的缓冲区复制数据到调用方缓冲区
    pub fn copy_packet(&self, idx: usize, buf: &mut [u8]) -> usize {
        let len = self.packet_length(idx).min(buf.len()).min(self.buf_size);
        if !self.bufs[idx].is_null() && len > 0 {
            // SAFETY: bufs[idx] 由 alloc_coherent 分配, 大小为 buf_size (向上取整到页);
            // buf 由调用方保证有效; len <= buf_size && len <= buf.len()。
            unsafe {
                core::ptr::copy_nonoverlapping(self.bufs[idx], buf.as_mut_ptr(), len);
            }
        }
        len
    }

    /// 清除指定索引的 DD 状态位
    pub fn clear_status(&mut self, idx: usize) {
        // SAFETY: idx 在 0..count 范围内; ptr 已分配。
        let desc = unsafe { &mut *self.ptr.add(idx) };
        desc.status = 0;
    }

    /// 推进 tail 指针到下一个描述符
    pub fn advance_tail(&mut self) {
        self.tail = (self.tail + 1) % self.count;
    }
}

// SAFETY: TxRing 仅被单一网络驱动实例独占持有并串行访问 (单核内核, 无跨核共享);
// 内部裸指针指向 alloc_coherent 分配的 DMA 一致内存, 生命周期与 TxRing 绑定,
// 故跨线程传递所有权是安全的。
unsafe impl Send for TxRing {}

// SAFETY: RxRing 同上; bufs 内所有裸指针均为 alloc_coherent 分配的 DMA 一致内存,
// 仅在驱动串行访问路径中读取。
unsafe impl Send for RxRing {}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(E1000_TX_RING_SIZE, 64);
        assert_eq!(E1000_RX_RING_SIZE, 128);
        assert_eq!(E1000_RX_BUFFER_SIZE, 2048);
    }

    #[test]
    fn test_descriptor_sizes() {
        assert_eq!(core::mem::size_of::<E1000TxDesc>(), 16);
        assert_eq!(core::mem::size_of::<E1000RxDesc>(), 16);
    }

    #[test]
    fn test_virt_to_phys_conversion() {
        let high_addr: u64 = KERNEL_BASE;
        assert_eq!(virt_to_phys(high_addr), 0);
        // virt_to_phys 仅对内核空间地址 (>= KERNEL_BASE) 成立;
        // 原用例传低地址 0x12345678 导致下溢 panic (2026-09-24 UT-06 实测修正).
        assert_eq!(virt_to_phys(KERNEL_BASE + 0x12345678), 0x12345678);
    }
}
