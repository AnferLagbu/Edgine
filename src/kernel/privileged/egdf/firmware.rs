//! 设备固件加载 (Firmware Loader)
//!
//! 将固件 blob 紧耦合到 EGDF 设备树节点, 驱动在 probe 时通过
//! `devtree_get_firmware(node_id)` 拿到; 用户态通过 `sys_fw_load` 写入,
//! `sys_fw_get_info` 读取元数据, `sys_fw_get` 拷贝到用户缓冲。
//!
//! ## 数据流
//!
//! ```text
//! sys_fw_load(node_id, path_ptr, path_len)
//!   ├── 从用户态路径读取文件 → heap Vec<u8>
//!   └── 写入 EGDFNode.firmware
//!
//! devtree_get_firmware(node_id) -> &FirmwareBlob   // 驱动 probe 时调用
//!
//! sys_fw_get_info(node_id, info_ptr)  -> size
//!   └── 拷贝 FirmwareInfo 到用户态
//!
//! sys_fw_get(node_id, buf_ptr, buf_len, offset) -> copied
//!   └── 从 blob 按 offset 拷贝到用户缓冲
//! ```
//!
//! ## 安全
//! - 内部数据由 `DEV_TREE` spinlock 保护
//! - 路径/缓冲在拷贝时按字节校验, 不假设用户态字符串

use super::devtree::{DEV_TREE, NodeId};
use alloc::vec::Vec;

/// 固件元数据 + 内容的最大尺寸上限 (16 MiB)
///
/// 超过此尺寸的固件应使用流式或块加载接口, 本实现不提供。
pub const MAX_FIRMWARE_SIZE: usize = 16 * 1024 * 1024;

/// 固件信息头 (用户态可见, 跨 ABI 稳定)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FirmwareInfo {
    /// blob 字节数
    pub size: u32,
    /// 用户提供的 name hash (注册时计算)
    pub name_hash: u32,
    /// 版本号 (0 表示未指定)
    pub version: u32,
    /// 保留字段
    pub _reserved: u32,
}

/// 固件 blob (附着在 `EGDFNode` 上)
#[derive(Debug, Clone)]
pub struct FirmwareBlob {
    /// 原始字节内容
    pub data: Vec<u8>,
    /// 名称 hash (FNV-1a 32-bit)
    pub name_hash: u32,
    /// 版本号 (0 = 未指定)
    pub version: u32,
}

impl FirmwareBlob {
    pub fn new(data: Vec<u8>, name_hash: u32, version: u32) -> Self {
        Self {
            data,
            name_hash,
            version,
        }
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// FNV-1a 32-bit hash
///
/// 用于固件名快速比对; 用户态与内核态均使用同一算法。
pub fn fnv1a_32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x01000193);
    }
    h
}

// ── devtree 固件绑定 ──

/// 将固件 blob 附着到指定设备节点
///
/// 替换已有固件 (新固件覆盖). 若 `data.len() > MAX_FIRMWARE_SIZE` 则返回 false.
pub fn devtree_attach_firmware(
    node_id: NodeId,
    data: Vec<u8>,
    name_hash: u32,
    version: u32,
) -> bool {
    if data.len() > MAX_FIRMWARE_SIZE {
        return false;
    }
    let mut tree = DEV_TREE.lock();
    tree.nodes
        .iter_mut()
        .find(|n| n.id == node_id)
        .map_or(false, |node| {
            node.firmware = Some(FirmwareBlob::new(data, name_hash, version));
            true
        })
}

/// 读取节点上的固件 (驱动 probe 用, 返回不可变引用)
///
/// 调用方需在临界区内使用返回的引用, 或拷贝数据后立即退出。
pub fn devtree_get_firmware(node_id: NodeId) -> Option<FirmwareBlob> {
    let tree = DEV_TREE.lock();
    tree.nodes
        .iter()
        .find(|n| n.id == node_id)
        .and_then(|n| n.firmware.clone())
}

/// 移除节点上的固件
pub fn devtree_detach_firmware(node_id: NodeId) -> bool {
    let mut tree = DEV_TREE.lock();
    tree.nodes
        .iter_mut()
        .find(|n| n.id == node_id)
        .map_or(false, |node| node.firmware.take().is_some())
}

// ── 错误码 (与 syscall 共享) ──

pub const FW_OK: i32 = 0;
pub const FW_ERR_NOT_FOUND: i32 = -1;
pub const FW_ERR_TOO_LARGE: i32 = -2;
pub const FW_ERR_OOM: i32 = -3;
pub const FW_ERR_INVALID: i32 = -4;
pub const FW_ERR_IO: i32 = -5;

// ── 锁静态检查 (在 no_std 下由审计工具覆盖, 此处仅文档化) ──
// DEV_TREE (irq_spinlock) → firmware 注册表访问
// 无跨锁依赖; 不存在死锁
