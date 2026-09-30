//! E1000 网卡 MMIO 硬件访问器 (framework 层).
//!
//! 职责:
//! - 提供 `E1000Io` 类型, 封装 `IoMem` 安全代理访问 E1000 寄存器.
//! - 维护所有 E1000 寄存器偏移常量与位掩码.
//!
//! B04-AUDIT-005 #4 修复 (2026-08-24):
//! 原 `E1000Io` 在 services/driver/net/e1000.rs, 造成 framework→services
//! 反向依赖 (违反 framekernel 单向数据流). 移至 framework 后, services 通过
//! `use` 引用 framework 的 `E1000Io`, 并在 services 内实现 `E1000NetDriver`
//! 业务逻辑 (probe/init/send/recv). 循环依赖彻底解除.
//!
//! 该模块**不依赖** services 层任何代码, 完全基于 framework::iomem::IoMem.

use crate::framework::iomem::IoMem;
use crate::framework::mm::PhysAddr;

// ============================================================================
// E1000 寄存器偏移常量
// ============================================================================

// B04-AUDIT-005 #4: 内部 register 常量改为 `pub(crate)` 让同 crate 内 services
// E1000NetDriver 内部方法 (send_packet/recv_packet/handle_irq) 引用. 仍不暴露给
// framework 外部.
// 中断原因 + RX tail 等外部确实需要的常量保留 `pub`.

// 控制寄存器 (位定义对齐 Intel 8254x 数据手册 / e1000_defines.h)
pub(crate) const E1000_CTRL: u32 = 0x0000;
/// 全局复位 (Global reset, bit26).
///
/// ISSUE-RT-001: 早期误用 bit31 (0x8000_0000). 该位实为 `E1000_CTRL_PHY_RST`
/// (PHY 复位), 82540EM 不会因它触发全局复位, 导致复位轮询超时 -> 初始化失败.
pub(crate) const E1000_CTRL_RST: u32 = 1 << 26;
pub(crate) const E1000_CTRL_SLU: u32 = 1 << 6;
pub(crate) const E1000_CTRL_ASDE: u32 = 1 << 5;
pub(crate) const E1000_CTRL_SPEED_1000: u32 = 2 << 8;
/// 强制双工 (Force Duplex, bit12). 早期误用 bit14.
pub(crate) const E1000_CTRL_FRCDPX: u32 = 1 << 12;
pub(crate) const E1000_CTRL_FD: u32 = 1 << 0;
pub(crate) const E1000_CTRL_FRCSPD: u32 = 1 << 11;

// 状态寄存器
pub(crate) const E1000_STATUS: u32 = 0x0008;
pub(crate) const E1000_STATUS_LU: u32 = 1 << 1;
pub(crate) const E1000_STATUS_FD: u32 = 1 << 0;
pub(crate) const E1000_STATUS_SPEED_1000: u32 = 2 << 6;
pub(crate) const E1000_STATUS_SPEED_100: u32 = 1 << 6;

// EEPROM 寄存器
pub(crate) const E1000_EERD: u32 = 0x0014;
pub(crate) const E1000_EERD_START: u32 = 1 << 0;
pub(crate) const E1000_EERD_DONE: u32 = 1 << 4;

// 接收控制
pub(crate) const E1000_RCTL: u32 = 0x0100;
pub(crate) const E1000_RCTL_EN: u32 = 1 << 1;
pub(crate) const E1000_RCTL_SBP: u32 = 1 << 2;
pub(crate) const E1000_RCTL_UPE: u32 = 1 << 3;
pub(crate) const E1000_RCTL_MPE: u32 = 1 << 4;
pub(crate) const E1000_RCTL_BAM: u32 = 1 << 15;
pub(crate) const E1000_RCTL_SECRC: u32 = 1 << 26;
/// RX 缓冲区尺寸字段 (RCTL.BSIZE, bits[17:16]): BSEX=0 时 00b 即 2048 字节.
///
/// 注意: bit25 是 `RCTL_BSEX` (缓冲区尺寸扩展), 并非尺寸编码. 早期误用
/// `1 << 25` 会置位 BSEX 并把 BSIZE 留在保留组合 (BSEX=1, 00b), 与已分配的
/// 2048 字节 RX 缓冲区不一致.
pub(crate) const E1000_RCTL_BSIZE_2048: u32 = 0x0;

// 发送控制
pub(crate) const E1000_TCTL: u32 = 0x0400;
pub(crate) const E1000_TCTL_EN: u32 = 1 << 1;
pub(crate) const E1000_TCTL_PSP: u32 = 1 << 3;
pub(crate) const E1000_TCTL_COLD_FD: u32 = 0x40 << 12;
pub(crate) const E1000_TCTL_CT_FD: u32 = 0x10 << 4;

// TX 描述符环寄存器
pub(crate) const E1000_TDBAL: u32 = 0x3800;
pub(crate) const E1000_TDBAH: u32 = 0x3804;
pub(crate) const E1000_TDLEN: u32 = 0x3808;
pub(crate) const E1000_TDH: u32 = 0x3810;
pub(crate) const E1000_TDT: u32 = 0x3818;

// RX 描述符环寄存器
pub(crate) const E1000_RDBAL: u32 = 0x2800;
pub(crate) const E1000_RDBAH: u32 = 0x2804;
pub(crate) const E1000_RDLEN: u32 = 0x2808;
pub(crate) const E1000_RDH: u32 = 0x2810;
/// RX tail 寄存器偏移 (framework 层 `dump_regs` 使用)
pub const E1000_RDT: u32 = 0x2818;

// 中断控制
pub(crate) const E1000_IMC: u32 = 0x00D8;
pub(crate) const E1000_ICR: u32 = 0x00C0;
pub(crate) const E1000_IMS: u32 = 0x00D0;
/// 中断原因: 链路状态变化
pub const E1000_ICR_LSC: u32 = 1 << 2;
/// 中断原因: RX 描述符最小阈值
pub const E1000_ICR_RXDMT0: u32 = 1 << 4;
/// 中断原因: 接收缓冲区溢出
pub const E1000_ICR_RXO: u32 = 1 << 6;
/// 中断原因: RX 定时器
pub const E1000_ICR_RXT0: u32 = 1 << 7;

// IPG (Inter-Packet Gap)
pub(crate) const E1000_IPG: u32 = 0x00B0;

// MAC 地址寄存器
pub(crate) const E1000_RAL0: u32 = 0x5400;
pub(crate) const E1000_RAH0: u32 = 0x5404;
pub(crate) const E1000_RAH_AV: u32 = 1 << 31;

// 超时常量
pub(crate) const E1000_TIMEOUT: u32 = 100000;

// ============================================================================
// 安全的 E1000 MMIO 访问器
// ============================================================================

/// 安全的 E1000 MMIO 访问器.
///
/// 包装 `IoMem`, 提供所有 E1000 寄存器的类型安全读写.
/// 该类型原属 services 层 (B04-19 上移 dma_ring 时一并迁移), B04-AUDIT-005 #4
/// 修复后正式归位 framework, services 通过 `use` 引用.
pub struct E1000Io {
    mmio: IoMem,
}

/// `E1000Io` 创建结果.
///
/// services 层的 `E1000NetDriver` 探测流程调用 `E1000Io::new`, 该函数可能因为
/// IoMem 映射失败返回错误. 由于 framework 不依赖 `KernelError` (services 类型),
/// 这里采用独立错误类型, services 层用 `?` 映射到 `DriverError::HardwareError`.
#[derive(Debug)]
pub enum E1000IoError {
    /// PCI BAR 映射 IoMem 失败
    IoMemMap,
}

impl E1000Io {
    /// 从物理地址创建 E1000 MMIO 访问器.
    ///
    /// # 参数
    /// - `phys`: E1000 BAR0 物理地址 (来自 PCI 枚举)
    /// - `len`: MMIO 区域大小 (通常 128KB)
    ///
    /// # Errors
    ///
    /// 当从 PCI BAR 映射物理地址失败时返回 [`E1000IoError::IoMemMap`].
    pub fn new(phys: PhysAddr, len: usize) -> Result<Self, E1000IoError> {
        let mmio =
            IoMem::from_pci_bar(phys, len, "e1000-bar0").map_err(|_| E1000IoError::IoMemMap)?;
        Ok(Self { mmio })
    }

    // ── 寄存器读写 ──

    /// 读取 32 位寄存器
    #[inline(always)]
    pub fn read32(&self, reg: u32) -> u32 {
        self.mmio.read_u32(reg as usize)
    }

    /// 写入 32 位寄存器
    #[inline(always)]
    pub fn write32(&self, reg: u32, val: u32) {
        self.mmio.write_u32(reg as usize, val);
    }

    // ── EEPROM 读取 ──

    /// 通过 EERD 寄存器读取 EEPROM 字.
    ///
    /// 轮询 EERD.DONE 位, 带超时保护.
    pub fn eeprom_read(&self, addr: u8) -> u16 {
        self.write32(E1000_EERD, (u32::from(addr) << 2) | E1000_EERD_START);
        let mut timeout: u32 = 0;
        while timeout < E1000_TIMEOUT {
            let val = self.read32(E1000_EERD);
            if val & E1000_EERD_DONE != 0 {
                return ((val >> 16) & 0xFFFF) as u16;
            }
            timeout += 1;
            core::hint::spin_loop();
        }
        0xFFFF
    }

    // ── 中断 ──

    /// 读取中断原因并应答 (write-1-to-clear)
    pub fn irq_ack(&self) -> u32 {
        let icr = self.read32(E1000_ICR);
        self.write32(E1000_ICR, icr);
        icr
    }

    /// 清除所有待处理中断
    pub fn irq_disable_all(&self) {
        self.write32(E1000_IMC, 0xFFFF_FFFF);
    }

    /// 启用指定中断
    pub fn irq_enable(&self, mask: u32) {
        self.write32(E1000_IMS, mask);
    }

    /// 读取中断状态
    pub fn icr(&self) -> u32 {
        self.read32(E1000_ICR)
    }

    /// 读取中断掩码
    pub fn ims(&self) -> u32 {
        self.read32(E1000_IMS)
    }

    // ── 链路状态 ──

    /// 链路是否 UP
    pub fn link_is_up(&self) -> bool {
        self.read32(E1000_STATUS) & E1000_STATUS_LU != 0
    }

    /// 读取链路速度与双工状态
    pub fn link_status(&self) -> (&'static str, &'static str) {
        let status = self.read32(E1000_STATUS);
        let speed = if status & E1000_STATUS_SPEED_1000 != 0 {
            "1000"
        } else if status & E1000_STATUS_SPEED_100 != 0 {
            "100"
        } else {
            "10"
        };
        let duplex = if status & E1000_STATUS_FD != 0 {
            "FD"
        } else {
            "HD"
        };
        (speed, duplex)
    }

    // ── 收发描述符环配置 ──

    /// 设置 RX 描述符环物理基地址
    pub fn set_rx_base(&self, phys: u64) {
        self.write32(E1000_RDBAL, phys as u32);
        self.write32(E1000_RDBAH, (phys >> 32) as u32);
    }

    /// 设置 TX 描述符环物理基地址
    pub fn set_tx_base(&self, phys: u64) {
        self.write32(E1000_TDBAL, phys as u32);
        self.write32(E1000_TDBAH, (phys >> 32) as u32);
    }

    /// 设置 RX 描述符环长度 (字节)
    pub fn set_rx_len(&self, len: u32) {
        self.write32(E1000_RDLEN, len);
    }

    /// 设置 TX 描述符环长度 (字节)
    pub fn set_tx_len(&self, len: u32) {
        self.write32(E1000_TDLEN, len);
    }

    /// 读取 RX head 指针 (硬件更新)
    pub fn rx_head(&self) -> u32 {
        self.read32(E1000_RDH)
    }

    /// 设置 RX tail 指针 (通知硬件接收范围)
    pub fn set_rx_tail(&self, val: u32) {
        self.write32(E1000_RDT, val);
    }

    /// 读取 TX head 指针 (硬件更新)
    pub fn tx_head(&self) -> u32 {
        self.read32(E1000_TDH)
    }

    /// 设置 TX tail 指针 (通知硬件发送范围)
    pub fn set_tx_tail(&self, val: u32) {
        self.write32(E1000_TDT, val);
    }

    /// 设置 RX head 指针 (初始化用, 硬件通常只读)
    pub fn set_rx_head_raw(&self, val: u32) {
        self.write32(E1000_RDH, val);
    }

    /// 设置 TX head 指针 (初始化用, 硬件通常只读)
    pub fn set_tx_head_raw(&self, val: u32) {
        self.write32(E1000_TDH, val);
    }

    // ── 控制寄存器 ──

    /// 读取控制寄存器
    pub fn ctrl(&self) -> u32 {
        self.read32(E1000_CTRL)
    }

    /// 写入控制寄存器
    pub fn set_ctrl(&self, val: u32) {
        self.write32(E1000_CTRL, val);
    }

    /// 读取接收控制寄存器
    pub fn rx_ctl(&self) -> u32 {
        self.read32(E1000_RCTL)
    }

    /// 写入接收控制寄存器
    pub fn set_rx_ctl(&self, val: u32) {
        self.write32(E1000_RCTL, val);
    }

    /// 读取发送控制寄存器
    pub fn tx_ctl(&self) -> u32 {
        self.read32(E1000_TCTL)
    }

    /// 写入发送控制寄存器
    pub fn set_tx_ctl(&self, val: u32) {
        self.write32(E1000_TCTL, val);
    }

    // ── MAC 地址 ──

    /// 写入 MAC 地址到 RAL0/RAH0 寄存器
    pub fn set_mac(&self, mac: [u8; 6]) {
        let ral = u32::from(mac[0])
            | (u32::from(mac[1]) << 8)
            | (u32::from(mac[2]) << 16)
            | (u32::from(mac[3]) << 24);
        let rah = u32::from(mac[4]) | (u32::from(mac[5]) << 8) | E1000_RAH_AV;
        self.write32(E1000_RAL0, ral);
        self.write32(E1000_RAH0, rah);
    }

    // ── IPG ──

    /// 设置 Inter-Packet Gap
    pub fn set_ipg(&self, val: u32) {
        self.write32(E1000_IPG, val);
    }
}
