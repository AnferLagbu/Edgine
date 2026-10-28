#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。
//! exFAT FAT 表操作

use super::super_block::ExfatSuperBlock;
use crate::functions::fs::KernelError;
use crate::privileged::egdf::{egdf_blk_read_sectors, egdf_blk_write_sectors};

/// FAT 表条目常量
pub const FAT_FREE: u32 = 0x0000_0000;
/// 坏簇标记 (BAD cluster)
pub const FAT_BAD: u32 = 0xFFFF_FFF7;
/// FAT 链结束标记 (END of chain)
pub const FAT_END: u32 = 0x0FFF_FFFF;

/// 从 FAT 表读取簇链 (底层读取失败时提前结束链并返回已收集的簇)
pub fn read_fat_chain(
    device_idx: u8,
    super_block: &ExfatSuperBlock,
    start_cluster: u32,
) -> alloc::vec::Vec<u32> {
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

        if entry >= 0x0FFF_FFF8 || entry == FAT_END {
            break;
        }

        if entry == FAT_FREE || entry == FAT_BAD {
            break;
        }

        current = entry;
    }

    chain
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
