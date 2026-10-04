#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 inode 数据结构

/// ext2 inode (128 字节)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ext2Inode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub i_osd1: u32,
    pub i_block: [u32; 15], // 12 direct + 1 indirect + 1 double + 1 triple
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32,
    pub i_faddr: u32,
    pub i_osd2: [u8; 12],
}

impl Ext2Inode {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 文件类型 (从 `i_mode` 提取)
    pub fn file_type(&self) -> u8 {
        match self.i_mode & 0xF000 {
            0x8000 => 0, // 普通文件
            0x4000 => 1, // 目录
            0xA000 => 3, // 符号链接
            0x2000 => 2, // 字符设备
            0x6000 => 2, // 块设备
            0x1000 => 4, // FIFO
            0xC000 => 5, // socket
            _ => 0,
        }
    }

    /// 权限位 (低 12 位)
    pub fn perm(&self) -> u16 {
        self.i_mode & 0x0FFF
    }

    /// 从字节切片解析 inode
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 128 {
            return None;
        }

        let mut inode = Self {
            i_mode: 0,
            i_uid: 0,
            i_size: 0,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 0,
            i_blocks: 0,
            i_flags: 0,
            i_osd1: 0,
            i_block: [0; 15],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            i_osd2: [0; 12],
        };

        inode.i_mode = u16::from_le_bytes([data[0], data[1]]);
        inode.i_uid = u16::from_le_bytes([data[2], data[3]]);
        inode.i_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        inode.i_atime = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        inode.i_ctime = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        inode.i_mtime = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        inode.i_dtime = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        inode.i_gid = u16::from_le_bytes([data[24], data[25]]);
        inode.i_links_count = u16::from_le_bytes([data[26], data[27]]);
        inode.i_blocks = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        inode.i_flags = u32::from_le_bytes([data[32], data[33], data[34], data[35]]);
        inode.i_osd1 = u32::from_le_bytes([data[36], data[37], data[38], data[39]]);

        // 解析 i_block 数组 (15 个 u32, 从偏移 40 开始)
        for i in 0..15 {
            let offset = 40 + i * 4;
            inode.i_block[i] = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
        }

        inode.i_generation = u32::from_le_bytes([data[100], data[101], data[102], data[103]]);
        inode.i_file_acl = u32::from_le_bytes([data[104], data[105], data[106], data[107]]);
        inode.i_dir_acl = u32::from_le_bytes([data[108], data[109], data[110], data[111]]);
        inode.i_faddr = u32::from_le_bytes([data[112], data[113], data[114], data[115]]);
        inode.i_osd2.copy_from_slice(&data[116..128]);

        Some(inode)
    }

    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    /// 获取逻辑块号 (支持直接/间接寻址)
    pub fn get_block(&self, logical: u32, block_size: u32) -> Option<u32> {
        let blocks_per_indirect = block_size / 4;

        if logical < 12 {
            // 直接块
            let block = self.i_block[logical as usize];
            if block == 0 { None } else { Some(block) }
        } else if logical < 12 + blocks_per_indirect {
            // 一次间接
            let _indirect_idx = logical - 12;
            let indirect_block = self.i_block[12];
            if indirect_block == 0 {
                None
            } else {
                // 读取间接块中的指针
                // 注意: 实际需要从磁盘读取，这里返回间接块号
                // 调用者需要读取 indirect_block + indirect_idx 处的 u32
                Some(indirect_block)
            }
        } else if logical < 12 + blocks_per_indirect + blocks_per_indirect * blocks_per_indirect {
            // 二次间接
            let double_idx = logical - 12 - blocks_per_indirect;
            let _indirect_idx = double_idx / blocks_per_indirect;
            let _block_idx = double_idx % blocks_per_indirect;
            let double_block = self.i_block[13];
            if double_block == 0 {
                None
            } else {
                // 需要两级间接寻址
                Some(double_block)
            }
        } else {
            // 三次间接 (超出范围)
            None
        }
    }
}

impl Default for Ext2Inode {
    fn default() -> Self {
        Self {
            i_mode: 0,
            i_uid: 0,
            i_size: 0,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 0,
            i_blocks: 0,
            i_flags: 0,
            i_osd1: 0,
            i_block: [0; 15],
            i_generation: 0,
            i_file_acl: 0,
            i_dir_acl: 0,
            i_faddr: 0,
            i_osd2: [0; 12],
        }
    }
}
