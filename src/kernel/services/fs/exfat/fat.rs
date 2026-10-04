#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! exFAT FAT 表操作

use super::super_block::ExfatSuperBlock;
use crate::framework::egdf::{egdf_blk_read_sectors, egdf_blk_write_sectors};
use crate::services::fs::KernelError;

/// FAT 表条目常量
pub const FAT_FREE: u32 = 0x00000000;
pub const FAT_BAD: u32 = 0xFFFFFFF7;
pub const FAT_END: u32 = 0x0FFFFFFF;

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 从 FAT 表读取簇链
///
/// # Errors
/// 当前实现不返回错误: 底层读取失败时提前结束链并返回已收集的簇.
pub fn read_fat_chain(
    device_idx: u8,
    super_block: &ExfatSuperBlock,
    start_cluster: u32,
) -> Result<alloc::vec::Vec<u32>, KernelError> {
    let mut chain = alloc::vec::Vec::new();
    let mut current = start_cluster;

    loop {
        if current < 2 || current >= super_block.cluster_count + 2 {
            break;
        }

        chain.push(current);

        // 读取 FAT 表条目
        let bytes_per_sector = super_block.bytes_per_sector();
        let fat_sector = super_block.fat_offset + (current * 4) / bytes_per_sector;
        let fat_offset = (current * 4) % bytes_per_sector;

        let mut sector_data = alloc::vec![0u8; bytes_per_sector as usize];
        if egdf_blk_read_sectors(device_idx, u64::from(fat_sector), 1, &mut sector_data).is_err() {
            break;
        }

        let entry = u32::from_le_bytes([
            sector_data[fat_offset as usize],
            sector_data[fat_offset as usize + 1],
            sector_data[fat_offset as usize + 2],
            sector_data[fat_offset as usize + 3],
        ]);

        if entry >= 0x0FFFFFF8 || entry == FAT_END {
            break;
        }

        if entry == FAT_FREE || entry == FAT_BAD {
            break;
        }

        current = entry;
    }

    Ok(chain)
}

/// 写入 FAT 表条目
///
/// # Errors
/// 当底层扇区读取或写入失败时返回 `Io`.
pub fn write_fat_entry(
    device_idx: u8,
    super_block: &ExfatSuperBlock,
    cluster: u32,
    value: u32,
) -> Result<(), KernelError> {
    let bytes_per_sector = super_block.bytes_per_sector();
    let fat_sector = super_block.fat_offset + (cluster * 4) / bytes_per_sector;
    let fat_offset = (cluster * 4) % bytes_per_sector;

    let mut sector_data = alloc::vec![0u8; bytes_per_sector as usize];
    egdf_blk_read_sectors(device_idx, u64::from(fat_sector), 1, &mut sector_data)?;

    let bytes = value.to_le_bytes();
    sector_data[fat_offset as usize] = bytes[0];
    sector_data[fat_offset as usize + 1] = bytes[1];
    sector_data[fat_offset as usize + 2] = bytes[2];
    sector_data[fat_offset as usize + 3] = bytes[3];

    egdf_blk_write_sectors(device_idx, u64::from(fat_sector), 1, &sector_data)?;

    Ok(())
}
