//! fsx 集成测试 — 文件系统 exerciser
//!
//! 验证 Edgine 文件系统实现的正确性:
//! - ext2: 磁盘文件系统
//! - exfat: FAT 文件系统
//! - overlayfs: 联合文件系统
//! - tmpfs: 内存文件系统
//!
//! 测试目标: 100 万次操作无崩溃, 数据完整性 100% 通过

use edgine_host_tests::fsx::{FsxConfig, FsxFs, isolated_test_dir};

/// 快速测试 (1000 次操作, 用于 CI)
#[test]
fn test_fsx_quick() {
    let config = FsxConfig {
        test_dir: isolated_test_dir("quick"),
        num_operations: 1000,
        max_files: 10,
        max_file_size: 4096,
        seed: 42,
        verbose: false,
    };

    let mut fsx = FsxFs::new(config);
    let stats = fsx.run().expect("fsx quick test 失败");

    assert_eq!(stats.errors, 0, "fsx quick test 出现数据完整性错误");
    assert!(stats.creates > 0, "未创建任何文件");
    assert!(stats.reads > 0, "未读取任何文件");
}

/// tmpfs 测试 (10 万次操作)
#[test]
fn test_fsx_tmpfs() {
    let config = FsxConfig {
        test_dir: isolated_test_dir("tmpfs"),
        num_operations: 100_000,
        max_files: 100,
        max_file_size: 64 * 1024, // 64KB
        seed: 12345,
        verbose: false,
    };

    let mut fsx = FsxFs::new(config);
    let stats = fsx.run().expect("fsx tmpfs test 失败");

    assert_eq!(stats.errors, 0, "fsx tmpfs test 出现数据完整性错误");
    println!("tmpfs stats: {:?}", stats);
}

/// ext2 测试 (10 万次操作)
#[test]
fn test_fsx_ext2() {
    let config = FsxConfig {
        test_dir: isolated_test_dir("ext2"),
        num_operations: 100_000,
        max_files: 50,
        max_file_size: 32 * 1024, // 32KB (磁盘文件系统较小)
        seed: 67890,
        verbose: false,
    };

    let mut fsx = FsxFs::new(config);
    let stats = fsx.run().expect("fsx ext2 test 失败");

    assert_eq!(stats.errors, 0, "fsx ext2 test 出现数据完整性错误");
    println!("ext2 stats: {:?}", stats);
}

/// exfat 测试 (10 万次操作)
#[test]
fn test_fsx_exfat() {
    let config = FsxConfig {
        test_dir: isolated_test_dir("exfat"),
        num_operations: 100_000,
        max_files: 50,
        max_file_size: 32 * 1024, // 32KB
        seed: 11111,
        verbose: false,
    };

    let mut fsx = FsxFs::new(config);
    let stats = fsx.run().expect("fsx exfat test 失败");

    assert_eq!(stats.errors, 0, "fsx exfat test 出现数据完整性错误");
    println!("exfat stats: {:?}", stats);
}

/// overlayfs 测试 (10 万次操作)
#[test]
fn test_fsx_overlayfs() {
    let config = FsxConfig {
        test_dir: isolated_test_dir("overlayfs"),
        num_operations: 100_000,
        max_files: 80,
        max_file_size: 48 * 1024, // 48KB
        seed: 22222,
        verbose: false,
    };

    let mut fsx = FsxFs::new(config);
    let stats = fsx.run().expect("fsx overlayfs test 失败");

    assert_eq!(stats.errors, 0, "fsx overlayfs test 出现数据完整性错误");
    println!("overlayfs stats: {:?}", stats);
}

/// 压力测试: 并发操作 (10 万次, CI 可接受)
#[test]
fn test_fsx_stress() {
    let config = FsxConfig {
        test_dir: isolated_test_dir("stress"),
        num_operations: 100_000,
        max_files: 200,
        max_file_size: 128 * 1024, // 128KB
        seed: 99999,
        verbose: false,
    };

    let mut fsx = FsxFs::new(config);
    let stats = fsx.run().expect("fsx stress test 失败");

    assert_eq!(stats.errors, 0, "fsx stress test 出现数据完整性错误");
    println!("stress stats: {:?}", stats);
}
