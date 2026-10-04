#![deny(unsafe_code)]
//! 设备固件加载 — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 委托 privileged/egdf/firmware 实际完成 firmware 附着/读取
//! - 提供 driver probe 使用的查询 API

use crate::privileged::egdf::NodeId;
use crate::privileged::egdf::firmware as fw;

/// 固件信息 (re-export)
pub use crate::privileged::egdf::FirmwareInfo;

/// 固件加载错误码
pub use crate::privileged::egdf::{FW_ERR_IO, FW_ERR_NOT_FOUND, FW_ERR_OOM, FW_ERR_TOO_LARGE};

/// 固件大小上限
pub use crate::privileged::egdf::MAX_FIRMWARE_SIZE;

/// 驱动 probe 时获取节点的固件
///
/// 返回的 `FirmwareBlob` 是克隆 (数据被克隆, 适合一次性消费);
/// 驱动应避免在 probe 路径上长期持有引用。
pub fn firmware_request(node_id: NodeId) -> Option<fw::FirmwareBlob> {
    fw::devtree_get_firmware(node_id)
}

/// 名称 hash 计算 (FNV-1a 32-bit)
pub fn firmware_name_hash(name: &str) -> u32 {
    fw::fnv1a_32(name)
}
