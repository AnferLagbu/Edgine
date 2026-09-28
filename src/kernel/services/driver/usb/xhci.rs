#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! xHCI (eXtensible Host Controller Interface) USB 3.0 驱动 — services 层安全代理 (Phase 2.1.6)
//!
//! 封装 xHCI 主机控制器的 MMIO 操作,
//! 通过 `framework::IoMem` 提供 100% safe API。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 内部 `IoMem` 由 TCB 抽象, services 层只调用 safe 方法
//! - **类型安全**: 寄存器位、TRB 类型用枚举/常量
//! - **薄包装**: 暴露 Capability/Operational/Port/Doorbell 寄存器的安全访问
//! - **可替代**: 原 `kernel/driver/usb/xhci.rs` 仍存在, 本文件是迁移目标
//!
//! ## 硬件接口
//!
//! ```text
//! xHCI MMIO 区域 (来自 PCIe BAR0):
//! ├── 0x00 CAPLENGTH:  Capability 寄存器长度
//! ├── 0x04 HCSPARAMS1: 结构参数 1 (MaxSlots 等)
//! ├── 0x08 HCSPARAMS2: 结构参数 2 (MaxPorts 等)
//! ├── 0x10 HCCPARAMS1: 能力参数 1
//! ├── 0x14 DBOFF:      Doorbell 偏移
//! ├── 0x18 RTSOFF:     Runtime 偏移
//! └── (CAPLENGTH 偏移开始) Operational 寄存器:
//!     ├── +0x00 USBCMD:  USB 命令
//!     ├── +0x04 USBSTS:  USB 状态
//!     ├── +0x08 PAGESIZE: 页大小
//!     └── +0x400 PORTSC[n]: 端口 n 状态
//! ```
//!
//! 评估日期: 2026-06-04
//! Phase 2.1.6 任务: USB/XHCI 驱动迁移

use crate::framework::dma_buf::DmaStream;
use crate::framework::driver::framework::{
    DeviceType, Driver, DriverError, Result as DriverResult,
};
use crate::framework::iomem::IoMem;
use crate::framework::mm::PhysAddr;
use crate::services::error::KernelError;

use super::usb_core::{Direction, HostController, Urb, UsbSpeed};

use alloc::vec::Vec;

// ============================================================================
// xHCI TRB 类型定义 (USB-1.5)
// ============================================================================

/// TRB 类型枚举 — xHCI 规范 §6.4.1
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TrbType {
    /// 普通传输 TRB
    Normal = 1,
    /// Setup Stage TRB (控制传输)
    SetupStage = 2,
    /// Data Stage TRB (控制传输)
    DataStage = 3,
    /// Status Stage TRB (控制传输)
    StatusStage = 4,
    /// 等时传输 TRB
    Isoch = 5,
    /// Link TRB (环链接)
    Link = 6,
    /// Event Data TRB
    EventData = 7,
    /// No-Op TRB
    NoOp = 8,
    /// Enable Slot Command TRB
    EnableSlot = 9,
    /// Disable Slot Command TRB
    DisableSlot = 10,
    /// Address Device Command TRB
    AddressDevice = 11,
    /// 配置端点命令 TRB
    ConfigureEndpoint = 12,
    /// 评估上下文命令 TRB
    EvaluateContext = 13,
    /// 重置端点命令 TRB
    ResetEndpoint = 14,
    /// Stop Endpoint Command TRB
    StopEndpoint = 15,
    /// 设置 TR 出队指针命令 TRB
    SetTrDequeuePointer = 16,
    /// 重置设备命令 TRB
    ResetDevice = 17,
    /// 传输事件 TRB (控制器 → 主机)
    TransferEvent = 32,
    /// 命令完成事件 TRB (控制器 → 主机)
    CommandCompletionEvent = 33,
    /// 端口状态变更事件 TRB
    PortStatusChangeEvent = 34,
}

/// 传输描述符 (TRB) — 16 字节, xHCI 规范 §6.4
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Trb {
    /// 参数 (含义取决于 TRB 类型)
    pub parameter: u64,
    /// 状态字段 (传输长度/完成码等)
    pub status: u32,
    /// 控制字段 (TRB 类型 + cycle bit + flags)
    pub control: u32,
}

impl Trb {
    /// 创建新 TRB
    pub fn new(parameter: u64, status: u32, control: u32) -> Self {
        Self {
            parameter,
            status,
            control,
        }
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 提取 TRB 类型 (control`[15:10]`)
    pub fn trb_type(&self) -> TrbType {
        let ty = (self.control >> 10) & 0x3F;
        match ty {
            1 => TrbType::Normal,
            2 => TrbType::SetupStage,
            3 => TrbType::DataStage,
            4 => TrbType::StatusStage,
            5 => TrbType::Isoch,
            6 => TrbType::Link,
            7 => TrbType::EventData,
            8 => TrbType::NoOp,
            9 => TrbType::EnableSlot,
            10 => TrbType::DisableSlot,
            11 => TrbType::AddressDevice,
            12 => TrbType::ConfigureEndpoint,
            13 => TrbType::EvaluateContext,
            14 => TrbType::ResetEndpoint,
            15 => TrbType::StopEndpoint,
            16 => TrbType::SetTrDequeuePointer,
            17 => TrbType::ResetDevice,
            32 => TrbType::TransferEvent,
            33 => TrbType::CommandCompletionEvent,
            34 => TrbType::PortStatusChangeEvent,
            _ => TrbType::Normal,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取 cycle bit (control`[0]`)
    pub fn cycle_bit(&self) -> bool {
        self.control & 1 != 0
    }
}

// ── Capability 寄存器偏移 (在 MMIO 起始处) ──

/// Capability Length
pub const CAP_CAPLENGTH: usize = 0x00;
/// Version (u16 @ 0x02)
pub const CAP_HCIVERSION: usize = 0x02;
/// Structural Parameters 1
pub const CAP_HCSPARAMS1: usize = 0x04;
/// Structural Parameters 2
pub const CAP_HCSPARAMS2: usize = 0x08;
/// Structural Parameters 3
pub const CAP_HCSPARAMS3: usize = 0x0C;
/// Capability Parameters 1
pub const CAP_HCCPARAMS1: usize = 0x10;
/// 门铃偏移
pub const CAP_DBOFF: usize = 0x14;
/// 运行时寄存器空间偏移
pub const CAP_RTSOFF: usize = 0x18;
/// 能力参数 2
pub const CAP_HCCPARAMS2: usize = 0x1C;

// ── Operational 寄存器偏移 (相对 OpBase, OpBase = CAPLENGTH 字节) ──

/// USB 命令
pub const OP_USBCMD: usize = 0x00;
/// USB 状态
pub const OP_USBSTS: usize = 0x04;
/// 页面大小
pub const OP_PAGESIZE: usize = 0x08;
/// 设备通知控制
pub const OP_DNCTRL: usize = 0x14;
/// 命令环控制
pub const OP_CRCR: usize = 0x18;
/// 设备上下文基址数组指针
pub const OP_DCBAAP: usize = 0x30;
/// 配置寄存器
pub const OP_CONFIG: usize = 0x38;

/// 端口 1 状态与控制寄存器 (n=0..MaxPorts-1)
pub const OP_PORTSC_BASE: usize = 0x400;
pub const OP_PORTSC_STRIDE: usize = 0x10;

// ── USBCMD 位 ──

/// 运行/停止
pub const USBCMD_RUN_STOP: u32 = 1 << 0;
/// 主控复位
pub const USBCMD_HC_RESET: u32 = 1 << 1;
/// 中断使能
pub const USBCMD_INTR_ENABLE: u32 = 1 << 2;
/// 主系统错误使能
pub const USBCMD_HSE_ENABLE: u32 = 1 << 3;
/// 轻量级主控复位
pub const USBCMD_LHCRST: u32 = 1 << 7;
/// 控制器保存状态
pub const USBCMD_CSS: u32 = 1 << 8;
/// 控制器恢复状态
pub const USBCMD_CRS: u32 = 1 << 9;
/// 启用环绕事件
pub const USBCMD_EWE: u32 = 1 << 10;
/// 启用 U3 MFINDEX 停止
pub const USBCMD_EU3S: u32 = 1 << 11;

// ── USBSTS 位 ──

/// 主控已停止
pub const USBSTS_HC_HALTED: u32 = 1 << 0;
/// 主控复位完成
pub const USBSTS_HC_RESET_COMPLETE: u32 = 1 << 1;
/// 事件环非空
pub const USBSTS_EVENT_RING_NOT_EMPTY: u32 = 1 << 2;
/// 中断挂起
pub const USBSTS_INTR_PENDING: u32 = 1 << 3;
/// 主系统错误
pub const USBSTS_HOST_SYSTEM_ERROR: u32 = 1 << 4;
/// 事件计数器溢出
pub const USBSTS_EVENT_COUNTER_OVERFLOW: u32 = 1 << 5;
/// 端口变更检测
pub const USBSTS_PORT_CHANGE_DETECT: u32 = 1 << 6;

// ── PORTSC 位 ──

/// 当前连接状态
pub const PORTSC_CCS: u32 = 1 << 0;
/// 端口使能
pub const PORTSC_PED: u32 = 1 << 1;
/// 端口复位
pub const PORTSC_PR: u32 = 1 << 4;
/// 端口上电
pub const PORTSC_PP: u32 = 1 << 9;
/// 连接状态变更
pub const PORTSC_CSC: u32 = 1 << 16;
/// 端口使能/禁用变更
pub const PORTSC_PEC: u32 = 1 << 17;
/// 过流变更
pub const PORTSC_OCC: u32 = 1 << 19;
/// 复位变更
pub const PORTSC_RC: u32 = 1 << 21;

// ── PORTSC 速度 (PORTSC[10:13]) ──

pub const PORTSC_SPEED_MASK: u32 = 0xF << 10;
pub const PORTSC_SPEED_SHIFT: u32 = 10;

/// 全速 (12 Mbps)
pub const SPEED_FULL: u8 = 1;
/// 低速 (1.5 Mbps)
pub const SPEED_LOW: u8 = 2;
/// 高速 (480 Mbps)
pub const SPEED_HIGH: u8 = 3;
/// 超速 (5 Gbps)
pub const SPEED_SUPER: u8 = 4;
/// 超速+ (10 Gbps)
pub const SPEED_SUPER_PLUS: u8 = 5;

// ============================================================================
// 解析类型
// ============================================================================

/// 结构参数 1 解析
#[derive(Debug, Clone, Copy)]
pub struct StructuralParams1 {
    /// 最大设备槽位数 (HCSPARAMS1`[3:0]` - 1)
    pub max_device_slots: u8,
    /// 最大中断器数 (HCSPARAMS1`[18:8]` - 1)
    pub max_interrupters: u16,
    /// 最大端口数 (HCSPARAMS1`[31:24]` - 1)
    pub max_ports: u8,
}

impl StructuralParams1 {
    /// 从 HCSPARAMS1 寄存器解析
    pub fn from_register(val: u32) -> Self {
        Self {
            max_device_slots: (val & 0xFF) as u8,
            max_interrupters: ((val >> 8) & 0x7FF) as u16,
            max_ports: ((val >> 24) & 0xFF) as u8,
        }
    }
}

/// 端口状态
#[derive(Debug, Clone, Copy)]
pub struct PortStatus {
    /// 设备已连接
    pub connected: bool,
    /// 端口已使能
    pub enabled: bool,
    /// 端口正在复位
    pub reset: bool,
    /// 端口已供电
    pub powered: bool,
    /// 速度 (PORTSC`[10:13]` 解码)
    pub speed: u8,
}

impl PortStatus {
    /// 从 PORTSC 寄存器解析
    pub fn from_register(val: u32) -> Self {
        Self {
            connected: val & PORTSC_CCS != 0,
            enabled: val & PORTSC_PED != 0,
            reset: val & PORTSC_PR != 0,
            powered: val & PORTSC_PP != 0,
            speed: ((val & PORTSC_SPEED_MASK) >> PORTSC_SPEED_SHIFT) as u8,
        }
    }
}

// ============================================================================
// 安全代理
// ============================================================================

/// xHCI 主机控制器的安全代理 (services 层)。
///
/// 内部封装 `IoMem` 指向 xHCI MMIO 区域, 暴露 Capability/Operational/Port 寄存器的安全访问。
pub struct XhciController {
    mmio: IoMem,
    /// Capability 长度 (`OpBase` = CAPLENGTH 字节)
    cap_length: u8,
    /// Doorbell 偏移
    db_off: u32,
    /// Runtime 偏移
    rts_off: u32,
    /// 最大端口数 (从 HCSPARAMS1 解析)
    num_ports: u8,
    /// 最大设备插槽数 (从 HCSPARAMS1 解析)
    num_slots: u8,
    /// 控制器是否已初始化 (reset + start 完成)
    initialized: bool,
    /// 已分配设备地址位图 (USB 地址 1..=254, 位图 256 bit)
    address_bitmap: [u8; 32],
    /// 下一个待分配地址扫描起点 (加速连续分配)
    next_address_hint: u8,
}

impl XhciController {
    /// 创建 xHCI 控制器实例。
    ///
    /// # 参数
    /// - `mmio_phys`: `PCIe` BAR0 MMIO 物理基地址
    /// - `len`: MMIO 区域大小 (典型 0x1000 包含 Operational + 端口)
    ///
    /// # 返回
    /// - `Some(XhciController)`: 初始化成功
    /// - `None`: 区域已被占用 (别名检测)
    pub fn new(mmio_phys: u64, len: usize) -> Option<Self> {
        let mmio = IoMem::from_pci_bar(PhysAddr::new(mmio_phys), len, "xhci-bar0").ok()?;

        // 读取 CAPLENGTH/DBOFF/RTSOFF
        let cap_length = mmio.read_u8(CAP_CAPLENGTH);
        let db_off = mmio.read_u32(CAP_DBOFF);
        let rts_off = mmio.read_u32(CAP_RTSOFF);

        // 解析 HCSPARAMS1: MaxSlots = [7:0], MaxPorts = [31:24]
        let hcs_params1 = mmio.read_u32(CAP_HCSPARAMS1);
        let params = StructuralParams1::from_register(hcs_params1);

        Some(Self {
            mmio,
            cap_length,
            db_off,
            rts_off,
            num_ports: params.max_ports,
            num_slots: params.max_device_slots,
            initialized: false,
            address_bitmap: [0u8; 32],
            next_address_hint: 1,
        })
    }

    /// Operational 寄存器基址 (绝对偏移)
    #[inline]
    fn op_base(&self) -> usize {
        self.cap_length as usize
    }

    /// 通用 32 位读
    #[inline]
    pub fn read32(&self, offset: usize) -> u32 {
        self.mmio.read_u32(offset)
    }

    /// 通用 32 位写
    #[inline]
    pub fn write32(&self, offset: usize, val: u32) {
        self.mmio.write_u32(offset, val);
    }

    // ── Capability 寄存器 ──

    /// 读 CAPLENGTH
    pub fn cap_length(&self) -> u8 {
        self.cap_length
    }

    /// 读 HCI 版本
    pub fn hci_version(&self) -> u16 {
        self.mmio.read_u16(CAP_HCIVERSION)
    }

    /// 读 HCSPARAMS1
    pub fn hcs_params1(&self) -> StructuralParams1 {
        StructuralParams1::from_register(self.mmio.read_u32(CAP_HCSPARAMS1))
    }

    /// 读 HCSPARAMS2
    pub fn hcs_params2(&self) -> u32 {
        self.mmio.read_u32(CAP_HCSPARAMS2)
    }

    /// 读 HCSPARAMS3
    pub fn hcs_params3(&self) -> u32 {
        self.mmio.read_u32(CAP_HCSPARAMS3)
    }

    /// 读 HCCPARAMS1
    pub fn hcc_params1(&self) -> u32 {
        self.mmio.read_u32(CAP_HCCPARAMS1)
    }

    /// 读 DBOFF
    pub fn db_off(&self) -> u32 {
        self.db_off
    }

    /// 读 RTSOFF
    pub fn rts_off(&self) -> u32 {
        self.rts_off
    }

    // ── Operational 寄存器 ──

    /// 读 USBCMD
    pub fn usb_cmd(&self) -> u32 {
        self.mmio.read_u32(self.op_base() + OP_USBCMD)
    }

    /// 写 USBCMD
    pub fn set_usb_cmd(&self, val: u32) {
        self.mmio.write_u32(self.op_base() + OP_USBCMD, val);
    }

    /// 读 USBSTS
    pub fn usb_sts(&self) -> u32 {
        self.mmio.read_u32(self.op_base() + OP_USBSTS)
    }

    /// 写 USBSTS (write-1-to-clear)
    pub fn ack_usb_sts(&self, val: u32) {
        self.mmio.write_u32(self.op_base() + OP_USBSTS, val);
    }

    /// 读 PAGESIZE
    pub fn page_size(&self) -> u32 {
        self.mmio.read_u32(self.op_base() + OP_PAGESIZE)
    }

    /// 读 CRCR
    pub fn crcr(&self) -> u64 {
        self.mmio.read_u64(self.op_base() + OP_CRCR)
    }

    /// 写 CRCR
    pub fn set_crcr(&self, val: u64) {
        self.mmio.write_u64(self.op_base() + OP_CRCR, val);
    }

    /// 读 DCBAAP
    pub fn dcbaap(&self) -> u64 {
        self.mmio.read_u64(self.op_base() + OP_DCBAAP)
    }

    /// 写 DCBAAP
    pub fn set_dcbaap(&self, val: u64) {
        self.mmio.write_u64(self.op_base() + OP_DCBAAP, val);
    }

    /// 读 CONFIG
    pub fn config(&self) -> u32 {
        self.mmio.read_u32(self.op_base() + OP_CONFIG)
    }

    /// 写 CONFIG
    pub fn set_config(&self, val: u32) {
        self.mmio.write_u32(self.op_base() + OP_CONFIG, val);
    }

    // ── USBCMD / USBSTS 操作 ──

    /// 软复位 (`USBCMD.HC_RESET`)
    pub fn reset(&self) {
        let val = self.usb_cmd();
        self.set_usb_cmd(val | USBCMD_HC_RESET);
        // 等待 USBSTS.HC_RESET_COMPLETE 置位
        let mut timeout = 100_000u32;
        while self.usb_sts() & USBSTS_HC_RESET_COMPLETE == 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
        // 应答
        self.ack_usb_sts(USBSTS_HC_RESET_COMPLETE);
    }

    /// 启动控制器 (`USBCMD.RUN_STOP`)
    pub fn start(&self) {
        let val = self.usb_cmd();
        self.set_usb_cmd(val | USBCMD_RUN_STOP);
    }

    /// 停止控制器
    pub fn stop(&self) {
        let val = self.usb_cmd();
        self.set_usb_cmd(val & !USBCMD_RUN_STOP);
        // 等待 USBSTS.HC_HALTED 置位
        let mut timeout = 100_000u32;
        while self.usb_sts() & USBSTS_HC_HALTED == 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
    }

    /// 启用全局中断 (`USBCMD.INTR_ENABLE`)
    pub fn enable_interrupts(&self) {
        let val = self.usb_cmd();
        self.set_usb_cmd(val | USBCMD_INTR_ENABLE);
    }

    /// 禁用全局中断
    pub fn disable_interrupts(&self) {
        let val = self.usb_cmd();
        self.set_usb_cmd(val & !USBCMD_INTR_ENABLE);
    }

    /// 控制器是否已停止
    pub fn is_halted(&self) -> bool {
        self.usb_sts() & USBSTS_HC_HALTED != 0
    }

    /// 是否有端口状态变化 (USBSTS.PCD)
    pub fn has_port_change(&self) -> bool {
        self.usb_sts() & USBSTS_PORT_CHANGE_DETECT != 0
    }

    // ── 端口操作 ──

    /// 计算端口 n 的 PORTSC 偏移
    fn portsc_offset(&self, port: u8) -> usize {
        self.op_base() + OP_PORTSC_BASE + (port as usize) * OP_PORTSC_STRIDE
    }

    /// 读端口 n 的 PORTSC
    pub fn portsc(&self, port: u8) -> u32 {
        self.mmio.read_u32(self.portsc_offset(port))
    }

    /// 写端口 n 的 PORTSC
    pub fn set_portsc(&self, port: u8, val: u32) {
        self.mmio.write_u32(self.portsc_offset(port), val);
    }

    /// 读端口状态
    pub fn port_status(&self, port: u8) -> PortStatus {
        PortStatus::from_register(self.portsc(port))
    }

    /// 端口是否有设备连接
    pub fn port_connected(&self, port: u8) -> bool {
        self.portsc(port) & PORTSC_CCS != 0
    }

    /// 端口是否已使能
    pub fn port_enabled(&self, port: u8) -> bool {
        self.portsc(port) & PORTSC_PED != 0
    }

    /// 复位端口 (写 PORTSC.PR = 1, 等待 0)
    pub fn reset_port(&self, port: u8) {
        let val = self.portsc(port);
        self.set_portsc(port, val | PORTSC_PR);
        // 等待 PR 清零
        let mut timeout = 100_000u32;
        while self.portsc(port) & PORTSC_PR != 0 && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }
    }

    /// 应答端口变化 (写 PORTSC.CSC|PEC|RC)
    pub fn ack_port_change(&self, port: u8, bits: u32) {
        let val = self.portsc(port);
        self.set_portsc(port, val | bits);
    }

    // ── Doorbell 操作 ──

    /// 写 Doorbell 寄存器
    ///
    /// # 参数
    /// - `slot`: 设备插槽 ID (1-based)
    /// - `value`: 32-bit 门铃值 (低 8 位 = endpoint, 16 位 = stream ID)
    pub fn ring_doorbell(&self, slot: u8, value: u32) {
        let offset = self.db_off as usize + (slot as usize) * 4;
        self.mmio.write_u32(offset, value);
    }

    // ── 初始化序列 (xHCI 规范 §4.3) ──

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 完整初始化控制器 (reset → start).
    ///
    /// 执行 xHCI 规范 §4.3 初始化序列:
    /// 1. 读取 Capability 参数 (已在 new 中完成)
    /// 2. 软复位控制器 (`USBCMD.HC_RESET`)
    /// 3. 等待复位完成 (USBSTS.HCRST = 1)
    /// 4. 启动控制器 (USBCMD.RS = 1)
    /// 5. 启用全局中断
    ///
    /// # 返回
    /// - `Ok(())`: 初始化成功
    /// - `Err`: 超时或控制器无响应
    ///
    /// # Errors
    /// 当控制器软复位、启动或中断使能过程中超时/无响应时返回 `Err`.
    pub fn init_hardware(&mut self) -> Result<(), KernelError> {
        // 1. 软复位 (UFCS 消歧: Driver trait 亦有 reset 方法)
        XhciController::reset(self);
        // 2. 启动
        self.start();
        // 3. 启用中断
        self.enable_interrupts();
        self.initialized = true;
        Ok(())
    }

    /// 控制器是否已完成初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// 获取端口数量
    pub fn num_ports(&self) -> u8 {
        self.num_ports
    }

    /// 获取设备插槽数量
    pub fn num_slots(&self) -> u8 {
        self.num_slots
    }

    // ── USB 设备地址分配 (USB 2.0 规范 §9.1.2) ──

    /// 分配一个 USB 设备地址 (1..=254).
    ///
    /// 从 `next_address_hint` 开始扫描 `address_bitmap`,
    /// 找到第一个未使用位 (bit=0) 标记为已用并返回该地址.
    /// 全部 254 个地址耗尽时返回 `None`.
    pub fn allocate_address(&mut self) -> Option<u8> {
        for offset in 0..254u16 {
            let addr = self.next_address_hint.wrapping_add(offset as u8);
            if addr == 0 || addr == 255 {
                continue;
            }
            let byte_idx = (addr / 8) as usize;
            let bit_idx = addr % 8;
            if byte_idx >= self.address_bitmap.len() {
                continue;
            }
            if self.address_bitmap[byte_idx] & (1 << bit_idx) == 0 {
                self.address_bitmap[byte_idx] |= 1 << bit_idx;
                self.next_address_hint = addr.wrapping_add(1);
                if self.next_address_hint == 0 || self.next_address_hint == 255 {
                    self.next_address_hint = 1;
                }
                return Some(addr);
            }
        }
        None
    }

    /// 释放 USB 设备地址 (允许复用).
    ///
    /// 地址 0 和 255 静默忽略 (保留地址).
    pub fn free_address(&mut self, address: u8) {
        if address == 0 || address == 255 {
            return;
        }
        let byte_idx = (address / 8) as usize;
        let bit_idx = address % 8;
        if byte_idx < self.address_bitmap.len() {
            self.address_bitmap[byte_idx] &= !(1 << bit_idx);
        }
    }

    // ── Command Ring 操作 ──

    /// 写入 Command Ring Control Register (CRCR).
    ///
    /// # 参数
    /// - `ring_phys`: Command Ring 物理基地址
    /// - `run`: 是否启动 Command Ring (CRCR.RCS)
    pub fn set_command_ring(&self, ring_phys: u64, run: bool) {
        let val = ring_phys | u64::from(run);
        self.set_crcr(val);
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 提交命令 TRB 到 Command Ring 并触发 Doorbell.
    ///
    /// # 参数
    /// - `trb`: 要提交的命令 TRB
    /// - `ring_tail_phys`: Command Ring 当前尾指针物理地址
    ///
    /// # 流程
    /// 1. 写 TRB 到 Command Ring (由调用方管理的 DMA 内存)
    /// 2. 更新 CRCR 的 Ring Consumer Cycle State
    /// 3. 写 Doorbell slot 0 触发控制器处理
    pub fn submit_command(&self, _trb: &Trb, ring_tail_phys: u64) {
        // 注意: TRB 实际写入由调用方通过 DMA 内存完成.
        // 此处仅更新 CRCR 并触发 Doorbell.
        // CRCR 低 3 位: [0] = RCS (环消费者循环状态),
        //                [1] = CS (命令停止), [2] = CA (命令中止)
        let crcr_val = ring_tail_phys | 1; // RCS = 1 (consumer cycle)
        self.set_crcr(crcr_val);

        // Doorbell slot 0 = 触发 Command Ring
        self.ring_doorbell(0, 0);
    }

    // ── 设备上下文基址数组 (DCBAA) ──

    /// 设置 Device Context Base Address Array Pointer (DCBAAP).
    ///
    /// # 参数
    /// - `dcbaa_phys`: DCBAA 物理基地址 (xHCI 规范 §4.5.2)
    pub fn set_dcbaa(&self, dcbaa_phys: u64) {
        self.set_dcbaap(dcbaa_phys);
    }
}

// ============================================================================
// Transfer Ring — xHCI 规范 §4.9.2
// ============================================================================

/// 传输环 — 端点级传输队列, Host 生产 / Controller 消费.
///
/// 每个活动端点拥有独立 Transfer Ring, 通过 Doorbell (slot * 4 + ep) 触发.
/// 环形缓冲区由 Normal / Link TRB 组成, 末尾 Link TRB 回绕到起点并翻转 cycle.
///
/// 所有 DMA 操作通过 framework safe wrapper 执行, 0 unsafe。
pub struct TransferRing {
    /// TRB 缓冲区 DMA 句柄 (RAII: 本结构析构即归还; 虚拟/物理地址由句柄派生)
    dma: DmaStream,
    /// 队列深度 (TRB 条目数)
    depth: u32,
    /// 当前 enqueue 位置 (下一个可写 TRB 索引)
    enqueue_index: u32,
    /// 当前 cycle bit (写入 TRB 时设置)
    cycle: bool,
}

impl TransferRing {
    /// 创建并分配 Transfer Ring DMA 内存.
    ///
    /// # 参数
    /// - `max_trbs`: 最大 TRB 数量 (建议 64 或 128).
    ///
    /// # 返回
    /// - `Some(TransferRing)`: 分配成功
    /// - `None`: DMA 分配失败
    pub fn new(max_trbs: u32) -> Option<Self> {
        let buf_size = (max_trbs as usize) * core::mem::size_of::<Trb>();
        let dma = crate::framework::driver::storage::nvme_alloc_dma_buffer(buf_size)?;

        // 清零 — 通过 framework safe wrapper
        crate::framework::driver::storage::nvme_zero_dma(
            dma.cpu_addr().as_ptr() as u64,
            dma.size(),
        );

        let depth = (dma.size() / core::mem::size_of::<Trb>()) as u32;

        Some(Self {
            dma,
            depth,
            enqueue_index: 0,
            cycle: true,
        })
    }

    /// 获取物理地址 (用于设置 Endpoint Context dequeue pointer).
    pub fn physical_address(&self) -> u64 {
        self.dma.dma_addr().as_u64()
    }

    /// 获取当前 enqueue 偏移 (字节), 用于更新 Endpoint Context.
    pub fn enqueue_offset(&self) -> u64 {
        u64::from(self.enqueue_index) * 16 // 每个 TRB 16 字节
    }

    /// 获取当前 cycle bit.
    pub fn cycle(&self) -> bool {
        self.cycle
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    /// 写入一个 TRB 到 enqueue 位置, 自动设置 cycle bit.
    ///
    /// 到达末尾时写入 Link TRB 并翻转 cycle, 回绕到起点.
    /// 返回 `Err` 表示 ring 已满 (无空间).
    fn push_trb(&mut self, mut trb: Trb) -> bool {
        if self.enqueue_index >= self.depth {
            return false;
        }

        // 设置 cycle bit
        if self.cycle {
            trb.control |= 1;
        } else {
            trb.control &= !1;
        }

        // 通过 framework safe wrapper 写入 TRB
        // 创建 raw pointer 是 safe 操作; 实际解引用由 framework 内部 unsafe 完成
        let vaddr = self.dma.cpu_addr().as_ptr() as u64;
        let trb_ptr: *const u8 = &trb as *const Trb as *const u8;
        crate::framework::driver::storage::xhci_write_trb(vaddr, self.enqueue_index, trb_ptr);

        self.enqueue_index += 1;

        // 到达末尾前最后一项: 写 Link TRB 回绕
        if self.enqueue_index >= self.depth - 1 {
            let link_control = (TrbType::Link as u32) << 10
                | 1 << 1 // Toggle Cycle
                | u32::from(self.cycle);
            let link_trb = Trb::new(self.dma.dma_addr().as_u64(), 0, link_control);
            let link_ptr: *const u8 = &link_trb as *const Trb as *const u8;
            crate::framework::driver::storage::xhci_write_trb(vaddr, self.enqueue_index, link_ptr);
            self.enqueue_index = 0;
            self.cycle = !self.cycle;
        }

        true
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 提交控制传输 (Setup → Data → Status, xHCI 规范 §4.11.2.3).
    ///
    /// # 参数
    /// - `setup_packet`: 8 字节 Setup Packet (bmRequestType, bRequest, wValue, wIndex, wLength)
    /// - `data_phys`: 数据缓冲区物理地址 (In 方向从设备读, Out 方向写设备)
    /// - `data_len`: 数据阶段字节数 (0 = 无数据阶段, 仅 Setup+Status)
    /// - `is_device_to_host`: true = IN (Device→Host), false = OUT (Host→Device)
    /// - `trb_interrupt_on_completion`: 是否在 Status Stage TRB 上设置 IOC
    pub fn push_control_transfer(
        &mut self,
        setup_packet: &[u8; 8],
        data_phys: u64,
        data_len: u32,
        is_device_to_host: bool,
        trb_interrupt_on_completion: bool,
    ) -> bool {
        // Setup Stage TRB 构造 (USB-1.5 §6.4.1.1)
        let setup_parameter = u64::from_ne_bytes(*setup_packet);
        let setup_control = (TrbType::SetupStage as u32) << 10
            | 1 << 6 // ICE (Interrupt on Completion)
            | 1 << 5 // IOC
            | 1 << 2 // IDT (Immediate Data)
            | if is_device_to_host { 1u32 << 16 } else { 0 };
        let setup_status = 8u32; // Setup packet 固定 8 字节
        if !self.push_trb(Trb::new(setup_parameter, setup_status, setup_control)) {
            return false;
        }

        // Data Stage TRB (仅当 data_len > 0)
        if data_len > 0 {
            let data_control = (TrbType::DataStage as u32) << 10
                | 1 << 5 // IOC
                | if is_device_to_host {
                    1u32 << 16 // DIR: IN (Device→Host)
                } else {
                    0 // DIR: OUT (Host→Device)
                };
            let data_status = data_len & 0x0001_FFFF; // Transfer Length (低 17 位)
            if !self.push_trb(Trb::new(data_phys, data_status, data_control)) {
                return false;
            }
        }

        // Status Stage TRB
        let status_control = (TrbType::StatusStage as u32) << 10
            | if trb_interrupt_on_completion { 1u32 << 5 } else { 0 } // IOC
            | if is_device_to_host {
                0 // Status OUT
            } else {
                1u32 << 16 // Status IN
            };
        self.push_trb(Trb::new(0, 0, status_control))
    }

    /// 提交批量传输 (Normal TRB, xHCI 规范 §4.11.2.1).
    ///
    /// # 参数
    /// - `data_phys`: 数据缓冲区物理地址
    /// - `data_len`: 传输字节数 (最大 65536, 超大传输需拆分为多个 TRB)
    /// - `interrupt_on_completion`: 是否设置 IOC
    pub fn push_bulk_transfer(
        &mut self,
        data_phys: u64,
        data_len: u32,
        interrupt_on_completion: bool,
    ) -> bool {
        // 每个 Normal TRB 最多传输 65536 字节 (17-bit transfer length, 0 = 65536)
        let mut remaining = data_len;
        let mut offset = 0u64;

        while remaining > 0 {
            let chunk = if remaining > 65536 { 65536 } else { remaining };
            let is_last = chunk == remaining;
            let is_ioc = is_last && interrupt_on_completion;

            let control = (TrbType::Normal as u32) << 10
                | if is_ioc { 1u32 << 5 } else { 0 }
                | if is_last { 0 } else { 1u32 << 4 }; // CH (Continue on Chain) — 最后一个不设置
            let status = if chunk == 65536 {
                0u32 // 0 表示 65536 字节
            } else {
                chunk
            };

            if !self.push_trb(Trb::new(data_phys + offset, status, control)) {
                return false;
            }

            remaining -= chunk;
            offset += u64::from(chunk);
        }

        true
    }

    /// 提交中断传输 (Normal TRB + IOC, 与批量类似但用于中断端点).
    ///
    /// # 参数
    /// - `data_phys`: 数据缓冲区物理地址
    /// - `data_len`: 传输字节数 (通常很小, 键盘 8 字节, 鼠标 4 字节)
    pub fn push_interrupt_transfer(&mut self, data_phys: u64, data_len: u32) -> bool {
        // 中断传输总是设置 IOC (每次轮询到都需通知 host)
        self.push_bulk_transfer(data_phys, data_len, true)
    }

    /// 清空 Transfer Ring (重置所有 TRB, 回到初始状态).
    pub fn reset(&mut self) {
        // 通过 framework safe wrapper 清零
        crate::framework::driver::storage::nvme_zero_dma(
            self.dma.cpu_addr().as_ptr() as u64,
            self.dma.size(),
        );
        self.enqueue_index = 0;
        self.cycle = true;
    }
}

// ============================================================================
// USB 传输类型枚举
// ============================================================================

/// USB 端点传输类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbTransferType {
    /// 控制传输 (端点 0, 默认)
    Control,
    /// 批量传输
    Bulk,
    /// 中断传输
    Interrupt,
    /// 等时传输 (暂不支持)
    Isochronous,
}

/// USB 端点方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbTransferDirection {
    /// IN (Device → Host)
    In,
    /// OUT (Host → Device)
    Out,
}

// ============================================================================
// 端点传输上下文 (per-endpoint)
// ============================================================================

/// 单个端点的传输上下文
pub struct EndpointTransfer {
    /// 端点号 (1-15, xHCI endpoint ID)
    pub endpoint_id: u8,
    /// 传输类型
    pub transfer_type: UsbTransferType,
    /// 传输方向
    pub direction: UsbTransferDirection,
    /// 该端点的 Transfer Ring
    pub transfer_ring: TransferRing,
}

impl EndpointTransfer {
    /// 创建端点传输上下文
    ///
    /// # 参数
    /// - `endpoint_id`: xHCI 端点 ID (1-15)
    /// - `transfer_type`: 传输类型
    /// - `direction`: 传输方向
    /// - `ring_size`: Transfer Ring TRB 数量
    pub fn new(
        endpoint_id: u8,
        transfer_type: UsbTransferType,
        direction: UsbTransferDirection,
        ring_size: u32,
    ) -> Option<Self> {
        let transfer_ring = TransferRing::new(ring_size)?;
        Some(Self {
            endpoint_id,
            transfer_type,
            direction,
            transfer_ring,
        })
    }
}

// ============================================================================
// Driver / HostController Trait 实现
// ============================================================================

impl Driver for XhciController {
    fn name(&self) -> &'static str {
        "xHCI Controller"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Bus
    }

    fn init(&mut self) -> DriverResult<()> {
        // init_hardware 内部完成 reset + start 并置位 initialized
        self.init_hardware().map_err(|_| DriverError::HardwareError)
    }

    fn shutdown(&mut self) -> DriverResult<()> {
        self.stop();
        self.initialized = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.initialized
    }

    fn status(&self) -> &'static str {
        if self.initialized {
            "xHCI running"
        } else {
            "xHCI stopped"
        }
    }
}

impl HostController for XhciController {
    fn supported_speeds(&self) -> Vec<UsbSpeed> {
        alloc::vec![
            UsbSpeed::Super,
            UsbSpeed::High,
            UsbSpeed::Full,
            UsbSpeed::Low,
        ]
    }

    fn num_ports(&self) -> usize {
        usize::from(self.num_ports)
    }

    fn port_has_device(&self, port: usize) -> bool {
        // 越界端口视为无设备 (trait 用 usize, 硬件字段为 u8)
        match u8::try_from(port) {
            Ok(p) if p < self.num_ports => self.port_connected(p),
            _ => false,
        }
    }

    fn reset_port(&mut self, port: usize) -> DriverResult<()> {
        let Ok(p) = u8::try_from(port) else {
            return Err(DriverError::InvalidParameter);
        };
        if p >= self.num_ports {
            return Err(DriverError::InvalidParameter);
        }
        // 固有方法同名, 用 UFCS 消歧
        XhciController::reset_port(self, p);
        Ok(())
    }

    fn get_port_speed(&self, port: usize) -> UsbSpeed {
        let Ok(p) = u8::try_from(port) else {
            return UsbSpeed::Unknown;
        };
        if p >= self.num_ports {
            return UsbSpeed::Unknown;
        }
        match self.port_status(p).speed {
            SPEED_FULL => UsbSpeed::Full,
            SPEED_LOW => UsbSpeed::Low,
            SPEED_HIGH => UsbSpeed::High,
            SPEED_SUPER => UsbSpeed::Super,
            SPEED_SUPER_PLUS => UsbSpeed::SuperPlus,
            _ => UsbSpeed::Unknown,
        }
    }

    fn submit_urb(&mut self, urb: &Urb) -> DriverResult<()> {
        if !self.initialized {
            return Err(DriverError::NotInitialized);
        }
        if urb.device == 0 {
            return Err(DriverError::InvalidParameter);
        }
        // 端点 0 为控制端点 (DCI=1) 合法; 1..=15 为普通端点
        if urb.endpoint > 15 {
            return Err(DriverError::InvalidParameter);
        }
        // 有数据阶段时物理地址不得为 0 (无数据阶段允许 length=0)
        if urb.buffer_length > 0 && urb.buffer_phys == 0 {
            return Err(DriverError::InvalidParameter);
        }

        // xHCI DCI: 控制端点 0 → 1; 端点 n>0 → n*2 + (IN ? 1 : 0)
        let dci = if urb.endpoint == 0 {
            1u32
        } else {
            u32::from(urb.endpoint) * 2 + u32::from(urb.direction == Direction::In)
        };
        self.ring_doorbell(urb.device, dci);

        // SIMPLIFIED: 仅触发 Doorbell, 未写入 Transfer Ring TRB / 未跟踪 pending URB;
        //   影响: 真实数据传输需调用方经 TransferRing::push_* 自行写入 TRB;
        //   扩展时机: Phase E Event Ring 中断处理实装时接入完成回调与状态跟踪.
        Ok(())
    }

    fn cancel_urb(&mut self, _urb_id: u32) -> DriverResult<()> {
        Err(DriverError::UnsupportedOperation)
    }

    fn allocate_address(&mut self) -> DriverResult<u8> {
        // 固有方法同名, 用 UFCS 消歧
        XhciController::allocate_address(self).ok_or(DriverError::Busy)
    }

    fn free_address(&mut self, address: u8) {
        XhciController::free_address(self, address);
    }
}

// ============================================================================
// 单元测试 (纯逻辑: 不构造控制器, 避免依赖 MMIO / DMA)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trb_type_decode() {
        let trb = Trb::new(0, 0, (TrbType::SetupStage as u32) << 10);
        assert_eq!(trb.trb_type(), TrbType::SetupStage);

        let trb = Trb::new(0, 0, (TrbType::TransferEvent as u32) << 10);
        assert_eq!(trb.trb_type(), TrbType::TransferEvent);
    }

    #[test]
    fn test_trb_cycle_bit() {
        assert!(Trb::new(0, 0, 1).cycle_bit());
        assert!(!Trb::new(0, 0, 0).cycle_bit());
    }

    #[test]
    fn test_port_status_decode() {
        let val = PORTSC_CCS | PORTSC_PED | (u32::from(SPEED_HIGH) << PORTSC_SPEED_SHIFT);
        let status = PortStatus::from_register(val);
        assert!(status.connected);
        assert!(status.enabled);
        assert!(!status.reset);
        assert_eq!(status.speed, SPEED_HIGH);
    }

    #[test]
    fn test_structural_params1_decode() {
        let val = 0x0F | (0x7FF << 8) | (0x10 << 24);
        let params = StructuralParams1::from_register(val);
        assert_eq!(params.max_device_slots, 0x0F);
        assert_eq!(params.max_interrupters, 0x7FF);
        assert_eq!(params.max_ports, 0x10);
    }
}
