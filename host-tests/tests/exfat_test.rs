//! exFAT 文件系统测试

use std::fs;
use std::path::Path;

#[test]
fn test_exfat_image_exists() {
    assert!(Path::new("exfat_test.img").exists(), "exFAT 测试镜像不存在");
}

#[test]
fn test_exfat_boot_signature() {
    let data = fs::read("exfat_test.img").unwrap();
    assert!(data.len() >= 512, "镜像太小");

    // 检查 boot signature
    assert_eq!(data[510], 0x55);
    assert_eq!(data[511], 0xAA);
}

#[test]
fn test_exfat_fs_name() {
    let data = fs::read("exfat_test.img").unwrap();
    let fs_name = &data[3..8];
    assert_eq!(fs_name, b"EXFAT");
}

#[test]
fn test_exfat_sector_size() {
    let data = fs::read("exfat_test.img").unwrap();
    let bytes_per_sector_shift = data[102];
    let sector_size = 1u32 << bytes_per_sector_shift.min(12); // 限制最大 12，防止溢出
    assert!((512..=4096).contains(&sector_size), "扇区大小无效");
}

#[test]
fn test_exfat_cluster_count() {
    let data = fs::read("exfat_test.img").unwrap();
    let cluster_count = u32::from_le_bytes([data[92], data[93], data[94], data[95]]);
    assert!(cluster_count > 0, "簇数量为 0");
}

#[test]
fn test_exfat_root_cluster() {
    let data = fs::read("exfat_test.img").unwrap();
    let root_cluster = u32::from_le_bytes([data[96], data[97], data[98], data[99]]);
    assert!(root_cluster >= 2, "根目录簇号无效");
}
