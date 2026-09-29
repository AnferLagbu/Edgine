#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! exFAT 目录项数据结构

use super::super_block::ExfatSuperBlock;
use crate::framework::driver::block::{read_sectors, with_device};
use crate::services::fs::KernelError;
use alloc::vec;
use alloc::vec::Vec;

/// exFAT 目录项类型
pub const DIR_ENTRY_END: u8 = 0x00; // 空目录项 (结束标记)
pub const DIR_ENTRY_DELETED: u8 = 0xE5; // 已删除
pub const DIR_ENTRY_FILE: u8 = 0x85; // 文件条目
pub const DIR_ENTRY_STREAM: u8 = 0xC0; // 流扩展 (文件名)
pub const DIR_ENTRY_VOLUME: u8 = 0x81; // 卷标
pub const DIR_ENTRY_ALLOC: u8 = 0xA0; // 分配位图

/// exFAT 目录项 (32 字节)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExfatDirEntry {
    pub entry_type: u8,
    pub data: [u8; 31],
}

impl ExfatDirEntry {
    /// 从字节切片解析目录项
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 32 {
            return None;
        }

        let mut entry = Self {
            entry_type: data[0],
            data: [0; 31],
        };
        entry.data.copy_from_slice(&data[1..32]);

        Some(entry)
    }

    /// 是否为文件条目
    pub fn is_file(&self) -> bool {
        self.entry_type == DIR_ENTRY_FILE
    }

    /// 是否为流扩展 (包含文件名)
    pub fn is_stream(&self) -> bool {
        self.entry_type == DIR_ENTRY_STREAM
    }

    /// 是否为空/结束标记
    pub fn is_end(&self) -> bool {
        self.entry_type == DIR_ENTRY_END
    }

    /// 获取文件条目的属性
    pub fn file_attributes(&self) -> u16 {
        if self.is_file() {
            u16::from_le_bytes([self.data[1], self.data[2]])
        } else {
            0
        }
    }

    /// 获取文件条目的起始簇号
    pub fn first_cluster(&self) -> u32 {
        if self.is_file() || self.is_stream() {
            u32::from_le_bytes([self.data[20], self.data[21], self.data[22], self.data[23]])
        } else {
            0
        }
    }

    /// 获取流扩展条目的文件大小
    pub fn stream_length(&self) -> u64 {
        if self.is_stream() {
            u64::from_le_bytes([
                self.data[4],
                self.data[5],
                self.data[6],
                self.data[7],
                self.data[8],
                self.data[9],
                self.data[10],
                self.data[11],
            ])
        } else {
            0
        }
    }

    /// 获取文件条目的有效数据长度
    pub fn valid_length(&self) -> u64 {
        if self.is_file() {
            u64::from_le_bytes([
                self.data[4],
                self.data[5],
                self.data[6],
                self.data[7],
                self.data[8],
                self.data[9],
                self.data[10],
                self.data[11],
            ])
        } else {
            0
        }
    }

    /// 从指定簇读取目录条目
    ///
    /// # Errors
    /// 当读取 FAT 链失败时返回 `NoSpace` 或底层错误;
    /// 当底层扇区读取失败时返回 `Io`.
    pub fn from_cluster(
        device_idx: u8,
        super_block: &ExfatSuperBlock,
        cluster: u32,
    ) -> Result<Vec<Self>, KernelError> {
        let mut entries = Vec::new();
        let chain = super::fat::read_fat_chain(device_idx, super_block, cluster)?;
        let cluster_size = super_block.bytes_per_cluster() as usize;

        for c in &chain {
            let mut cluster_data = vec![0u8; cluster_size];
            let sector = super_block.cluster_to_sector(*c);
            let sectors_per_cluster = super_block.sectors_per_cluster();

            for i in 0..sectors_per_cluster {
                let offset = i as usize * super_block.bytes_per_sector() as usize;
                let result = with_device(device_idx as usize, |dev| {
                    read_sectors(
                        dev,
                        u64::from(sector + i),
                        1,
                        &mut cluster_data[offset..offset + super_block.bytes_per_sector() as usize],
                    )
                });

                if !matches!(result, Some(Ok(()))) {
                    return Err(KernelError::Io);
                }
            }

            let mut offset = 0;
            while offset + 32 <= cluster_data.len() {
                if let Some(entry) = Self::from_bytes(&cluster_data[offset..]) {
                    if entry.is_end() {
                        break;
                    }
                    entries.push(entry);
                }
                offset += 32;
            }
        }

        Ok(entries)
    }
}
