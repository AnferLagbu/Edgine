#![deny(unsafe_code)]
//! 显示子系统 — services 层安全实现
//!
//! 提供 HDMI/DisplayPort 驱动的 safe 业务逻辑:
//! - DDC I2C bitbang 协议 (通过 IoMem 安全代理)
//! - EDID 解析 (纯数据)
//! - 视频模式管理与时序参数派生
//! - 像素时钟 PLL 配置
//!
//! 所有硬件寄存器访问通过 framework 的 IoMem 安全接口, 0 unsafe.

/// DDC (Display Data Channel) I2C bitbang 协议
pub mod ddc;

/// HDMI 控制器驱动
pub mod hdmi;

/// DisplayPort 控制器驱动 (从 framework 迁移, 0 unsafe)
pub mod dp;

/// 显示控制器管理策略 (从 framework 迁移, 0 unsafe)
pub mod controller;

// 重新导出 DisplayPort 公共类型
pub use dp::{
    AuxCommand, AuxTransaction, DpController, DpError, DpIo, Dpcd, LaneCount, LinkRate,
    REQUIRED_IOMEM_SIZE, TrainingState, assert_iomem_size_at_least,
};
