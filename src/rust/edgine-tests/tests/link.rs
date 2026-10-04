//! link/symlink/readlink 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// link
// ============================================================================

#[test]
fn test_link_null_oldpath_rejected() {
    assert_eq!(link_path_validate(0, 0x1000), Err(Errno::EFAULT));
}

#[test]
fn test_link_null_newpath_rejected() {
    assert_eq!(link_path_validate(0x1000, 0), Err(Errno::EFAULT));
}

#[test]
fn test_link_both_null_rejected() {
    assert_eq!(link_path_validate(0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_link_valid() {
    assert_eq!(link_path_validate(0x1000, 0x2000), Ok(()));
    assert_eq!(link_path_validate(0xdead_beef, 0xcafe_babe), Ok(()));
}

// ============================================================================
// symlink
// ============================================================================

#[test]
fn test_symlink_null_target_rejected() {
    assert_eq!(symlink_path_validate(0, 0x1000), Err(Errno::EFAULT));
}

#[test]
fn test_symlink_null_linkpath_rejected() {
    assert_eq!(symlink_path_validate(0x1000, 0), Err(Errno::EFAULT));
}

#[test]
fn test_symlink_both_null_rejected() {
    assert_eq!(symlink_path_validate(0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_symlink_valid() {
    assert_eq!(symlink_path_validate(0x1000, 0x2000), Ok(()));
}

// ============================================================================
// readlink
// ============================================================================

#[test]
fn test_readlink_null_path_rejected() {
    assert_eq!(readlink_validate(0, 0x1000, 64), Err(Errno::EFAULT));
}

#[test]
fn test_readlink_null_buf_rejected() {
    assert_eq!(readlink_validate(0x1000, 0, 64), Err(Errno::EFAULT));
}

#[test]
fn test_readlink_bufsiz_zero_rejected() {
    assert_eq!(readlink_validate(0x1000, 0x2000, 0), Err(Errno::EINVAL));
}

#[test]
fn test_readlink_valid() {
    assert_eq!(readlink_validate(0x1000, 0x2000, 64), Ok(()));
    assert_eq!(readlink_validate(0x1000, 0x2000, 4096), Ok(()));
    assert_eq!(readlink_validate(0x1000, 0x2000, 1), Ok(()));
}
