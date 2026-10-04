#![deny(unsafe_code)]
//! `UNKFS` ZIL Persistence Layer (WAL 磁盘持久化)
//!
//! 增强现有内存 ZIL:
//! 1. 每个记录附带 CRC32 校验和
//! 2. 设计 ZIL block 磁盘布局 (header + records)
//! 3. 崩溃恢复时扫描 ZIL blocks 回放未提交事务
//!
//! ## 磁盘布局
//!
//! ```text
//! ┌──────────────────────┐
//! │  ZilBlockHeader (64B)│
//! │  - magic: 0xZIL1     │
//! │  - txg: u64          │
//! │  - count: u16        │
//! │  - checksum: u32     │
//! │  - next_block: u64   │
//! ├──────────────────────┤
//! │  Record 0            │
//! │  Record 1            │
//! │  ...                 │
//! │  Record N-1          │
//! ├──────────────────────┤
//! │  ZilBlockTrailer (8B)│
//! │  - tail_magic: 0xEND │
//! │  - tail_checksum: u32│
//! └──────────────────────┘
//! ```

use super::spa::HV_POOL_BLOCK_SIZE;
use super::zil::{UnkfsZil, UnkfsZilRecord};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

/// P0-I-15 / J-04 (2026-09-08, G-09 长期最优): ZIL 持久化层错误类型.
///
/// 契约修正: 损坏块整体拒绝 (非"单条损坏跳过").
/// - 历史上 P0-I-15 曾承诺"损坏 record 跳过", 但三层 CRC (record ⊆ data ⊆ block)
///   结构性冗余使 record 级容错分支数学上不可达 (设计前提不成立);
/// - 现收敛为块级单一校验 (block CRC, ZFS 语义): 任何损坏 → 整块拒绝返回空.
/// - `try_deserialize_record` 的 Err (CRC/UnknownRecordType/BufferTooShort) 在
///   block CRC 通过后不可达, 保留为防御性解析校验 + 单元测试直接验证对象.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnkfsZilPersistError {
    /// 缓冲区长度不足, 不可能通过外部 `assert!(buf.len() >= 256)` 触发
    BufferTooShort { need: usize, got: usize },
    /// CRC32 校验失败 — 物理 bit rot 或上层写入错误
    CrcMismatch { expected: u32, computed: u32 },
    /// record 类型字段不在 enum 范围内 (1..=13)
    UnknownRecordType(u8),
    /// header / trailer magic 或 version 异常
    InvalidBlock,
}

impl UnkfsZilPersistError {
    pub fn as_static_str(&self) -> &'static str {
        match self {
            Self::BufferTooShort { .. } => "buffer_too_short",
            Self::CrcMismatch { .. } => "crc_mismatch",
            Self::UnknownRecordType(_) => "unknown_record_type",
            Self::InvalidBlock => "invalid_block",
        }
    }
}

const ZIL_MAGIC: u32 = 0x5A494C31u32; // "ZIL1"
const ZIL_TAIL_MAGIC: u32 = 0x454E4400u32; // "END\0"
const ZIL_BLOCK_SIZE: usize = HV_POOL_BLOCK_SIZE as usize;
const ZIL_HEADER_SIZE: usize = 64;
const ZIL_TRAILER_SIZE: usize = 8;
const ZIL_RECORD_DISK_SIZE: usize = 256;
const ZIL_MAX_RECORDS_PER_BLOCK: usize =
    (ZIL_BLOCK_SIZE - ZIL_HEADER_SIZE - ZIL_TRAILER_SIZE) / ZIL_RECORD_DISK_SIZE;

/// 每条记录实际写入磁盘的字节布局:
///    记录类型(1) + 事务组(8) + 对象 ID(8) + 父对象(8) + 偏移(8) + 大小(4)
///  + 序号(8) + 名称(128) + 数据哈希(32) + 记录 CRC(4) = 209
const ZIL_RECORD_PAYLOAD: usize = 209;
const _ASSERT_RECORD_FITS: () = assert!(ZIL_RECORD_PAYLOAD <= ZIL_RECORD_DISK_SIZE);

#[repr(C)]
#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct ZilBlockHeader {
    pub magic: u32,
    pub version: u16,
    pub flags: u16,
    pub txg: u64,
    pub seq_start: u64,
    pub record_count: u16,
    pub record_capacity: u16,
    pub total_size: u32,
    pub header_checksum: u32,
    /// J-04 (2026-09-08, G-09 长期最优): 已弃用 — 保持 0 (块级单一校验, block CRC
    /// 覆盖 record 区, data CRC 为结构性冗余). 字段保留仅因 #[repr(C)] 磁盘布局
    /// 兼容 (64B header, 落盘数据不受影响), 序列化不再写入、反序列化不再校验.
    pub data_checksum: u32,
    pub next_block: u64,
    pub padding: [u8; 16],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
pub struct ZilBlockTrailer {
    pub tail_magic: u32,
    pub block_checksum: u32,
}

const _ASSERT_HEADER_SIZE: () = {
    assert!(
        core::mem::size_of::<ZilBlockHeader>() == ZIL_HEADER_SIZE,
        "ZilBlockHeader size mismatch"
    );
};

const _ASSERT_TRAILER_SIZE: () = {
    assert!(
        core::mem::size_of::<ZilBlockTrailer>() == ZIL_TRAILER_SIZE,
        "ZilBlockTrailer size mismatch"
    );
};

impl ZilBlockHeader {
    pub fn new(txg: u64, seq_start: u64) -> Self {
        Self {
            magic: ZIL_MAGIC,
            version: 1,
            flags: 0,
            txg,
            seq_start,
            record_count: 0,
            record_capacity: ZIL_MAX_RECORDS_PER_BLOCK as u16,
            total_size: ZIL_BLOCK_SIZE as u32,
            header_checksum: 0,
            data_checksum: 0,
            next_block: 0,
            padding: [0; 16],
        }
    }

    pub fn is_valid(&self) -> bool {
        self.magic == ZIL_MAGIC && self.version >= 1
    }

    pub fn compute_header_checksum(&mut self) {
        self.header_checksum = 0;
        self.header_checksum = crc32_checksum(self.as_bytes());
    }

    pub fn verify_header(&self) -> bool {
        let mut copy = *self;
        copy.compute_header_checksum();
        copy.header_checksum == self.header_checksum
    }

    /// E6-6: 使用 `IntoBytes` + Immutable derive 编译期验证无 padding, `as_bytes` 为 safe 方法
    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }

    /// E6-6: safe 反序列化, 逐字段读取替代 unsafe 指针转换
    pub fn from_block(block: &[u8]) -> Option<Self> {
        let size = core::mem::size_of::<Self>();
        if block.len() < size {
            return None;
        }
        Some(Self {
            magic: u32::from_le_bytes(block[0..4].try_into().ok()?),
            version: u16::from_le_bytes(block[4..6].try_into().ok()?),
            flags: u16::from_le_bytes(block[6..8].try_into().ok()?),
            txg: u64::from_le_bytes(block[8..16].try_into().ok()?),
            seq_start: u64::from_le_bytes(block[16..24].try_into().ok()?),
            record_count: u16::from_le_bytes(block[24..26].try_into().ok()?),
            record_capacity: u16::from_le_bytes(block[26..28].try_into().ok()?),
            total_size: u32::from_le_bytes(block[28..32].try_into().ok()?),
            header_checksum: u32::from_le_bytes(block[32..36].try_into().ok()?),
            data_checksum: u32::from_le_bytes(block[36..40].try_into().ok()?),
            next_block: u64::from_le_bytes(block[40..48].try_into().ok()?),
            padding: {
                let mut p = [0u8; 16];
                p.copy_from_slice(&block[48..64]);
                p
            },
        })
    }
}

impl ZilBlockTrailer {
    pub fn new() -> Self {
        Self {
            tail_magic: ZIL_TAIL_MAGIC,
            block_checksum: 0,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn is_valid(&self) -> bool {
        self.tail_magic == ZIL_TAIL_MAGIC
    }

    /// E6-6: 使用 `IntoBytes` + Immutable derive 编译期验证无 padding, `as_bytes` 为 safe 方法
    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn crc32_checksum(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

fn serialize_record(record: &UnkfsZilRecord, buf: &mut [u8]) {
    debug_assert!(
        buf.len() >= ZIL_RECORD_PAYLOAD,
        "serialize_record buffer too small: {} < {}",
        buf.len(),
        ZIL_RECORD_PAYLOAD
    );
    buf[0] = record.rec_type as u8;
    buf[1..9].copy_from_slice(&record.txg.to_le_bytes());
    buf[9..17].copy_from_slice(&record.obj_id.to_le_bytes());
    buf[17..25].copy_from_slice(&record.parent_obj.to_le_bytes());
    buf[25..33].copy_from_slice(&record.offset.to_le_bytes());
    buf[33..37].copy_from_slice(&record.size.to_le_bytes());
    buf[37..45].copy_from_slice(&record.seq.to_le_bytes());
    buf[45..173].copy_from_slice(&record.name);
    // 将 [u64; 4] 安全地转换为字节序列
    for (i, val) in record.data_hash.iter().enumerate() {
        let off = 173 + i * 8;
        buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
    }
    let payload_end = 173 + 32;
    let rec_crc = crc32_checksum(&buf[..payload_end]);
    buf[payload_end..payload_end + 4].copy_from_slice(&rec_crc.to_le_bytes());
}

fn try_deserialize_record(buf: &[u8]) -> Result<UnkfsZilRecord, UnkfsZilPersistError> {
    let actual_size = 173 + 32 + 4;
    if buf.len() < actual_size {
        return Err(UnkfsZilPersistError::BufferTooShort {
            need: actual_size,
            got: buf.len(),
        });
    }
    let rec_crc =
        u32::from_le_bytes(buf[actual_size - 4..actual_size].try_into().map_err(|_| {
            UnkfsZilPersistError::BufferTooShort {
                need: 4,
                got: actual_size - 4,
            }
        })?);
    let computed = crc32_checksum(&buf[..actual_size - 4]);
    if rec_crc != computed {
        return Err(UnkfsZilPersistError::CrcMismatch {
            expected: rec_crc,
            computed,
        });
    }

    let rec_type = buf[0];
    let txg = u64::from_le_bytes(
        buf[1..9]
            .try_into()
            .map_err(|_| UnkfsZilPersistError::BufferTooShort { need: 8, got: 8 })?,
    );
    let obj_id = u64::from_le_bytes(
        buf[9..17]
            .try_into()
            .map_err(|_| UnkfsZilPersistError::BufferTooShort { need: 8, got: 8 })?,
    );
    let parent_obj = u64::from_le_bytes(
        buf[17..25]
            .try_into()
            .map_err(|_| UnkfsZilPersistError::BufferTooShort { need: 8, got: 8 })?,
    );
    let offset = u64::from_le_bytes(
        buf[25..33]
            .try_into()
            .map_err(|_| UnkfsZilPersistError::BufferTooShort { need: 8, got: 8 })?,
    );
    let size = u32::from_le_bytes(
        buf[33..37]
            .try_into()
            .map_err(|_| UnkfsZilPersistError::BufferTooShort { need: 4, got: 4 })?,
    );
    let seq = u64::from_le_bytes(
        buf[37..45]
            .try_into()
            .map_err(|_| UnkfsZilPersistError::BufferTooShort { need: 8, got: 8 })?,
    );

    let mut name = [0u8; 128];
    name.copy_from_slice(&buf[45..173]);

    let mut data_hash = [0u64; 4];
    for (i, val) in data_hash.iter_mut().enumerate() {
        let off = 173 + i * 8;
        *val = u64::from_le_bytes(
            buf[off..off + 8]
                .try_into()
                .map_err(|_| UnkfsZilPersistError::BufferTooShort { need: 8, got: 8 })?,
        );
    }

    let rec_type_enum = match rec_type {
        1 => super::zil::UnkfsZilRecordType::Create,
        2 => super::zil::UnkfsZilRecordType::Remove,
        3 => super::zil::UnkfsZilRecordType::Link,
        4 => super::zil::UnkfsZilRecordType::Rename,
        5 => super::zil::UnkfsZilRecordType::Write,
        6 => super::zil::UnkfsZilRecordType::Truncate,
        7 => super::zil::UnkfsZilRecordType::SetAttr,
        8 => super::zil::UnkfsZilRecordType::Acl,
        9 => super::zil::UnkfsZilRecordType::CreateAcl,
        10 => super::zil::UnkfsZilRecordType::Mkdir,
        11 => super::zil::UnkfsZilRecordType::Symlink,
        12 => super::zil::UnkfsZilRecordType::DedupRef,
        13 => super::zil::UnkfsZilRecordType::DedupUnref,
        other => return Err(UnkfsZilPersistError::UnknownRecordType(other)),
    };

    Ok(UnkfsZilRecord {
        rec_type: rec_type_enum,
        txg,
        obj_id,
        parent_obj,
        offset,
        size,
        name,
        data_hash,
        seq,
    })
}

pub struct UnkfsZilPersist {
    pub zil_blocks_written: AtomicBool,
}

// SAFETY (Framekernel P2.2.2): UnkfsZilPersist 全部字段 (AtomicBool) 自动 Send + Sync。

impl UnkfsZilPersist {
    pub const fn new() -> Self {
        Self {
            zil_blocks_written: AtomicBool::new(false),
        }
    }

    pub fn serialize_zil_to_block(zil: &UnkfsZil, txg: u64) -> Option<Vec<u8>> {
        let records = zil.records.lock();
        if records.is_empty() {
            return None;
        }

        let count = records.len().min(ZIL_MAX_RECORDS_PER_BLOCK);
        let mut block = vec![0u8; ZIL_BLOCK_SIZE];

        let mut header = ZilBlockHeader::new(txg, records[0].seq);
        header.record_count = count as u16;
        header.compute_header_checksum();

        let header_bytes = header.as_bytes();
        block[..ZIL_HEADER_SIZE].copy_from_slice(header_bytes);

        let record_area = ZIL_HEADER_SIZE;
        for i in 0..count {
            let offset = record_area + i * ZIL_RECORD_DISK_SIZE;
            serialize_record(
                &records[i],
                &mut block[offset..offset + ZIL_RECORD_DISK_SIZE],
            );
        }

        // J-04 (2026-09-08, G-09 长期最优): 块级单一校验 — 不再写入 data_checksum
        // (保持 0). 历史: B08-14 曾在此处计算 data_checksum 并二次重算
        // header_checksum (data_checksum 参与 header CRC). 现 block CRC (trailer)
        // 已完整覆盖 header + record 区, data CRC 为结构性冗余 (record ⊆ data ⊆
        // block 三层收敛为块级单一校验, ZFS 语义), 移除序列化侧计算后
        // header_checksum 仅需计算一次 (data_checksum 恒 0, 无二次重算需求).

        let trailer_offset = ZIL_BLOCK_SIZE - ZIL_TRAILER_SIZE;
        let trailer = ZilBlockTrailer::new();
        let trailer_bytes = trailer.as_bytes();
        block[trailer_offset..].copy_from_slice(trailer_bytes);

        let full_checksum = crc32_checksum(&block[..trailer_offset]);
        block[trailer_offset + 4..trailer_offset + 8].copy_from_slice(&full_checksum.to_le_bytes());

        Some(block)
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn deserialize_zil_from_block(block: &[u8]) -> Vec<UnkfsZilRecord> {
        let mut records = Vec::new();

        if block.len() < ZIL_BLOCK_SIZE {
            return records;
        }

        let header = match ZilBlockHeader::from_block(block) {
            Some(h) => h,
            None => return records,
        };

        if !header.is_valid() || !header.verify_header() {
            return records;
        }

        let trailer_offset = ZIL_BLOCK_SIZE - ZIL_TRAILER_SIZE;
        let tail_magic_raw = block[trailer_offset..trailer_offset + 4]
            .try_into()
            .ok()
            .map(u32::from_le_bytes);
        let block_checksum_raw = block[trailer_offset + 4..trailer_offset + 8]
            .try_into()
            .ok()
            .map(u32::from_le_bytes);
        let (tail_magic, block_checksum) = match (tail_magic_raw, block_checksum_raw) {
            (Some(m), Some(c)) => (m, c),
            _ => return records,
        };
        let trailer = ZilBlockTrailer {
            tail_magic,
            block_checksum,
        };

        if !trailer.is_valid() {
            return records;
        }

        // J-04 (2026-09-08, G-09 长期最优): 块级单一校验 — block CRC 是唯一完整性
        // 防线 (覆盖 header + record 区). 失败时 klog 记录期望 vs 实际 CRC (硬件
        // bit rot 可诊断). 数据完整性: 原 data CRC (header.data_checksum) 已被
        // block CRC 完全覆盖 (record ⊆ data ⊆ block 三层结构性冗余收敛), 移除.
        let computed = crc32_checksum(&block[..ZIL_BLOCK_SIZE - ZIL_TRAILER_SIZE]);
        if computed != trailer.block_checksum {
            crate::slog_warn!(
                FS,
                "ZIL 回放: 损坏 block 拒绝 (CRC 不匹配 expected={:#X} computed={:#X}, 覆盖 offset 0..{})",
                trailer.block_checksum,
                computed,
                ZIL_BLOCK_SIZE - ZIL_TRAILER_SIZE
            );
            return records;
        }

        let record_area = ZIL_HEADER_SIZE;
        for i in 0..header.record_count as usize {
            let offset = record_area + i * ZIL_RECORD_DISK_SIZE;
            // J-04 (2026-09-08, G-09 长期最优): 删除 record 级容错死代码 — block CRC
            // 通过后 record 解析必成功 (record CRC ⊆ block CRC 覆盖范围, 数学上不可达).
            // Err 为不可达防御: 若发生说明块级校验有盲区, 整个 block 拒绝 (损坏块
            // 拒绝契约), 而非静默跳过单条制造半持久化错觉.
            let Ok(record) = try_deserialize_record(&block[offset..offset + ZIL_RECORD_DISK_SIZE])
            else {
                #[cfg(debug_assertions)]
                crate::slog_warn!(FS, "ZIL 回放: record 解析失败 (index={}), 整块拒绝", i);
                return Vec::new();
            };
            records.push(record);
        }

        records.sort_by_key(|r| r.seq);
        records
    }

    pub fn mark_written(&self) {
        self.zil_blocks_written.store(true, Ordering::Release);
    }
}

/// 公开的 CRC32 包装, 用于单元测试
pub fn crc32_test_wrapper(data: &[u8]) -> u32 {
    crc32_checksum(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_deterministic() {
        let data = b"Hello, ZIL!";
        let c1 = crc32_checksum(data);
        let c2 = crc32_checksum(data);
        assert_eq!(c1, c2);
        assert_ne!(c1, 0);
    }

    #[test]
    fn test_serialize_roundtrip() {
        let rec = UnkfsZilRecord {
            rec_type: super::super::zil::UnkfsZilRecordType::Write,
            txg: 42,
            obj_id: 100,
            parent_obj: 0,
            offset: 4096,
            size: 512,
            name: [0; 128],
            data_hash: [1, 2, 3, 4],
            seq: 1,
        };

        let mut buf = [0u8; ZIL_RECORD_DISK_SIZE];
        serialize_record(&rec, &mut buf);

        // P0-I-15 修复: 改用 Result 返回, 成功路径在测试中可知, 使用 .expect()
        let deserialized = try_deserialize_record(&buf).expect("P0-I-15: roundtrip must succeed");
        assert_eq!(deserialized.txg, 42);
        assert_eq!(deserialized.obj_id, 100);
        assert_eq!(deserialized.offset, 4096);
        assert_eq!(deserialized.size, 512);
        assert_eq!(deserialized.seq, 1);
        assert_eq!(deserialized.data_hash, [1, 2, 3, 4]);
    }

    #[test]
    fn test_deserialize_corrupted_data() {
        let rec = UnkfsZilRecord {
            rec_type: super::super::zil::UnkfsZilRecordType::Create,
            txg: 1,
            obj_id: 0,
            parent_obj: 0,
            offset: 0,
            size: 0,
            name: [0; 128],
            data_hash: [0; 4],
            seq: 0,
        };

        let mut buf = [0u8; ZIL_RECORD_DISK_SIZE];
        serialize_record(&rec, &mut buf);

        buf[10] ^= 0xFF; // corrupt
        // P0-I-15 修复: 损坏 record 必须返回 CrcMismatch 而非 None
        let err = try_deserialize_record(&buf).expect_err("P0-I-15: corrupt must fail");
        assert!(matches!(err, UnkfsZilPersistError::CrcMismatch { .. }));
    }
}
