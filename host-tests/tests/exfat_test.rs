//! exFAT 文件系统测试
//!
//! 夹具由 `build.rs` 恒生成到 `OUT_DIR` (确定性最小镜像, 不入仓); 断言走
//! 内核真实解析器 `ExfatSuperBlock::from_bytes`, 而非只读原始字节.

use std::fs;
use std::path::Path;

use edgine::kernel::functions::fs::exfat::super_block::ExfatSuperBlock;

/// 夹具路径 (构建期生成)
fn fixture() -> &'static str {
    concat!(env!("OUT_DIR"), "/exfat_test.img")
}

#[test]
fn test_exfat_image_exists() {
    assert!(Path::new(fixture()).exists(), "exFAT 测试镜像不存在");
}

/// 夹具必须能被内核真实解析器接受, 且各字段落在规范偏移上.
#[test]
fn test_exfat_superblock_parses_via_kernel() {
    let data = fs::read(fixture()).unwrap();
    let sb = ExfatSuperBlock::from_bytes(&data).expect("引导扇区应可解析");

    assert_eq!(&sb.fs_name[..5], &ExfatSuperBlock::MAGIC);
    assert_eq!(sb.boot_signature, ExfatSuperBlock::BOOT_SIGNATURE);
    assert_eq!(sb.volume_serial, 0x1234_5678);
    assert_eq!(sb.fs_revision, 0x0100);
    assert_eq!(sb.volume_flags, 0, "卷标志位于偏移 106");
    assert_eq!(sb.bytes_per_sector_shift, 9, "扇区位移位于偏移 108");
    assert_eq!(sb.sectors_per_cluster_shift, 0);
    assert_eq!(sb.num_fats, 1);
    assert_eq!(sb.cluster_count, 4);
    assert_eq!(sb.root_dir_first_cluster, 2);
    assert_eq!(sb.bytes_per_sector(), 512);
    assert_eq!(sb.bytes_per_cluster(), 512);
    assert_eq!(sb.data_start_sector(), 2);
    assert_eq!(sb.cluster_to_sector(2), 2);
}

/// `repr(C)` 布局必须与磁盘规范逐字节对齐 (防止字段再漂移).
#[test]
fn test_exfat_superblock_layout_matches_disk_spec() {
    use core::mem::{offset_of, size_of};

    assert_eq!(size_of::<ExfatSuperBlock>(), ExfatSuperBlock::SIZE);
    assert_eq!(
        offset_of!(ExfatSuperBlock, volume_serial),
        100,
        "卷序列号偏移"
    );
    assert_eq!(
        offset_of!(ExfatSuperBlock, volume_flags),
        106,
        "卷标志偏移 (旧实现误置于 100)"
    );
    assert_eq!(
        offset_of!(ExfatSuperBlock, bytes_per_sector_shift),
        108,
        "扇区位移偏移 (旧实现误置于 102)"
    );
    assert_eq!(offset_of!(ExfatSuperBlock, boot_code), 120, "引导代码偏移");
    assert_eq!(offset_of!(ExfatSuperBlock, boot_signature), 510, "签名偏移");
}

/// 非法形态必须返回 `None`, 不得把畸形引导扇区当成合法卷.
#[test]
fn test_exfat_superblock_rejects_malformed() {
    let base = fs::read(fixture()).unwrap();

    let mut bad_name = base.clone();
    bad_name[3..8].copy_from_slice(b"NTFS!");
    assert!(
        ExfatSuperBlock::from_bytes(&bad_name).is_none(),
        "非 EXFAT 魔数应拒绝"
    );

    let mut bad_signature = base.clone();
    bad_signature[510] = 0;
    assert!(
        ExfatSuperBlock::from_bytes(&bad_signature).is_none(),
        "签名缺失应拒绝"
    );

    let mut bad_shift = base.clone();
    bad_shift[108] = 8;
    assert!(
        ExfatSuperBlock::from_bytes(&bad_shift).is_none(),
        "扇区位移 8 (256 B) 越界应拒绝"
    );

    let mut bad_root = base;
    bad_root[96..100].copy_from_slice(&1u32.to_le_bytes());
    assert!(
        ExfatSuperBlock::from_bytes(&bad_root).is_none(),
        "根目录簇号 < 2 应拒绝"
    );
}
