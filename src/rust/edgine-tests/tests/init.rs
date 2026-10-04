// SPDX-License-Identifier: MPL-2.0
//! init 启动子系统单元测试 (host 模拟)

use edgine_tests::{
    init_status_after_launch, INIT_STATUS_LOADING, INIT_STATUS_NOT_STARTED,
    INIT_STATUS_RUNNING, INIT_STATUS_UNPACKING,
};

// ============================================================================
// init 启动状态常量
// ============================================================================

#[test]
fn test_init_status_constants_distinct() {
    assert_ne!(INIT_STATUS_NOT_STARTED, INIT_STATUS_UNPACKING);
    assert_ne!(INIT_STATUS_UNPACKING, INIT_STATUS_LOADING);
    assert_ne!(INIT_STATUS_LOADING, INIT_STATUS_RUNNING);
    assert_ne!(INIT_STATUS_NOT_STARTED, INIT_STATUS_RUNNING);
}

#[test]
fn test_init_status_progress_order() {
    // 启动流程是单调上升: 0 < 1 < 2 < 3
    assert!(INIT_STATUS_NOT_STARTED < INIT_STATUS_UNPACKING);
    assert!(INIT_STATUS_UNPACKING < INIT_STATUS_LOADING);
    assert!(INIT_STATUS_LOADING < INIT_STATUS_RUNNING);
}

#[test]
fn test_init_status_initial() {
    // 初始状态应为 0 (未启动)
    assert_eq!(INIT_STATUS_NOT_STARTED, 0);
    assert_eq!(INIT_STATUS_RUNNING, 3);
}

#[test]
fn test_init_status_after_launch_running() {
    // 模拟 launch 完成后 status=3
    assert_eq!(init_status_after_launch(), INIT_STATUS_RUNNING);
}

// ============================================================================
// cpio newc 头格式常量验证
// ============================================================================

/// cpio newc magic 头
const CPIO_NEWC_MAGIC: &[u8; 6] = b"070701";

#[test]
fn test_cpio_newc_magic() {
    assert_eq!(CPIO_NEWC_MAGIC, b"070701");
}

#[test]
fn test_cpio_newc_filetype_bits() {
    // cpio newc mode 高 16 位, 文件类型掩码 0o170000
    const CPIO_S_IFMT: u32 = 0o170000;
    const CPIO_S_IFDIR: u32 = 0o040000;
    const CPIO_S_IFREG: u32 = 0o100000;
    const CPIO_S_IFLNK: u32 = 0o120000;
    assert_eq!(CPIO_S_IFMT & CPIO_S_IFDIR, CPIO_S_IFDIR);
    assert_eq!(CPIO_S_IFMT & CPIO_S_IFREG, CPIO_S_IFREG);
    assert_eq!(CPIO_S_IFMT & CPIO_S_IFLNK, CPIO_S_IFLNK);
}
