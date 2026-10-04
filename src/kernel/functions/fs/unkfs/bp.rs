#![deny(unsafe_code)]
pub const HV_DVA_MAX: usize = 2;
pub const HV_BP_CHECKSUM_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
#[repr(C)]
pub struct UnkfsDva {
    pub offset: u64,
    pub asize: u32,
    pub vdev_id: u16,
    pub gang: u8,
    pub _pad: [u8; 1],
}

impl UnkfsDva {
    pub const BYTES: usize = core::mem::size_of::<Self>();

    pub const fn null() -> Self {
        Self {
            offset: 0,
            asize: 0,
            vdev_id: 0,
            gang: 0,
            _pad: [0; 1],
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_null(&self) -> bool {
        self.vdev_id == 0 && self.offset == 0 && self.asize == 0
    }

    pub fn new(vdev_id: u16, offset: u64, asize: u32) -> Self {
        Self {
            vdev_id,
            offset,
            asize,
            gang: 0,
            _pad: [0; 1],
        }
    }

    #[expect(
        clippy::return_self_not_must_use,
        reason = "return_self_not_must_use: 返回 Self 是 builder/fluent API; 当前优先 expect"
    )]
    pub fn with_gang(mut self) -> Self {
        self.gang = 1;
        self
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_gang(&self) -> bool {
        self.gang != 0
    }

    /// E6-6: safe 反序列化
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::BYTES {
            return None;
        }
        Some(Self {
            offset: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            asize: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            vdev_id: u16::from_le_bytes(bytes[12..14].try_into().ok()?),
            gang: bytes[14],
            _pad: [bytes[15]],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(clippy::upper_case_acronyms)] // ZSTD/ZLE/LZ4 压缩算法名
pub enum UnkfsCompType {
    Off = 0,
    LZ4 = 1,
    ZSTD = 2,
    Gzip1 = 3,
    Gzip9 = 4,
    ZLE = 5,
}

impl UnkfsCompType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::LZ4,
            2 => Self::ZSTD,
            3 => Self::Gzip1,
            4 => Self::Gzip9,
            5 => Self::ZLE,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnkfsCksumType {
    Off = 0,
    Fletcher2 = 1,
    Fletcher4 = 2,
    SHA256 = 3,
    EdonR = 4,
}

impl UnkfsCksumType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Fletcher2,
            2 => Self::Fletcher4,
            3 => Self::SHA256,
            4 => Self::EdonR,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
#[repr(C)]
pub struct UnkfsBpProp {
    pub logical_size: u32,
    pub physical_size: u32,
    pub level: u8,
    pub comp_type: u8,
    pub cksum_type: u8,
    pub encrypted: u8,
    pub byteorder: u8,
    pub _pad: [u8; 3],
}

impl UnkfsBpProp {
    pub const BYTES: usize = core::mem::size_of::<Self>();

    pub const fn default() -> Self {
        Self {
            logical_size: 0,
            physical_size: 0,
            level: 0,
            comp_type: UnkfsCompType::Off as u8,
            cksum_type: UnkfsCksumType::Fletcher4 as u8,
            encrypted: 0,
            byteorder: 0,
            _pad: [0; 3],
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn comp_type(&self) -> UnkfsCompType {
        UnkfsCompType::from_u8(self.comp_type)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn cksum_type(&self) -> UnkfsCksumType {
        UnkfsCksumType::from_u8(self.cksum_type)
    }

    pub fn set_comp_type(&mut self, v: UnkfsCompType) {
        self.comp_type = v as u8;
    }

    pub fn set_cksum_type(&mut self, v: UnkfsCksumType) {
        self.cksum_type = v as u8;
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_encrypted(&self) -> bool {
        self.encrypted != 0
    }

    pub fn set_encrypted(&mut self, v: bool) {
        self.encrypted = u8::from(v);
    }

    /// E6-6: safe 反序列化
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::BYTES {
            return None;
        }
        Some(Self {
            logical_size: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            physical_size: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            level: bytes[8],
            comp_type: bytes[9],
            cksum_type: bytes[10],
            encrypted: bytes[11],
            byteorder: bytes[12],
            _pad: [bytes[13], bytes[14], bytes[15]],
        })
    }
}

#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
#[repr(C)]
pub struct UnkfsBlockPointer {
    pub dva: [UnkfsDva; HV_DVA_MAX],
    pub prop: UnkfsBpProp,
    pub checksum: [u64; 4],
    pub birth_txg: u64,
    pub fill: u64,
    pub _pad: [u64; 2],
}

impl UnkfsBlockPointer {
    pub const fn null() -> Self {
        Self {
            dva: [UnkfsDva::null(); HV_DVA_MAX],
            prop: UnkfsBpProp::default(),
            checksum: [0; 4],
            birth_txg: 0,
            fill: 0,
            _pad: [0; 2],
        }
    }

    pub fn is_null(&self) -> bool {
        self.dva[0].is_null() && self.dva[1].is_null()
    }

    pub fn is_hole(&self) -> bool {
        self.dva[0].is_null() && self.dva[1].is_null() && self.birth_txg == 0
    }

    pub fn logical_size(&self) -> u32 {
        self.prop.logical_size
    }

    pub fn physical_size(&self) -> u32 {
        self.prop.physical_size
    }

    pub fn get_dva(&self, idx: usize) -> Option<&UnkfsDva> {
        if idx < HV_DVA_MAX && !self.dva[idx].is_null() {
            Some(&self.dva[idx])
        } else {
            None
        }
    }

    pub fn set_dva(&mut self, idx: usize, dva: UnkfsDva) {
        if idx < HV_DVA_MAX {
            self.dva[idx] = dva;
        }
    }

    pub fn set_birth(&mut self, txg: u64) {
        self.birth_txg = txg;
    }

    pub fn is_data(&self) -> bool {
        self.prop.level == 0 && !self.is_hole()
    }

    pub fn is_metadata(&self) -> bool {
        self.prop.level > 0 && !self.is_hole()
    }

    /// E6-6: 使用 `IntoBytes` + Immutable derive 编译期验证无 padding, `as_bytes` 为 safe 方法
    pub const BYTES: usize = core::mem::size_of::<Self>();

    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }

    /// E6-6: safe 反序列化, 手动构建替代 unsafe `copy_nonoverlapping`
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::BYTES {
            return None;
        }
        // 安全方式: 逐字段读取, 避免任何 unsafe
        let mut bp = Self::null();
        let mut off = 0usize;
        for i in 0..HV_DVA_MAX {
            bp.dva[i] = UnkfsDva::from_bytes(&bytes[off..off + UnkfsDva::BYTES])?;
            off += UnkfsDva::BYTES;
        }
        bp.prop = UnkfsBpProp::from_bytes(&bytes[off..off + UnkfsBpProp::BYTES])?;
        off += UnkfsBpProp::BYTES;
        // checksum, birth_txg, fill, _pad 直接从字节切片读取
        for i in 0..4 {
            bp.checksum[i] =
                u64::from_le_bytes(bytes[off + i * 8..off + i * 8 + 8].try_into().ok()?);
        }
        off += 32;
        bp.birth_txg = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        off += 8;
        bp.fill = u64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        Some(bp)
    }
}

// DECISION-080: 块指针纯逻辑断言以本文件源侧 #[cfg(test)] 为唯一归属
// (host 可编译, 不再占用 kernel_test 注册表轨).
#[cfg(test)]
mod tests {
    use super::*;

    // ==== 块指针基础语义 ====

    /// 空块指针应为 null, 且无有效 DVA.
    #[test]
    fn test_bp_null() {
        let bp = UnkfsBlockPointer::null();
        assert!(bp.is_null(), "null bp should be null");
        assert!(bp.get_dva(0).is_none(), "null bp dva should be None");
    }

    /// set_dva / get_dva 往返应保持一致.
    #[test]
    fn test_bp_dva_set_get() {
        let mut bp = UnkfsBlockPointer::null();
        let dva = UnkfsDva::new(0, 4096, 8192);
        bp.set_dva(0, dva);
        let Some(got) = bp.get_dva(0) else {
            panic!("dva not set");
        };
        assert_eq!(got.vdev_id, 0, "vdev_id mismatch");
        assert_eq!(got.offset, 4096, "offset mismatch");
        assert_eq!(got.asize, 8192, "asize mismatch");
    }

    /// set_birth 应写入 birth_txg.
    #[test]
    fn test_bp_birth_txg() {
        let mut bp = UnkfsBlockPointer::null();
        bp.set_birth(42);
        assert_eq!(bp.birth_txg, 42, "birth txg mismatch");
    }
}
