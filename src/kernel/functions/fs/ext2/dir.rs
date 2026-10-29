#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 目录项数据结构

/// ext2 目录项 (变长, 最小 8 字节)
#[derive(Debug, Clone)]
pub struct Ext2DirEntry {
    /// inode 号
    pub inode: u32,
    /// 目录项长度 (含填充)
    pub rec_len: u16,
    /// 名称长度 (不含终止符)
    pub name_len: u8,
    /// 文件类型 (ext2 dirent 类型码)
    pub file_type: u8,
    /// 文件名 (最多 255 字节, 未占满部分填 0)
    pub name: [u8; 255],
}

impl Ext2DirEntry {
    /// 从字节切片解析目录项
    #[must_use]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }

        let inode = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let rec_len = u16::from_le_bytes([data[4], data[5]]);
        let name_len = data[6];
        let file_type = data[7];

        if rec_len < 8 || (rec_len as usize) > data.len() {
            return None;
        }

        let mut name = [0u8; 255];
        let name_end = name_len as usize;
        if name_end + 8 <= data.len() {
            name[..name_end].copy_from_slice(&data[8..8 + name_end]);
        }

        Some(Self {
            inode,
            rec_len,
            name_len,
            file_type,
            name,
        })
    }

    /// 获取文件名
    #[must_use]
    pub fn get_name(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[..len]).unwrap_or("")
    }

    /// 目录项实际大小 (对齐到 4 字节)
    #[must_use]
    pub fn actual_size(&self) -> u16 {
        (u16::from(self.name_len) + 8 + 3) & !3
    }
}
