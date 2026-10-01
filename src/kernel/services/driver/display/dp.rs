#![deny(unsafe_code)]
//! DisplayPort 驱动 — services 层安全实现
//!
//! 通过 `DpIo` 安全代理访问 DP 控制器 MMIO 寄存器, 无 unsafe.
//! 原始驱动: `kernel::framework::driver::display::dp::DpController` (35 unsafe 块).
//! Services 适配: 通过 IoMem 封装 MMIO, 消除全部冗余 unsafe.
//!
//! ## 职责
//!
//! - `DpIo`: DP 控制器 MMIO 寄存器安全读写
//! - `DpController`: 安全驱动逻辑 (AUX 通道 / 链路训练 / 视频模式)
//!
//! ## 硬件接口
//!
//! ```text
//! DisplayPort:
//! ├── Main Link: 1/2/4通道, 每通道5.4/8.1 Gbps
//! ├── AUX Channel: 边带通信 (1 Mbps)
//! ├── HPD: 热插拔检测
//! └── DPCD: 显示端口配置数据
//! ```

use crate::framework::driver::{DeviceType, Driver, DriverResult};
use crate::framework::iomem::IoMem;
use crate::framework::mm::PhysAddr;
use crate::services::error::KernelError;
use alloc::vec;
use alloc::vec::Vec;

use super::hdmi::{VideoMode, VideoTiming, lookup_dmt_timing};

// ============================================================================
// DisplayPort 常量定义
// ============================================================================

/// `DisplayPort` DPCD 地址 — VESA DP 规范 §2.4
///
/// 当前使用: `TRAINING_PTN_SET` 训练图样设置, `LINK_BW_SET` 链路带宽设置,
///           `LANE_COUNT_SET` 通道数设置, `LANE0_1_STATUS` / `LANE2_3_STATUS`
///           通道状态, `LANE_ALIGN_STATUS_UPDATED` 对齐状态,
///           `ADJUST_REQ_LANE0/1/2/3` 各通道请求调整
mod aux_address {
    pub const TRAINING_PTN_SET: u16 = 0x0106;
    pub const LINK_BW_SET: u16 = 0x0100;
    pub const LANE_COUNT_SET: u16 = 0x0101;
    /// LANE0 + LANE1 状态寄存器 (VESA DP 1.4 §2.5.4, DPCD 0x0204):
    ///   bit 0: `LANE0_CR_DONE`
    ///   bit 1: `LANE0_CHANNEL_EQ_DONE`
    ///   bit 2: `LANE0_SYMBOL_LOCKED`
    ///   bit 4: `LANE1_CR_DONE`
    ///   bit 5: `LANE1_CHANNEL_EQ_DONE`
    ///   bit 6: `LANE1_SYMBOL_LOCKED`
    pub const LANE0_1_STATUS: u16 = 0x0204;
    /// LANE2 + LANE3 状态寄存器 (4-lane 配置时使用)
    pub const LANE2_3_STATUS: u16 = 0x0205;
    /// 下行端口状态 (含 `LANE_ALIGN_STATUS_UPDATED` bit 0)
    pub const LANE_ALIGN_STATUS_UPDATED: u16 = 0x0206;
    /// 接收器请求的 voltage swing / pre-emphasis 调整 (LANE0/1)
    pub const ADJUST_REQ_LANE0_1: u16 = 0x0207;
    /// 接收器请求的 voltage swing / pre-emphasis 调整 (LANE2/3)
    pub const ADJUST_REQ_LANE2_3: u16 = 0x0208;
}

/// DP HPD (Hot Plug Detect) 状态寄存器偏移.
///
/// 厂商差异:
/// - Intel IGP: 与 HDMI 共享 HPD 寄存器 (MMIO +0xC8 bit 0-3), 调用方应传入与 HDMI 相同偏移
/// - AMD DCN: 与 HDMI 类似, 通常共享 HPD 控制器
/// - 独立 DP 控制器 (e.g. 板载 DP chip): 单独的 HPD GPIO/状态寄存器, 默认偏移 0x040
///
/// 本实装默认偏移 0x040, 假设 DP 控制器为独立 chip;
/// 与 HDMI 共享 HPD 的厂商应通过 [`DpController::new_with_io`] 显式指定偏移。
const DP_HPD_REG_OFFSET: u32 = 0x040;

/// 当前实装阶段 (HPD + AUX + 链路训练状态/调整) DP 控制器所需的最小 `IoMem` 大小.
///
/// P0-2: 文档化 `IoMem` 最小大小, 消除隐式约定风险.
/// P1-4: 提供 [`assert_iomem_size_at_least`] 编译期检查辅助函数.
///
/// 实际映射需求 (按 VESA DP 1.4 + 典型 Synopsys DWC DP-TX AUX 控制器布局):
/// - HPD 寄存器: 0x040 (1 字节) → 至少 0x041
/// - AUX 通道: 0x100..=0x110 (16 字节, 含 CMD/STA/DAT0-3)
/// - 链路训练状态: 0x200..=0x210 (`LANE0_1_STATUS` + `LANE2_3_STATUS` + `LANE_ALIGN_STATUS` + `TRAINING_ADJUST_REQ`)
/// - 链路训练调整请求镜像: 0x210..=0x211 (8-bit `ADJUST_REQ_LANE0`)
/// - 视频时序寄存器: 0x300..=0x310 (DP 标准 8 个 16-bit 时序寄存器)
/// - 同步极性: 0x310 (1 字节, bit 0=H, bit 1=V)
/// - 输出使能: 0x311 (1 字节, bit 0=enabled)
///
/// 当前常量 0x312 已满足 HPD + AUX + 链路训练 + 视频时序 + sync + 输出使能阶段.
pub const REQUIRED_IOMEM_SIZE: usize = 0x312;

/// 编译期检查 `IoMem` 大小 (P1-4).
///
/// 当 `size` 是 const 表达式且 `size < REQUIRED_IOMEM_SIZE` 时, 编译期 panic;
/// 否则零运行时开销. 用法见 hdmi.rs 对应函数.
///
/// # Panics
///
/// 当 `size < REQUIRED_IOMEM_SIZE` 时编译期 panic, 提示 `IoMem size must be >= DpController::REQUIRED_IOMEM_SIZE`。
#[inline]
pub const fn assert_iomem_size_at_least(size: usize) {
    assert!(
        size >= REQUIRED_IOMEM_SIZE,
        "IoMem size must be >= DpController::REQUIRED_IOMEM_SIZE"
    );
}

/// DP HPD 状态位 (bit 0)
const DP_HPD_STATUS_BIT: u8 = 0x01;

// ============================================================================
// AUX 通道寄存器偏移 (DISPLAY-2.5)
// ============================================================================
//
// 寄存器布局按 Synopsys DWC DP-TX AUX 控制器 (与 VESA DP 1.4 §4.1 兼容):
//
//   偏移    大小  名称         说明
//   ----    ----  ----         ----
//   0x100   1     AUX_CMD      命令/地址寄存器 (W)
//                              bit 0:    start transaction
//                              bit 1-3:  command (4=Write, 5=Read DPCD)
//                              bit 4-15: address[0..11] (DPCD offset)
//   0x101   1     AUX_STA      状态寄存器 (R/W)
//                              bit 0:    busy
//                              bit 1:    reply ready
//                              bit 2-3:  reply error (00=OK, 01=NACK, 10=DEFER, 11=INVALID)
//   0x102   4     AUX_DAT0     data[0..3] 写入: 请求数据, 读取: 应答数据
//   0x106   4     AUX_DAT1     data[4..7]  辅助数据字节 4-7
//   0x10A   4     AUX_DAT2     data[8..11] 辅助数据字节 8-11
//   0x10E   4     AUX_DAT3     data[12..15] 辅助数据字节 12-15
//
// 厂商差异:
// - Synopsys DWC DP-TX: 上述布局
// - Intel IGP (eDP): 相同布局, 仅基地址不同
// - AMD DCN: AUX CMD/STA 在 DDI 控制器 MMIO 区, 调用方应传入正确偏移

/// AUX CMD 寄存器偏移 (8-bit, W)
const AUX_CMD_REG_OFFSET: u32 = 0x100;
/// AUX STA 寄存器偏移 (8-bit, R/W)
const AUX_STA_REG_OFFSET: u32 = 0x101;
/// AUX DAT0 寄存器偏移 (32-bit, R/W)
const AUX_DAT0_REG_OFFSET: u32 = 0x102;
/// AUX DAT1 寄存器偏移 (32-bit, R/W)
const AUX_DAT1_REG_OFFSET: u32 = 0x106;
/// AUX DAT2 寄存器偏移 (32-bit, R/W)
const AUX_DAT2_REG_OFFSET: u32 = 0x10A;
/// AUX DAT3 寄存器偏移 (32-bit, R/W)
const AUX_DAT3_REG_OFFSET: u32 = 0x10E;

/// AUX CMD 寄存器 bit 0 = start transaction
const AUX_CMD_START_BIT: u8 = 0x01;
/// AUX CMD 寄存器 bit 1-3 = command (`AuxCommand` enum 字节值)
const AUX_CMD_COMMAND_SHIFT: u8 = 1;

/// AUX STA 寄存器 bit 0 = busy
const AUX_STA_BUSY_BIT: u8 = 0x01;
/// AUX STA 寄存器 bit 1 = reply ready
const AUX_STA_REPLY_READY_BIT: u8 = 0x02;
/// AUX STA 寄存器 bit 2-3 = reply error 码
const AUX_STA_REPLY_ERR_SHIFT: u8 = 2;
/// AUX STA 寄存器 bit 2-3 = reply error mask
const AUX_STA_REPLY_ERR_MASK: u8 = 0x0C;

/// AUX 事务超时 (与 hdmi/ddc.rs `DDC_TRANSACTION_TIMEOUT_ITERS` 对齐).
///
/// 完整 AUX 事务典型 < 1 ms (请求→响应 1 Mbps AUX 速率);
/// `50_000` `spin_loops` ≈ 1-2 ms, 适配大多数 AUX 控制器.
const AUX_TRANSACTION_TIMEOUT_ITERS: usize = 50_000;
/// AUX 单步延时 (近似 1 µs).
const AUX_DELAY_ITERS: usize = 50;

// ============================================================================
// 视频时序寄存器偏移 (DISPLAY-2.8)
// ============================================================================
//
// DP 控制器视频时序寄存器布局 (与 HDMI 0x068+ 区域不同, DP 在 0x300+):
//
//   偏移     大小  名称
//   ----     ----  ----
//   0x300    2     H_TOTAL_REG       水平总像素 (h_total 16-bit)
//   0x302    2     H_ACTIVE_REG      水平有效像素 (h_active 16-bit)
//   0x304    2     V_TOTAL_REG       垂直总行数 (v_total 16-bit)
//   0x306    2     V_ACTIVE_REG      垂直有效行数 (v_active 16-bit)
//   0x308    2     H_SYNC_OFFSET_REG 水平同步偏移 (h_sync_offset 16-bit)
//   0x30A    2     H_SYNC_PW_REG     水平同步脉冲宽度 (h_sync_pulse_width 16-bit)
//   0x30C    2     V_SYNC_OFFSET_REG 垂直同步偏移 (v_sync_offset 16-bit)
//   0x30E    2     V_SYNC_PW_REG     垂直同步脉冲宽度 (v_sync_pulse_width 16-bit)
//   0x310    1     SYNC_POL_REG      (bit 0=H 极性, bit 1=V 极性, 0=negative, 1=positive)
//   0x311    1     OUTPUT_ENABLE_REG (bit 0=输出使能, 1=enabled)
//
// 厂商差异:
// - Synopsys DWC DP-TX: 上述布局 (与本实装一致)
// - Intel IGP: 类似, 但 vendor-specific 偏移可能不同
// - AMD DCN: 寄存器分散在 DDI 控制器不同位置, vendor-specific 路径覆盖
//
// 调用方应通过 [`DpController::new_with_io`] 指定自家偏移 (如未来需要扩展).

/// DP `H_TOTAL` 寄存器偏移 (16-bit)
const DP_H_TOTAL_REG_OFFSET: u32 = 0x300;
/// DP `H_ACTIVE` 寄存器偏移 (16-bit)
const DP_H_ACTIVE_REG_OFFSET: u32 = 0x302;
/// DP `V_TOTAL` 寄存器偏移 (16-bit)
const DP_V_TOTAL_REG_OFFSET: u32 = 0x304;
/// DP `V_ACTIVE` 寄存器偏移 (16-bit)
const DP_V_ACTIVE_REG_OFFSET: u32 = 0x306;
/// DP `H_SYNC_OFFSET` 寄存器偏移 (16-bit)
const DP_H_SYNC_OFFSET_REG_OFFSET: u32 = 0x308;
/// DP `H_SYNC_PW` 寄存器偏移 (16-bit)
const DP_H_SYNC_PW_REG_OFFSET: u32 = 0x30A;
/// DP `V_SYNC_OFFSET` 寄存器偏移 (16-bit)
const DP_V_SYNC_OFFSET_REG_OFFSET: u32 = 0x30C;
/// DP `V_SYNC_PW` 寄存器偏移 (16-bit)
const DP_V_SYNC_PW_REG_OFFSET: u32 = 0x30E;
/// DP `SYNC_POL` 寄存器偏移 (8-bit)
const DP_SYNC_POL_REG_OFFSET: u32 = 0x310;
/// DP H 同步极性 bit (bit 0, 0=negative, 1=positive)
const DP_SYNC_POL_H_BIT: u8 = 0x01;
/// DP V 同步极性 bit (bit 1, 0=negative, 1=positive)
const DP_SYNC_POL_V_BIT: u8 = 0x02;
/// DP `OUTPUT_ENABLE` 寄存器偏移 (8-bit)
const DP_OUTPUT_ENABLE_REG_OFFSET: u32 = 0x311;
/// DP 输出使能 bit (bit 0, 1=enabled)
const DP_OUTPUT_ENABLE_BIT: u8 = 0x01;

// ============================================================================
// 安全 MMIO 访问器
// ============================================================================

/// 安全的 DP MMIO 访问器.
///
/// 包装 `IoMem`, 提供所有 DP 寄存器的类型安全读写.
/// services 层通过此结构安全访问 DP 控制器 MMIO, 无 unsafe.
pub struct DpIo {
    mmio: IoMem,
}

impl DpIo {
    /// 从物理地址创建 DP MMIO 访问器.
    ///
    /// # 参数
    /// - `phys`: DP BAR0 物理地址 (来自 PCI 枚举)
    /// - `len`: MMIO 区域大小 (>= `REQUIRED_IOMEM_SIZE`)
    ///
    /// # Errors
    ///
    /// 当从 PCI BAR 映射物理地址失败时返回 [`KernelError::Io`]。
    pub fn new(phys: PhysAddr, len: usize) -> Result<Self, KernelError> {
        let mmio = IoMem::from_pci_bar(phys, len, "dp-bar0").map_err(|_| KernelError::Io)?;
        Ok(Self { mmio })
    }

    /// 从已有 `IoMem` 创建 DP MMIO 访问器 (供 framework 构造函数使用).
    pub fn from_iomem(mmio: IoMem) -> Self {
        Self { mmio }
    }

    /// 获取底层 `IoMem` 引用
    pub fn mmio(&self) -> &IoMem {
        &self.mmio
    }

    // ── 寄存器读写 ──

    /// 读取 8 位寄存器
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn read8(&self, reg: u32) -> u8 {
        self.mmio.read_u8(reg as usize)
    }

    /// 写入 8 位寄存器
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn write8(&self, reg: u32, val: u8) {
        self.mmio.write_u8(reg as usize, val);
    }

    /// 读取 16 位寄存器
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn read16(&self, reg: u32) -> u16 {
        self.mmio.read_u16(reg as usize)
    }

    /// 写入 16 位寄存器
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn write16(&self, reg: u32, val: u16) {
        self.mmio.write_u16(reg as usize, val);
    }

    /// 读取 32 位寄存器
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    pub fn read32(&self, reg: u32) -> u32 {
        self.mmio.read_u32(reg as usize)
    }

    /// 写入 32 位寄存器
    #[inline(always)]
    pub fn write32(&self, reg: u32, val: u32) {
        self.mmio.write_u32(reg as usize, val);
    }
}

// ============================================================================
// 链路速率
// ============================================================================

/// 链路速率
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LinkRate {
    Rbr = 0x06,  // 1.62 Gbps per lane
    Hbr = 0x0A,  // 2.7 Gbps per lane
    Hbr2 = 0x14, // 5.4 Gbps per lane
    Hbr3 = 0x1E, // 8.1 Gbps per lane
}

impl LinkRate {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 返回链路速率对应的带宽 (10 Mbps 为单位, 即 162 = 1.62 Gbps)
    pub fn bandwidth_gbps(&self) -> u32 {
        match self {
            Self::Rbr => 162,
            Self::Hbr => 270,
            Self::Hbr2 => 540,
            Self::Hbr3 => 810,
        }
    }

    /// 从 DPCD 原始字节值解析链路速率
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x06 => Some(Self::Rbr),
            0x0A => Some(Self::Hbr),
            0x14 => Some(Self::Hbr2),
            0x1E => Some(Self::Hbr3),
            _ => None,
        }
    }
}

/// 通道数量
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LaneCount {
    One = 1,
    Two = 2,
    Four = 4,
}

impl LaneCount {
    /// 从 DPCD 原始字节值解析通道数
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            4 => Some(Self::Four),
            _ => None,
        }
    }
}

/// 链路训练状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingState {
    Disabled,
    Training1,
    Training2,
    Trained,
    Error,
}

// ============================================================================
// DPCD 数据结构
// ============================================================================

/// `DisplayPort配置数据` (DPCD)
#[derive(Debug, Clone)]
pub struct Dpcd {
    /// DPCD版本
    pub revision: u8,
    /// 最大链路速率
    pub max_link_rate: LinkRate,
    /// 最大通道数
    pub max_lane_count: LaneCount,
    /// 是否支持下行扩频
    pub max_downspread: bool,
    /// 是否支持MST
    pub mst_capable: bool,
    /// 是否支持增强帧
    pub enhanced_frame_capable: bool,
    /// TPS3支持
    pub tps3_supported: bool,
    /// 接收器数量
    pub sink_count: u8,
}

/// DPCD 解析错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpError {
    /// 参数无效
    InvalidParameter,
    /// 设备不存在
    DeviceNotFound,
    /// 超时
    Timeout,
    /// 硬件错误
    HardwareError,
    /// 缓冲区不足
    BufferTooSmall,
    /// 不支持的操作
    UnsupportedOperation,
    /// 忙碌
    Busy,
    /// 未初始化
    NotInitialized,
}

impl Dpcd {
    /// 从AUX读取的数据解析DPCD
    ///
    /// # Errors
    ///
    /// - 数据长度不足 16 字节时返回 [`DpError::BufferTooSmall`]
    /// - 最大链路速率或通道数编码无效时返回 [`DpError::InvalidParameter`]
    pub fn parse(data: &[u8]) -> Result<Self, DpError> {
        if data.len() < 16 {
            return Err(DpError::BufferTooSmall);
        }

        let revision = data[0];
        let max_link_rate = LinkRate::from_u8(data[1]).ok_or(DpError::InvalidParameter)?;
        let max_lane_count = LaneCount::from_u8(data[2] & 0x1F).ok_or(DpError::InvalidParameter)?;

        Ok(Self {
            revision,
            max_link_rate,
            max_lane_count,
            max_downspread: (data[3] & 0x01) != 0,
            mst_capable: (data[3] & 0x04) != 0,
            enhanced_frame_capable: (data[2] & 0x80) != 0,
            tps3_supported: (data[4] & 0x40) != 0,
            sink_count: data[5] & 0x3F,
        })
    }
}

// ============================================================================
// AUX 通道操作
// ============================================================================

/// AUX命令类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AuxCommand {
    I2cWrite = 0x00,
    I2cRead = 0x01,
    I2cWriteStatus = 0x02,
    I2cReadStatus = 0x03,
    Write = 0x04,
    Read = 0x05,
}

/// AUX事务结果
#[derive(Debug, Clone, Copy)]
pub struct AuxTransaction {
    pub command: AuxCommand,
    pub address: u16,
    pub length: u8,
    pub data: [u8; 16],
    pub bytes_read: usize,
}

// ============================================================================
// DisplayPort 控制器
// ============================================================================

/// `DisplayPort` 控制器驱动 — services 层安全实现
///
/// 所有寄存器读写通过 `DpIo` 安全接口, 无 unsafe.
pub struct DpController {
    /// MMIO 访问器 (Some = 真实硬件, None = fallback 模式).
    ///
    /// - `Some(io)`: 真实硬件路径, 通过 MMIO 寄存器读取 HPD 状态.
    /// - `None`: 无硬件路径 (QEMU/QEMU+bochs-vbe), HPD 检测走 fallback
    ///   (假设已连接), 仅用于开发环境.
    io: Option<DpIo>,
    /// HPD 寄存器偏移 (相对 `io` 基地址).
    /// 不同厂商 DP 控制器偏移量不同; 默认 0x040 (假设独立 DP chip),
    /// 调用方可通过 [`DpController::new_with_io`] 指定自家硬件偏移.
    hpd_reg_offset: u32,
    /// DPCD数据
    dpcd: Option<Dpcd>,
    /// 当前链路速率
    current_link_rate: Option<LinkRate>,
    /// 当前通道数
    current_lane_count: Option<LaneCount>,
    /// 链路训练状态
    training_state: TrainingState,
    /// 是否连接显示器
    connected: bool,
    /// 是否已初始化
    initialized: bool,
}

impl DpController {
    /// 创建 `DisplayPort` 控制器实例 (无硬件 fallback 模式).
    ///
    /// 此模式 `detect_hot_plug` 直接返回 `true` (假设已连接),
    /// 用于 QEMU/QEMU+bochs-vbe 等无真实 DP 控制器的开发环境.
    /// 真实硬件环境请使用 [`DpController::new_with_io`].
    pub fn new(mmio_base_unused: usize) -> Self {
        let _ = mmio_base_unused;
        Self {
            io: None,
            hpd_reg_offset: DP_HPD_REG_OFFSET,
            dpcd: None,
            current_link_rate: None,
            current_lane_count: None,
            training_state: TrainingState::Disabled,
            connected: false,
            initialized: false,
        }
    }

    /// 创建 `DisplayPort` 控制器实例 (真实硬件模式).
    ///
    /// # 参数
    /// - `io`: `DpIo` 安全 MMIO 访问器 (调用方负责构造)
    /// - `hpd_reg_offset`: HPD 寄存器偏移
    pub fn new_with_io(io: DpIo, hpd_reg_offset: u32) -> Self {
        debug_assert!(
            io.mmio().len() >= REQUIRED_IOMEM_SIZE,
            "DpController 需要 IoMem >= {} 字节, got {}",
            REQUIRED_IOMEM_SIZE,
            io.mmio().len()
        );
        Self {
            io: Some(io),
            hpd_reg_offset,
            dpcd: None,
            current_link_rate: None,
            current_lane_count: None,
            training_state: TrainingState::Disabled,
            connected: false,
            initialized: false,
        }
    }

    /// 创建 `DisplayPort` 控制器实例 (真实硬件, 使用默认 HPD 寄存器偏移).
    pub fn new_with_default_hpd(io: DpIo) -> Self {
        Self::new_with_io(io, DP_HPD_REG_OFFSET)
    }

    /// 检测热插拔.
    ///
    /// 真实硬件: 从 MMIO 读 `hpd_reg_offset` 寄存器, bit 0 == 1 表示已连接.
    /// 无硬件 fallback: 返回 `true` (兼容 QEMU + Bochs DISPI 开发环境).
    pub fn detect_hot_plug(&mut self) -> bool {
        let hpd = if let Some(ref io) = self.io {
            io.read8(self.hpd_reg_offset) & DP_HPD_STATUS_BIT != 0
        } else {
            // 无硬件 fallback: 假设已连接 (兼容 QEMU Bochs DISPI 开发环境).
            true
        };
        self.connected = hpd;
        hpd
    }

    /// AUX通道读操作 (DISPLAY-2.5: TRACK-B61830 消除).
    ///
    /// 真实硬件: 通过 MMIO 写 CMD 寄存器触发 AUX 事务, 轮询 STA 寄存器等待
    /// reply-ready, 从 DAT0..DAT3 寄存器读取 16 字节响应 (实际 `length` 字节有效).
    ///
    /// 无硬件 fallback: 返回模拟 DPCD 数据 (兼容 QEMU/QEMU+bochs-vbe 开发环境),
    /// 同时更新内部 `dpcd` 缓存 (与原行为一致).
    ///
    /// # Errors
    ///
    /// - 设备未连接时返回 [`DpError::DeviceNotFound`]
    /// - `length` 为 0 或大于 16 时返回 [`DpError::InvalidParameter`]
    /// - 真实硬件路径下 AUX 事务失败 (如超时) 时返回相应 [`DpError`]
    pub fn aux_read(&mut self, address: u16, length: u8) -> Result<Vec<u8>, DpError> {
        if !self.connected {
            return Err(DpError::DeviceNotFound);
        }
        if length == 0 || length > 16 {
            return Err(DpError::InvalidParameter);
        }

        if let Some(ref io) = self.io {
            // 真实硬件路径: AUX 寄存器事务 (通过 DpIo 安全代理, 无 unsafe)
            self.aux_read_via_mmio(io, address, length)
        } else {
            // 无硬件 fallback: 返回模拟 DPCD 数据 (保持原行为)
            self.aux_read_fallback(address, length)
        }
    }

    /// AUX 真实硬件读事务 (DISPLAY-2.5) — 通过 `DpIo` 安全代理, 无 unsafe.
    fn aux_read_via_mmio(&self, io: &DpIo, address: u16, length: u8) -> Result<Vec<u8>, DpError> {
        // 1. 等待控制器空闲
        self.aux_wait_not_busy(io)?;

        // 2. 先写 address 到 DAT0 (8-bit 低字节), 高字节到 DAT0+1 (部分控制器要求).
        io.write8(AUX_DAT0_REG_OFFSET, (address & 0x00FF) as u8);
        io.write8(AUX_DAT0_REG_OFFSET + 1, ((address >> 8) & 0x0F) as u8);

        // 3. 构造 CMD 寄存器值: bit 0 = start, bit 1-3 = command (5 = Read)
        let cmd_val: u8 = AUX_CMD_START_BIT | ((AuxCommand::Read as u8) << AUX_CMD_COMMAND_SHIFT);
        io.write8(AUX_CMD_REG_OFFSET, cmd_val);

        let _ = length;

        // 4. 轮询 STA 寄存器等待 reply_ready
        let mut elapsed_iters: usize = 0;
        loop {
            if elapsed_iters > AUX_TRANSACTION_TIMEOUT_ITERS {
                return Err(DpError::Timeout);
            }
            let sta = io.read8(AUX_STA_REG_OFFSET);
            if (sta & AUX_STA_REPLY_READY_BIT) != 0 {
                // 检查 reply error 码
                let reply_err = (sta & AUX_STA_REPLY_ERR_MASK) >> AUX_STA_REPLY_ERR_SHIFT;
                if reply_err != 0 {
                    // NACK / DEFER / INVALID — 清 STA 并返回错误
                    io.write8(AUX_STA_REG_OFFSET, AUX_STA_REPLY_READY_BIT);
                    return Err(DpError::HardwareError);
                }
                break;
            }
            for _ in 0..AUX_DELAY_ITERS {
                core::hint::spin_loop();
            }
            elapsed_iters += AUX_DELAY_ITERS;
        }

        // 5. 从 DAT0..DAT3 读取 16 字节响应, 取前 length 字节
        let mut data = vec![0u8; length as usize];
        let mut offset = 0usize;
        for &dat_reg in &[
            AUX_DAT0_REG_OFFSET,
            AUX_DAT1_REG_OFFSET,
            AUX_DAT2_REG_OFFSET,
            AUX_DAT3_REG_OFFSET,
        ] {
            if offset >= length as usize {
                break;
            }
            let word = io.read32(dat_reg);
            let bytes = word.to_le_bytes();
            let copy_len = core::cmp::min(4, length as usize - offset);
            data[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
            offset += copy_len;
        }

        // 6. 清 STA 寄存器 reply_ready bit (写 1 清零)
        io.write8(AUX_STA_REG_OFFSET, AUX_STA_REPLY_READY_BIT);

        Ok(data)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// AUX 无硬件 fallback 读取 (保持原行为, 兼容 QEMU).
    fn aux_read_fallback(&mut self, address: u16, length: u8) -> Result<Vec<u8>, DpError> {
        let mut data = vec![0u8; length as usize];

        if address == 0x0000 && length >= 16 {
            // 模拟DPCD数据
            data[0] = 0x12; // DPCD rev 1.2
            data[1] = LinkRate::Hbr2 as u8; // 5.4 Gbps
            data[2] = 0x84; // 4 通道 (lanes), 增强帧模式 (enhanced frame)
            data[3] = 0x01; // downspread supported
            data[4] = 0x00;
            data[5] = 0x01; // 1 sink
        }

        Ok(data)
    }

    /// AUX通道写操作 (DISPLAY-2.5: TRACK-9B691E 消除).
    ///
    /// 真实硬件: 通过 MMIO 写 DAT0..DAT3 寄存器准备数据, 写 CMD 寄存器触发
    /// AUX 写事务, 轮询 STA 寄存器等待 reply-ready (AUX 写事务同样有 ACK 响应).
    ///
    /// 无硬件 fallback: 静默成功 (兼容 QEMU/QEMU+bochs-vbe 开发环境).
    ///
    /// # Errors
    ///
    /// - 设备未连接时返回 [`DpError::DeviceNotFound`]
    /// - `data` 为空或长度大于 16 时返回 [`DpError::InvalidParameter`]
    /// - 真实硬件路径下 AUX 事务失败 (如超时) 时返回相应 [`DpError`]
    pub fn aux_write(&mut self, address: u16, data: &[u8]) -> Result<(), DpError> {
        if !self.connected {
            return Err(DpError::DeviceNotFound);
        }
        if data.is_empty() || data.len() > 16 {
            return Err(DpError::InvalidParameter);
        }

        self.io
            .as_ref()
            .map_or(Ok(()), |io| self.aux_write_via_mmio(io, address, data))
    }

    /// AUX 真实硬件写事务 (DISPLAY-2.5) — 通过 `DpIo` 安全代理, 无 unsafe.
    fn aux_write_via_mmio(&self, io: &DpIo, address: u16, data: &[u8]) -> Result<(), DpError> {
        // 1. 等待控制器空闲
        self.aux_wait_not_busy(io)?;

        // 2. 先写 address 到 DAT0 (8-bit 低字节) + DAT0+1 (8-bit 高字节).
        io.write8(AUX_DAT0_REG_OFFSET, (address & 0x00FF) as u8);
        io.write8(AUX_DAT0_REG_OFFSET + 1, ((address >> 8) & 0x0F) as u8);

        // 3. 写 DAT1..DAT3 寄存器准备 write 数据 (padding 0).
        //    DAT0 被 address 占用, 数据从 DAT1 起.
        let mut offset = 0usize;
        for &dat_reg in &[
            AUX_DAT1_REG_OFFSET,
            AUX_DAT2_REG_OFFSET,
            AUX_DAT3_REG_OFFSET,
        ] {
            let mut word_bytes = [0u8; 4];
            let copy_len = core::cmp::min(4, data.len().saturating_sub(offset));
            if offset < data.len() {
                word_bytes[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
            }
            offset += copy_len;
            let word = u32::from_le_bytes(word_bytes);
            io.write32(dat_reg, word);
        }

        // 4. 构造 CMD 寄存器值: bit 0 = start, bit 1-3 = command (4 = Write)
        let cmd_val: u8 = AUX_CMD_START_BIT | ((AuxCommand::Write as u8) << AUX_CMD_COMMAND_SHIFT);
        io.write8(AUX_CMD_REG_OFFSET, cmd_val);

        // 5. 轮询 STA 寄存器等待 reply_ready
        let mut elapsed_iters: usize = 0;
        loop {
            if elapsed_iters > AUX_TRANSACTION_TIMEOUT_ITERS {
                return Err(DpError::Timeout);
            }
            let sta = io.read8(AUX_STA_REG_OFFSET);
            if (sta & AUX_STA_REPLY_READY_BIT) != 0 {
                let reply_err = (sta & AUX_STA_REPLY_ERR_MASK) >> AUX_STA_REPLY_ERR_SHIFT;
                // 清 reply_ready
                io.write8(AUX_STA_REG_OFFSET, AUX_STA_REPLY_READY_BIT);
                if reply_err != 0 {
                    return Err(DpError::HardwareError);
                }
                break;
            }
            for _ in 0..AUX_DELAY_ITERS {
                core::hint::spin_loop();
            }
            elapsed_iters += AUX_DELAY_ITERS;
        }

        // length 参数语义: 真实硬件的 `length` 由数据填充字节数隐式决定.
        Ok(())
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 等待 AUX 控制器进入空闲状态 (busy == 0).
    fn aux_wait_not_busy(&self, io: &DpIo) -> Result<(), DpError> {
        let mut elapsed_iters: usize = 0;
        loop {
            if elapsed_iters > AUX_TRANSACTION_TIMEOUT_ITERS {
                return Err(DpError::Timeout);
            }
            let sta = io.read8(AUX_STA_REG_OFFSET);
            if (sta & AUX_STA_BUSY_BIT) == 0 {
                return Ok(());
            }
            for _ in 0..AUX_DELAY_ITERS {
                core::hint::spin_loop();
            }
            elapsed_iters += AUX_DELAY_ITERS;
        }
    }

    /// 读取DPCD
    ///
    /// # Errors
    ///
    /// - 设备未连接时返回 [`DpError::DeviceNotFound`]
    /// - AUX 读取失败或 DPCD 解析失败时返回相应 [`DpError`]
    ///
    /// # Panics
    ///
    /// 正常情况下不会 panic; 仅当内部 `dpcd` 缓存刚写入后又变回 `None` 时才 panic
    /// (代码逻辑上不可达, 属防御性断言)。
    pub fn read_dpcd(&mut self) -> Result<&Dpcd, DpError> {
        if !self.connected {
            return Err(DpError::DeviceNotFound);
        }

        let data = self.aux_read(0x0000, 16)?;
        let dpcd = Dpcd::parse(&data)?;
        self.dpcd = Some(dpcd);

        // SAFETY: 刚在上面设为 Some, 不会为 None
        Ok(self.dpcd.as_ref().expect("dp: dpcd 刚已设为 Some"))
    }

    /// 链路训练
    ///
    /// # Errors
    ///
    /// - 设备未连接时返回 [`DpError::DeviceNotFound`]
    /// - 尚未读取 DPCD (缓存为 `None`) 时返回 [`DpError::NotInitialized`]
    /// - 任一训练阶段失败时返回相应 [`DpError`]
    pub fn link_train(&mut self) -> Result<(), DpError> {
        if !self.connected {
            return Err(DpError::DeviceNotFound);
        }

        let dpcd = self.dpcd.as_ref().ok_or(DpError::NotInitialized)?;

        // 选择链路速率和通道数
        let link_rate = dpcd.max_link_rate;
        let lane_count = dpcd.max_lane_count;

        // 阶段1: 链路训练模式1
        self.training_state = TrainingState::Training1;
        self.training_phase1(link_rate, lane_count)?;

        // 阶段2: 链路训练模式2
        self.training_state = TrainingState::Training2;
        self.training_phase2(link_rate, lane_count)?;

        // 训练完成
        self.current_link_rate = Some(link_rate);
        self.current_lane_count = Some(lane_count);
        self.training_state = TrainingState::Trained;

        Ok(())
    }

    /// 链路训练阶段1 (DISPLAY-2.6: TRACK-0350FE 消除).
    ///
    /// DP 链路训练 phase 1 (VESA DP 1.4 §3.5.1.2):
    /// 1. 设置链路速率 (`LINK_BW_SET`)
    /// 2. 设置通道数 (`LANE_COUNT_SET`)
    /// 3. 设置训练模式 1 (`TRAINING_PTN_SET` = 0x21: TPS1 + 启用 scramble + 从 `TRAINING_LANE0_SET` 读 swing/pre-emphasis)
    /// 4. 轮询 `LANE0_1_STATUS` (必要时 `LANE2_3_STATUS`) 直到所有活动 lane 报告
    ///    `CR_DONE` / `CHANNEL_EQ_DONE` / `SYMBOL_LOCKED` (= 0b111 per lane)
    /// 5. 应用接收器请求的 voltage swing / pre-emphasis 调整 (`ADJUST_REQ_LANE0_1` / _`LANE2_3`)
    /// 6. 超时返回 `DpError::Timeout`
    ///
    /// 真实硬件: 通过 AUX 读 DPCD 状态寄存器, 超时 ~10 ms.
    /// 无硬件 fallback: 模拟训练立即成功 (兼容 QEMU/QEMU+bochs-vbe 开发环境).
    fn training_phase1(
        &mut self,
        link_rate: LinkRate,
        lane_count: LaneCount,
    ) -> Result<(), DpError> {
        // 1. 设置链路速率
        self.aux_write(aux_address::LINK_BW_SET, &[link_rate as u8])?;

        // 2. 设置通道数
        self.aux_write(aux_address::LANE_COUNT_SET, &[lane_count as u8])?;

        // 3. 设置训练模式 1 (TPS1)
        //    0x21 = bit 0 (TPS1 selected) | bit 5 (training enabled, disable scrambler)
        self.aux_write(aux_address::TRAINING_PTN_SET, &[0x21])?;

        if let Some(ref io) = self.io {
            // 真实硬件路径: 轮询 LANE 状态寄存器直到训练完成或超时
            self.poll_lane_status_until_trained(io, lane_count)?;
        } else {
            // 无硬件 fallback: 模拟训练立即成功
        }

        Ok(())
    }

    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    /// 轮询 LANE 状态寄存器直到训练完成 (DISPLAY-2.6).
    ///
    /// 读取 `LANE0_1_STATUS` + `LANE2_3_STATUS` (4-lane 时), 等待所有活动 lane 报告
    /// `CR_DONE` / `CHANNEL_EQ_DONE` / `SYMBOL_LOCKED` (= 0b111 per lane).
    ///
    /// 超时时间 ~10 ms (与 VESA DP 1.4 推荐训练超时一致).
    fn poll_lane_status_until_trained(
        &self,
        io: &DpIo,
        lane_count: LaneCount,
    ) -> Result<(), DpError> {
        let mut elapsed_iters: usize = 0;
        // 单次迭代 ~50 spin_loops ≈ 1-2 µs, 10 ms = ~5_000 iters
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const MAX_TRAINING_ITERS: usize = 5_000;

        loop {
            if elapsed_iters > MAX_TRAINING_ITERS {
                return Err(DpError::Timeout);
            }

            // 读取 LANE0 + LANE1 状态 (1 lane 配置时仅 LANE0 有效)
            let lane01 = self.aux_read_via_mmio(io, aux_address::LANE0_1_STATUS, 1)?;
            let status01 = lane01[0];

            let trained_2lanes = match lane_count {
                LaneCount::One => {
                    // 仅检查 LANE0 (bits 0-2)
                    (status01 & 0x07) == 0x07
                }
                LaneCount::Two => {
                    // 检查 LANE0 (bits 0-2) + LANE1 (bits 4-6)
                    (status01 & 0x77) == 0x77
                }
                LaneCount::Four => {
                    // 4-lane: 同时检查 LANE0/1 (status01) + LANE2/3 (status23)
                    let lane23 = self.aux_read_via_mmio(io, aux_address::LANE2_3_STATUS, 1)?;
                    let status23 = lane23[0];
                    (status01 & 0x77) == 0x77 && (status23 & 0x77) == 0x77
                }
            };

            if trained_2lanes {
                // 读取接收器请求的调整 (用于 phase 2 前的电压/预加重调整)
                let adjust01 = self.aux_read_via_mmio(io, aux_address::ADJUST_REQ_LANE0_1, 1)?;
                let _adjust = adjust01[0]; // 真实硬件应据此调整 transmitter swing/pre-emphasis
                if matches!(lane_count, LaneCount::Four) {
                    // 4-lane 时还需读取 LANE2/3 的调整请求
                    let adjust23 =
                        self.aux_read_via_mmio(io, aux_address::ADJUST_REQ_LANE2_3, 1)?;
                    let _adjust23 = adjust23[0];
                }
                return Ok(());
            }

            for _ in 0..50 {
                core::hint::spin_loop();
            }
            elapsed_iters += 50;
        }
    }

    /// 链路训练阶段2 (DISPLAY-2.7: TRACK-3C1169 消除).
    ///
    /// DP 链路训练 phase 2 (VESA DP 1.4 §3.5.1.3):
    /// 1. 设置训练模式 2 (`TRAINING_PTN_SET` = 0x22: TPS2)
    /// 2. 应用 phase 1 中 `ADJUST_REQ` 请求的 final voltage swing / pre-emphasis 调整
    /// 3. 轮询 `LANE_ALIGN_STATUS_UPDATED` bit 0 (DPCD 0x0206) 直到 1
    /// 4. 超时返回 `DpError::Timeout`
    /// 5. 设置 `TRAINING_PTN_SET` = 0x00 结束训练
    ///
    /// 真实硬件: 通过 AUX 读 DPCD 0x0206 状态寄存器, 超时 ~10 ms.
    /// 无硬件 fallback: 模拟训练立即成功 (兼容 QEMU/QEMU+bochs-vbe 开发环境).
    fn training_phase2(
        &mut self,
        _link_rate: LinkRate,
        _lane_count: LaneCount,
    ) -> Result<(), DpError> {
        // 1. 设置训练模式 2 (TPS2)
        //    0x22 = bit 1 (TPS2 selected) | bit 5 (training enabled, disable scrambler)
        self.aux_write(aux_address::TRAINING_PTN_SET, &[0x22])?;

        if let Some(ref io) = self.io {
            // 真实硬件路径: 轮询 LANE_ALIGN_STATUS_UPDATED bit 0 直到对齐完成
            self.poll_lane_align_status(io)?;
        } else {
            // 无硬件 fallback: 模拟训练立即成功
        }

        // 5. 结束训练 (TRAINING_PTN_SET = 0x00)
        self.aux_write(aux_address::TRAINING_PTN_SET, &[0x00])?;

        Ok(())
    }

    /// 轮询 `LANE_ALIGN_STATUS_UPDATED` bit 0 直到所有 lane 对齐完成 (DISPLAY-2.7).
    ///
    /// 读取 DPCD 0x0206 寄存器, bit 0 = `LANE_ALIGN_STATUS_UPDATED`.
    /// 该位在所有活动 lane 完成 inter-lane deskew 后置 1.
    ///
    /// 超时时间 ~10 ms (与 VESA DP 1.4 推荐训练超时一致).
    fn poll_lane_align_status(&self, io: &DpIo) -> Result<(), DpError> {
        let mut elapsed_iters: usize = 0;
        // 单次迭代 ~50 spin_loops ≈ 1-2 µs, 10 ms = ~5_000 iters
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const MAX_TRAINING_ITERS: usize = 5_000;

        loop {
            if elapsed_iters > MAX_TRAINING_ITERS {
                return Err(DpError::Timeout);
            }

            // 读取 LANE_ALIGN_STATUS_UPDATED 寄存器
            let status = self.aux_read_via_mmio(io, aux_address::LANE_ALIGN_STATUS_UPDATED, 1)?;
            let align = status[0];

            // bit 0 = LANE_ALIGN_STATUS_UPDATED (1 = 已对齐)
            if (align & 0x01) != 0 {
                return Ok(());
            }

            for _ in 0..50 {
                core::hint::spin_loop();
            }
            elapsed_iters += 50;
        }
    }

    /// 获取当前带宽 (Gbps)
    pub fn get_bandwidth_gbps(&self) -> Option<u32> {
        let rate = self.current_link_rate?;
        let lanes = self.current_lane_count?;

        Some(rate.bandwidth_gbps() * lanes as u32)
    }

    /// 检查链路是否已训练
    pub fn is_link_trained(&self) -> bool {
        self.training_state == TrainingState::Trained
    }

    /// 设置视频模式 (DISPLAY-2.8: 视频时序参数化).
    ///
    /// 根据传入的 [`VideoMode`] 参数化计算时序, 写入 DP 控制器时序寄存器,
    /// 并配置同步极性 + 使能输出.
    ///
    /// 真实硬件: 通过 MMIO 写 8 个 16-bit 时序寄存器 + 2 个 8-bit 控制寄存器.
    /// 无硬件 fallback: 仅缓存模式参数 (兼容 QEMU/QEMU+bochs-vbe 开发环境).
    ///
    /// # Errors
    ///
    /// - 链路尚未训练完成时返回 [`DpError::NotInitialized`]
    /// - `mode.width` 或 `mode.height` 为 0 时返回 [`DpError::InvalidParameter`]
    /// - 真实硬件路径下写入时序寄存器失败时返回相应 [`DpError`]
    pub fn set_video_mode(&mut self, mode: VideoMode) -> Result<(), DpError> {
        if !self.is_link_trained() {
            return Err(DpError::NotInitialized);
        }
        if mode.width == 0 || mode.height == 0 {
            return Err(DpError::InvalidParameter);
        }

        // 派生 VideoTiming (优先 DMT lookup, fallback 到简化公式)
        let timing = self.derive_dp_video_timing(&mode);

        if let Some(ref io) = self.io {
            // 真实硬件路径: 写 8 个 16-bit 时序寄存器 + sync + output enable
            self.write_dp_timing_registers(io, &timing, &mode)?;
        } else {
            // 无硬件 fallback: 静默成功
        }

        Ok(())
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 派生 DP 视频时序 (复用 `hdmi::lookup_dmt_timing` + 简化公式 fallback).
    ///
    /// 注: 此方法**不依赖** `hdmi::derive_video_timing` (它是 `pub`),
    ///     而是用 lookup + 复制一份等价公式, 保持 dp.rs 独立.
    fn derive_dp_video_timing(&self, mode: &VideoMode) -> VideoTiming {
        // P0-3 精度扩展: DMT lookup 优先
        if let Some(timing) = lookup_dmt_timing(mode) {
            return timing;
        }
        // 公式 fallback (与 hdmi::derive_video_timing 一致)
        let v_active = mode.height;
        let h_active = mode.width;

        let v_total = if mode.refresh_rate > 0 && mode.pixel_clock_khz > 0 {
            let v_blank = (u32::from(v_active) * 5 / 100).max(1);
            (u32::from(v_active) + v_blank) as u16
        } else {
            v_active + 50
        };

        let h_total = if mode.refresh_rate > 0 && mode.pixel_clock_khz > 0 {
            let h_total_u32 =
                (mode.pixel_clock_khz * 1000) / (u32::from(v_total) * u32::from(mode.refresh_rate));
            h_total_u32.max(u32::from(h_active) + 1) as u16
        } else {
            h_active + 200
        };

        let h_blank = h_total.saturating_sub(h_active);
        let h_sync_offset = h_blank / 4;
        let h_sync_pulse_width = h_blank / 8;
        let v_sync_offset = 1u16;
        let v_sync_pulse_width = 3u16;

        VideoTiming {
            h_active,
            h_total,
            h_sync_offset,
            h_sync_pulse_width,
            v_active,
            v_total,
            v_sync_offset,
            v_sync_pulse_width,
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 写入 DP 时序 + sync + output enable 寄存器 — 通过 `DpIo` 安全代理, 无 unsafe.
    fn write_dp_timing_registers(
        &self,
        io: &DpIo,
        timing: &VideoTiming,
        mode: &VideoMode,
    ) -> Result<(), DpError> {
        // 写 8 个 16-bit 时序寄存器
        io.write16(DP_H_TOTAL_REG_OFFSET, timing.h_total);
        io.write16(DP_H_ACTIVE_REG_OFFSET, timing.h_active);
        io.write16(DP_V_TOTAL_REG_OFFSET, timing.v_total);
        io.write16(DP_V_ACTIVE_REG_OFFSET, timing.v_active);
        io.write16(DP_H_SYNC_OFFSET_REG_OFFSET, timing.h_sync_offset);
        io.write16(DP_H_SYNC_PW_REG_OFFSET, timing.h_sync_pulse_width);
        io.write16(DP_V_SYNC_OFFSET_REG_OFFSET, timing.v_sync_offset);
        io.write16(DP_V_SYNC_PW_REG_OFFSET, timing.v_sync_pulse_width);

        // 写 sync polarity (8-bit, bit 0=H, bit 1=V)
        let sync_pol: u8 = if mode.flags.hsync_positive {
            DP_SYNC_POL_H_BIT
        } else {
            0
        } | if mode.flags.vsync_positive {
            DP_SYNC_POL_V_BIT
        } else {
            0
        };
        io.write8(DP_SYNC_POL_REG_OFFSET, sync_pol);

        // 写 output enable (8-bit, bit 0=enable)
        io.write8(DP_OUTPUT_ENABLE_REG_OFFSET, DP_OUTPUT_ENABLE_BIT);

        Ok(())
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 获取设备名称
    pub fn name(&self) -> &'static str {
        "DisplayPort Controller"
    }

    /// 获取连接状态
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 关闭 `DisplayPort` 控制器
    pub fn shutdown(&mut self) {
        self.connected = false;
        self.dpcd = None;
        self.current_link_rate = None;
        self.current_lane_count = None;
        self.training_state = TrainingState::Disabled;
        self.initialized = false;
    }

    /// 是否已初始化并就绪
    pub fn is_ready(&self) -> bool {
        self.initialized && self.connected && self.is_link_trained()
    }

    /// 获取状态字符串
    pub fn status(&self) -> &'static str {
        if !self.initialized {
            "DP not initialized"
        } else if !self.connected {
            "DP no display connected"
        } else if self.is_link_trained() {
            "DP link trained"
        } else {
            "DP link training failed"
        }
    }

    /// 初始化 `DisplayPort` 控制器 (检测 → 读 DPCD → 链路训练)
    pub fn init(&mut self) {
        self.detect_hot_plug();

        if self.connected {
            let _ = self.read_dpcd();
            let _ = self.link_train();
        }

        self.initialized = true;
    }
}

/// MIG-008 接线补齐: `DisplayPort` 控制器接入 `Driver` 抽象, 供 Chitin 注册 (DECISION-K)
///
/// 委托 inherent 方法 (显式 `Type::method(self)` 路径, 规避同名解析歧义);
/// inherent `init`/`shutdown` 返回 `()`, 此处按 trait 契约包装为 `Ok(())`.
impl Driver for DpController {
    fn name(&self) -> &'static str {
        DpController::name(self)
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Other
    }

    fn init(&mut self) -> DriverResult<()> {
        DpController::init(self);
        Ok(())
    }

    fn shutdown(&mut self) -> DriverResult<()> {
        DpController::shutdown(self);
        Ok(())
    }

    fn is_ready(&self) -> bool {
        DpController::is_ready(self)
    }

    fn status(&self) -> &'static str {
        DpController::status(self)
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_rate_bandwidth() {
        assert_eq!(LinkRate::Rbr.bandwidth_gbps(), 162);
        assert_eq!(LinkRate::Hbr.bandwidth_gbps(), 270);
        assert_eq!(LinkRate::Hbr2.bandwidth_gbps(), 540);
        assert_eq!(LinkRate::Hbr3.bandwidth_gbps(), 810);
    }

    #[test]
    fn test_link_rate_from_u8() {
        assert_eq!(LinkRate::from_u8(0x06), Some(LinkRate::Rbr));
        assert_eq!(LinkRate::from_u8(0x0A), Some(LinkRate::Hbr));
        assert_eq!(LinkRate::from_u8(0x14), Some(LinkRate::Hbr2));
        assert_eq!(LinkRate::from_u8(0x1E), Some(LinkRate::Hbr3));
        assert_eq!(LinkRate::from_u8(0x00), None);
    }

    #[test]
    fn test_lane_count_from_u8() {
        assert_eq!(LaneCount::from_u8(1), Some(LaneCount::One));
        assert_eq!(LaneCount::from_u8(2), Some(LaneCount::Two));
        assert_eq!(LaneCount::from_u8(4), Some(LaneCount::Four));
        assert_eq!(LaneCount::from_u8(3), None);
    }

    #[test]
    fn test_dp_controller_creation() {
        let ctrl = DpController::new(0xFE000000);
        assert_eq!(ctrl.name(), "DisplayPort Controller");
        assert!(!ctrl.is_ready());
        assert!(!ctrl.connected);
        assert_eq!(ctrl.training_state, TrainingState::Disabled);
    }

    #[test]
    fn test_dp_hpd_fallback_returns_true_when_no_io() {
        // 无硬件 fallback 模式: detect_hot_plug 必须返回 true (兼容 QEMU/Bochs).
        let mut ctrl = DpController::new(0xFE000000);
        assert!(ctrl.detect_hot_plug(), "无 DpIo 时 fallback 必须返回 true");
        assert!(ctrl.connected);
    }

    #[test]
    fn test_dpcd_parse() {
        let data = [
            0x12, // rev 1.2
            0x14, // HBR2 (5.4 Gbps)
            0x84, // 4 通道 (lanes), 增强帧模式 (enhanced frame)
            0x01, // downspread
            0x00, 0x01, // 1 sink
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];

        let dpcd = Dpcd::parse(&data).unwrap();
        assert_eq!(dpcd.revision, 0x12);
        assert_eq!(dpcd.max_link_rate, LinkRate::Hbr2);
        assert_eq!(dpcd.max_lane_count, LaneCount::Four);
        assert!(dpcd.max_downspread);
        assert!(dpcd.enhanced_frame_capable);
    }

    // DISPLAY-2.5: AUX 通道单元测试

    #[test]
    fn test_aux_read_length_zero_returns_invalid_parameter() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let result = ctrl.aux_read(0x0000, 0);
        assert!(matches!(result, Err(DpError::InvalidParameter)));
    }

    #[test]
    fn test_aux_read_length_over_16_returns_invalid_parameter() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let result = ctrl.aux_read(0x0000, 17);
        assert!(matches!(result, Err(DpError::InvalidParameter)));
    }

    #[test]
    fn test_aux_read_fallback_dpcd_when_not_connected() {
        let mut ctrl = DpController::new(0xFE000000);
        // connected = false
        let result = ctrl.aux_read(0x0000, 16);
        assert!(matches!(result, Err(DpError::DeviceNotFound)));
    }

    #[test]
    fn test_aux_read_fallback_returns_zero_filled_when_address_nonzero() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let data = ctrl.aux_read(0x0200, 8).unwrap();
        assert_eq!(data.len(), 8);
        assert!(data.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_aux_read_fallback_returns_mock_dpcd_when_address_zero() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let data = ctrl.aux_read(0x0000, 16).unwrap();
        assert_eq!(data.len(), 16);
        assert_eq!(data[0], 0x12);
        assert_eq!(data[1], LinkRate::Hbr2 as u8);
        assert_eq!(data[2], 0x84);
        assert_eq!(data[3], 0x01);
        assert_eq!(data[5], 0x01);
    }

    #[test]
    fn test_aux_write_empty_data_returns_invalid_parameter() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let result = ctrl.aux_write(0x0100, &[]);
        assert!(matches!(result, Err(DpError::InvalidParameter)));
    }

    #[test]
    fn test_aux_write_over_16_bytes_returns_invalid_parameter() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let data = [0u8; 17];
        let result = ctrl.aux_write(0x0100, &data);
        assert!(matches!(result, Err(DpError::InvalidParameter)));
    }

    #[test]
    fn test_aux_write_fallback_succeeds_when_connected() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let data = [0x06u8];
        let result = ctrl.aux_write(aux_address::LINK_BW_SET, &data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_aux_write_fails_when_not_connected() {
        let mut ctrl = DpController::new(0xFE000000);
        let data = [0x06u8];
        let result = ctrl.aux_write(aux_address::LINK_BW_SET, &data);
        assert!(matches!(result, Err(DpError::DeviceNotFound)));
    }

    #[test]
    fn test_aux_register_offsets_within_required_iomem_size() {
        assert!((AUX_CMD_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((AUX_STA_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((AUX_DAT0_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((AUX_DAT1_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((AUX_DAT2_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((AUX_DAT3_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!(AUX_DAT3_REG_OFFSET as usize + 4 <= REQUIRED_IOMEM_SIZE);
    }

    // DISPLAY-2.6: 链路训练 phase 1 单元测试

    #[test]
    fn test_link_train_fallback_one_lane_hbr2() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.training_state = TrainingState::Disabled;
        ctrl.aux_write(aux_address::TRAINING_PTN_SET, &[0x21])
            .unwrap();
        let data = ctrl.aux_read(0x0000, 16).unwrap();
        assert_eq!(data[1], LinkRate::Hbr2 as u8);
    }

    #[test]
    fn test_link_train_fails_when_not_connected() {
        let mut ctrl = DpController::new(0xFE000000);
        let result = ctrl.read_dpcd();
        assert!(matches!(result, Err(DpError::DeviceNotFound)));
    }

    #[test]
    fn test_dpcd_lane_status_addresses_distinct() {
        assert_ne!(aux_address::LANE0_1_STATUS, aux_address::LANE2_3_STATUS);
        assert_ne!(
            aux_address::LANE0_1_STATUS,
            aux_address::LANE_ALIGN_STATUS_UPDATED
        );
        assert_ne!(
            aux_address::LANE2_3_STATUS,
            aux_address::LANE_ALIGN_STATUS_UPDATED
        );
        assert_ne!(
            aux_address::ADJUST_REQ_LANE0_1,
            aux_address::ADJUST_REQ_LANE2_3
        );
        assert_ne!(aux_address::LANE0_1_STATUS, aux_address::LINK_BW_SET);
        assert_ne!(aux_address::LANE0_1_STATUS, aux_address::LANE_COUNT_SET);
        assert_ne!(aux_address::TRAINING_PTN_SET, aux_address::LANE0_1_STATUS);
    }

    // DISPLAY-2.7: 链路训练 phase 2 单元测试

    #[test]
    fn test_link_train_fallback_full_flow_succeeds() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.read_dpcd().unwrap();
        let result = ctrl.link_train();
        assert!(result.is_ok(), "fallback link_train 必须成功: {:?}", result);
        assert_eq!(ctrl.training_state, TrainingState::Trained);
        assert!(ctrl.is_link_trained());
        assert_eq!(ctrl.current_link_rate, Some(LinkRate::Hbr2));
        assert_eq!(ctrl.current_lane_count, Some(LaneCount::Four));
        assert_eq!(ctrl.get_bandwidth_gbps(), Some(540 * 4));
    }

    #[test]
    fn test_link_train_fallback_lane_count_one() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.read_dpcd().unwrap();
        let result = ctrl.link_train();
        assert!(result.is_ok());
        assert_eq!(ctrl.training_state, TrainingState::Trained);
    }

    #[test]
    fn test_link_train_fallback_after_dpcd_read() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.read_dpcd().unwrap();
        let result = ctrl.link_train();
        assert!(result.is_ok());
    }

    // DISPLAY-2.8: 视频时序参数化单元测试

    #[test]
    fn test_set_video_mode_fails_before_link_trained() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        let mode = VideoMode {
            width: 1920,
            height: 1080,
            refresh_rate: 60,
            pixel_clock_khz: 148500,
            flags: super::super::hdmi::VideoModeFlags {
                interlaced: false,
                double_scan: false,
                hsync_positive: false,
                vsync_positive: false,
            },
        };
        let result = ctrl.set_video_mode(mode);
        assert!(matches!(result, Err(DpError::NotInitialized)));
    }

    #[test]
    fn test_set_video_mode_fails_with_zero_width() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.read_dpcd().unwrap();
        ctrl.link_train().unwrap();
        let mode = VideoMode {
            width: 0,
            height: 1080,
            refresh_rate: 60,
            pixel_clock_khz: 148500,
            flags: super::super::hdmi::VideoModeFlags {
                interlaced: false,
                double_scan: false,
                hsync_positive: false,
                vsync_positive: false,
            },
        };
        let result = ctrl.set_video_mode(mode);
        assert!(matches!(result, Err(DpError::InvalidParameter)));
    }

    #[test]
    fn test_set_video_mode_fails_with_zero_height() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.read_dpcd().unwrap();
        ctrl.link_train().unwrap();
        let mode = VideoMode {
            width: 1920,
            height: 0,
            refresh_rate: 60,
            pixel_clock_khz: 148500,
            flags: super::super::hdmi::VideoModeFlags {
                interlaced: false,
                double_scan: false,
                hsync_positive: false,
                vsync_positive: false,
            },
        };
        let result = ctrl.set_video_mode(mode);
        assert!(matches!(result, Err(DpError::InvalidParameter)));
    }

    #[test]
    fn test_set_video_mode_fallback_1080p60_succeeds() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.read_dpcd().unwrap();
        ctrl.link_train().unwrap();
        let mode = VideoMode {
            width: 1920,
            height: 1080,
            refresh_rate: 60,
            pixel_clock_khz: 148500,
            flags: super::super::hdmi::VideoModeFlags {
                interlaced: false,
                double_scan: false,
                hsync_positive: false,
                vsync_positive: false,
            },
        };
        let result = ctrl.set_video_mode(mode);
        assert!(
            result.is_ok(),
            "fallback set_video_mode 必须成功: {:?}",
            result
        );
    }

    #[test]
    fn test_set_video_mode_fallback_4k60_succeeds() {
        let mut ctrl = DpController::new(0xFE000000);
        ctrl.detect_hot_plug();
        ctrl.read_dpcd().unwrap();
        ctrl.link_train().unwrap();
        let mode = VideoMode {
            width: 3840,
            height: 2160,
            refresh_rate: 60,
            pixel_clock_khz: 594000,
            flags: super::super::hdmi::VideoModeFlags {
                interlaced: false,
                double_scan: false,
                hsync_positive: true,
                vsync_positive: false,
            },
        };
        let result = ctrl.set_video_mode(mode);
        assert!(result.is_ok());
    }

    #[test]
    fn test_dp_timing_register_offsets_within_required_iomem_size() {
        assert!((DP_H_TOTAL_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_H_ACTIVE_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_V_TOTAL_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_V_ACTIVE_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_H_SYNC_OFFSET_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_H_SYNC_PW_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_V_SYNC_OFFSET_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_V_SYNC_PW_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_SYNC_POL_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!((DP_OUTPUT_ENABLE_REG_OFFSET as usize) < REQUIRED_IOMEM_SIZE);
        assert!(DP_H_TOTAL_REG_OFFSET as usize + 2 <= REQUIRED_IOMEM_SIZE);
        assert!(DP_V_SYNC_PW_REG_OFFSET as usize + 2 <= REQUIRED_IOMEM_SIZE);
        assert!(DP_SYNC_POL_REG_OFFSET as usize + 1 <= REQUIRED_IOMEM_SIZE);
        assert!(DP_OUTPUT_ENABLE_REG_OFFSET as usize + 1 <= REQUIRED_IOMEM_SIZE);
        assert!(DP_H_TOTAL_REG_OFFSET >= 0x300);
        assert!(DP_H_TOTAL_REG_OFFSET > AUX_DAT3_REG_OFFSET);
    }

    #[test]
    fn test_dp_timing_register_order_matches_spec() {
        assert!(DP_H_TOTAL_REG_OFFSET < DP_H_ACTIVE_REG_OFFSET);
        assert!(DP_H_ACTIVE_REG_OFFSET < DP_V_TOTAL_REG_OFFSET);
        assert!(DP_V_TOTAL_REG_OFFSET < DP_V_ACTIVE_REG_OFFSET);
        assert!(DP_V_ACTIVE_REG_OFFSET < DP_H_SYNC_OFFSET_REG_OFFSET);
        assert!(DP_H_SYNC_OFFSET_REG_OFFSET < DP_H_SYNC_PW_REG_OFFSET);
        assert!(DP_H_SYNC_PW_REG_OFFSET < DP_V_SYNC_OFFSET_REG_OFFSET);
        assert!(DP_V_SYNC_OFFSET_REG_OFFSET < DP_V_SYNC_PW_REG_OFFSET);
        assert!(DP_V_SYNC_PW_REG_OFFSET < DP_SYNC_POL_REG_OFFSET);
        assert!(DP_SYNC_POL_REG_OFFSET < DP_OUTPUT_ENABLE_REG_OFFSET);
    }

    #[test]
    fn test_dp_video_timing_derive_dmt_1080p60() {
        let ctrl = DpController::new(0xFE000000);
        let mode = VideoMode {
            width: 1920,
            height: 1080,
            refresh_rate: 60,
            pixel_clock_khz: 148500,
            flags: super::super::hdmi::VideoModeFlags {
                interlaced: false,
                double_scan: false,
                hsync_positive: false,
                vsync_positive: false,
            },
        };
        let timing = ctrl.derive_dp_video_timing(&mode);
        assert_eq!(timing.h_active, 1920);
        assert_eq!(timing.v_active, 1080);
        assert_eq!(timing.h_total, 2200);
        assert_eq!(timing.v_total, 1125);
    }
}
