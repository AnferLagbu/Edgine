//! 测试夹具生成器: exFAT / ext2 最小磁盘镜像.
//!
//! 两个集成测试只断言引导扇区 / 超级块头部字段, 不需要完整磁盘镜像. 夹具在
//! 构建期恒生成到 `OUT_DIR`:
//!
//! - 不向仓库提交 11 MB 二进制 (旧 `exfat_test.img` 10 MB + `ext2_test.img` 1 MB);
//! - 所有机器与 CI 拿到同一份确定性字节, 避免"本机真镜像 / 新克隆生成镜像"
//!   两套输入导致同名测试验的不是同一份数据;
//! - 不依赖 mkfs 等宿主工具.
//!
//! 生成物只覆盖头部字段, **不是可挂载文件系统**. 测试若将来需要目录遍历,
//! 应改为经真实 mkfs 生成并纳入工具链前置检查, 而不是在此处补目录结构.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR 由 cargo 注入"));
    write(&out, "exfat_test.img", &exfat_image());
    write(&out, "ext2_test.img", &ext2_image());
}

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    fs::write(dir.join(name), bytes).unwrap_or_else(|e| panic!("写夹具 {name} 失败: {e}"));
}

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// exFAT 引导扇区 (4096 字节). 字段偏移按 exFAT 规范:
/// 102 属卷序列号, 扇区位移在 108, 簇位移在 109, FAT 数在 110.
fn exfat_image() -> Vec<u8> {
    let mut img = vec![0u8; 4096];
    img[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
    img[3..11].copy_from_slice(b"EXFAT   ");
    put_u64(&mut img, 64, 0); // PartitionOffset
    put_u64(&mut img, 72, 8); // VolumeLength (扇区数)
    put_u32(&mut img, 80, 1); // FatOffset
    put_u32(&mut img, 84, 1); // FatLength
    put_u32(&mut img, 88, 2); // ClusterHeapOffset
    put_u32(&mut img, 92, 4); // ClusterCount (>0)
    put_u32(&mut img, 96, 2); // FirstClusterOfRootDirectory (>=2)
    put_u32(&mut img, 100, 0x1234_5678); // VolumeSerialNumber
    put_u16(&mut img, 104, 0x0100); // FileSystemRevision 1.00
    img[108] = 9; // BytesPerSectorShift -> 512 字节/扇区
    img[109] = 0; // SectorsPerClusterShift -> 1 扇区/簇
    img[110] = 1; // NumberOfFats
    img[510] = 0x55; // BootSignature
    img[511] = 0xAA;
    img
}

/// ext2 最小镜像 (2048 字节 = 1 KiB 引导块 + 1 KiB 超级块).
///
/// `s_rev_level = 0` (`GOOD_OLD_REV`): 解析器按默认值补齐动态字段, 无需写 1 KiB
/// 以后的扩展区.
fn ext2_image() -> Vec<u8> {
    let mut img = vec![0u8; 2048];
    let sb = 1024;
    put_u32(&mut img, sb, 16); // s_inodes_count
    put_u32(&mut img, sb + 4, 8); // s_blocks_count
    put_u32(&mut img, sb + 24, 0); // s_log_block_size -> 1024
    put_u32(&mut img, sb + 76, 0); // s_rev_level
    put_u16(&mut img, sb + 56, 0xEF53); // s_magic
    img
}
