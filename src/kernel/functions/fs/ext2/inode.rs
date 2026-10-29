#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 inode 数据结构

/// ext2 inode (128 字节)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
#[derive(Default)]
pub struct Ext2Inode {
    /// 文件类型与权限位
    pub mode: u16,
    /// 所有者 UID
    pub uid: u16,
    /// 文件大小 (字节, 低 32 位)
    pub size: u32,
    /// 最后访问时间 (Unix 时间戳)
    pub atime: u32,
    /// inode 变更时间 (Unix 时间戳)
    pub ctime: u32,
    /// 最后修改时间 (Unix 时间戳)
    pub mtime: u32,
    /// 删除时间 (0 = 未删除)
    pub dtime: u32,
    /// 所属组 GID
    pub gid: u16,
    /// 硬链接数
    pub links_count: u16,
    /// 占用块数 (512 字节为单位)
    pub blocks: u32,
    /// 文件标志位
    pub flags: u32,
    /// 操作系统保留字段 1
    pub osd1: u32,
    /// 块指针数组 (12 直接 + 1 一次间接 + 1 二次间接 + 1 三次间接)
    pub block: [u32; 15], // 12 direct + 1 indirect + 1 double + 1 triple
    /// 文件版本号 (NFS 使用)
    pub generation: u32,
    /// 扩展属性块号
    pub file_acl: u32,
    /// 目录 ACL 块号 (高 32 位大小)
    pub dir_acl: u32,
    /// 碎片地址
    pub faddr: u32,
    /// 操作系统保留字段 2
    pub osd2: [u8; 12],
}

impl Ext2Inode {
    /// 文件类型 (从 `mode` 提取)
    #[must_use]
    pub fn file_type(&self) -> u8 {
        match self.mode & 0xF000 {
            0x4000 => 1,          // 目录
            0xA000 => 3,          // 符号链接
            0x2000 | 0x6000 => 2, // 字符设备 / 块设备
            0x1000 => 4,          // FIFO
            0xC000 => 5,          // socket
            _ => 0,               // 普通文件 (0x8000) 及未知类型
        }
    }

    /// 权限位 (低 12 位)
    #[must_use]
    pub fn perm(&self) -> u16 {
        self.mode & 0x0FFF
    }

    /// 从字节切片解析 inode
    #[must_use]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 128 {
            return None;
        }

        let mut inode = Self {
            mode: 0,
            uid: 0,
            size: 0,
            atime: 0,
            ctime: 0,
            mtime: 0,
            dtime: 0,
            gid: 0,
            links_count: 0,
            blocks: 0,
            flags: 0,
            osd1: 0,
            block: [0; 15],
            generation: 0,
            file_acl: 0,
            dir_acl: 0,
            faddr: 0,
            osd2: [0; 12],
        };

        inode.mode = u16::from_le_bytes([data[0], data[1]]);
        inode.uid = u16::from_le_bytes([data[2], data[3]]);
        inode.size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        inode.atime = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        inode.ctime = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        inode.mtime = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        inode.dtime = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        inode.gid = u16::from_le_bytes([data[24], data[25]]);
        inode.links_count = u16::from_le_bytes([data[26], data[27]]);
        inode.blocks = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        inode.flags = u32::from_le_bytes([data[32], data[33], data[34], data[35]]);
        inode.osd1 = u32::from_le_bytes([data[36], data[37], data[38], data[39]]);

        // 解析 block 数组 (15 个 u32, 从偏移 40 开始)
        for (i, slot) in inode.block.iter_mut().enumerate() {
            let offset = 40 + i * 4;
            *slot = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
        }

        inode.generation = u32::from_le_bytes([data[100], data[101], data[102], data[103]]);
        inode.file_acl = u32::from_le_bytes([data[104], data[105], data[106], data[107]]);
        inode.dir_acl = u32::from_le_bytes([data[108], data[109], data[110], data[111]]);
        inode.faddr = u32::from_le_bytes([data[112], data[113], data[114], data[115]]);
        inode.osd2.copy_from_slice(&data[116..128]);

        Some(inode)
    }

    /// 获取逻辑块号 (支持直接/间接寻址)
    #[must_use]
    pub fn get_block(&self, logical: u32, block_size: u32) -> Option<u32> {
        let blocks_per_indirect = block_size / 4;

        if logical < 12 {
            // 直接块
            let block = self.block[logical as usize];
            if block == 0 { None } else { Some(block) }
        } else if logical < 12 + blocks_per_indirect {
            // 一次间接
            let indirect_block = self.block[12];
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
            let double_block = self.block[13];
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
