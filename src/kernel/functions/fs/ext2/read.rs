#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 读取核心逻辑

use super::block_group::Ext2BlockGroupDescriptor;
use super::dir::Ext2DirEntry;
use super::inode::Ext2Inode;
use super::super_block::Ext2SuperBlock;
use crate::functions::fs::KernelError;
use crate::privileged::egdf::egdf_blk_read_sectors;
use alloc::format;
use alloc::vec::Vec;

/// ext2 文件系统实例
pub struct Ext2Fs {
    /// 超级块
    pub super_block: Ext2SuperBlock,
    /// 块组描述符表
    pub block_groups: Vec<Ext2BlockGroupDescriptor>,
    /// 块设备索引
    pub device_idx: u8,
    /// inode 缓存 (`inode_num` -> inode)
    inode_cache: Vec<(u32, Ext2Inode)>,
}

impl Ext2Fs {
    /// 打开 ext2 文件系统
    ///
    /// # Errors
    /// 当超级块或块组描述符表读取失败时返回 `Io`;
    /// 当引导数据不是合法 ext2 超级块时返回 `InvalidArgument`.
    pub fn open(device_idx: u8) -> Result<Self, KernelError> {
        // 读取超级块 (偏移 1024 字节)
        let mut sb_data = [0u8; 1024];
        let sb_sector = 1024 / 512; // 扇区 2
        let sb_sector_count = 1024 / 512; // 2 扇区

        egdf_blk_read_sectors(device_idx, sb_sector as u64, sb_sector_count, &mut sb_data)?;

        // 解析超级块
        let super_block =
            Ext2SuperBlock::from_bytes(&sb_data).ok_or(KernelError::InvalidArgument)?;

        // 读取块组描述符表
        let bgd_block = u64::from(super_block.bgd_block());
        let block_size = super_block.block_size() as usize;
        let bgd_sector = (bgd_block * block_size as u64) / 512;
        let bgd_sector_count = (block_size / 512).max(1) as u32;

        let mut bgd_data = alloc::vec![0u8; block_size];
        egdf_blk_read_sectors(device_idx, bgd_sector, bgd_sector_count, &mut bgd_data)?;

        let bg_count = super_block.block_group_count() as usize;
        let block_groups = Ext2BlockGroupDescriptor::from_table(&bgd_data, bg_count);

        Ok(Self {
            super_block,
            block_groups,
            device_idx,
            inode_cache: Vec::new(),
        })
    }

    /// 读取 inode
    ///
    /// # Errors
    /// 当 inode 所属块组越界或 inode 数据解析失败时返回 `InvalidArgument`;
    /// 当底层扇区读取失败时返回 `Io`.
    pub fn read_inode(&mut self, inode_num: u32) -> Result<Ext2Inode, KernelError> {
        // 检查缓存
        for (num, inode) in &self.inode_cache {
            if *num == inode_num {
                return Ok(*inode);
            }
        }

        // 计算 inode 位置
        let block_group = (inode_num - 1) / self.super_block.s_inodes_per_group;
        let index = (inode_num - 1) % self.super_block.s_inodes_per_group;

        if block_group as usize >= self.block_groups.len() {
            return Err(KernelError::InvalidArgument);
        }

        let bgd = &self.block_groups[block_group as usize];
        let inode_table_block = bgd.bg_inode_table;
        let inode_size = self.super_block.inode_size() as usize;
        let block_size = self.super_block.block_size() as usize;

        // 计算扇区位置
        let inode_offset = inode_table_block as usize * block_size + index as usize * inode_size;
        let sector = inode_offset / 512;
        let sector_count = (inode_size / 512).max(1) as u32;

        let mut inode_data = alloc::vec![0u8; inode_size];
        egdf_blk_read_sectors(
            self.device_idx,
            sector as u64,
            sector_count,
            &mut inode_data,
        )?;

        let inode = Ext2Inode::from_bytes(&inode_data).ok_or(KernelError::InvalidArgument)?;

        // 缓存
        self.inode_cache.push((inode_num, inode));

        Ok(inode)
    }

    /// 读取数据块
    ///
    /// # Errors
    /// 当底层扇区读取失败时返回 `Io`.
    pub fn read_block(&self, block_num: u32) -> Result<Vec<u8>, KernelError> {
        let block_size = self.super_block.block_size() as usize;
        let mut data = alloc::vec![0u8; block_size];

        let sector = u64::from(block_num) * block_size as u64 / 512;
        let sector_count = (block_size / 512) as u32;

        egdf_blk_read_sectors(self.device_idx, sector, sector_count, &mut data)?;
        Ok(data)
    }

    /// 读取目录内容
    ///
    /// # Errors
    /// 当 inode 不是目录时返回 `NotADirectory`;
    /// 当 inode 或底层数据块读取失败时返回对应的 `KernelError`.
    pub fn read_dir(&mut self, inode_num: u32) -> Result<Vec<Ext2DirEntry>, KernelError> {
        let inode = self.read_inode(inode_num)?;

        if inode.file_type() != 1 {
            return Err(KernelError::NotADirectory);
        }

        let mut entries = Vec::new();
        let block_size = self.super_block.block_size();
        let file_size = inode.i_size;

        // 读取直接块
        let mut bytes_read = 0u32;
        let mut block_idx = 0u32;

        while bytes_read < file_size && block_idx < 12 {
            let block_num = inode.i_block[block_idx as usize];
            if block_num == 0 {
                break;
            }

            let block_data = self.read_block(block_num)?;
            let mut offset = 0;

            while offset < block_size as usize {
                if offset + 8 > block_data.len() {
                    break;
                }

                if let Some(entry) = Ext2DirEntry::from_bytes(&block_data[offset..]) {
                    if entry.inode != 0 {
                        entries.push(entry.clone());
                    }
                    offset += entry.rec_len as usize;
                    if entry.rec_len == 0 {
                        break;
                    }
                } else {
                    break;
                }
            }

            bytes_read += block_size;
            block_idx += 1;
        }

        Ok(entries)
    }

    /// 查找路径
    ///
    /// # Errors
    /// 当中间目录读取失败时返回底层 `KernelError`;
    /// 当路径中某组件不存在时返回 `FileNotFound`.
    pub fn lookup_path(&mut self, path: &str) -> Result<u32, KernelError> {
        let mut current_inode = 2; // 根目录 inode

        if path == "/" || path.is_empty() {
            return Ok(current_inode);
        }

        for component in path.trim_start_matches('/').split('/') {
            if component.is_empty() {
                continue;
            }

            let entries = self.read_dir(current_inode)?;
            let mut found = false;

            for entry in &entries {
                if entry.get_name() == component {
                    current_inode = entry.inode;
                    found = true;
                    break;
                }
            }

            if !found {
                return Err(KernelError::FileNotFound);
            }
        }

        Ok(current_inode)
    }

    /// 读取文件内容
    ///
    /// # Errors
    /// 当 inode 不是普通文件时返回 `InvalidArgument`;
    /// 当 inode 或底层数据块读取失败时返回对应的 `KernelError`.
    pub fn read_file(
        &mut self,
        inode_num: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, KernelError> {
        let inode = self.read_inode(inode_num)?;

        if inode.file_type() != 0 {
            return Err(KernelError::InvalidArgument);
        }

        let file_size = u64::from(inode.i_size);
        if offset >= file_size {
            return Ok(0);
        }

        let block_size = u64::from(self.super_block.block_size());
        let mut bytes_read = 0;
        let mut pos = offset;

        while bytes_read < buf.len() && pos < file_size {
            let block_idx = (pos / block_size) as u32;
            let block_offset = (pos % block_size) as usize;

            // 获取物理块号
            let block_num = self.get_physical_block(&inode, block_idx)?;
            if block_num == 0 {
                break;
            }

            let block_data = self.read_block(block_num)?;
            let available = block_size as usize - block_offset;
            let to_read = (buf.len() - bytes_read)
                .min(available)
                .min((file_size - pos) as usize);

            buf[bytes_read..bytes_read + to_read]
                .copy_from_slice(&block_data[block_offset..block_offset + to_read]);

            bytes_read += to_read;
            pos += to_read as u64;
        }

        Ok(bytes_read)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    /// 获取物理块号 (处理间接寻址)
    fn get_physical_block(&mut self, inode: &Ext2Inode, logical: u32) -> Result<u32, KernelError> {
        let block_size = self.super_block.block_size();
        let blocks_per_indirect = block_size / 4;

        if logical < 12 {
            // 直接块
            Ok(inode.i_block[logical as usize])
        } else if logical < 12 + blocks_per_indirect {
            // 一次间接
            let indirect_block = inode.i_block[12];
            if indirect_block == 0 {
                return Ok(0);
            }

            let idx = logical - 12;
            let block_data = self.read_block(indirect_block)?;
            let offset = idx as usize * 4;

            if offset + 4 > block_data.len() {
                return Ok(0);
            }

            Ok(u32::from_le_bytes([
                block_data[offset],
                block_data[offset + 1],
                block_data[offset + 2],
                block_data[offset + 3],
            ]))
        } else if logical < 12 + blocks_per_indirect + blocks_per_indirect * blocks_per_indirect {
            // 二次间接
            let double_block = inode.i_block[13];
            if double_block == 0 {
                return Ok(0);
            }

            let idx = logical - 12 - blocks_per_indirect;
            let indirect_idx = idx / blocks_per_indirect;
            let block_idx = idx % blocks_per_indirect;

            // 读取二级间接块
            let double_data = self.read_block(double_block)?;
            let indirect_offset = indirect_idx as usize * 4;

            if indirect_offset + 4 > double_data.len() {
                return Ok(0);
            }

            let indirect_block = u32::from_le_bytes([
                double_data[indirect_offset],
                double_data[indirect_offset + 1],
                double_data[indirect_offset + 2],
                double_data[indirect_offset + 3],
            ]);

            if indirect_block == 0 {
                return Ok(0);
            }

            // 读取一级间接块
            let indirect_data = self.read_block(indirect_block)?;
            let block_offset = block_idx as usize * 4;

            if block_offset + 4 > indirect_data.len() {
                return Ok(0);
            }

            Ok(u32::from_le_bytes([
                indirect_data[block_offset],
                indirect_data[block_offset + 1],
                indirect_data[block_offset + 2],
                indirect_data[block_offset + 3],
            ]))
        } else {
            // 三次间接
            let triple_block = inode.i_block[14];
            if triple_block == 0 {
                return Ok(0);
            }

            let idx =
                logical - 12 - blocks_per_indirect - blocks_per_indirect * blocks_per_indirect;
            let l1_idx = idx / (blocks_per_indirect * blocks_per_indirect);
            let l2_idx = (idx / blocks_per_indirect) % blocks_per_indirect;
            let l3_idx = idx % blocks_per_indirect;

            // 读取三级间接块
            let triple_data = self.read_block(triple_block)?;
            let l1_offset = l1_idx as usize * 4;
            if l1_offset + 4 > triple_data.len() {
                return Ok(0);
            }
            let l2_block = u32::from_le_bytes([
                triple_data[l1_offset],
                triple_data[l1_offset + 1],
                triple_data[l1_offset + 2],
                triple_data[l1_offset + 3],
            ]);
            if l2_block == 0 {
                return Ok(0);
            }

            // 读取二级间接块
            let l2_data = self.read_block(l2_block)?;
            let l2_offset = l2_idx as usize * 4;
            if l2_offset + 4 > l2_data.len() {
                return Ok(0);
            }
            let l3_block = u32::from_le_bytes([
                l2_data[l2_offset],
                l2_data[l2_offset + 1],
                l2_data[l2_offset + 2],
                l2_data[l2_offset + 3],
            ]);
            if l3_block == 0 {
                return Ok(0);
            }

            // 读取一级间接块
            let l3_data = self.read_block(l3_block)?;
            let l3_offset = l3_idx as usize * 4;
            if l3_offset + 4 > l3_data.len() {
                return Ok(0);
            }

            Ok(u32::from_le_bytes([
                l3_data[l3_offset],
                l3_data[l3_offset + 1],
                l3_data[l3_offset + 2],
                l3_data[l3_offset + 3],
            ]))
        }
    }

    /// 分配一个新 inode
    ///
    /// # Errors
    /// 当所有块组都没有空闲 inode 时返回 `NoSpace`;
    /// 当底层位图操作失败时返回 `Io`.
    pub fn allocate_inode(&mut self) -> Result<u32, KernelError> {
        // 遍历块组寻找有空闲 inode 的组
        for (i, bgd) in self.block_groups.iter().enumerate() {
            if bgd.bg_free_inodes_count > 0 {
                let inode_num = super::alloc::alloc_inode_in_group(
                    self.device_idx,
                    &self.super_block,
                    bgd,
                    i as u32,
                )?;
                return Ok(inode_num);
            }
        }
        Err(KernelError::NoSpace)
    }

    /// 分配一个新块
    ///
    /// # Errors
    /// 当所有块组都没有空闲块时返回 `NoSpace`;
    /// 当底层位图操作失败时返回 `Io`.
    pub fn allocate_block(&mut self) -> Result<u32, KernelError> {
        // 遍历块组寻找有空闲块的组
        for (i, bgd) in self.block_groups.iter().enumerate() {
            if bgd.bg_free_blocks_count > 0 {
                let block_num = super::alloc::alloc_block_in_group(
                    self.device_idx,
                    &self.super_block,
                    bgd,
                    i as u32,
                )?;
                return Ok(block_num);
            }
        }
        Err(KernelError::NoSpace)
    }

    /// 释放一个块
    ///
    /// # Errors
    /// 当 `block_num` 所属块组越界时返回 `InvalidArgument`;
    /// 当底层位图操作失败时返回 `Io`.
    pub fn deallocate_block(&mut self, block_num: u32) -> Result<(), KernelError> {
        let group_idx = block_num / self.super_block.s_blocks_per_group;
        if group_idx as usize >= self.block_groups.len() {
            return Err(KernelError::InvalidArgument);
        }

        super::alloc::free_block(
            self.device_idx,
            &self.super_block,
            &self.block_groups[group_idx as usize],
            block_num,
        )
    }

    /// 写入 inode 到磁盘
    ///
    /// # Errors
    /// 错误条件与 [`super::alloc::write_inode`] 相同, 参见其 `# Errors` 段.
    pub fn save_inode(&self, inode_num: u32, inode: &Ext2Inode) -> Result<(), KernelError> {
        super::alloc::write_inode(
            self.device_idx,
            &self.super_block,
            &self.block_groups,
            inode_num,
            inode,
        )
    }

    /// 写入数据块
    ///
    /// # Errors
    /// 错误条件与 [`super::alloc::write_block`] 相同, 参见其 `# Errors` 段.
    pub fn save_block(&self, block_num: u32, data: &[u8]) -> Result<(), KernelError> {
        super::alloc::write_block(self.device_idx, &self.super_block, block_num, data)
    }

    /// 写入文件内容
    ///
    /// # Errors
    /// 当 inode 不是普通文件时返回 `InvalidArgument`;
    /// 当 inode 读取、块分配或底层 I/O 失败时返回对应的 `KernelError`.
    pub fn write_file(
        &mut self,
        inode_num: u32,
        offset: u64,
        buf: &[u8],
    ) -> Result<usize, KernelError> {
        let mut inode = self.read_inode(inode_num)?;

        if inode.file_type() != 0 {
            return Err(KernelError::InvalidArgument);
        }

        let block_size = u64::from(self.super_block.block_size());
        let mut bytes_written = 0;
        let mut pos = offset;

        while bytes_written < buf.len() {
            let block_idx = (pos / block_size) as u32;
            let block_offset = (pos % block_size) as usize;

            // 获取或分配物理块
            let block_num = if block_idx < 12 {
                if inode.i_block[block_idx as usize] == 0 {
                    let new_block = self.allocate_block()?;
                    inode.i_block[block_idx as usize] = new_block;
                    new_block
                } else {
                    inode.i_block[block_idx as usize]
                }
            } else {
                // 间接块处理
                self.get_or_alloc_indirect_block(&mut inode, block_idx)?
            };

            // 读取现有块内容（如果需要部分写入）
            let mut block_data =
                if block_offset > 0 || buf.len() - bytes_written < block_size as usize {
                    self.read_block(block_num)?
                } else {
                    alloc::vec![0u8; block_size as usize]
                };

            // 写入数据
            let available = block_size as usize - block_offset;
            let to_write = (buf.len() - bytes_written).min(available);
            block_data[block_offset..block_offset + to_write]
                .copy_from_slice(&buf[bytes_written..bytes_written + to_write]);

            // 保存块
            self.save_block(block_num, &block_data)?;

            bytes_written += to_write;
            pos += to_write as u64;
        }

        // 更新 inode 大小
        let new_size = (offset + bytes_written as u64).max(u64::from(inode.i_size));
        inode.i_size = new_size as u32;
        inode.i_mtime = crate::arch!(timestamp()) as u32;
        inode.i_blocks = ((new_size + u64::from(self.super_block.block_size()) - 1)
            / u64::from(self.super_block.block_size())
            * 512
            / u64::from(self.super_block.block_size())) as u32;

        self.save_inode(inode_num, &inode)?;

        // 更新缓存
        for (num, cached) in &mut self.inode_cache {
            if *num == inode_num {
                *cached = inode;
                break;
            }
        }

        Ok(bytes_written)
    }

    /// 获取或分配间接块
    fn get_or_alloc_indirect_block(
        &mut self,
        inode: &mut Ext2Inode,
        logical: u32,
    ) -> Result<u32, KernelError> {
        let blocks_per_indirect = self.super_block.block_size() / 4;

        if logical < 12 + blocks_per_indirect {
            // 一次间接
            if inode.i_block[12] == 0 {
                let new_block = self.allocate_block()?;
                inode.i_block[12] = new_block;
                // 清零间接块
                let zero_data = alloc::vec![0u8; self.super_block.block_size() as usize];
                self.save_block(new_block, &zero_data)?;
            }

            let idx = logical - 12;
            let block_data = self.read_block(inode.i_block[12])?;
            let offset = idx as usize * 4;

            if offset + 4 > block_data.len() {
                return Err(KernelError::InvalidArgument);
            }

            let existing = u32::from_le_bytes([
                block_data[offset],
                block_data[offset + 1],
                block_data[offset + 2],
                block_data[offset + 3],
            ]);

            if existing == 0 {
                let new_block = self.allocate_block()?;
                // 写回间接块
                let mut new_block_data = block_data.clone();
                new_block_data[offset..offset + 4].copy_from_slice(&new_block.to_le_bytes());
                self.save_block(inode.i_block[12], &new_block_data)?;
                Ok(new_block)
            } else {
                Ok(existing)
            }
        } else if logical < 12 + blocks_per_indirect + blocks_per_indirect * blocks_per_indirect {
            // 二次间接
            if inode.i_block[13] == 0 {
                let new_block = self.allocate_block()?;
                inode.i_block[13] = new_block;
                let zero_data = alloc::vec![0u8; self.super_block.block_size() as usize];
                self.save_block(new_block, &zero_data)?;
            }

            let idx = logical - 12 - blocks_per_indirect;
            let l1_idx = idx / blocks_per_indirect;
            let l2_idx = idx % blocks_per_indirect;

            // 读取二级间接块
            let l1_data = self.read_block(inode.i_block[13])?;
            let l1_offset = l1_idx as usize * 4;

            let l2_block = u32::from_le_bytes([
                l1_data[l1_offset],
                l1_data[l1_offset + 1],
                l1_data[l1_offset + 2],
                l1_data[l1_offset + 3],
            ]);

            let l2_block = if l2_block == 0 {
                let new_block = self.allocate_block()?;
                let zero_data = alloc::vec![0u8; self.super_block.block_size() as usize];
                self.save_block(new_block, &zero_data)?;

                // 更新一级间接块
                let mut new_l1_data = l1_data.clone();
                new_l1_data[l1_offset..l1_offset + 4].copy_from_slice(&new_block.to_le_bytes());
                self.save_block(inode.i_block[13], &new_l1_data)?;

                new_block
            } else {
                l2_block
            };

            // 读取二级间接块
            let l2_data = self.read_block(l2_block)?;
            let l2_offset = l2_idx as usize * 4;

            let existing = u32::from_le_bytes([
                l2_data[l2_offset],
                l2_data[l2_offset + 1],
                l2_data[l2_offset + 2],
                l2_data[l2_offset + 3],
            ]);

            if existing == 0 {
                let new_block = self.allocate_block()?;
                let mut new_l2_data = l2_data.clone();
                new_l2_data[l2_offset..l2_offset + 4].copy_from_slice(&new_block.to_le_bytes());
                self.save_block(l2_block, &new_l2_data)?;
                Ok(new_block)
            } else {
                Ok(existing)
            }
        } else {
            // 三次间接
            Err(KernelError::NotSupported)
        }
    }

    /// 创建目录项
    ///
    /// # Errors
    /// 当父 inode 不是目录时返回 `NotADirectory`;
    /// 当 inode/数据块读取或底层 I/O 失败时返回对应的 `KernelError`.
    pub fn create_dir_entry(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        child_inode_num: u32,
        file_type: u8,
    ) -> Result<(), KernelError> {
        let mut parent_inode = self.read_inode(parent_inode_num)?;
        if parent_inode.file_type() != 1 {
            return Err(KernelError::NotADirectory);
        }

        let block_size = self.super_block.block_size() as usize;

        // 构造目录项
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(255);
        let rec_len = ((name_len + 8 + 3) & !3) as u16;

        let mut dir_entry = alloc::vec![0u8; rec_len as usize];
        dir_entry[0..4].copy_from_slice(&child_inode_num.to_le_bytes());
        dir_entry[4..6].copy_from_slice(&rec_len.to_le_bytes());
        dir_entry[6] = name_len as u8;
        dir_entry[7] = file_type;
        dir_entry[8..8 + name_len].copy_from_slice(&name_bytes[..name_len]);

        // 找到最后一个直接块
        let mut last_block_idx = 0;
        let mut last_block_num = 0;
        let mut insert_offset = 0;

        for i in 0..12 {
            if parent_inode.i_block[i] != 0 {
                last_block_idx = i;
                last_block_num = parent_inode.i_block[i];
                let block_data = self.read_block(last_block_num)?;

                // 找到块末尾的空闲空间
                let mut offset = 0;
                while offset < block_size {
                    if offset + 8 > block_data.len() {
                        break;
                    }
                    let entry_inode = u32::from_le_bytes([
                        block_data[offset],
                        block_data[offset + 1],
                        block_data[offset + 2],
                        block_data[offset + 3],
                    ]);
                    let entry_rec_len =
                        u16::from_le_bytes([block_data[offset + 4], block_data[offset + 5]]);

                    if entry_inode == 0 {
                        // 空闲目录项
                        insert_offset = offset;
                        break;
                    }

                    offset += entry_rec_len as usize;
                }

                if offset < block_size {
                    insert_offset = offset;
                    break;
                }
            }
        }

        if last_block_num == 0 {
            // 分配新块
            let new_block = self.allocate_block()?;
            parent_inode.i_block[0] = new_block;
            last_block_num = new_block;
            insert_offset = 0;
        }

        // 读取块并插入目录项
        let mut block_data = self.read_block(last_block_num)?;
        if insert_offset + rec_len as usize > block_size {
            // 块已满，分配新块
            let new_block = self.allocate_block()?;
            parent_inode.i_block[last_block_idx + 1] = new_block;
            last_block_num = new_block;
            block_data = alloc::vec![0u8; block_size];
            insert_offset = 0;
        }

        // 插入目录项
        let entry_len = dir_entry.len();
        block_data[insert_offset..insert_offset + entry_len].copy_from_slice(&dir_entry);

        // 保存块
        self.save_block(last_block_num, &block_data)?;

        // 更新父目录 inode
        parent_inode.i_size += entry_len as u32;
        parent_inode.i_mtime = crate::arch!(timestamp()) as u32;
        self.save_inode(parent_inode_num, &parent_inode)?;

        Ok(())
    }

    /// 删除目录项
    ///
    /// # Errors
    /// 当父 inode 不是目录时返回 `NotADirectory`;
    /// 当目录项不存在时返回 `FileNotFound`;
    /// 当底层 I/O 失败时返回对应的 `KernelError`.
    pub fn remove_dir_entry(
        &mut self,
        parent_inode_num: u32,
        name: &str,
    ) -> Result<u32, KernelError> {
        let mut parent_inode = self.read_inode(parent_inode_num)?;
        if parent_inode.file_type() != 1 {
            return Err(KernelError::NotADirectory);
        }

        let block_size = self.super_block.block_size() as usize;

        for i in 0..12 {
            let block_num = parent_inode.i_block[i];
            if block_num == 0 {
                continue;
            }

            let mut block_data = self.read_block(block_num)?;
            let mut offset = 0;

            while offset < block_size {
                if offset + 8 > block_data.len() {
                    break;
                }

                let entry_inode = u32::from_le_bytes([
                    block_data[offset],
                    block_data[offset + 1],
                    block_data[offset + 2],
                    block_data[offset + 3],
                ]);
                let entry_rec_len =
                    u16::from_le_bytes([block_data[offset + 4], block_data[offset + 5]]);
                let entry_name_len = block_data[offset + 6];

                if entry_inode != 0 && entry_name_len as usize == name.len() {
                    let entry_name = core::str::from_utf8(
                        &block_data[offset + 8..offset + 8 + entry_name_len as usize],
                    )
                    .unwrap_or("");
                    if entry_name == name {
                        // 找到目录项，标记为已删除
                        let removed_inode = entry_inode;
                        block_data[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());

                        // 保存块
                        self.save_block(block_num, &block_data)?;

                        // 更新父目录 inode
                        parent_inode.i_mtime = crate::arch!(timestamp()) as u32;
                        self.save_inode(parent_inode_num, &parent_inode)?;

                        return Ok(removed_inode);
                    }
                }

                if entry_rec_len == 0 {
                    break;
                }
                offset += entry_rec_len as usize;
            }
        }

        Err(KernelError::FileNotFound)
    }

    /// 创建目录
    ///
    /// # Errors
    /// 当父路径无效或底层 I/O 失败时返回对应的 `KernelError`;
    /// 当同名目录已存在时返回 `AlreadyExists`.
    pub fn mkdir(&mut self, parent_path: &str, name: &str) -> Result<u32, KernelError> {
        let parent_inode_num = self.lookup_path(parent_path)?;

        // 检查目录是否已存在
        let entries = self.read_dir(parent_inode_num)?;
        for entry in &entries {
            if entry.get_name() == name {
                return Err(KernelError::AlreadyExists);
            }
        }

        // 分配新 inode
        let new_inode_num = self.allocate_inode()?;

        // 初始化目录 inode
        let block_size = self.super_block.block_size() as usize;
        let new_block = self.allocate_block()?;
        let timestamp = crate::arch!(timestamp()) as u32;

        let mut i_block = [0u32; 15];
        i_block[0] = new_block;

        let new_inode = Ext2Inode {
            i_mode: 0x4000 | 0o755, // 目录，rwxr-xr-x
            i_size: block_size as u32,
            i_links_count: 2, // . 和 ..
            i_blocks: (block_size / 512) as u32,
            i_block,
            i_mtime: timestamp,
            i_ctime: timestamp,
            ..Ext2Inode::default()
        };

        // 初始化目录块（. 和 ..）
        let mut dir_data = alloc::vec![0u8; block_size];

        // . 目录项
        dir_data[0..4].copy_from_slice(&new_inode_num.to_le_bytes());
        dir_data[4..6].copy_from_slice(&8u16.to_le_bytes());
        dir_data[6] = 1; // name_len
        dir_data[7] = 1; // file_type (目录)
        dir_data[8] = b'.';

        // .. 目录项
        dir_data[8 + 8..8 + 8 + 4].copy_from_slice(&parent_inode_num.to_le_bytes());
        dir_data[8 + 8 + 4..8 + 8 + 6].copy_from_slice(&8u16.to_le_bytes());
        dir_data[8 + 8 + 6] = 2; // name_len
        dir_data[8 + 8 + 7] = 1; // file_type (目录)
        dir_data[8 + 8 + 8..8 + 8 + 10].copy_from_slice(b"..");

        // 保存目录块
        self.save_block(new_block, &dir_data)?;

        // 保存新 inode
        self.save_inode(new_inode_num, &new_inode)?;

        // 创建目录项
        self.create_dir_entry(parent_inode_num, name, new_inode_num, 1)?;

        Ok(new_inode_num)
    }

    /// 删除目录
    ///
    /// # Errors
    /// 当目录非空时返回 `Busy`;
    /// 当路径解析、目录项删除或底层 I/O 失败时返回对应的 `KernelError`.
    pub fn rmdir(&mut self, parent_path: &str, name: &str) -> Result<(), KernelError> {
        let parent_inode_num = self.lookup_path(parent_path)?;
        let dir_path = if parent_path == "/" {
            format!("/{name}")
        } else {
            format!("{parent_path}/{name}")
        };
        let dir_inode_num = self.lookup_path(&dir_path)?;

        // 检查目录是否为空
        let entries = self.read_dir(dir_inode_num)?;
        let valid_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.get_name() != "." && e.get_name() != "..")
            .collect();

        if !valid_entries.is_empty() {
            return Err(KernelError::Busy); // 目录非空
        }

        // 删除目录项
        self.remove_dir_entry(parent_inode_num, name)?;

        // 释放 inode 和块
        let dir_inode = self.read_inode(dir_inode_num)?;

        // 释放数据块
        for i in 0..12 {
            if dir_inode.i_block[i] != 0 {
                self.deallocate_block(dir_inode.i_block[i])?;
            }
        }

        // 释放 inode
        let group_idx =
            (dir_inode_num - self.super_block.first_inode()) / self.super_block.s_inodes_per_group;
        if (group_idx as usize) < self.block_groups.len() {
            super::alloc::free_inode(
                self.device_idx,
                &self.super_block,
                &self.block_groups[group_idx as usize],
                dir_inode_num,
            )?;
        }

        Ok(())
    }

    /// 创建符号链接
    ///
    /// # Errors
    /// 当父路径无效时返回底层 `KernelError`;
    /// 当同名链接已存在时返回 `AlreadyExists`.
    pub fn symlink(&mut self, target: &str, link_path: &str) -> Result<u32, KernelError> {
        // 解析路径
        let (parent_path, name) = link_path.rfind('/').map_or(("/", link_path), |pos| {
            if pos == 0 {
                ("/", &link_path[1..])
            } else {
                (&link_path[..pos], &link_path[pos + 1..])
            }
        });

        let parent_inode_num = self.lookup_path(parent_path)?;

        // 检查是否已存在
        let entries = self.read_dir(parent_inode_num)?;
        for entry in &entries {
            if entry.get_name() == name {
                return Err(KernelError::AlreadyExists);
            }
        }

        // 分配新 inode
        let new_inode_num = self.allocate_inode()?;

        let target_bytes = target.as_bytes();
        let target_len = target_bytes.len();

        let mut new_inode = Ext2Inode::default();
        new_inode.i_mode = 0xA000 | 0o777; // 符号链接，rwxrwxrwx
        new_inode.i_links_count = 1;
        new_inode.i_mtime = crate::arch!(timestamp()) as u32;
        new_inode.i_ctime = new_inode.i_mtime;

        if target_len <= 60 {
            // 小于 60 字节，直接存储在 i_block 中
            let mut block_data = [0u8; 60];
            block_data[..target_len].copy_from_slice(target_bytes);
            // 将 60 字节拆分为 15 个 u32
            for i in 0..15 {
                let offset = i * 4;
                new_inode.i_block[i] = u32::from_le_bytes([
                    block_data[offset],
                    block_data[offset + 1],
                    block_data[offset + 2],
                    block_data[offset + 3],
                ]);
            }
            new_inode.i_size = target_len as u32;
        } else {
            // 大于 60 字节，分配块存储
            let block = self.allocate_block()?;
            new_inode.i_block[0] = block;
            new_inode.i_size = target_len as u32;
            new_inode.i_blocks = (self.super_block.block_size() / 512) as u32;

            let mut block_data = alloc::vec![0u8; self.super_block.block_size() as usize];
            block_data[..target_len].copy_from_slice(target_bytes);
            self.save_block(block, &block_data)?;
        }

        // 保存 inode
        self.save_inode(new_inode_num, &new_inode)?;

        // 创建目录项
        self.create_dir_entry(parent_inode_num, name, new_inode_num, 3)?;

        Ok(new_inode_num)
    }

    /// 读取符号链接目标
    ///
    /// # Errors
    /// 当 inode 不是符号链接或目标块无效时返回 `InvalidArgument`;
    /// 当底层数据块读取失败时返回 `Io`.
    pub fn readlink(&mut self, inode_num: u32) -> Result<alloc::vec::Vec<u8>, KernelError> {
        let inode = self.read_inode(inode_num)?;

        if (inode.i_mode & 0xF000) != 0xA000 {
            return Err(KernelError::InvalidArgument);
        }

        let file_size = inode.i_size as usize;

        if file_size <= 60 {
            // 从 i_block 读取
            let mut data = alloc::vec![0u8; file_size];
            for i in 0..15 {
                let offset = i * 4;
                if offset + 4 <= file_size {
                    let bytes = inode.i_block[i].to_le_bytes();
                    let copy_len = (file_size - offset).min(4);
                    data[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
                }
            }
            Ok(data)
        } else {
            // 从块读取
            let block_num = inode.i_block[0];
            if block_num == 0 {
                return Err(KernelError::InvalidArgument);
            }

            let block_data = self.read_block(block_num)?;
            Ok(block_data[..file_size].to_vec())
        }
    }

    /// 创建硬链接
    ///
    /// # Errors
    /// 当目标或父路径无效时返回底层 `KernelError`;
    /// 当同名链接已存在时返回 `AlreadyExists`.
    pub fn link(&mut self, target_path: &str, link_path: &str) -> Result<u32, KernelError> {
        let target_inode_num = self.lookup_path(target_path)?;

        // 解析链接路径
        let (parent_path, name) = link_path.rfind('/').map_or(("/", link_path), |pos| {
            if pos == 0 {
                ("/", &link_path[1..])
            } else {
                (&link_path[..pos], &link_path[pos + 1..])
            }
        });

        let parent_inode_num = self.lookup_path(parent_path)?;

        // 检查是否已存在
        let entries = self.read_dir(parent_inode_num)?;
        for entry in &entries {
            if entry.get_name() == name {
                return Err(KernelError::AlreadyExists);
            }
        }

        // 读取目标 inode
        let mut target_inode = self.read_inode(target_inode_num)?;

        // 增加链接计数
        target_inode.i_links_count += 1;
        target_inode.i_ctime = crate::arch!(timestamp()) as u32;
        self.save_inode(target_inode_num, &target_inode)?;

        // 获取文件类型
        let file_type = target_inode.file_type();

        // 创建目录项
        self.create_dir_entry(parent_inode_num, name, target_inode_num, file_type)?;

        Ok(target_inode_num)
    }
}
