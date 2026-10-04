// SPDX-License-Identifier: MPL-2.0
//! functions/storage 参数验证单元测试

use edgine_tests::{
    disk_format_validate, disk_info_validate, disk_list_validate, disk_partition_validate, Errno,
};

// ============================================================================
// disk_list
// ============================================================================

#[test]
fn test_disk_list_null_ptr() {
    assert_eq!(disk_list_validate(0, 4), Err(Errno::EFAULT));
}

#[test]
fn test_disk_list_zero_count() {
    assert_eq!(disk_list_validate(0x1000, 0), Err(Errno::EINVAL));
}

#[test]
fn test_disk_list_valid() {
    assert_eq!(disk_list_validate(0x1000, 4), Ok(()));
    assert_eq!(disk_list_validate(0x1000, 1), Ok(()));
}

// ============================================================================
// disk_info
// ============================================================================

#[test]
fn test_disk_info_null_ptr() {
    assert_eq!(disk_info_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_disk_info_valid() {
    assert_eq!(disk_info_validate(0x2000), Ok(()));
}

// ============================================================================
// disk_format
// ============================================================================

#[test]
fn test_disk_format_null_ptr() {
    assert_eq!(disk_format_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_disk_format_valid() {
    assert_eq!(disk_format_validate(0x3000), Ok(()));
}

// ============================================================================
// disk_partition
// ============================================================================

#[test]
fn test_disk_partition_zero() {
    assert_eq!(disk_partition_validate(0), Err(Errno::EINVAL));
}

#[test]
fn test_disk_partition_overflow() {
    // 大于 u32::MAX 视为非法 (LBA 用 u32)
    assert_eq!(disk_partition_validate(u32::MAX as u64 + 1), Err(Errno::EINVAL));
    assert_eq!(disk_partition_validate(u64::MAX), Err(Errno::EINVAL));
}

#[test]
fn test_disk_partition_valid() {
    assert_eq!(disk_partition_validate(1), Ok(()));
    assert_eq!(disk_partition_validate(2048), Ok(()));
    assert_eq!(disk_partition_validate(u32::MAX as u64), Ok(()));
}
