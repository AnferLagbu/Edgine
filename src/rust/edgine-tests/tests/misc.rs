//! rename / sync / fsync / time 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// rename
// ============================================================================

#[test]
fn test_rename_null_oldpath_rejected() {
    assert_eq!(rename_path_validate(0, 0x1000), Err(Errno::EFAULT));
}

#[test]
fn test_rename_null_newpath_rejected() {
    assert_eq!(rename_path_validate(0x1000, 0), Err(Errno::EFAULT));
}

#[test]
fn test_rename_both_null_rejected() {
    assert_eq!(rename_path_validate(0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_rename_same_path_rejected() {
    assert_eq!(rename_path_validate(0x1000, 0x1000), Err(Errno::EINVAL));
}

#[test]
fn test_rename_valid() {
    assert_eq!(rename_path_validate(0x1000, 0x2000), Ok(()));
    assert_eq!(rename_path_validate(0xdead_beef, 0xcafe_babe), Ok(()));
}

// ============================================================================
// fsync
// ============================================================================

#[test]
fn test_fsync_negative_fd_rejected() {
    assert_eq!(fsync_fd_validate(-1), Err(Errno::EBADF));
    assert_eq!(fsync_fd_validate(i32::MIN), Err(Errno::EBADF));
}

#[test]
fn test_fsync_valid_fd() {
    assert_eq!(fsync_fd_validate(0), Ok(()));
    assert_eq!(fsync_fd_validate(255), Ok(()));
    assert_eq!(fsync_fd_validate(i32::MAX), Ok(()));
}

// ============================================================================
// time
// ============================================================================

#[test]
fn test_time_null_tloc_ok() {
    // tloc 可为 NULL
    assert_eq!(time_tloc_validate(0), Ok(()));
}

#[test]
fn test_time_unaligned_tloc_rejected() {
    assert_eq!(time_tloc_validate(0x1001), Err(Errno::EINVAL));
    assert_eq!(time_tloc_validate(0x1007), Err(Errno::EINVAL));
}

#[test]
fn test_time_aligned_tloc_ok() {
    assert_eq!(time_tloc_validate(0x1000), Ok(()));
    assert_eq!(time_tloc_validate(0x1008), Ok(()));
    assert_eq!(time_tloc_validate(0xdead_beef_dead_bee8), Ok(()));
}
