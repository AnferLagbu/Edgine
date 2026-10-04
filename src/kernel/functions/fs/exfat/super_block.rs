#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! exFAT 超级块数据结构

/// exFAT 超级块 (BPB + BPBX, 从偏移 0 开始)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExfatSuperBlock {
    // 引导扇区
    pub jump_boot: [u8; 3],
    pub fs_name: [u8; 8],
    pub must_be_zero: [u8; 53],
    pub partition_offset: u64,
    pub volume_length: u64,
    pub fat_offset: u32,
    pub fat_length: u32,
    pub cluster_heap_offset: u32,
    pub cluster_count: u32,
    pub root_dir_first_cluster: u32,
    pub volume_flags: u16,
    pub bytes_per_sector_shift: u8,
    pub sectors_per_cluster_shift: u8,
    pub num_fats: u8,
    pub drive_select: u8,
    percent_in_use: u8,
    pub reserved: [u8; 7],
    pub boot_code: [u8; 390],
    pub boot_signature: u16,
}

impl ExfatSuperBlock {
    pub const MAGIC: [u8; 5] = *b"EXFAT";
    pub const BOOT_SIGNATURE: u16 = 0xAA55;

    /// 从字节切片解析超级块
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 512 {
            return None;
        }

        // 检查跳转码
        if data[0] != 0xEB && data[0] != 0xE9 {
            return None;
        }

        // 检查 "EXFAT" 魔数
        let fs_name = &data[3..8];
        if fs_name != Self::MAGIC {
            return None;
        }

        // 检查 boot signature
        if data[510] != 0x55 || data[511] != 0xAA {
            return None;
        }

        // 手动解析超级块字段
        let sb = Self {
            jump_boot: [data[0], data[1], data[2]],
            fs_name: [
                data[3], data[4], data[5], data[6], data[7], data[8], data[9], data[10],
            ],
            must_be_zero: [0; 53],
            partition_offset: u64::from_le_bytes(data[64..72].try_into().ok()?),
            volume_length: u64::from_le_bytes(data[72..80].try_into().ok()?),
            fat_offset: u32::from_le_bytes(data[80..84].try_into().ok()?),
            fat_length: u32::from_le_bytes(data[84..88].try_into().ok()?),
            cluster_heap_offset: u32::from_le_bytes(data[88..92].try_into().ok()?),
            cluster_count: u32::from_le_bytes(data[92..96].try_into().ok()?),
            root_dir_first_cluster: u32::from_le_bytes(data[96..100].try_into().ok()?),
            volume_flags: u16::from_le_bytes(data[100..102].try_into().ok()?),
            bytes_per_sector_shift: data[102],
            sectors_per_cluster_shift: data[103],
            num_fats: data[104],
            drive_select: data[105],
            percent_in_use: data[106],
            reserved: [0; 7],
            boot_code: [0; 390],
            boot_signature: u16::from_le_bytes(data[510..512].try_into().ok()?),
        };

        Some(sb)
    }

    /// 每扇区字节数 (1 << `bytes_per_sector_shift`)
    pub fn bytes_per_sector(&self) -> u32 {
        1u32 << self.bytes_per_sector_shift
    }

    /// 每簇扇区数 (1 << `sectors_per_cluster_shift`)
    pub fn sectors_per_cluster(&self) -> u32 {
        1u32 << self.sectors_per_cluster_shift
    }

    /// 每簇字节数
    pub fn bytes_per_cluster(&self) -> u32 {
        self.bytes_per_sector() * self.sectors_per_cluster()
    }

    /// 数据区起始扇区
    pub fn data_start_sector(&self) -> u32 {
        self.cluster_heap_offset
    }

    /// 获取逻辑簇号对应的扇区号
    pub fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.cluster_heap_offset + (cluster - 2) * self.sectors_per_cluster()
    }
}
