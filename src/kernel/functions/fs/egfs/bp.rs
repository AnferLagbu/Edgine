#![deny(unsafe_code)]
#![deny(missing_docs)]
/// 块指针中可容纳的 DVA 槽位数 (即镜像副本数上限).
pub const HV_DVA_MAX: usize = 2;
/// 块指针校验和的字节数 (4 个 64 位字).
pub const HV_BP_CHECKSUM_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
#[repr(C)]
/// 数据虚拟地址 (DVA): 定位一个块在某个 vdev 上的位置与长度.
pub struct EgfsDva {
    /// 在 vdev 内的字节偏移.
    pub offset: u64,
    /// 分配的实际字节数 (压缩/校验后的物理长度).
    pub asize: u32,
    /// 所属 vdev 的编号; 0 表示空 DVA.
    pub vdev_id: u16,
    /// 非零表示该 DVA 属于 gang 块.
    pub gang: u8,
    /// 对齐填充, 保持 16 字节布局.
    pub pad: [u8; 1],
}

impl EgfsDva {
    /// 序列化后的固定字节数.
    pub const BYTES: usize = core::mem::size_of::<Self>();

    /// 构造全零的空 DVA.
    #[must_use]
    pub const fn null() -> Self {
        Self {
            offset: 0,
            asize: 0,
            vdev_id: 0,
            gang: 0,
            pad: [0; 1],
        }
    }

    /// vdev 编号、偏移与长度均为 0 时视为空 DVA.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.vdev_id == 0 && self.offset == 0 && self.asize == 0
    }

    /// 以 vdev 编号、字节偏移与分配长度构造 DVA.
    #[must_use]
    pub fn new(vdev_id: u16, offset: u64, asize: u32) -> Self {
        Self {
            vdev_id,
            offset,
            asize,
            gang: 0,
            pad: [0; 1],
        }
    }

    /// 把本 DVA 标记为 gang 块成员并返回自身.
    #[must_use]
    pub fn with_gang(mut self) -> Self {
        self.gang = 1;
        self
    }

    /// 是否为 gang 块成员.
    #[must_use]
    pub fn is_gang(self) -> bool {
        self.gang != 0
    }

    /// E6-6: safe 反序列化
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::BYTES {
            return None;
        }
        Some(Self {
            offset: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            asize: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            vdev_id: u16::from_le_bytes(bytes[12..14].try_into().ok()?),
            gang: bytes[14],
            pad: [bytes[15]],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// 块的压缩算法类型.
pub enum EgfsCompType {
    /// 不压缩.
    Off = 0,
    /// LZ4 快速压缩.
    LZ4 = 1,
    /// Zstandard 压缩.
    Zstd = 2,
    /// gzip 最低压缩级别.
    Gzip1 = 3,
    /// gzip 最高压缩级别.
    Gzip9 = 4,
    /// 零长度编码 (整块全零时只存标记).
    Zle = 5,
}

impl EgfsCompType {
    /// 由原始类型值解析压缩算法; 未知值按不压缩处理.
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::LZ4,
            2 => Self::Zstd,
            3 => Self::Gzip1,
            4 => Self::Gzip9,
            5 => Self::Zle,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// 块的校验和算法类型.
pub enum EgfsCksumType {
    /// 不校验.
    Off = 0,
    /// Fletcher-2 校验.
    Fletcher2 = 1,
    /// Fletcher-4 校验.
    Fletcher4 = 2,
    /// SHA-256 校验.
    SHA256 = 3,
    /// Edon-R 校验.
    EdonR = 4,
}

impl EgfsCksumType {
    /// 由原始类型值解析校验算法; 未知值按不校验处理.
    #[must_use]
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
/// 块指针的属性区: 尺寸、层级、压缩/校验类型与加密标志.
pub struct EgfsBpProp {
    /// 逻辑数据字节数.
    pub logical_size: u32,
    /// 落盘后的物理字节数.
    pub physical_size: u32,
    /// 块树层级; 0 表示数据块, 大于 0 表示元数据块.
    pub level: u8,
    /// 压缩算法编码 (见 `EgfsCompType`).
    pub comp_type: u8,
    /// 校验算法编码 (见 `EgfsCksumType`).
    pub cksum_type: u8,
    /// 非零表示块已加密.
    pub encrypted: u8,
    /// 非零表示块采用反字节序.
    pub byteorder: u8,
    /// 对齐填充, 保持 16 字节布局.
    pub pad: [u8; 3],
}

impl EgfsBpProp {
    /// 序列化后的固定字节数.
    pub const BYTES: usize = core::mem::size_of::<Self>();

    /// 构造默认属性: 不压缩、Fletcher4 校验、未加密.
    #[must_use]
    pub const fn default() -> Self {
        Self {
            logical_size: 0,
            physical_size: 0,
            level: 0,
            comp_type: EgfsCompType::Off as u8,
            cksum_type: EgfsCksumType::Fletcher4 as u8,
            encrypted: 0,
            byteorder: 0,
            pad: [0; 3],
        }
    }

    /// 把压缩算法编码解析为枚举.
    #[must_use]
    pub fn comp_type(self) -> EgfsCompType {
        EgfsCompType::from_u8(self.comp_type)
    }

    /// 把校验算法编码解析为枚举.
    #[must_use]
    pub fn cksum_type(self) -> EgfsCksumType {
        EgfsCksumType::from_u8(self.cksum_type)
    }

    /// 设置压缩算法编码.
    pub fn set_comp_type(&mut self, v: EgfsCompType) {
        self.comp_type = v as u8;
    }

    /// 设置校验算法编码.
    pub fn set_cksum_type(&mut self, v: EgfsCksumType) {
        self.cksum_type = v as u8;
    }

    /// 块是否已加密.
    #[must_use]
    pub fn is_encrypted(self) -> bool {
        self.encrypted != 0
    }

    /// 设置加密标志.
    pub fn set_encrypted(&mut self, v: bool) {
        self.encrypted = u8::from(v);
    }

    /// E6-6: safe 反序列化
    #[must_use]
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
            pad: [bytes[13], bytes[14], bytes[15]],
        })
    }
}

#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
#[repr(C)]
/// 块指针: 最多两份 DVA、属性区、校验和与出生事务组.
pub struct EgfsBlockPointer {
    /// 数据虚拟地址槽位 (互为镜像副本).
    pub dva: [EgfsDva; HV_DVA_MAX],
    /// 块属性 (尺寸、层级、压缩与校验类型).
    pub prop: EgfsBpProp,
    /// 块内容的 4×64 位校验和.
    pub checksum: [u64; 4],
    /// 块写入时的事务组编号.
    pub birth_txg: u64,
    /// 块填充计数, 供校验与调试使用.
    pub fill: u64,
    /// 对齐填充, 保持固定布局.
    pub pad: [u64; 2],
}

impl EgfsBlockPointer {
    /// 构造全零的空块指针.
    #[must_use]
    pub const fn null() -> Self {
        Self {
            dva: [EgfsDva::null(); HV_DVA_MAX],
            prop: EgfsBpProp::default(),
            checksum: [0; 4],
            birth_txg: 0,
            fill: 0,
            pad: [0; 2],
        }
    }

    /// 两份 DVA 均为空时视为空指针.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.dva[0].is_null() && self.dva[1].is_null()
    }

    /// 无 DVA 且无出生事务组时表示稀疏空洞.
    #[must_use]
    pub fn is_hole(&self) -> bool {
        self.dva[0].is_null() && self.dva[1].is_null() && self.birth_txg == 0
    }

    /// 返回逻辑数据字节数.
    #[must_use]
    pub fn logical_size(&self) -> u32 {
        self.prop.logical_size
    }

    /// 返回落盘后的物理字节数.
    #[must_use]
    pub fn physical_size(&self) -> u32 {
        self.prop.physical_size
    }

    /// 取指定槽位的非空 DVA; 槽位越界或为空时返回 `None`.
    #[must_use]
    pub fn get_dva(&self, idx: usize) -> Option<&EgfsDva> {
        if idx < HV_DVA_MAX && !self.dva[idx].is_null() {
            Some(&self.dva[idx])
        } else {
            None
        }
    }

    /// 写入指定槽位的 DVA; 槽位越界时忽略.
    pub fn set_dva(&mut self, idx: usize, dva: EgfsDva) {
        if idx < HV_DVA_MAX {
            self.dva[idx] = dva;
        }
    }

    /// 设置块的出生事务组编号.
    pub fn set_birth(&mut self, txg: u64) {
        self.birth_txg = txg;
    }

    /// 层级为 0 且非空洞时表示数据块.
    #[must_use]
    pub fn is_data(&self) -> bool {
        self.prop.level == 0 && !self.is_hole()
    }

    /// 层级大于 0 且非空洞时表示元数据块.
    #[must_use]
    pub fn is_metadata(&self) -> bool {
        self.prop.level > 0 && !self.is_hole()
    }

    /// E6-6: 使用 `IntoBytes` + Immutable derive 编译期验证无 padding, `as_bytes` 为 safe 方法
    pub const BYTES: usize = core::mem::size_of::<Self>();

    /// 以字节切片视图暴露块指针的序列化内容.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }

    /// E6-6: safe 反序列化, 手动构建替代 unsafe `copy_nonoverlapping`
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::BYTES {
            return None;
        }
        // 安全方式: 逐字段读取, 避免任何 unsafe
        let mut bp = Self::null();
        let mut off = 0usize;
        for slot in &mut bp.dva {
            *slot = EgfsDva::from_bytes(&bytes[off..off + EgfsDva::BYTES])?;
            off += EgfsDva::BYTES;
        }
        bp.prop = EgfsBpProp::from_bytes(&bytes[off..off + EgfsBpProp::BYTES])?;
        off += EgfsBpProp::BYTES;
        // checksum, birth_txg, fill, pad 直接从字节切片读取
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
        let bp = EgfsBlockPointer::null();
        assert!(bp.is_null(), "null bp should be null");
        assert!(bp.get_dva(0).is_none(), "null bp dva should be None");
    }

    /// set_dva / get_dva 往返应保持一致.
    #[test]
    fn test_bp_dva_set_get() {
        let mut bp = EgfsBlockPointer::null();
        let dva = EgfsDva::new(0, 4096, 8192);
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
        let mut bp = EgfsBlockPointer::null();
        bp.set_birth(42);
        assert_eq!(bp.birth_txg, 42, "birth txg mismatch");
    }
}
