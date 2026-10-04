//! UNKFS ZIL 序列化/回放集成测试
//!
//! 验证内核 `functions::fs/unkfs/zil_persist` 的序列化/反序列化契约:
//! 1. 合法 block 完整回放全部 record, 字段保真
//! 2. 损坏/截断/坏 magic block 被拒绝 (返回空, 不 panic)
//! 3. 空 ZIL 不产生 block
//!
//! ## B08-14 迁移 (2026-09-06)
//! 原文件为自包含 mini-persist 镜像 (B08-21 登记, 镜像内核 `try_deserialize_record`
//! / `deserialize_zil_from_block`), 含本地常量/序列化/CRC 复刻. 已删除, 改引内核
//! `functions::fs/unkfs/zil_persist` 真实实现 (host-test feature 暴露).
//!
//! ## 语义契约 (2026-09-08 J-04 更新, G-09 长期最优)
//! 原 P0-I-15 契约"单条 record 损坏 → 跳过返回其余"已修正为"损坏块拒绝":
//! 三层 CRC (record ⊆ data ⊆ block) 结构性冗余使 record 级容错分支数学上不可达,
//! 收敛为块级单一校验 (block CRC, ZFS 语义) — 任何损坏 → 整个 block 拒绝返回空.
//! 本文件断言与内核契约一致 (损坏 → 空, 不 panic). G-09 已随 J-04 修复关闭.

use edgine::kernel::functions::fs::unkfs::zil::{UnkfsZil, UnkfsZilRecord, UnkfsZilRecordType};
use edgine::kernel::functions::fs::unkfs::zil_persist::{UnkfsZilPersist, crc32_test_wrapper};

const REC_DISK_SIZE: usize = 256;
const HEADER_SIZE: usize = 64;

/// 用内核 serialize 构造含 n 条 write record 的合法 block.
fn build_block(n: usize, txg: u64) -> Vec<u8> {
    let zil = UnkfsZil::new();
    zil.init();
    for i in 0..n {
        zil.add_record(UnkfsZilRecord::new_write(
            txg,
            100 + i as u64,
            4096 * i as u64,
            512,
        ));
    }
    UnkfsZilPersist::serialize_zil_to_block(&zil, txg).expect("serialize 应成功")
}

#[test]
fn healthy_block_replays_all_records() {
    let block = build_block(3, 1);
    let result = UnkfsZilPersist::deserialize_zil_from_block(&block);
    assert_eq!(result.len(), 3, "合法 block 应回放全部 3 条");
    // 字段保真: obj_id / offset / rec_type
    assert_eq!(result[0].rec_type, UnkfsZilRecordType::Write);
    assert_eq!(result[0].obj_id, 100);
    assert_eq!(result[1].obj_id, 101);
    assert_eq!(result[0].offset, 0);
    assert_eq!(result[1].offset, 4096);
    assert!(result.iter().all(|r| r.seq > 0), "seq 应已分配递增");
}

#[test]
fn roundtrip_preserves_write_fields() {
    let block = build_block(2, 5);
    let result = UnkfsZilPersist::deserialize_zil_from_block(&block);
    assert_eq!(result.len(), 2);
    for (i, rec) in result.iter().enumerate() {
        assert_eq!(rec.txg, 5);
        assert_eq!(rec.obj_id, 100 + i as u64);
        assert_eq!(rec.size, 512);
    }
}

#[test]
fn single_corrupt_record_rejected_by_block_crc() {
    // 语义差异: 内核块级 data_crc 使单条损坏 → 整个 block 拒绝 (返回空).
    // 见文件头"语义差异登记".
    let mut block = build_block(2, 1);
    block[HEADER_SIZE + REC_DISK_SIZE + 10] ^= 0xFF; // 破坏第 2 条 record payload
    let result = UnkfsZilPersist::deserialize_zil_from_block(&block);
    assert!(result.is_empty(), "块级 data_crc 拒绝损坏 block → 空");
}

#[test]
fn all_records_corrupted_returns_empty_without_panic() {
    let mut block = build_block(4, 1);
    for i in 0..4 {
        block[HEADER_SIZE + i * REC_DISK_SIZE + 10] ^= 0xFF;
    }
    let result = UnkfsZilPersist::deserialize_zil_from_block(&block);
    assert!(result.is_empty(), "损坏 block → 空, 不 panic");
}

#[test]
fn truncated_block_is_rejected_silently() {
    let block = vec![0u8; 100]; // 远小于 ZIL_BLOCK_SIZE
    let result = UnkfsZilPersist::deserialize_zil_from_block(&block);
    assert!(result.is_empty(), "截断 block → 空, 不 panic");
}

#[test]
fn bad_magic_block_is_rejected_silently() {
    // 构造合法块后破坏 magic (offset 0)
    let mut block = build_block(1, 1);
    block[0..4].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
    let result = UnkfsZilPersist::deserialize_zil_from_block(&block);
    assert!(result.is_empty(), "错误 magic → 空");
}

#[test]
fn empty_zil_serializes_to_none() {
    let zil = UnkfsZil::new();
    zil.init();
    assert!(
        UnkfsZilPersist::serialize_zil_to_block(&zil, 1).is_none(),
        "空 ZIL 不应产生 block"
    );
}

#[test]
fn crc32_deterministic_and_sensitive() {
    let data = b"hello world";
    let c1 = crc32_test_wrapper(data);
    let c2 = crc32_test_wrapper(data);
    assert_eq!(c1, c2, "CRC 确定性");
    let mut altered = data.to_vec();
    altered[0] ^= 0x01;
    let c3 = crc32_test_wrapper(&altered);
    assert_ne!(c1, c3, "CRC 对单字节扰动敏感");
    // 空输入也确定
    assert_eq!(crc32_test_wrapper(b""), crc32_test_wrapper(b""));
}
