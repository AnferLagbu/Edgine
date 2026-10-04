// SPDX-License-Identifier: MPL-2.0
//! functions/mm/mremap 参数验证单元测试

use edgine_tests::{
    mremap_validate, mremap_validate_flags, Errno, MREMAP_MAYMOVE,
};

// ============================================================================
// mremap 入参
// ============================================================================

#[test]
fn test_mremap_zero_addr() {
    assert_eq!(mremap_validate(0, 4096, 8192), Err(Errno::EINVAL));
}

#[test]
fn test_mremap_zero_old_size() {
    assert_eq!(mremap_validate(0x1000, 0, 8192), Err(Errno::EINVAL));
}

#[test]
fn test_mremap_zero_new_size() {
    assert_eq!(mremap_validate(0x1000, 4096, 0), Err(Errno::EINVAL));
}

#[test]
fn test_mremap_unaligned_addr() {
    // old_addr 必须页对齐
    assert_eq!(mremap_validate(0x1001, 4096, 8192), Err(Errno::EINVAL));
    assert_eq!(mremap_validate(0xFFF, 4096, 8192), Err(Errno::EINVAL));
}

#[test]
fn test_mremap_valid() {
    assert_eq!(mremap_validate(0x1000, 4096, 8192), Ok(()));
    assert_eq!(mremap_validate(0x7FFF_F000, 4096, 4096), Ok(()));
    assert_eq!(mremap_validate(0x1000, 0x100000, 0x200000), Ok(()));
}

// ============================================================================
// mremap flags
// ============================================================================

#[test]
fn test_mremap_flags_maymove() {
    assert_eq!(mremap_validate_flags(0), Ok(()));
    assert_eq!(mremap_validate_flags(MREMAP_MAYMOVE), Ok(()));
}

#[test]
fn test_mremap_flags_fixed_rejected() {
    // MREMAP_FIXED=2 不支持
    assert_eq!(mremap_validate_flags(2), Err(Errno::EINVAL));
    assert_eq!(mremap_validate_flags(MREMAP_MAYMOVE | 2), Err(Errno::EINVAL));
    assert_eq!(mremap_validate_flags(0xFF), Err(Errno::EINVAL));
}
