#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! exFAT 块分配器

use super::fat::FAT_END;
use super::super_block::ExfatSuperBlock;
use crate::framework::chitin::{chitin_blk_read_sectors, chitin_blk_write_sectors};
use crate::services::fs::KernelError;
use alloc::vec;

/// 分配一个空闲簇
///
/// # Errors
/// 当 FAT 表条目写入失败时返回底层 `KernelError`;
/// 当扫描完所有簇仍找不到空闲簇时返回 `NoSpace`.
pub fn alloc_cluster(device_idx: u8, super_block: &ExfatSuperBlock) -> Result<u32, KernelError> {
    let bytes_per_sector = super_block.bytes_per_sector() as usize;

    // 读取 FAT 表扫描空闲簇
    for cluster in 2..super_block.cluster_count + 2 {
        let fat_sector = super_block.fat_offset + (cluster * 4) / bytes_per_sector as u32;
        let fat_offset = (cluster * 4) % bytes_per_sector as u32;

        let mut sector_data = vec![0u8; bytes_per_sector];
        if chitin_blk_read_sectors(device_idx, u64::from(fat_sector), 1, &mut sector_data).is_err()
        {
            continue;
        }

        let entry = u32::from_le_bytes([
            sector_data[fat_offset as usize],
            sector_data[fat_offset as usize + 1],
            sector_data[fat_offset as usize + 2],
            sector_data[fat_offset as usize + 3],
        ]);

        if entry == 0 {
            // 找到空闲簇，标记为 FAT_END
            super::fat::write_fat_entry(device_idx, super_block, cluster, FAT_END)?;
            return Ok(cluster);
        }
    }

    Err(KernelError::NoSpace)
}

/// 释放一个簇链
///
/// # Errors
/// 当读取 FAT 链或写入 FAT 条目失败时返回对应的 `KernelError`.
pub fn free_cluster_chain(
    device_idx: u8,
    super_block: &ExfatSuperBlock,
    start_cluster: u32,
) -> Result<(), KernelError> {
    let chain = super::fat::read_fat_chain(device_idx, super_block, start_cluster)?;

    for cluster in chain {
        super::fat::write_fat_entry(device_idx, super_block, cluster, 0)?;
    }

    Ok(())
}

/// 写入一个簇的数据
///
/// # Errors
/// 当 `data` 长度超过簇大小时返回 `InvalidArgument`;
/// 当底层扇区写入失败时返回 `Io`.
pub fn write_cluster(
    device_idx: u8,
    super_block: &ExfatSuperBlock,
    cluster: u32,
    data: &[u8],
) -> Result<(), KernelError> {
    let bytes_per_sector = super_block.bytes_per_sector() as usize;
    let sectors_per_cluster = super_block.sectors_per_cluster() as usize;
    let cluster_size = bytes_per_sector * sectors_per_cluster;

    if data.len() > cluster_size {
        return Err(KernelError::InvalidArgument);
    }

    let mut buf = vec![0u8; cluster_size];
    buf[..data.len()].copy_from_slice(data);

    let sector = super_block.cluster_to_sector(cluster);

    for i in 0..sectors_per_cluster {
        let offset = i * bytes_per_sector;
        chitin_blk_write_sectors(
            device_idx,
            u64::from(sector + i as u32),
            1,
            &buf[offset..offset + bytes_per_sector],
        )?;
    }

    Ok(())
}

/// 读取一个簇的数据
///
/// # Errors
/// 当 `buf` 长度超过簇大小时返回 `InvalidArgument`;
/// 当底层扇区读取失败时返回 `Io`.
pub fn read_cluster(
    device_idx: u8,
    super_block: &ExfatSuperBlock,
    cluster: u32,
    buf: &mut [u8],
) -> Result<(), KernelError> {
    let bytes_per_sector = super_block.bytes_per_sector() as usize;
    let sectors_per_cluster = super_block.sectors_per_cluster() as usize;
    let cluster_size = bytes_per_sector * sectors_per_cluster;

    if buf.len() > cluster_size {
        return Err(KernelError::InvalidArgument);
    }

    let sector = super_block.cluster_to_sector(cluster);
    let mut temp_buf = vec![0u8; cluster_size];

    for i in 0..sectors_per_cluster {
        let offset = i * bytes_per_sector;
        chitin_blk_read_sectors(
            device_idx,
            u64::from(sector + i as u32),
            1,
            &mut temp_buf[offset..offset + bytes_per_sector],
        )?;
    }

    let copy_len = buf.len().min(cluster_size);
    buf[..copy_len].copy_from_slice(&temp_buf[..copy_len]);

    Ok(())
}
