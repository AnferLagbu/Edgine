#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 超级块数据结构

/// ext2 超级块 (磁盘偏移 1024 字节, 大小 1024 字节)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ext2SuperBlock {
    pub s_inodes_count: u32,
    pub s_blocks_count: u32,
    pub s_r_blocks_count: u32,
    pub s_free_blocks_count: u32,
    pub s_free_inodes_count: u32,
    pub s_first_data_block: u32,
    pub s_log_block_size: u32,
    pub s_log_frag_size: u32,
    pub s_blocks_per_group: u32,
    pub s_frags_per_group: u32,
    pub s_inodes_per_group: u32,
    pub s_mtime: u32,
    pub s_wtime: u32,
    pub s_mnt_count: u16,
    pub s_max_mnt_count: u16,
    pub s_magic: u16,
    pub s_state: u16,
    pub s_errors: u16,
    pub s_minor_rev_level: u16,
    pub s_lastcheck: u32,
    pub s_checkinterval: u32,
    pub s_creator_os: u32,
    pub s_rev_level: u32,
    pub s_def_resuid: u16,
    pub s_def_resgid: u16,
    // EXT2_DYNAMIC_REV
    pub s_first_ino: u32,
    pub s_inode_size: u16,
    pub s_block_group_nr: u16,
    pub s_feature_compat: u32,
    pub s_feature_incompat: u32,
    pub s_feature_ro_compat: u32,
    pub s_uuid: [u8; 16],
    pub s_volume_name: [u8; 16],
    pub s_last_mounted: [u8; 64],
    pub s_algo_bitmap: u32,
    // 性能调整
    pub s_prealloc_blocks: u8,
    pub s_prealloc_dir_blocks: u8,
    pub _padding: [u8; 2],
    // journaling
    pub s_journal_uuid: [u8; 16],
    pub s_journal_inum: u32,
    pub s_journal_dev: u32,
    pub s_last_orphan: u32,
    // 哈希种子
    pub s_hash_seed: [u32; 4],
    pub s_def_hash_version: u8,
    pub _padding2: [u8; 3],
    // 其他
    pub s_default_mount_opts: u32,
    pub s_first_meta_bg: u32,
    // 保留
    pub _reserved: [u8; 760],
}

impl Ext2SuperBlock {
    pub const MAGIC: u16 = 0xEF53;
    pub const EXT2_BASE_OFFSET: u64 = 1024;

    /// 从字节切片解析超级块
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 1024 {
            return None;
        }

        // 手动解析超级块字段
        let magic = u16::from_le_bytes([data[56], data[57]]);
        if magic != Self::MAGIC {
            return None;
        }

        let rev_level = u32::from_le_bytes([data[76], data[77], data[78], data[79]]);

        let mut uuid = [0u8; 16];
        let mut volume_name = [0u8; 16];
        let mut last_mounted = [0u8; 64];

        // EXT2_DYNAMIC_REV 字段
        if rev_level >= 1 {
            uuid.copy_from_slice(&data[104..120]);
            volume_name.copy_from_slice(&data[120..136]);
            last_mounted.copy_from_slice(&data[136..200]);
        }

        let sb = Self {
            s_inodes_count: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            s_blocks_count: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            s_r_blocks_count: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            s_free_blocks_count: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            s_free_inodes_count: u32::from_le_bytes([data[16], data[17], data[18], data[19]]),
            s_first_data_block: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            s_log_block_size: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
            s_log_frag_size: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            s_blocks_per_group: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            s_frags_per_group: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
            s_inodes_per_group: u32::from_le_bytes([data[40], data[41], data[42], data[43]]),
            s_mtime: u32::from_le_bytes([data[44], data[45], data[46], data[47]]),
            s_wtime: u32::from_le_bytes([data[48], data[49], data[50], data[51]]),
            s_mnt_count: u16::from_le_bytes([data[52], data[53]]),
            s_max_mnt_count: u16::from_le_bytes([data[54], data[55]]),
            s_magic: magic,
            s_state: u16::from_le_bytes([data[58], data[59]]),
            s_errors: u16::from_le_bytes([data[60], data[61]]),
            s_minor_rev_level: u16::from_le_bytes([data[62], data[63]]),
            s_lastcheck: u32::from_le_bytes([data[64], data[65], data[66], data[67]]),
            s_checkinterval: u32::from_le_bytes([data[68], data[69], data[70], data[71]]),
            s_creator_os: u32::from_le_bytes([data[72], data[73], data[74], data[75]]),
            s_rev_level: rev_level,
            s_def_resuid: u16::from_le_bytes([data[80], data[81]]),
            s_def_resgid: u16::from_le_bytes([data[82], data[83]]),
            s_first_ino: if rev_level >= 1 {
                u32::from_le_bytes([data[84], data[85], data[86], data[87]])
            } else {
                1
            },
            s_inode_size: if rev_level >= 1 {
                u16::from_le_bytes([data[88], data[89]])
            } else {
                128
            },
            s_block_group_nr: if rev_level >= 1 {
                u16::from_le_bytes([data[90], data[91]])
            } else {
                0
            },
            s_feature_compat: if rev_level >= 1 {
                u32::from_le_bytes([data[92], data[93], data[94], data[95]])
            } else {
                0
            },
            s_feature_incompat: if rev_level >= 1 {
                u32::from_le_bytes([data[96], data[97], data[98], data[99]])
            } else {
                0
            },
            s_feature_ro_compat: if rev_level >= 1 {
                u32::from_le_bytes([data[100], data[101], data[102], data[103]])
            } else {
                0
            },
            s_uuid: uuid,
            s_volume_name: volume_name,
            s_last_mounted: last_mounted,
            s_algo_bitmap: if rev_level >= 1 {
                u32::from_le_bytes([data[200], data[201], data[202], data[203]])
            } else {
                0
            },
            s_prealloc_blocks: 0,
            s_prealloc_dir_blocks: 0,
            _padding: [0; 2],
            s_journal_uuid: [0; 16],
            s_journal_inum: 0,
            s_journal_dev: 0,
            s_last_orphan: 0,
            s_hash_seed: [0; 4],
            s_def_hash_version: 0,
            _padding2: [0; 3],
            s_default_mount_opts: 0,
            s_first_meta_bg: 0,
            _reserved: [0; 760],
        };

        Some(sb)
    }

    /// 块大小 (字节)
    pub fn block_size(&self) -> u32 {
        1024u32 << self.s_log_block_size
    }

    /// inode 大小 (字节)
    pub fn inode_size(&self) -> u16 {
        if self.s_rev_level >= 1 {
            self.s_inode_size
        } else {
            128
        }
    }

    /// 第一个 inode
    pub fn first_inode(&self) -> u32 {
        if self.s_rev_level >= 1 {
            self.s_first_ino
        } else {
            1
        }
    }

    /// 块组描述符表起始块
    pub fn bgd_block(&self) -> u32 {
        if self.s_log_block_size == 0 {
            // 1KB 块: BGD 在第 2 块
            2
        } else {
            // >1KB 块: BGD 在第 1 块 (紧跟超级块)
            1
        }
    }

    /// 块组数量
    pub fn block_group_count(&self) -> u32 {
        (self.s_blocks_count + self.s_blocks_per_group - 1) / self.s_blocks_per_group
    }
}

impl Default for Ext2SuperBlock {
    fn default() -> Self {
        Self {
            s_inodes_count: 0,
            s_blocks_count: 0,
            s_r_blocks_count: 0,
            s_free_blocks_count: 0,
            s_free_inodes_count: 0,
            s_first_data_block: 0,
            s_log_block_size: 0,
            s_log_frag_size: 0,
            s_blocks_per_group: 0,
            s_frags_per_group: 0,
            s_inodes_per_group: 0,
            s_mtime: 0,
            s_wtime: 0,
            s_mnt_count: 0,
            s_max_mnt_count: 0,
            s_magic: 0,
            s_state: 0,
            s_errors: 0,
            s_minor_rev_level: 0,
            s_lastcheck: 0,
            s_checkinterval: 0,
            s_creator_os: 0,
            s_rev_level: 0,
            s_def_resuid: 0,
            s_def_resgid: 0,
            s_first_ino: 0,
            s_inode_size: 0,
            s_block_group_nr: 0,
            s_feature_compat: 0,
            s_feature_incompat: 0,
            s_feature_ro_compat: 0,
            s_uuid: [0; 16],
            s_volume_name: [0; 16],
            s_last_mounted: [0; 64],
            s_algo_bitmap: 0,
            s_prealloc_blocks: 0,
            s_prealloc_dir_blocks: 0,
            _padding: [0; 2],
            s_journal_uuid: [0; 16],
            s_journal_inum: 0,
            s_journal_dev: 0,
            s_last_orphan: 0,
            s_hash_seed: [0; 4],
            s_def_hash_version: 0,
            _padding2: [0; 3],
            s_default_mount_opts: 0,
            s_first_meta_bg: 0,
            _reserved: [0; 760],
        }
    }
}
