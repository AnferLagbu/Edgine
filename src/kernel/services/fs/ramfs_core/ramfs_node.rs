#![deny(unsafe_code)]

use crate::services::fs::VFS_MAX_NAME;

pub(crate) const DIRECT_BLOCKS: usize = 12;

#[derive(Debug, Clone, Copy)]
pub struct RamFsNode {
    pub node_id: u32,
    pub file_type: u8,
    pub sensitivity: u8,
    pub owner_pwm: u64,
    pub group_pwm: u64,
    pub perm: u16,
    pub size: u32,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub direct_blocks: [u32; DIRECT_BLOCKS],
    pub indirect_block: u32,
    pub double_indirect_block: u32,
    pub link_count: u32,
    pub used: bool,
}

impl RamFsNode {
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

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct RamFsDirEntry {
    pub node: u32,
    pub file_type: u8,
    pub name: [u8; VFS_MAX_NAME],
}

impl RamFsDirEntry {
    pub fn new() -> Self {
        Self {
            node: 0,
            file_type: 0,
            name: [0; VFS_MAX_NAME],
        }
    }

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

    pub fn write_at(&self, data: &mut [u8], offset: usize) {
        data[offset..offset + 4].copy_from_slice(&self.node.to_le_bytes());
        data[offset + 4] = self.file_type;
        data[offset + 5..offset + 8].fill(0);
        let name_end = offset + 8 + VFS_MAX_NAME;
        data[offset + 8..name_end].copy_from_slice(&self.name);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RamFsACE {
    pub node_id: u32,
    pub pwm: u64,
    pub allow_mask: u64,
    pub deny_mask: u64,
    pub used: bool,
}

impl RamFsACE {
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
