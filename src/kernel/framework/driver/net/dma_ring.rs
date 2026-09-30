//! E1000 DMA 描述符环 (硬件格式, repr(C))
//!
//! B04-19: 从 services::driver::net::e1000 上移到 framework/driver/net/dma_ring.rs,
//! 解除 framework ↔ services 双向依赖 (F3 循环依赖).
//!
//! 描述符结构与状态/命令位掩码是硬件 DMA 格式, 归属 framework TCB 范畴
//! (任何涉及 DMA 描述符 unsafe 操作的代码都引用此处).
//! 驱动业务逻辑 (services::driver::net::e1000 的 E1000NetDriver) 与 MMIO 安全访问器
//! (framework::driver::net::e1000_io 的 E1000Io) 分居 services / framework 两侧.

// ============================================================================
// 描述符结构体 (硬件格式, repr(C))
// ============================================================================

/// E1000 TX 描述符 (16 字节, 与硬件 DMA 格式一致)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct E1000TxDesc {
    pub addr: u64,
    pub length: u16,
    pub cso: u8,
    pub cmd: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

/// E1000 RX 描述符 (16 字节, 与硬件 DMA 格式一致)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct E1000RxDesc {
    pub addr: u64,
    pub length: u16,
    pub checksum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

// ============================================================================
// 描述符状态/命令常量
// ============================================================================

/// TX 描述符命令: End Of Packet
pub const E1000_TXD_CMD_EOP: u8 = 1 << 0;
/// TX 描述符命令: Insert FCS
pub const E1000_TXD_CMD_IFCS: u8 = 1 << 1;
/// TX 描述符命令: Report Status
pub const E1000_TXD_CMD_RS: u8 = 1 << 3;
/// TX 描述符状态: Descriptor Done
pub const E1000_TXD_STAT_DD: u8 = 1 << 0;

/// RX 描述符状态: Descriptor Done
pub const E1000_RXD_STAT_DD: u8 = 1 << 0;
/// RX 描述符错误: CRC Error
pub const E1000_RXD_ERR_CE: u8 = 1 << 0;
/// RX 描述符错误: Symbol Error
pub const E1000_RXD_ERR_SE: u8 = 1 << 1;
/// RX 描述符错误: Sequence Error
pub const E1000_RXD_ERR_SEQ: u8 = 1 << 2;
/// RX 描述符错误: Receive Error
pub const E1000_RXD_ERR_RXE: u8 = 1 << 3;

// ============================================================================
// 环大小常量 (TX/RX 描述符数, RX 缓冲区大小)
// ============================================================================

/// TX 描述符环大小
pub const E1000_TX_RING_SIZE: usize = 64;
/// RX 描述符环大小
pub const E1000_RX_RING_SIZE: usize = 128;
/// RX 缓冲区大小
pub const E1000_RX_BUFFER_SIZE: usize = 2048;
