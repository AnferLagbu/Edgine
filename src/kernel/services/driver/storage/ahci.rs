#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! AHCI (Advanced Host Controller Interface) SATA 驱动 — services 层安全代理 (Phase 2.1.4)
//!
//! 封装 AHCI HBA 的 MMIO 操作, 通过 `framework::IoMem` 提供 100% safe API。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 内部 `IoMem` 由 TCB 抽象, services 层只调用 safe 方法
//! - **类型安全**: 端口号、命令寄存器位用枚举/常量
//! - **完整驱动逻辑**: 命令列表管理、FIS 传输、DMA 引擎、端口初始化/读写
//! - **DMA 通过 framework**: 所有 DMA 分配/释放通过 framework safe wrapper
//!
//! ## 硬件接口
//!
//! ```text
//! ABAR (SATA HBA MMIO region):
//! ├── 0x00 GHC_CAP: Host Capabilities
//! ├── 0x04 GHC_GHC: Global Host Control
//! ├── 0x08 GHC_IS:  Interrupt Status
//! ├── 0x0C GHC_PI:  Ports Implemented
//! ├── 0x10 GHC_VS:  Version
//! ├── 0x100 + n*0x80: Port n registers
//! │   ├── 0x00 PxCLB: Command List Base
//! │   ├── 0x08 PxFB:  FIS Base
//! │   ├── 0x10 PxIS:  Interrupt Status
//! │   ├── 0x14 PxIE:  Interrupt Enable
//! │   ├── 0x18 PxCMD: Command and Status
//! │   ├── 0x20 PxTFD: Task File Data
//! │   ├── 0x24 PxSIG: Signature
//! │   ├── 0x28 PxSSTS: SATA Status
//! │   └── 0x38 PxCI:  Command Issue
//! ```
//!
//! 评估日期: 2026-06-04
//! Phase 2.1.4 任务: 存储设备 (AHCI) 迁移

use crate::framework::driver::BlockDevice;
use crate::framework::driver::storage::H2dFis;
use crate::framework::iomem::IoMem;
use crate::framework::mm::PhysAddr;

use super::AHCI_CONTROLLERS;

// Services 层日志
use crate::slog_info;
use crate::slog_warn;

// ============================================================================
// HBA 寄存器偏移
// ============================================================================

/// Host Capabilities (R)
pub const GHC_CAP: usize = 0x00;
/// 全局主控控制 (RW)
pub const GHC_GHC: usize = 0x04;
/// Interrupt Status (R)
pub const GHC_IS: usize = 0x08;
/// Ports Implemented (R)
pub const GHC_PI: usize = 0x0C;
/// Version (R)
pub const GHC_VS: usize = 0x10;

// ============================================================================
// GHC 寄存器位
// ============================================================================

/// HBA Reset
pub const GHC_HR: u32 = 1 << 0;
/// Interrupt Enable
pub const GHC_IE: u32 = 1 << 1;
/// AHCI Enable
pub const GHC_AE: u32 = 1 << 31;

// ============================================================================
// 端口寄存器区域
// ============================================================================

/// 第一个端口寄存器偏移
pub const PORT_REG_BASE: usize = 0x100;
/// 端口寄存器步长
pub const PORT_REG_STRIDE: usize = 0x80;
/// AHCI 最大端口数
pub const AHCI_MAX_PORTS: usize = 32;

// ============================================================================
// 端口寄存器偏移 (相对端口基址)
// ============================================================================

/// Command List Base (低)
pub const PxCLB: usize = 0x00;
/// Command List Base (高)
pub const PxCLBU: usize = 0x04;
/// FIS Base (低)
pub const PxFB: usize = 0x08;
/// FIS Base (高)
pub const PxFBU: usize = 0x0C;
/// Interrupt Status
pub const PxIS: usize = 0x10;
/// Interrupt Enable
pub const PxIE: usize = 0x14;
/// Command and Status
pub const PxCMD: usize = 0x18;
/// 任务文件数据
pub const PxTFD: usize = 0x20;
/// Signature
pub const PxSIG: usize = 0x24;
/// SATA Status
pub const PxSSTS: usize = 0x28;
/// SATA Control
pub const PxSCTL: usize = 0x2C;
/// SATA Error
pub const PxSERR: usize = 0x30;
/// SATA Active
pub const PxSACT: usize = 0x34;
/// Command Issue
pub const PxCI: usize = 0x38;
/// SATA Notification
pub const PxSNTF: usize = 0x3C;

// ============================================================================
// PxCMD 寄存器位
// ============================================================================

/// 启动 (PxCMD.ST)
pub const PxCMD_ST: u32 = 1 << 0;
/// FIS 接收使能 (PxCMD.FRE)
pub const PxCMD_FRE: u32 = 1 << 4;
/// FIS 接收进行中 (PxCMD.FR)
pub const PxCMD_FR: u32 = 1 << 14;
/// 命令列表进行中 (PxCMD.CR)
pub const PxCMD_CR: u32 = 1 << 15;

// ============================================================================
// PxTFD 寄存器位
// ============================================================================

/// Error (PxTFD.ERR)
pub const PxTFD_ERR: u32 = 1 << 0;
/// DRQ (data request)
pub const PxTFD_DRQ: u32 = 1 << 3;
/// Busy
pub const PxTFD_BSY: u32 = 1 << 7;

// ============================================================================
// PxSSTS 寄存器位
// ============================================================================

/// Device Detection (`PxSSTS` `[3:0]`)
pub const PxSSTS_DET: u32 = 0xF;

// ============================================================================
// PxSCTL 寄存器位
// ============================================================================

/// Device Control (`PxSCTL` `[3:0]`)
pub const PxSCTL_DET: u32 = 0xF;
/// DET=1: 发起 COMRESET (端口复位, 重建 PHY 链路)
pub const PxSCTL_DET_COMRESET: u32 = 0x1;

// ============================================================================
// PxIS 寄存器位
// ============================================================================

/// D2H Register FIS
pub const PxIS_DHRS: u32 = 1 << 0;
/// DMA Setup FIS
pub const PxIS_DPS: u32 = 1 << 5;
/// 端口连接变更
pub const PxIS_PCS: u32 = 1 << 9;
/// Task File Error
pub const PxIS_TFE: u32 = 1 << 30;

// ============================================================================
// 设备签名 (PxSIG)
// ============================================================================

/// SATA 设备签名
pub const SATA_SIG_ATA: u32 = 0x0000_0101;
/// ATAPI 设备签名
pub const SATA_SIG_ATAPI: u32 = 0xEB14_0101;

// ============================================================================
// 常量
// ============================================================================

/// 命令槽数量
pub const CMD_SLOTS: usize = 32;
/// 每个端口命令槽数量
pub const PORT_CMD_SLOTS: usize = 32;
/// 命令头大小 (字节)
pub const CMD_HEADER_SIZE: usize = 32;
/// 命令列表总大小
pub const CMD_LIST_SIZE: usize = PORT_CMD_SLOTS * CMD_HEADER_SIZE;
/// FIS 接收缓冲区大小 (一页)
pub const FIS_BUFFER_SIZE: usize = 4096;
/// 命令表大小 (CFIS + ACMD + PRDT)
pub const CMD_TABLE_SIZE: usize = 256;
/// 单次传输最大扇区数
pub const MAX_SECTORS_PER_CMD: u16 = 128;
/// 扇区大小
pub const SECTOR_SIZE: usize = 512;

// ============================================================================
// 设备类型
// ============================================================================

/// AHCI 端口连接的设备类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AhciDeviceKind {
    /// SATA 磁盘 (ATA)
    Ata,
    /// ATAPI 光驱
    Atapi,
    /// 无设备
    None,
}

impl AhciDeviceKind {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 从 `PxSIG` 寄存器解析
    pub fn from_signature(sig: u32) -> Self {
        match sig {
            SATA_SIG_ATA => Self::Ata,
            SATA_SIG_ATAPI => Self::Atapi,
            0xFFFF_FFFF | 0x0000_0000 => Self::None,
            _ => Self::None,
        }
    }
}

// ============================================================================
// 端口状态
// ============================================================================

/// AHCI 端口 SATA 状态 (`PxSSTS`) 解析
#[derive(Debug, Clone, Copy)]
pub struct SataStatus {
    /// 设备检测 (`PxSSTS` `[3:0]`)
    /// 0=未连接, 1=已连接但未建立通信, 3=建立通信, 4=离线
    pub device_detection: u8,
    /// 接口速度 (`PxSSTS` `[7:4]`)
    pub interface_speed: u8,
}

impl SataStatus {
    /// 从 `PxSSTS` 寄存器解析
    pub fn from_register(val: u32) -> Self {
        Self {
            device_detection: (val & 0x0F) as u8,
            interface_speed: ((val >> 4) & 0x0F) as u8,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 设备是否已建立通信 (PxSSTS.DET == 3)
    pub fn is_connected(&self) -> bool {
        self.device_detection == 3
    }
}

// ============================================================================
// AHCI 端口 (services 层安全驱动)
// ============================================================================

/// AHCI 端口 DMA 资源句柄
pub struct AhciPortDma {
    /// 命令列表 DMA 虚拟地址 (用于填充 Command Header)
    pub cmd_list_virt: u64,
    /// 命令列表物理地址 (低 32 位, 写 `PxCLB`)
    pub cmd_list_low: u32,
    /// 命令列表物理地址 (高 32 位, 写 `PxCLBU`)
    pub cmd_list_high: u32,
    /// FIS 缓冲区物理地址 (低 32 位, 写 `PxFB`)
    pub fis_low: u32,
    /// FIS 缓冲区物理地址 (高 32 位, 写 `PxFBU`)
    pub fis_high: u32,
    /// 命令表 DMA 虚拟地址 (填充 FIS + PRDT)
    pub cmd_table_virt: u64,
    /// 命令表 DMA 物理地址
    pub cmd_table_phys: u64,
}

/// AHCI 端口 — services 层安全驱动
pub struct AhciPort {
    /// 端口号
    pub port_num: u8,
    /// 端口寄存器基址 (MMIO offset from ABAR)
    port_offset: usize,
    /// 设备是否存在
    pub device_present: bool,
    /// 设备签名
    pub signature: u32,
    /// 设备类型
    pub device_kind: AhciDeviceKind,
    /// 该端口对应的 Chitin 块设备下标 (无盘或未注册时为 `None`)
    drive: Option<u8>,
    /// DMA 资源
    dma: Option<AhciPortDma>,
    /// 端口已初始化
    port_initialized: bool,
}

impl AhciPort {
    /// 创建端口实例
    pub fn new(port_num: u8) -> Self {
        Self {
            port_num,
            port_offset: PORT_REG_BASE + (port_num as usize) * PORT_REG_STRIDE,
            device_present: false,
            signature: 0,
            device_kind: AhciDeviceKind::None,
            drive: None,
            dma: None,
            port_initialized: false,
        }
    }

    /// 端口对应的 Chitin 块设备下标 (无盘/未注册为 `None`)
    pub fn drive(&self) -> Option<u8> {
        self.drive
    }

    /// 记录端口对应的 Chitin 块设备下标
    pub fn set_drive(&mut self, drive: Option<u8>) {
        self.drive = drive;
    }

    /// 端口 MMIO 基址偏移 (相对 ABAR)
    pub fn port_offset(&self) -> usize {
        self.port_offset
    }

    // ── 端口寄存器读写 (通过 IoMem) ──

    /// 读端口 32 位寄存器 (使用控制器的 `IoMem`)
    pub fn port_read32(&self, hba: &AhciHba, offset: usize) -> u32 {
        hba.port_read32(self.port_num, offset)
    }

    /// 写端口 32 位寄存器
    pub fn port_write32(&self, hba: &AhciHba, offset: usize, val: u32) {
        hba.port_write32(self.port_num, offset, val);
    }

    /// 读 `PxCLB` (Command List Base Address, 32-bit)
    pub fn cmd_list_base(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxCLB)
    }

    /// 写 `PxCLB`
    pub fn set_cmd_list_base(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxCLB, val);
    }

    /// 写 `PxCLBU`
    pub fn set_cmd_list_base_upper(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxCLBU, val);
    }

    /// 设置 Command List 64-bit 物理地址
    pub fn set_cmd_list(&self, hba: &AhciHba, paddr: u64) {
        self.set_cmd_list_base(hba, (paddr & 0xFFFF_FFFF) as u32);
        self.set_cmd_list_base_upper(hba, ((paddr >> 32) & 0xFFFF_FFFF) as u32);
    }

    /// 读 `PxFB` (FIS Base Address, 32-bit)
    pub fn fis_base(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxFB)
    }

    /// 写 `PxFB`
    pub fn set_fis_base(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxFB, val);
    }

    /// 写 `PxFBU`
    pub fn set_fis_base_upper(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxFBU, val);
    }

    /// 设置 FIS 64-bit 物理地址
    pub fn set_fis(&self, hba: &AhciHba, paddr: u64) {
        self.set_fis_base(hba, (paddr & 0xFFFF_FFFF) as u32);
        self.set_fis_base_upper(hba, ((paddr >> 32) & 0xFFFF_FFFF) as u32);
    }

    /// 读 `PxIS` (Interrupt Status)
    pub fn interrupt_status(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxIS)
    }

    /// 应答端口中断 (`PxIS` write-1-to-clear)
    pub fn ack_interrupt(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxIS, val);
    }

    /// 读 `PxIE` (Interrupt Enable)
    pub fn interrupt_enable(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxIE)
    }

    /// 写 `PxIE`
    pub fn set_interrupt_enable(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxIE, val);
    }

    /// 读 `PxCMD`
    pub fn port_cmd(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxCMD)
    }

    /// 写 `PxCMD`
    pub fn set_port_cmd(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxCMD, val);
    }

    /// 读 `PxTFD` (Task File Data)
    pub fn port_tfd(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxTFD)
    }

    /// 读 `PxSIG` (设备签名)
    pub fn port_signature(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxSIG)
    }

    /// 读 `PxSSTS`
    pub fn port_sata_status(&self, hba: &AhciHba) -> SataStatus {
        SataStatus::from_register(self.port_read32(hba, PxSSTS))
    }

    /// 读 `PxCI` (Command Issue)
    pub fn port_cmd_issue(&self, hba: &AhciHba) -> u32 {
        self.port_read32(hba, PxCI)
    }

    /// 写 `PxCI` (发布命令)
    pub fn set_port_cmd_issue(&self, hba: &AhciHba, val: u32) {
        self.port_write32(hba, PxCI, val);
    }

    /// 端口是否忙碌 (PxTFD.BSY)
    pub fn port_is_busy(&self, hba: &AhciHba) -> bool {
        self.port_tfd(hba) & PxTFD_BSY != 0
    }

    /// 端口是否有数据请求 (PxTFD.DRQ)
    pub fn port_has_drq(&self, hba: &AhciHba) -> bool {
        self.port_tfd(hba) & PxTFD_DRQ != 0
    }

    /// 端口是否有错误 (PxTFD.ERR)
    pub fn port_has_error(&self, hba: &AhciHba) -> bool {
        self.port_tfd(hba) & PxTFD_ERR != 0
    }

    // ── 端口操作 ──

    /// 发起 COMRESET 序列重建端口 PHY 链路 (AHCI 1.3.1 §3.3.4)
    ///
    /// HBA 复位 (GHC.HR) 会清除端口链路状态; 软件必须经 `PxSCTL.DET`
    /// 发起 COMRESET (置 1 保持 → 写 0 释放) 后, 设备侧才会重建
    /// `PxSSTS.DET=3` (在位 + 通信建立)。缺失该序列时带盘端口扫描
    /// 恒报"设备不在位" (首次带盘冒烟发现, framework 版同构缺失,
    /// pc ich9-ahci 与 q35 内建 SATA 均复现)。
    pub fn comreset(&self, hba: &AhciHba) {
        // DET=1: 发出 COMRESET 并保持 (真实硬件 PHY 时序要求 >1ms)
        let sctl = self.port_read32(hba, PxSCTL);
        self.port_write32(hba, PxSCTL, (sctl & !PxSCTL_DET) | PxSCTL_DET_COMRESET);
        let mut hold = 1_000_000u32;
        while hold > 0 {
            hold -= 1;
            core::hint::spin_loop();
        }

        // DET=0: 释放 COMRESET, 设备开始链路建立
        let sctl = self.port_read32(hba, PxSCTL);
        self.port_write32(hba, PxSCTL, sctl & !PxSCTL_DET);

        // 轮询 PxSSTS.DET==3 (设备在位 + 通信建立); 超时按设备缺位
        // 处理, 由调用方 detect_device 最终判定
        let mut timeout = 1_000_000u32;
        while timeout > 0 {
            if SataStatus::from_register(self.port_read32(hba, PxSSTS)).is_connected() {
                break;
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
    }

    /// 使能 FIS 接收 (PxCMD.FRE) 并等待设备签名锁存
    ///
    /// COMRESET 后设备在位信息经 D2H Register FIS 上报, HBA 须先开
    /// FIS 接收引擎 PxSIG 才会被锁存 (AHCI 1.3.1 §3.3.5)。QEMU 于
    /// PxCMD 写入时惰性补发 init D2H (hw/ide/ahci.c `ahci_init_d2h`,
    /// 其注释自述为 hack), 不使能 FRE 则 `PxSIG` 恒 `0xFFFFFFFF`,
    /// 端口扫描判不出设备 (带盘冒烟实证)。
    pub fn enable_fis_receive(&mut self, hba: &AhciHba) -> bool {
        // 清零中断状态
        self.port_write32(hba, PxIS, 0xFFFF_FFFF);

        // FRE=1
        let cmd = self.port_cmd(hba);
        self.set_port_cmd(hba, cmd | PxCMD_FRE);

        // 等待 FR 置位
        let mut timeout = 1_000_000u64;
        while self.port_cmd(hba) & PxCMD_FR == 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            slog_warn!(Driver, "端口 {} FRE 超时", self.port_num);
            return false;
        }

        // 等待设备签名锁存 (D2H FIS 到达; QEMU 同步锁存, 真实硬件轮询)
        let mut sig_wait = 1_000_000u32;
        while self.port_signature(hba) == 0xFFFF_FFFF && sig_wait > 0 {
            sig_wait -= 1;
            core::hint::spin_loop();
        }
        true
    }

    /// 检测设备 (读 `PxSSTS` + `PxSIG`)
    pub fn detect_device(&mut self, hba: &AhciHba) -> bool {
        let ssts = self.port_sata_status(hba);
        if ssts.is_connected() {
            self.signature = self.port_signature(hba);
            self.device_kind = AhciDeviceKind::from_signature(self.signature);
            self.device_present = matches!(self.device_kind, AhciDeviceKind::Ata);
            self.device_present
        } else {
            self.device_present = false;
            self.device_kind = AhciDeviceKind::None;
            false
        }
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 分配 DMA 内存并设置寄存器 (幂等: 已分配时直接返回)
    pub fn setup_dma(&mut self, hba: &AhciHba) -> bool {
        // 幂等防护: enable() 与 init_controller 检测序列都会调用,
        // 重复分配会泄漏先前的 DMA 资源
        if self.dma.is_some() {
            return true;
        }
        let handle = if let Some(h) = crate::framework::driver::storage::ahci_alloc_port_dma() {
            h
        } else {
            slog_warn!(Driver, "端口 {} DMA 分配失败", self.port_num);
            return false;
        };

        // 写入寄存器
        self.set_cmd_list(hba, handle.cmd_list_phys);
        self.set_fis(hba, handle.fis_phys);

        self.dma = Some(AhciPortDma {
            cmd_list_virt: handle.cmd_list_virt,
            cmd_list_low: handle.cmd_list_phys as u32,
            cmd_list_high: (handle.cmd_list_phys >> 32) as u32,
            fis_low: handle.fis_phys as u32,
            fis_high: (handle.fis_phys >> 32) as u32,
            cmd_table_virt: handle.cmd_table_virt,
            cmd_table_phys: handle.cmd_table_phys,
        });

        true
    }

    /// 启用端口 (FRE + ST)
    pub fn enable(&mut self, hba: &AhciHba) -> bool {
        // 分配 DMA
        if !self.setup_dma(hba) {
            return false;
        }

        // 清零中断状态
        self.port_write32(hba, PxIS, 0xFFFF_FFFF);

        // 启用 FIS 接收 (FRE)
        let cmd = self.port_cmd(hba);
        self.set_port_cmd(hba, cmd | PxCMD_FRE);

        // 等待 FR 置位
        let mut timeout = 1_000_000u64;
        while self.port_cmd(hba) & PxCMD_FR == 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            slog_warn!(Driver, "端口 {} FRE 超时", self.port_num);
            return false;
        }

        // 启动命令处理 (ST)
        let cmd = self.port_cmd(hba);
        self.set_port_cmd(hba, cmd | PxCMD_ST);

        // 等待 CR 置位
        timeout = 1_000_000;
        while self.port_cmd(hba) & PxCMD_CR == 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        if timeout == 0 {
            slog_warn!(Driver, "端口 {} CR 超时", self.port_num);
            return false;
        }

        self.port_initialized = true;
        true
    }

    /// 停止端口 (ST=0, FRE=0)
    pub fn disable(&mut self, hba: &AhciHba) {
        if !self.port_initialized {
            return;
        }

        // 停止命令处理 (ST=0)
        let cmd = self.port_cmd(hba);
        self.set_port_cmd(hba, cmd & !PxCMD_ST);
        let mut timeout = 1_000_000u64;
        while self.port_cmd(hba) & PxCMD_CR != 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }

        // 停止 FIS 接收 (FRE=0)
        let cmd = self.port_cmd(hba);
        self.set_port_cmd(hba, cmd & !PxCMD_FRE);
        timeout = 1_000_000;
        while self.port_cmd(hba) & PxCMD_FR != 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }

        self.port_initialized = false;
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::ref_as_ptr,
        reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
    )]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 提交 DMA 命令并等待完成
    ///
    /// 通过 framework safe wrapper 填充命令头、FIS、PRDT,
    /// 然后发布命令并轮询完成。
    fn submit_dma_command(
        &mut self,
        hba: &AhciHba,
        fis: &H2dFis,
        buffer_phys: u64,
        byte_count: u32,
        is_write: bool,
    ) -> Result<(), ()> {
        let dma = match self.dma.as_ref() {
            Some(d) => d,
            None => return Err(()),
        };

        let slot = 0u32; // 使用 slot 0

        // 1. 填充命令表: H2D FIS + PRDT
        crate::framework::driver::storage::ahci_fill_h2d_fis(
            dma.cmd_table_virt,
            fis as *const _ as *const u8 as usize,
            core::mem::size_of::<H2dFis>(),
        );
        crate::framework::driver::storage::ahci_fill_prdt(
            dma.cmd_table_virt,
            0,
            buffer_phys,
            byte_count,
            true, // IOC
        );

        // 2. 填充命令头
        crate::framework::driver::storage::ahci_fill_cmd_header(
            dma.cmd_list_virt,
            slot,
            5, // FIS length = 5 DWORDs
            is_write,
            1, // PRDTL = 1 entry
            dma.cmd_table_phys,
        );

        // 3. 等待端口空闲
        let mut timeout = 1_000_000u64;
        while self.port_is_busy(hba) || self.port_has_drq(hba) {
            timeout -= 1;
            if timeout == 0 {
                return Err(());
            }
            core::hint::spin_loop();
        }

        // 4. 清零中断状态
        self.ack_interrupt(hba, 0xFFFF_FFFF);

        // 5. 发布命令 (sfence + PxCI)
        crate::arch!(fence_w());
        self.set_port_cmd_issue(hba, 1 << slot);

        // 6. 等待完成 (错误完成 TFES 同样视为完成信号, 否则越界/中止类
        //    失败只置 TFES 不置 DHRS, 会白烧满超时 — 首次带盘冒烟实证)
        timeout = 5_000_000;
        while timeout > 0 {
            let is = self.interrupt_status(hba);
            if is & (PxIS_DHRS | PxIS_DPS | PxIS_PCS | PxIS_TFE) != 0 {
                break;
            }
            timeout -= 1;
            core::hint::spin_loop();
        }

        if timeout == 0 {
            self.clear_port_error_status(hba);
            return Err(());
        }

        // 7. 检查错误
        if self.interrupt_status(hba) & PxIS_TFE != 0 {
            self.clear_port_error_status(hba);
            return Err(());
        }
        if self.port_has_error(hba) {
            self.clear_port_error_status(hba);
            return Err(());
        }

        Ok(())
    }

    /// 命令失败后的最小错误恢复 (AHCI 1.3.1 §6.4.2)
    ///
    /// PxSERR 状态/诊断位为 W1C, 错误完成后保持置位; 不清除将影响
    /// 端口后续命令处理。PxIS 同步清除避免残留中断状态误判。
    fn clear_port_error_status(&self, hba: &AhciHba) {
        self.port_write32(hba, PxSERR, 0xFFFF_FFFF);
        self.ack_interrupt(hba, 0xFFFF_FFFF);
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 读取扇区 (DMA)
    ///
    /// # Errors
    ///
    /// - 端口未初始化或设备未就绪时返回 `Err(())`
    /// - `count` 为 0 或超过单命令最大扇区数时返回 `Err(())`
    /// - DMA 缓冲区分配失败时返回 `Err(())`
    /// - 命令提交超时、传输错误 (TFE) 或端口出错时返回 `Err(())`
    pub fn read(&mut self, hba: &AhciHba, lba: u64, count: u16, buffer: *mut u8) -> Result<(), ()> {
        if !self.port_initialized || !self.device_present {
            return Err(());
        }
        if count == 0 || count > MAX_SECTORS_PER_CMD {
            return Err(());
        }

        let byte_count = u32::from(count) * SECTOR_SIZE as u32;

        // 分配 DMA 缓冲区 (RAII 句柄: 函数返回即归还)
        let buf =
            match crate::framework::driver::storage::ahci_alloc_dma_buffer(byte_count as usize) {
                Some(v) => v,
                None => return Err(()),
            };
        let buf_vaddr = buf.cpu_addr().as_ptr() as u64;
        let buf_paddr = buf.dma_addr().as_u64();

        let fis = H2dFis::read_dma(lba, count);
        let result = self.submit_dma_command(hba, &fis, buf_paddr, byte_count, false);

        // 复制数据到用户 buffer
        if result.is_ok() {
            crate::framework::driver::storage::ahci_copy_from_dma(
                buffer,
                buf_vaddr,
                byte_count as usize,
            );
        }

        result
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 写入扇区 (DMA)
    ///
    /// # Errors
    ///
    /// - 端口未初始化或设备未就绪时返回 `Err(())`
    /// - `count` 为 0 或超过单命令最大扇区数时返回 `Err(())`
    /// - DMA 缓冲区分配失败时返回 `Err(())`
    /// - 命令提交超时、传输错误 (TFE) 或端口出错时返回 `Err(())`
    pub fn write(
        &mut self,
        hba: &AhciHba,
        lba: u64,
        count: u16,
        buffer: *const u8,
    ) -> Result<(), ()> {
        if !self.port_initialized || !self.device_present {
            return Err(());
        }
        if count == 0 || count > MAX_SECTORS_PER_CMD {
            return Err(());
        }

        let byte_count = u32::from(count) * SECTOR_SIZE as u32;

        // 分配 DMA 缓冲区 (RAII 句柄: 函数返回即归还)
        let buf =
            match crate::framework::driver::storage::ahci_alloc_dma_buffer(byte_count as usize) {
                Some(v) => v,
                None => return Err(()),
            };
        let buf_vaddr = buf.cpu_addr().as_ptr() as u64;
        let buf_paddr = buf.dma_addr().as_u64();

        // 复制数据到 DMA 缓冲区
        crate::framework::driver::storage::ahci_copy_to_dma(buf_vaddr, buffer, byte_count as usize);

        let fis = H2dFis::write_dma(lba, count);
        let result = self.submit_dma_command(hba, &fis, buf_paddr, byte_count, true);

        result
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (buf_vaddr/buf_paddr 虚拟/物理地址对); 与同文件 read/write_dma 一致, 重命名破坏领域语义连续性"
    )]
    /// ATA IDENTIFY DEVICE (0xEC, PIO-in 经 PRDT 传输)
    ///
    /// 读取 512B 设备标识到 `buffer`。AHCI 下 PIO-in 数据同样经命令表
    /// PRDT 由 HBA 搬运, 与 DMA 读共用提交路径。
    ///
    /// # Errors
    ///
    /// - 端口未初始化或设备未就绪时返回 `Err(())`
    /// - DMA 缓冲区分配失败或命令提交失败时返回 `Err(())`
    pub fn identify(&mut self, hba: &AhciHba, buffer: *mut u8) -> Result<(), ()> {
        if !self.port_initialized || !self.device_present {
            return Err(());
        }

        let byte_count = SECTOR_SIZE as u32;

        // 分配 DMA 缓冲区 (RAII 句柄: 函数返回即归还)
        let Some(buf) =
            crate::framework::driver::storage::ahci_alloc_dma_buffer(byte_count as usize)
        else {
            return Err(());
        };
        let buf_vaddr = buf.cpu_addr().as_ptr() as u64;
        let buf_paddr = buf.dma_addr().as_u64();

        let fis = H2dFis::identify();
        let result = self.submit_dma_command(hba, &fis, buf_paddr, byte_count, false);

        if result.is_ok() {
            crate::framework::driver::storage::ahci_copy_from_dma(
                buffer,
                buf_vaddr,
                byte_count as usize,
            );
        }

        result
    }
}

// ============================================================================
// AHCI HBA (services 层安全代理)
// ============================================================================

/// AHCI HBA (Host Bus Adapter) — services 层安全代理
///
/// 封装 ABAR MMIO 区域, 提供所有 HBA + 端口寄存器的安全访问。
pub struct AhciHba {
    mmio: IoMem,
}

impl AhciHba {
    /// 创建 AHCI HBA 实例
    ///
    /// # 参数
    /// - `abar_phys`: ABAR MMIO 物理基地址 (来自 PCI BAR5)
    /// - `len`: ABAR MMIO 区域大小 (通常 0x2000 = 8KB)
    pub fn new(abar_phys: u64, len: usize) -> Option<Self> {
        let mmio = IoMem::from_pci_bar(PhysAddr::new(abar_phys), len, "ahci-abar").ok()?;
        Some(Self { mmio })
    }

    // ── HBA 全局寄存器 ──

    /// 读 HBA 能力寄存器 (`GHC_CAP`)
    #[inline]
    pub fn capabilities(&self) -> u32 {
        self.mmio.read_u32(GHC_CAP)
    }

    /// 读 GHC 全局控制寄存器
    #[inline]
    pub fn ghc(&self) -> u32 {
        self.mmio.read_u32(GHC_GHC)
    }

    /// 写 GHC 全局控制寄存器
    #[inline]
    pub fn set_ghc(&self, val: u32) {
        self.mmio.write_u32(GHC_GHC, val);
    }

    /// 读中断状态 (`GHC_IS`)
    #[inline]
    pub fn interrupt_status(&self) -> u32 {
        self.mmio.read_u32(GHC_IS)
    }

    /// 写中断状态 (`GHC_IS`) 应答
    #[inline]
    pub fn ack_interrupt(&self, val: u32) {
        self.mmio.write_u32(GHC_IS, val);
    }

    /// 读已实现端口位图 (`GHC_PI`)
    #[inline]
    pub fn ports_implemented(&self) -> u32 {
        self.mmio.read_u32(GHC_PI)
    }

    /// 读 HBA 版本 (`GHC_VS`)
    #[inline]
    pub fn version(&self) -> u32 {
        self.mmio.read_u32(GHC_VS)
    }

    /// 启用 AHCI 模式 (GHC.AE = 1)
    pub fn enable_ahci(&self) {
        let val = self.ghc();
        self.set_ghc(val | GHC_AE);
    }

    /// 启用全局中断 (GHC.IE = 1)
    pub fn enable_interrupts(&self) {
        let val = self.ghc();
        self.set_ghc(val | GHC_IE);
    }

    /// 禁用全局中断 (GHC.IE = 0)
    pub fn disable_interrupts(&self) {
        let val = self.ghc();
        self.set_ghc(val & !GHC_IE);
    }

    /// 软重置 HBA (GHC.HR = 1)
    pub fn reset(&self) {
        let val = self.ghc();
        self.set_ghc(val | GHC_HR);
        let mut timeout = 100_000u32;
        while self.ghc() & GHC_HR != 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
    }

    /// 列出已实现的端口索引
    pub fn implemented_ports(&self) -> alloc::vec::Vec<u8> {
        let pi = self.ports_implemented();
        let mut ports = alloc::vec::Vec::new();
        for i in 0..AHCI_MAX_PORTS {
            if pi & (1 << i) != 0 {
                ports.push(i as u8);
            }
        }
        ports
    }

    // ── 端口 MMIO 访问 ──

    /// 读端口 n 的 32 位寄存器
    pub fn port_read32(&self, port: u8, offset: usize) -> u32 {
        let off = PORT_REG_BASE + (port as usize) * PORT_REG_STRIDE + offset;
        self.mmio.read_u32(off)
    }

    /// 写端口 n 的 32 位寄存器
    pub fn port_write32(&self, port: u8, offset: usize, val: u32) {
        let off = PORT_REG_BASE + (port as usize) * PORT_REG_STRIDE + offset;
        self.mmio.write_u32(off, val);
    }
}

// ============================================================================
// AHCI 控制器 (services 层安全驱动)
// ============================================================================

/// AHCI 控制器 — services 层安全驱动
///
/// 管理 HBA 初始化、端口枚举、端口读写。
pub struct AhciController {
    /// HBA 安全代理
    hba: AhciHba,
    /// 端口列表
    ports: alloc::vec::Vec<AhciPort>,
    /// 已实现端口位图
    port_bitmap: u32,
    /// 控制器已初始化
    initialized: bool,
}

/// 端口扫描结果
///
/// 记录 [`AhciController::scan_ports`] 单次扫描发现的端口在位/离位变化,
/// 供 services 侧热插拔分发使用。
#[derive(Debug, Default)]
pub struct PortChanges {
    /// 新近在位的端口: (`port_index`, 硬件端口号)
    pub added: alloc::vec::Vec<(usize, u8)>,
    /// 新近离位的端口: (`port_index`, 硬件端口号, 已注册的 Chitin 块设备下标)
    pub removed: alloc::vec::Vec<(usize, u8, u8)>,
}

impl AhciController {
    /// 创建 AHCI 控制器实例
    pub fn new(abar_phys: u64, len: usize) -> Option<Self> {
        let hba = AhciHba::new(abar_phys, len)?;
        Some(Self {
            hba,
            ports: alloc::vec::Vec::new(),
            port_bitmap: 0,
            initialized: false,
        })
    }

    /// 获取 HBA 引用
    pub fn hba(&self) -> &AhciHba {
        &self.hba
    }

    /// 获取端口数量
    pub fn port_count(&self) -> usize {
        self.ports.len()
    }

    /// 获取端口 (可变引用)
    pub fn get_port(&mut self, index: usize) -> Option<&mut AhciPort> {
        self.ports.get_mut(index)
    }

    /// 获取端口 (不可变引用)
    pub fn port(&self, index: usize) -> Option<&AhciPort> {
        self.ports.get(index)
    }

    /// 按硬件端口号查找 `ports` 下标
    ///
    /// `ports` 按下标与硬件端口号一一对应 (见 [`Self::init_controller`]),
    /// 该映射用于把热插拔事件携带的硬件端口号还原为内部索引。
    pub fn port_index_of(&self, port_num: u8) -> Option<usize> {
        self.ports.iter().position(|p| p.port_num == port_num)
    }

    /// 上电单个端口并按需返回是否带盘就绪
    ///
    /// 检测序列 (AHCI 1.3.1 §3.3.4/§3.3.5): COMRESET 重建 PHY 链路 →
    /// 分配端口 DMA → 使能 FIS 接收 (锁存 `PxSIG`) → 读 SSTS/SIG 判定
    /// 在位与设备类型 → 启用端口。
    ///
    /// COMRESET 后 `PxSSTS.DET != 3` 即设备缺位, 此时在分配 DMA 之前
    /// 短路返回, 避免为空端口浪费 DMA 资源 (QEMU ich9-ahci 默认实现 6 个
    /// 端口, 逐一分配会产生无用占用)。
    fn bring_up_port(port: &mut AhciPort, hba: &AhciHba) -> bool {
        port.comreset(hba);
        if !port.port_sata_status(hba).is_connected() {
            port.device_present = false;
            port.device_kind = AhciDeviceKind::None;
            return false;
        }

        if port.setup_dma(hba)
            && port.enable_fis_receive(hba)
            && port.detect_device(hba)
            && port.enable(hba)
        {
            slog_info!(
                Driver,
                "端口 {} 已启用 (sig={:08X}, kind={:?})",
                port.port_num,
                port.signature,
                port.device_kind
            );
            return true;
        }
        slog_warn!(Driver, "端口 {} 启用失败", port.port_num);
        false
    }

    /// 初始化控制器 (HBA reset + 端口枚举)
    pub fn init_controller(&mut self) -> bool {
        // 确保 AHCI 模式已启用
        let mut ghc_val = self.hba.ghc();
        if ghc_val & GHC_AE == 0 {
            ghc_val |= GHC_AE;
            self.hba.set_ghc(ghc_val);
        }

        // HBA 复位 (GHC.HR 自清零; 按 AHCI 1.3.1 §3.3.1 复位会清 GHC.AE,
        // 必须重新置位后方可访问端口寄存器)
        self.hba.reset();
        self.hba.enable_ahci();

        // 启用中断
        self.hba.enable_interrupts();

        // 获取已实现的端口
        self.port_bitmap = self.hba.ports_implemented();
        slog_info!(
            Driver,
            "HBA 就绪, 已实现端口位图 PI={:#010x}",
            self.port_bitmap
        );

        // 初始化每个端口: 无条件 push 全部已实现端口, 使 `ports` 下标与硬件
        // 端口号一一对应 (固定槽位)。这样 `AhciBlockDevice.port_index` 在设备
        // 热插拔前后保持有效, 而不会因中间端口无盘导致后续端口索引漂移。
        for i in 0..AHCI_MAX_PORTS {
            if self.port_bitmap & (1u32 << i) == 0 {
                continue;
            }
            let mut port = AhciPort::new(i as u8);
            Self::bring_up_port(&mut port, &self.hba);
            self.ports.push(port);
        }

        self.initialized = true;
        let active = self.ports.iter().filter(|p| p.device_present).count();
        slog_info!(Driver, "控制器初始化完成, {} 端口活动", active);
        true
    }

    /// 扫描全部端口, 返回自上次扫描以来的在位/离位变化
    ///
    /// 用于 SATA 端口级热插拔: 逐端口读 `PxSSTS` 判定链路状态, 对新增在位
    /// 端口执行上电检测, 对离位端口执行停止并按已注册的 Chitin 块设备下标
    /// 记录移除。移除项的 `drive` 字段**保留不清**, 供后续分发阶段解析。
    pub fn scan_ports(&mut self) -> PortChanges {
        let mut changes = PortChanges::default();
        if !self.initialized {
            return changes;
        }

        let AhciController {
            hba,
            ports,
            port_bitmap: _,
            initialized: _,
        } = self;

        for pi in 0..ports.len() {
            let connected = ports[pi].port_sata_status(hba).is_connected();
            if connected && !ports[pi].device_present {
                if Self::bring_up_port(&mut ports[pi], hba) {
                    changes.added.push((pi, ports[pi].port_num));
                }
            } else if !connected && ports[pi].device_present {
                ports[pi].disable(hba);
                ports[pi].device_present = false;
                ports[pi].device_kind = AhciDeviceKind::None;
                if let Some(drive) = ports[pi].drive() {
                    changes.removed.push((pi, ports[pi].port_num, drive));
                }
            }
        }
        changes
    }

    /// 关闭控制器
    pub fn shutdown(&mut self) {
        for port in &mut self.ports {
            port.disable(&self.hba);
        }

        // 清除 AHCI 模式
        let ghc = self.hba.ghc();
        self.hba.set_ghc(ghc & !GHC_AE);

        self.initialized = false;
    }

    /// 控制器是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// 拆借端口与 HBA (块适配器使用: [`AhciPort::read`]/[`AhciPort::write`]
    /// 需同时持有端口可变引用与 HBA 共享引用, 链式借用会产生双重可变借用)
    fn split_port_hba(&mut self, index: usize) -> Option<(&mut AhciPort, &AhciHba)> {
        let AhciController {
            hba,
            ports,
            port_bitmap: _,
            initialized: _,
        } = self;
        let port = ports.get_mut(index)?;
        Some((port, hba))
    }
}

// ============================================================================
// BlockDevice 适配器 (DECISION-H storage 专项 1 号子步: _block 路径迁 services)
// ============================================================================

/// AHCI 端口的 `BlockDevice` 适配器
///
/// 不直接持有控制器, 经 (`controller_index`, `port_index`) 在 services 全局
/// [`AHCI_CONTROLLERS`] 注册表中查找 (services 权威, framework `_block` 已随
/// 3 号子步退位删除)。仅含 `usize`/`u64` 纯数据字段, `Send + Sync` 自动派生, 0 unsafe。
pub struct AhciBlockDevice {
    /// 控制器在 [`AHCI_CONTROLLERS`] 中的索引
    controller_index: usize,
    /// 端口索引
    port_index: usize,
    /// 缓存的磁盘容量 (512 字节扇区数)
    total_sectors: u64,
}

impl AhciBlockDevice {
    /// 为指定的 AHCI 端口创建适配器
    ///
    /// 端口无设备或探测容量为 0 时返回 `None`。
    pub fn new(controller_index: usize, port_index: usize) -> Option<Self> {
        {
            let mut controllers = AHCI_CONTROLLERS.lock();
            let controller = controllers.get_mut(controller_index)?;
            let (port, _hba) = controller.split_port_hba(port_index)?;
            if !port.device_present {
                return None;
            }
        }

        let total_sectors = Self::probe_disk_size(controller_index, port_index);
        if total_sectors == 0 {
            return None;
        }

        Some(Self {
            controller_index,
            port_index,
            total_sectors,
        })
    }

    /// 探测 AHCI 磁盘容量 (ATA IDENTIFY DEVICE, word 100-103)
    ///
    /// 单命令直读容量, 取代二分探测 (二分需读越界 LBA 触发设备错误,
    /// TCG 下 24 次命令 × 5M 自旋超时 ≈ 50s, 首次带盘冒烟实证)。
    /// word 100-103 (字节偏移 200..208 LE, u48 有效) 按 de facto 语义
    /// 取总扇区数 (Linux ata_id_u64(id,100) 直接作 n_sectors)。
    fn probe_disk_size(ci: usize, pi: usize) -> u64 {
        let mut controllers = AHCI_CONTROLLERS.lock();
        let Some(controller) = controllers.get_mut(ci) else {
            return 0;
        };
        let Some((port, hba)) = controller.split_port_hba(pi) else {
            return 0;
        };

        let mut buf = [0u8; SECTOR_SIZE];
        if port.identify(hba, buf.as_mut_ptr()).is_err() {
            return 0;
        }

        let mut raw = [0u8; 8];
        raw.copy_from_slice(&buf[200..208]);
        // word100-103 按 de facto 语义 = 总扇区数 (Linux ata_id_u64(id,100)
        // 直接作 n_sectors, QEMU 写 nb_sectors); 不做 +1, 否则末扇区越界
        u64::from_le_bytes(raw) & 0x0000_FFFF_FFFF_FFFF
    }
}

impl BlockDevice for AhciBlockDevice {
    fn blk_read(&mut self, sector: u64, buf: &mut [u8]) -> i32 {
        let mut controllers = AHCI_CONTROLLERS.lock();
        let Some(controller) = controllers.get_mut(self.controller_index) else {
            return -1;
        };
        let Some((port, hba)) = controller.split_port_hba(self.port_index) else {
            return -1;
        };
        match port.read(hba, sector, 1, buf.as_mut_ptr()) {
            Ok(()) => 0,
            Err(()) => -1,
        }
    }

    fn blk_write(&mut self, sector: u64, buf: &[u8]) -> i32 {
        let mut controllers = AHCI_CONTROLLERS.lock();
        let Some(controller) = controllers.get_mut(self.controller_index) else {
            return -1;
        };
        let Some((port, hba)) = controller.split_port_hba(self.port_index) else {
            return -1;
        };
        match port.write(hba, sector, 1, buf.as_ptr()) {
            Ok(()) => 0,
            Err(()) => -1,
        }
    }

    fn blk_is_present(&self) -> bool {
        let mut controllers = AHCI_CONTROLLERS.lock();
        controllers
            .get_mut(self.controller_index)
            .map_or(false, |controller| {
                controller
                    .split_port_hba(self.port_index)
                    .map_or(false, |(port, _hba)| port.device_present)
            })
    }

    fn blk_total_sectors(&self) -> u64 {
        self.total_sectors
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
    fn test_h2d_fis_identify() {
        let fis = H2dFis::identify();
        assert_eq!(fis.fis_type, 0x27);
        assert_eq!(fis.command, 0xEC);
        assert_eq!(fis.device, 0xA0);
    }

    #[test]
    fn test_ahci_device_kind_from_signature() {
        assert_eq!(
            AhciDeviceKind::from_signature(SATA_SIG_ATA),
            AhciDeviceKind::Ata
        );
        assert_eq!(
            AhciDeviceKind::from_signature(SATA_SIG_ATAPI),
            AhciDeviceKind::Atapi
        );
        assert_eq!(
            AhciDeviceKind::from_signature(0x00000000),
            AhciDeviceKind::None
        );
        assert_eq!(
            AhciDeviceKind::from_signature(0xFFFFFFFF),
            AhciDeviceKind::None
        );
    }

    #[test]
    fn test_sata_status_parse() {
        let ssts = SataStatus::from_register(0x0000_0313); // DET=3, Speed=1
        assert!(ssts.is_connected());
        assert_eq!(ssts.device_detection, 3);
        assert_eq!(ssts.interface_speed, 1);

        let ssts_disconnected = SataStatus::from_register(0x0000_0000);
        assert!(!ssts_disconnected.is_connected());
    }

    #[test]
    fn test_constants() {
        assert_eq!(SECTOR_SIZE, 512);
        assert_eq!(CMD_SLOTS, 32);
        assert_eq!(AHCI_MAX_PORTS, 32);
        assert_eq!(MAX_SECTORS_PER_CMD, 128);
    }

    #[test]
    fn test_ahci_port_drive_mapping() {
        // 端口 → Chitin 块设备下标的回写/读取/清除 (端口级热插拔墓碑化依赖)
        let mut port = AhciPort::new(3);
        assert_eq!(port.port_num, 3);
        assert_eq!(port.port_offset(), PORT_REG_BASE + 3 * PORT_REG_STRIDE);
        assert_eq!(port.drive(), None, "新建端口不应携带块设备下标");

        port.set_drive(Some(7));
        assert_eq!(port.drive(), Some(7), "应记录回写的块设备下标");

        port.set_drive(None);
        assert_eq!(port.drive(), None, "墓碑化后应清除块设备下标");
    }

    #[test]
    fn test_port_changes_default_empty() {
        let changes = PortChanges::default();
        assert!(changes.added.is_empty(), "默认无新增端口");
        assert!(changes.removed.is_empty(), "默认无移除端口");
    }
}
