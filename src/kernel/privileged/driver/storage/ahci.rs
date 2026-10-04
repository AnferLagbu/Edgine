//! AHCI/SATA 驱动 (AHCI/SATA Driver)
//!
//! 提供AHCI (Advanced Host Controller Interface) SATA支持：
//! - **SATA接口**: 传统SATA SSD和HDD
//! - **DMA读写**: 通过PRDT进行DMA传输
//! - **多端口**: 支持多个SATA端口
//! - **LBA48**: 支持大容量磁盘
//!
//! ## 硬件规格
//!
//! ```text
//! AHCI Controller:
//! ├── HBA Memory (ABAR)
//! │   ├── Generic Host Control (GHC)
//! │   ├── Port Registers (0x100 + 0x80*n)
//! │   │   ├── PxCLB: 命令列表基地址
//! │   │   ├── PxFB: FIS基地址
//! │   │   ├── PxIS: 中断状态
//! │   │   ├── PxIE: 中断使能
//! │   │   ├── PxCMD: 命令和状态
//! │   │   ├── PxTFD: 任务文件数据
//! │   │   └── PxCI: 命令发布
//! │   └── ...
//! └── Command List & FIS Buffer (DMA)
//! ```
//!
//! # Safety
//! AHCI驱动涉及MMIO寄存器和DMA操作。

// ============================================================================
// SATA 命令定义
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AtaCommand {
    ReadDma = 0x25,  // 读 DMA (LBA28) — 也用作 READ DMA EXT (LBA48)
    WriteDma = 0x35, // 写 DMA (LBA28) — 也用作 WRITE DMA EXT (LBA48)
    Identify = 0xEC,
    ReadFpdmaQueued = 0x60,
    WriteFpdmaQueued = 0x61,
}

// ============================================================================
// AHCI 数据结构
// ============================================================================

/// AHCI 命令头 (AHCI 1.3.1 §4.2.2, 32 字节)
///
/// 布局按规范固定: DW0 = CFL/W/PMP + PRDTL(高 16 位), DW1 = PRDBC,
/// DW2 = CTBA, DW3 = CTBAU, DW4-7 保留。PRDTL 不是独立 DW —— 布局错位
/// 会使硬件从 DW2 读到的 CTBA 为 0 (命令表指向物理地址 0), 设备静默
/// 丢弃命令 (首次带盘冒烟实证: 所有命令提交超时)。
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct AhciCommandHeader {
    pub dw0: u32,   // CFL(5) | A(1) | W(1) | P(1) | R(1) | B(1) | C(1) | PMP(4) | PRDTL(16)
    pub prdbc: u32, // DW1: PRDT 已传输字节计数 (硬件维护, 软件清零)
    pub ctba: u32,  // DW2: 命令表基址低32位
    pub ctbau: u32, // DW3: 命令表基址高32位
    pub rsvd: [u32; 4],
}

impl AhciCommandHeader {
    pub fn new() -> Self {
        Self {
            dw0: 0,
            prdbc: 0,
            ctba: 0,
            ctbau: 0,
            rsvd: [0; 4],
        }
    }
}

/// 物理区域描述符 (PRDT entry)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct PhysicalRegionDescriptor {
    pub dba: u32,  // 数据基址低32位
    pub dbau: u32, // 数据基址高32位
    pub rsvd: u32,
    pub dbc: u32, // 字节计数 (高1位 = 中断完成标记)
}

/// AHCI 命令表
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct AhciCommandTable {
    pub cfis: [u8; 64], // 命令FIS
    pub acmd: [u8; 16], // ATAPI命令
    pub rsvd: [u8; 48],
    pub prdt: [PhysicalRegionDescriptor; 8],
}

// ============================================================================
// FIS 结构
// ============================================================================

/// 主机到设备FIS
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct H2dFis {
    pub fis_type: u8,
    pub flags: u8,
    pub command: u8,
    pub feature0: u8,
    pub lba0: u8,
    pub lba1: u8,
    pub lba2: u8,
    pub lba3: u8,
    pub device: u8,
    pub lba4: u8,
    pub lba5: u8,
    pub feature1: u8,
    pub count0: u8,
    pub count1: u8,
    pub icc: u8,
    pub control: u8,
    pub rsvd: [u32; 4],
}

impl H2dFis {
    pub fn new() -> Self {
        Self {
            fis_type: 0,
            flags: 0,
            command: 0,
            feature0: 0,
            feature1: 0,
            lba0: 0,
            lba1: 0,
            lba2: 0,
            lba3: 0,
            lba4: 0,
            lba5: 0,
            device: 0,
            count0: 0,
            count1: 0,
            icc: 0,
            control: 0,
            rsvd: [0; 4],
        }
    }

    /// 创建读DMA FIS (LBA48)
    pub fn read_dma(lba: u64, count: u16) -> Self {
        let mut fis = Self::new();
        fis.fis_type = 0x27; // H2D
        fis.flags = 0x80; // 写命令
        fis.command = 0x25; // READ DMA EXT
        fis.device = 0x40; // LBA 模式
        fis.lba0 = (lba & 0xFF) as u8;
        fis.lba1 = ((lba >> 8) & 0xFF) as u8;
        fis.lba2 = ((lba >> 16) & 0xFF) as u8;
        fis.lba3 = ((lba >> 24) & 0xFF) as u8;
        fis.lba4 = ((lba >> 32) & 0xFF) as u8;
        fis.lba5 = ((lba >> 40) & 0xFF) as u8;
        fis.count0 = (count & 0xFF) as u8;
        fis.count1 = ((count >> 8) & 0xFF) as u8;
        fis
    }

    /// 创建写DMA FIS (LBA48)
    pub fn write_dma(lba: u64, count: u16) -> Self {
        let mut fis = Self::new();
        fis.fis_type = 0x27;
        fis.flags = 0x80;
        fis.command = 0x35; // WRITE DMA EXT
        fis.device = 0x40;
        fis.lba0 = (lba & 0xFF) as u8;
        fis.lba1 = ((lba >> 8) & 0xFF) as u8;
        fis.lba2 = ((lba >> 16) & 0xFF) as u8;
        fis.lba3 = ((lba >> 24) & 0xFF) as u8;
        fis.lba4 = ((lba >> 32) & 0xFF) as u8;
        fis.lba5 = ((lba >> 40) & 0xFF) as u8;
        fis.count0 = (count & 0xFF) as u8;
        fis.count1 = ((count >> 8) & 0xFF) as u8;
        fis
    }

    /// 创建Identify FIS
    pub fn identify() -> Self {
        let mut fis = Self::new();
        fis.fis_type = 0x27;
        fis.flags = 0x80;
        fis.command = 0xEC;
        fis.device = 0xA0;
        fis.count0 = 1;
        fis
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_h2d_fis_read() {
        let fis = H2dFis::read_dma(0x1000, 8);
        assert_eq!(fis.fis_type, 0x27);
        assert_eq!(fis.command, 0x25);
        assert_eq!(fis.count0, 8);
    }

    #[test]
    fn test_h2d_fis_write() {
        let fis = H2dFis::write_dma(0x2000, 16);
        assert_eq!(fis.fis_type, 0x27);
        assert_eq!(fis.command, 0x35);
        assert_eq!(fis.count0, 16);
    }

    #[test]
    fn test_cmd_header_structure() {
        assert_eq!(core::mem::size_of::<AhciCommandHeader>(), 32);
        assert_eq!(core::mem::size_of::<PhysicalRegionDescriptor>(), 16);
        assert_eq!(
            core::mem::size_of::<AhciCommandTable>(),
            64 + 16 + 48 + 8 * 16
        );
    }

    #[test]
    fn test_prdt_structure() {
        assert_eq!(core::mem::size_of::<PhysicalRegionDescriptor>(), 16);
    }
}
