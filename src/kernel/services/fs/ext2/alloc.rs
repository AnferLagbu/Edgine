#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 块分配器

use super::block_group::Ext2BlockGroupDescriptor;
use super::inode::Ext2Inode;
use super::super_block::Ext2SuperBlock;
use crate::framework::egdf::{egdf_blk_read_sectors, egdf_blk_write_sectors};
use crate::services::fs::KernelError;

/// 在指定块组中分配一个空闲块
///
/// # Errors
/// 当底层位图读取/写入失败时返回 `Io`;
/// 当块组内没有空闲块时返回 `NoSpace`.
pub fn alloc_block_in_group(
    device_idx: u8,
    super_block: &Ext2SuperBlock,
    bgd: &Ext2BlockGroupDescriptor,
    group_idx: u32,
) -> Result<u32, KernelError> {
    let block_size = super_block.block_size() as usize;

    // 读取块位图
    let bitmap_sector = u64::from(bgd.bg_block_bitmap) * block_size as u64 / 512;
    let bitmap_sector_count = (block_size / 512).max(1) as u32;
    let mut bitmap_data = alloc::vec![0u8; block_size];

    egdf_blk_read_sectors(
        device_idx,
        bitmap_sector,
        bitmap_sector_count,
        &mut bitmap_data,
    )?;

    // 扫描位图寻找空闲块
    let blocks_per_group = super_block.s_blocks_per_group;
    let first_block = group_idx * blocks_per_group;

    for byte_idx in 0..block_size {
        if bitmap_data[byte_idx] == 0xFF {
            continue; // 整字节已满
        }

        for bit_idx in 0..8 {
            let block_offset = byte_idx * 8 + bit_idx;
            if block_offset as u32 >= blocks_per_group {
                break;
            }

            if (bitmap_data[byte_idx] & (1 << bit_idx)) == 0 {
                // 找到空闲块，设置位
                bitmap_data[byte_idx] |= 1 << bit_idx;

                // 写回位图
                egdf_blk_write_sectors(
                    device_idx,
                    bitmap_sector,
                    bitmap_sector_count,
                    &bitmap_data,
                )?;

                return Ok(first_block + block_offset as u32);
            }
        }
    }

    Err(KernelError::NoSpace)
}

/// 释放一个块
///
/// # Errors
/// 当底层位图读取/写入失败时返回 `Io`.
pub fn free_block(
    device_idx: u8,
    super_block: &Ext2SuperBlock,
    bgd: &Ext2BlockGroupDescriptor,
    block_num: u32,
) -> Result<(), KernelError> {
    let block_size = super_block.block_size() as usize;
    let block_offset = block_num % super_block.s_blocks_per_group;

    // 读取块位图
    let bitmap_sector = u64::from(bgd.bg_block_bitmap) * block_size as u64 / 512;
    let bitmap_sector_count = (block_size / 512).max(1) as u32;
    let mut bitmap_data = alloc::vec![0u8; block_size];

    egdf_blk_read_sectors(
        device_idx,
        bitmap_sector,
        bitmap_sector_count,
        &mut bitmap_data,
    )?;

    // 清除位
    let byte_idx = (block_offset / 8) as usize;
    let bit_idx = (block_offset % 8) as usize;
    bitmap_data[byte_idx] &= !(1 << bit_idx);

    // 写回位图
    egdf_blk_write_sectors(device_idx, bitmap_sector, bitmap_sector_count, &bitmap_data)?;

    Ok(())
}

/// 写入一个块
///
/// # Errors
/// 当 `data` 长度超过块大小时返回 `InvalidArgument`;
/// 当底层扇区写入失败时返回 `Io`.
pub fn write_block(
    device_idx: u8,
    super_block: &Ext2SuperBlock,
    block_num: u32,
    data: &[u8],
) -> Result<(), KernelError> {
    let block_size = super_block.block_size() as usize;
    if data.len() > block_size {
        return Err(KernelError::InvalidArgument);
    }

    let mut buf = alloc::vec![0u8; block_size];
    buf[..data.len()].copy_from_slice(data);

    let sector = u64::from(block_num) * block_size as u64 / 512;
    let sector_count = (block_size / 512) as u32;

    egdf_blk_write_sectors(device_idx, sector, sector_count, &buf)?;

    Ok(())
}

/// 写入 inode 到磁盘
///
/// # Errors
/// 当 `inode_num` 所属块组超出 `block_groups` 范围时返回 `InvalidArgument`;
/// 当底层扇区写入失败时返回 `Io`.
pub fn write_inode(
    device_idx: u8,
    super_block: &Ext2SuperBlock,
    block_groups: &[Ext2BlockGroupDescriptor],
    inode_num: u32,
    inode: &Ext2Inode,
) -> Result<(), KernelError> {
    let block_group = (inode_num - 1) / super_block.s_inodes_per_group;
    let index = (inode_num - 1) % super_block.s_inodes_per_group;

    if block_group as usize >= block_groups.len() {
        return Err(KernelError::InvalidArgument);
    }

    let bgd = &block_groups[block_group as usize];
    let inode_table_block = bgd.bg_inode_table;
    let inode_size = super_block.inode_size() as usize;
    let block_size = super_block.block_size() as usize;

    // 序列化 inode
    let mut inode_data = alloc::vec![0u8; inode_size];
    inode_data[0..2].copy_from_slice(&inode.i_mode.to_le_bytes());
    inode_data[2..4].copy_from_slice(&inode.i_uid.to_le_bytes());
    inode_data[4..8].copy_from_slice(&inode.i_size.to_le_bytes());
    inode_data[8..12].copy_from_slice(&inode.i_atime.to_le_bytes());
    inode_data[12..16].copy_from_slice(&inode.i_ctime.to_le_bytes());
    inode_data[16..20].copy_from_slice(&inode.i_mtime.to_le_bytes());
    inode_data[20..24].copy_from_slice(&inode.i_dtime.to_le_bytes());
    inode_data[24..26].copy_from_slice(&inode.i_gid.to_le_bytes());
    inode_data[26..28].copy_from_slice(&inode.i_links_count.to_le_bytes());
    inode_data[28..32].copy_from_slice(&inode.i_blocks.to_le_bytes());
    inode_data[32..36].copy_from_slice(&inode.i_flags.to_le_bytes());
    inode_data[36..40].copy_from_slice(&inode.i_osd1.to_le_bytes());

    // 写入 i_block 数组
    for i in 0..15 {
        let offset = 40 + i * 4;
        inode_data[offset..offset + 4].copy_from_slice(&inode.i_block[i].to_le_bytes());
    }

    inode_data[100..104].copy_from_slice(&inode.i_generation.to_le_bytes());
    inode_data[104..108].copy_from_slice(&inode.i_file_acl.to_le_bytes());
    inode_data[108..112].copy_from_slice(&inode.i_dir_acl.to_le_bytes());
    inode_data[112..116].copy_from_slice(&inode.i_faddr.to_le_bytes());
    inode_data[116..128].copy_from_slice(&inode.i_osd2);

    // 计算扇区位置
    let inode_offset = inode_table_block as usize * block_size + index as usize * inode_size;
    let sector = inode_offset / 512;
    let sector_count = (inode_size / 512).max(1) as u32;

    egdf_blk_write_sectors(device_idx, sector as u64, sector_count, &inode_data)?;

    Ok(())
}

/// 在指定块组中分配一个空闲 inode
///
/// # Errors
/// 当底层位图读取/写入失败时返回 `Io`;
/// 当块组内没有空闲 inode 时返回 `NoSpace`.
pub fn alloc_inode_in_group(
    device_idx: u8,
    super_block: &Ext2SuperBlock,
    bgd: &Ext2BlockGroupDescriptor,
    group_idx: u32,
) -> Result<u32, KernelError> {
    let block_size = super_block.block_size() as usize;

    // 读取 inode 位图
    let bitmap_sector = u64::from(bgd.bg_inode_bitmap) * block_size as u64 / 512;
    let bitmap_sector_count = (block_size / 512).max(1) as u32;
    let mut bitmap_data = alloc::vec![0u8; block_size];

    egdf_blk_read_sectors(
        device_idx,
        bitmap_sector,
        bitmap_sector_count,
        &mut bitmap_data,
    )?;

    // 扫描位图寻找空闲 inode
    let inodes_per_group = super_block.s_inodes_per_group;
    let first_inode = group_idx * inodes_per_group + super_block.first_inode();

    for byte_idx in 0..block_size {
        if bitmap_data[byte_idx] == 0xFF {
            continue;
        }

        for bit_idx in 0..8 {
            let inode_offset = byte_idx * 8 + bit_idx;
            if inode_offset as u32 >= inodes_per_group {
                break;
            }

            if (bitmap_data[byte_idx] & (1 << bit_idx)) == 0 {
                // 找到空闲 inode，设置位
                bitmap_data[byte_idx] |= 1 << bit_idx;

                // 写回位图
                egdf_blk_write_sectors(
                    device_idx,
                    bitmap_sector,
                    bitmap_sector_count,
                    &bitmap_data,
                )?;

                return Ok(first_inode + inode_offset as u32);
            }
        }
    }

    Err(KernelError::NoSpace)
}

/// 释放一个 inode
///
/// # Errors
/// 当底层位图读取/写入失败时返回 `Io`.
pub fn free_inode(
    device_idx: u8,
    super_block: &Ext2SuperBlock,
    bgd: &Ext2BlockGroupDescriptor,
    inode_num: u32,
) -> Result<(), KernelError> {
    let block_size = super_block.block_size() as usize;
    let inode_offset = (inode_num - super_block.first_inode()) % super_block.s_inodes_per_group;

    // 读取 inode 位图
    let bitmap_sector = u64::from(bgd.bg_inode_bitmap) * block_size as u64 / 512;
    let bitmap_sector_count = (block_size / 512).max(1) as u32;
    let mut bitmap_data = alloc::vec![0u8; block_size];

    egdf_blk_read_sectors(
        device_idx,
        bitmap_sector,
        bitmap_sector_count,
        &mut bitmap_data,
    )?;

    // 清除位
    let byte_idx = (inode_offset / 8) as usize;
    let bit_idx = (inode_offset % 8) as usize;
    bitmap_data[byte_idx] &= !(1 << bit_idx);

    // 写回位图
    egdf_blk_write_sectors(device_idx, bitmap_sector, bitmap_sector_count, &bitmap_data)?;

    Ok(())
}
