#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 位图操作 (只读: 仅查询)

/// ext2 位图 (只读)
pub struct Ext2Bitmap {
    data: alloc::vec::Vec<u8>,
}

impl Ext2Bitmap {
    /// 从字节切片创建位图
    pub fn from_bytes(data: &[u8]) -> Self {
        Self {
            data: data.to_vec(),
        }
    }

    /// 检查指定位是否已设置
    pub fn is_set(&self, bit: u32) -> bool {
        let byte_idx = (bit / 8) as usize;
        let bit_idx = (bit % 8) as usize;

        if byte_idx >= self.data.len() {
            return false;
        }

        (self.data[byte_idx] & (1 << bit_idx)) != 0
    }

    /// 统计已设置的位数
    pub fn count_used(&self) -> u32 {
        let mut count = 0;
        for byte in &self.data {
            count += byte.count_ones();
        }
        count
    }

    /// 统计未设置的位数
    pub fn count_free(&self, total: u32) -> u32 {
        total - self.count_used()
    }
}
