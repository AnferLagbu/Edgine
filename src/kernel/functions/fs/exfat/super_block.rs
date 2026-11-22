#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。
//! exFAT 超级块 (引导扇区) 数据结构与解析

/// exFAT 超级块 (磁盘偏移 0, 长度 512 字节)
///
/// 字段顺序与 exFAT 规范逐字节对齐, 可直接按 `repr(C)` 布局解读; 布局由
/// `offset_of!` 断言测试锁定 (禁止再出现"字段错位 2 字节"这类静默漂移).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExfatSuperBlock {
    /// 跳转引导指令 (首字节 0xEB / 0xE9)
    pub jump_boot: [u8; 3],
    /// 文件系统名 ("EXFAT" + 填充, 共 8 字节)
    pub fs_name: [u8; 8],
    /// 必须为零的保留区 (偏移 11..64)
    pub must_be_zero: [u8; 53],
    /// 分区起始偏移 (扇区)
    pub partition_offset: u64,
    /// 卷总扇区数
    pub volume_length: u64,
    /// FAT 起始扇区
    pub fat_offset: u32,
    /// FAT 长度 (扇区)
    pub fat_length: u32,
    /// 簇堆起始扇区
    pub cluster_heap_offset: u32,
    /// 簇总数
    pub cluster_count: u32,
    /// 根目录首簇号 (最小合法值 2)
    pub root_dir_first_cluster: u32,
    /// 卷序列号 (偏移 100)
    pub volume_serial: u32,
    /// 文件系统版本 (偏移 104, 如 0x0100 表示 1.00)
    pub fs_revision: u16,
    /// 卷标志 (偏移 106: 脏位 / 媒体故障位)
    pub volume_flags: u16,
    /// 每扇区字节数的移位量 (`log2`, 偏移 108, 合法 9..=12)
    pub bytes_per_sector_shift: u8,
    /// 每簇扇区数的移位量 (`log2`, 偏移 109)
    pub sectors_per_cluster_shift: u8,
    /// FAT 表数量 (偏移 110)
    pub num_fats: u8,
    /// 驱动器选择号 (偏移 111)
    pub drive_select: u8,
    percent_in_use: u8,
    /// 保留字段 (偏移 113..120)
    pub reserved: [u8; 7],
    /// 引导代码区 (偏移 120..510)
    pub boot_code: [u8; 390],
    /// 引导扇区签名 (0xAA55, 偏移 510)
    pub boot_signature: u16,
}

impl ExfatSuperBlock {
    /// 文件系统魔数 ("EXFAT")
    pub const MAGIC: [u8; 5] = *b"EXFAT";
    /// 引导扇区结束签名 (0xAA55)
    pub const BOOT_SIGNATURE: u16 = 0xAA55;
    /// 引导扇区字节长度
    pub const SIZE: usize = 512;

    /// 从字节切片解析超级块
    ///
    /// 拒绝以下形态: 长度不足 512、跳转码/魔数/签名不符、保留区非零、
    /// 扇区位移越界 (合法 9..=12)、簇位移使簇尺寸超过 32 GiB 上限、
    /// 簇数为 0、根目录簇号小于 2.
    #[must_use]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }

        // 跳转码
        if data[0] != 0xEB && data[0] != 0xE9 {
            return None;
        }

        // "EXFAT" 魔数 (偏移 3..8)
        if data[3..8] != Self::MAGIC {
            return None;
        }

        // 保留区必须为零 (偏移 11..64)
        if data[11..64].iter().any(|b| *b != 0) {
            return None;
        }

        // 引导扇区签名 (偏移 510)
        if data[510] != 0x55 || data[511] != 0xAA {
            return None;
        }

        // 扇区位移: 规范限定 9..=12 (512 B .. 4096 B)
        let bytes_per_sector_shift = data[108];
        if !(9..=12).contains(&bytes_per_sector_shift) {
            return None;
        }

        // 簇位移上界: 簇尺寸不得超过 2^25 字节
        let sectors_per_cluster_shift = data[109];
        if u32::from(sectors_per_cluster_shift)
            > 25u32.saturating_sub(u32::from(bytes_per_sector_shift))
        {
            return None;
        }

        let cluster_count = u32::from_le_bytes(data[92..96].try_into().ok()?);
        if cluster_count == 0 {
            return None;
        }

        let root_dir_first_cluster = u32::from_le_bytes(data[96..100].try_into().ok()?);
        if root_dir_first_cluster < 2 {
            return None;
        }

        Some(Self {
            jump_boot: [data[0], data[1], data[2]],
            fs_name: data[3..11].try_into().ok()?,
            must_be_zero: [0; 53],
            partition_offset: u64::from_le_bytes(data[64..72].try_into().ok()?),
            volume_length: u64::from_le_bytes(data[72..80].try_into().ok()?),
            fat_offset: u32::from_le_bytes(data[80..84].try_into().ok()?),
            fat_length: u32::from_le_bytes(data[84..88].try_into().ok()?),
            cluster_heap_offset: u32::from_le_bytes(data[88..92].try_into().ok()?),
            cluster_count,
            root_dir_first_cluster,
            volume_serial: u32::from_le_bytes(data[100..104].try_into().ok()?),
            fs_revision: u16::from_le_bytes(data[104..106].try_into().ok()?),
            volume_flags: u16::from_le_bytes(data[106..108].try_into().ok()?),
            bytes_per_sector_shift,
            sectors_per_cluster_shift,
            num_fats: data[110],
            drive_select: data[111],
            percent_in_use: data[112],
            reserved: data[113..120].try_into().ok()?,
            boot_code: data[120..510].try_into().ok()?,
            boot_signature: u16::from_le_bytes(data[510..512].try_into().ok()?),
        })
    }

    /// 每扇区字节数 (1 << `bytes_per_sector_shift`)
    #[must_use]
    pub fn bytes_per_sector(&self) -> u32 {
        1u32 << self.bytes_per_sector_shift
    }

    /// 每簇扇区数 (1 << `sectors_per_cluster_shift`)
    #[must_use]
    pub fn sectors_per_cluster(&self) -> u32 {
        1u32 << self.sectors_per_cluster_shift
    }

    /// 每簇字节数
    #[must_use]
    pub fn bytes_per_cluster(&self) -> u32 {
        self.bytes_per_sector() * self.sectors_per_cluster()
    }

    /// 数据区起始扇区
    #[must_use]
    pub fn data_start_sector(&self) -> u32 {
        self.cluster_heap_offset
    }

    /// 获取逻辑簇号对应的扇区号
    ///
    /// 调用方须保证 `cluster >= 2`; `from_bytes` 已校验根目录簇号, 目录项
    /// 簇号由 [`super::dir::ExfatDirEntry`] 解析后同样以 2 为下界.
    #[must_use]
    pub fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.cluster_heap_offset + (cluster - 2) * self.sectors_per_cluster()
    }
}
