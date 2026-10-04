#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 块组描述符数据结构

/// ext2 块组描述符 (32 字节)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ext2BlockGroupDescriptor {
    pub bg_block_bitmap: u32,
    pub bg_inode_bitmap: u32,
    pub bg_inode_table: u32,
    pub bg_free_blocks_count: u16,
    pub bg_free_inodes_count: u16,
    pub bg_used_dirs_count: u16,
    pub bg_pad: u16,
    pub bg_reserved: [u8; 12],
}

impl Ext2BlockGroupDescriptor {
    /// 从字节切片解析块组描述符
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 32 {
            return None;
        }

        let mut desc = Self {
            bg_block_bitmap: 0,
            bg_inode_bitmap: 0,
            bg_inode_table: 0,
            bg_free_blocks_count: 0,
            bg_free_inodes_count: 0,
            bg_used_dirs_count: 0,
            bg_pad: 0,
            bg_reserved: [0; 12],
        };

        desc.bg_block_bitmap = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        desc.bg_inode_bitmap = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        desc.bg_inode_table = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        desc.bg_free_blocks_count = u16::from_le_bytes([data[12], data[13]]);
        desc.bg_free_inodes_count = u16::from_le_bytes([data[14], data[15]]);
        desc.bg_used_dirs_count = u16::from_le_bytes([data[16], data[17]]);
        desc.bg_pad = u16::from_le_bytes([data[18], data[19]]);
        desc.bg_reserved.copy_from_slice(&data[20..32]);

        Some(desc)
    }

    /// 从块组描述符表解析多个描述符
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
