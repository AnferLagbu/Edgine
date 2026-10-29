#![deny(unsafe_code)]
#![deny(missing_docs)]
//! `RamFS` 节点与目录项数据结构 — functions 层安全实现
//!
//! 定义 `RamFS` 的 inode 节点 (`RamFsNode`)、目录项 (`RamFsDirEntry`) 与
//! 访问控制项 (`RamFsACE`) 的内存布局, 全部为 `Copy` 结构体, 0 unsafe.

use crate::functions::fs::VFS_MAX_NAME;

pub(crate) const DIRECT_BLOCKS: usize = 12;

/// `RamFS` 节点 (inode) — 文件/目录的元数据与块索引
#[derive(Debug, Clone, Copy)]
pub struct RamFsNode {
    /// 节点编号 (在 `RamFS` 节点表中的索引)
    pub node_id: u32,
    /// 文件类型编码 (对应 `VfsFileType`)
    pub file_type: u8,
    /// 敏感级别 (0 = 公开)
    pub sensitivity: u8,
    /// 属主 PWM 标识
    pub owner_pwm: u64,
    /// 属组 PWM 标识
    pub group_pwm: u64,
    /// 权限位
    pub perm: u16,
    /// 文件字节数
    pub size: u32,
    /// 访问时间 (秒)
    pub atime: u64,
    /// 修改时间 (秒)
    pub mtime: u64,
    /// 状态变更时间 (秒)
    pub ctime: u64,
    /// 直接块号数组 (最多 `DIRECT_BLOCKS` 个)
    pub direct_blocks: [u32; DIRECT_BLOCKS],
    /// 一级间接块号 (0 表示未分配)
    pub indirect_block: u32,
    /// 二级间接块号 (0 表示未分配)
    pub double_indirect_block: u32,
    /// 硬链接计数
    pub link_count: u32,
    /// 节点是否已分配使用
    pub used: bool,
}

impl Default for RamFsNode {
    /// 使用默认构造 (等价于 `Self::new`)。
    fn default() -> Self {
        Self::new()
    }
}

impl RamFsNode {
    /// 构造全零的未使用节点
    #[must_use]
    pub const fn new() -> Self {
        Self {
            node_id: 0,
            file_type: 0,
            sensitivity: 0,
            owner_pwm: 0,
            group_pwm: 0,
            perm: 0,
            size: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            direct_blocks: [0; DIRECT_BLOCKS],
            indirect_block: 0,
            double_indirect_block: 0,
            link_count: 0,
            used: false,
        }
    }
}

/// `RamFS` 目录项 — `repr(C)` 序列化布局 (节点号 + 类型 + 名字)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct RamFsDirEntry {
    /// 目标节点编号
    pub node: u32,
    /// 目标文件类型编码
    pub file_type: u8,
    /// 目录项名字 (NUL 结尾, 定长 `VFS_MAX_NAME`)
    pub name: [u8; VFS_MAX_NAME],
}

impl Default for RamFsDirEntry {
    /// 使用默认构造 (等价于 `Self::new`)。
    fn default() -> Self {
        Self::new()
    }
}

impl RamFsDirEntry {
    /// 构造空目录项 (节点号/类型/名字全零)
    #[must_use]
    pub fn new() -> Self {
        Self {
            node: 0,
            file_type: 0,
            name: [0; VFS_MAX_NAME],
        }
    }

    /// 设置目录项名字 (超长截断至 `VFS_MAX_NAME - 1` 字节并补 NUL)
    pub fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(VFS_MAX_NAME - 1);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name[len] = 0;
    }

    /// 从字节切片读取目录项.
    ///
    /// # Panics
    /// 当 `offset..offset+4` 或 `offset+8..offset+8+VFS_MAX_NAME` 超出 `data` 长度时发生越界 panic (内部使用 `expect`).
    #[must_use]
    pub fn read_at(data: &[u8], offset: usize) -> Self {
        let mut entry = Self::new();
        let node_bytes: [u8; 4] = data[offset..offset + 4]
            .try_into()
            .expect("ramfs: read_at node slice OOB");
        entry.node = u32::from_le_bytes(node_bytes);
        entry.file_type = data[offset + 4];
        let name_end = offset + 8 + VFS_MAX_NAME;
        entry.name.copy_from_slice(&data[offset + 8..name_end]);
        entry
    }

    /// 将目录项写入字节切片 (与 [`Self::read_at`] 互逆)
    ///
    /// # Panics
    /// 当 `offset..offset+8+VFS_MAX_NAME` 超出 `data` 长度时发生越界 panic.
    pub fn write_at(&self, data: &mut [u8], offset: usize) {
        data[offset..offset + 4].copy_from_slice(&self.node.to_le_bytes());
        data[offset + 4] = self.file_type;
        data[offset + 5..offset + 8].fill(0);
        let name_end = offset + 8 + VFS_MAX_NAME;
        data[offset + 8..name_end].copy_from_slice(&self.name);
    }
}

/// `RamFS` 访问控制项 (ACE) — 针对单个节点的 PWM 授权记录
#[derive(Debug, Clone, Copy)]
pub struct RamFsACE {
    /// 作用的目标节点编号
    pub node_id: u32,
    /// 被授权的 PWM 标识
    pub pwm: u64,
    /// 允许操作掩码
    pub allow_mask: u64,
    /// 拒绝操作掩码
    pub deny_mask: u64,
    /// 条目是否已分配使用
    pub used: bool,
}

impl Default for RamFsACE {
    /// 使用默认构造 (等价于 `Self::new`)。
    fn default() -> Self {
        Self::new()
    }
}

impl RamFsACE {
    /// 构造全零的未使用访问控制项
    #[must_use]
    pub const fn new() -> Self {
        Self {
            node_id: 0,
            pwm: 0,
            allow_mask: 0,
            deny_mask: 0,
            used: false,
        }
    }
}
