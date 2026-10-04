//! mount/umount2 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// mount
// ============================================================================

#[test]
fn test_mount_null_target_rejected() {
    assert_eq!(mount_target_validate(0, 0x1000, 0), Err(Errno::EFAULT));
}

#[test]
fn test_mount_null_fstype_rejected() {
    assert_eq!(mount_target_validate(0x1000, 0, 0), Err(Errno::EINVAL));
}

#[test]
fn test_mount_both_null_rejected() {
    assert_eq!(mount_target_validate(0, 0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_mount_valid() {
    // 无 source
    assert_eq!(mount_target_validate(0x1000, 0x2000, 0), Ok(()));
    // 有 source
    assert_eq!(mount_target_validate(0x1000, 0x2000, 0x3000), Ok(()));
}

// ============================================================================
// umount2
// ============================================================================

#[test]
fn test_umount2_null_target_rejected() {
    assert_eq!(umount2_target_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_umount2_valid() {
    assert_eq!(umount2_target_validate(0x1000), Ok(()));
    assert_eq!(umount2_target_validate(0xdead_beef), Ok(()));
}
