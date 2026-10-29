#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 块组描述符数据结构

/// ext2 块组描述符 (32 字节)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ext2BlockGroupDescriptor {
    /// 块位图所在块号
    pub block_bitmap: u32,
    /// inode 位图所在块号
    pub inode_bitmap: u32,
    /// inode 表起始块号
    pub inode_table: u32,
    /// 本组空闲块数
    pub free_blocks_count: u16,
    /// 本组空闲 inode 数
    pub free_inodes_count: u16,
    /// 本组已用目录数
    pub used_dirs_count: u16,
    /// 填充字段 (对齐用)
    pub pad: u16,
    /// 保留字段 (12 字节)
    pub reserved: [u8; 12],
}

impl Ext2BlockGroupDescriptor {
    /// 从字节切片解析块组描述符
    #[must_use]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 32 {
            return None;
        }

        let mut desc = Self {
            block_bitmap: 0,
            inode_bitmap: 0,
            inode_table: 0,
            free_blocks_count: 0,
            free_inodes_count: 0,
            used_dirs_count: 0,
            pad: 0,
            reserved: [0; 12],
        };

        desc.block_bitmap = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        desc.inode_bitmap = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        desc.inode_table = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        desc.free_blocks_count = u16::from_le_bytes([data[12], data[13]]);
        desc.free_inodes_count = u16::from_le_bytes([data[14], data[15]]);
        desc.used_dirs_count = u16::from_le_bytes([data[16], data[17]]);
        desc.pad = u16::from_le_bytes([data[18], data[19]]);
        desc.reserved.copy_from_slice(&data[20..32]);

        Some(desc)
    }

    /// 从块组描述符表解析多个描述符
    #[must_use]
    pub fn from_table(data: &[u8], count: usize) -> alloc::vec::Vec<Self> {
        let size = 32;
        let mut descs = alloc::vec::Vec::with_capacity(count);

        for i in 0..count {
            let offset = i * size;
            if offset + size > data.len() {
                break;
            }
            if let Some(desc) = Self::from_bytes(&data[offset..]) {
                descs.push(desc);
            }
        }

        descs
    }
}
