//! access/faccessat/unlink 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// access
// ============================================================================

#[test]
fn test_access_null_path_rejected() {
    assert_eq!(access_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_access_valid_path() {
    assert_eq!(access_validate(0x1000), Ok(()));
    assert_eq!(access_validate(0x7fff_ffff_ffff), Ok(()));
}

#[test]
fn test_access_mode_valid() {
    assert_eq!(access_mode_validate(F_OK), Ok(()));
    assert_eq!(access_mode_validate(R_OK), Ok(()));
    assert_eq!(access_mode_validate(W_OK), Ok(()));
    assert_eq!(access_mode_validate(X_OK), Ok(()));
    assert_eq!(access_mode_validate(0), Ok(()));
}

#[test]
fn test_access_mode_combined() {
    // R_OK | W_OK | X_OK
    assert_eq!(access_mode_validate(0o7), Ok(()));
    assert_eq!(access_mode_validate(0o5), Ok(())); // R+X
    assert_eq!(access_mode_validate(0o3), Ok(())); // W+X
}

#[test]
fn test_access_mode_negative_rejected() {
    assert_eq!(access_mode_validate(-1), Err(Errno::EINVAL));
    assert_eq!(access_mode_validate(i32::MIN), Err(Errno::EINVAL));
}

#[test]
fn test_access_mode_too_large_rejected() {
    assert_eq!(access_mode_validate(0o10), Err(Errno::EINVAL));
    assert_eq!(access_mode_validate(0o77), Err(Errno::EINVAL));
    assert_eq!(access_mode_validate(0o777), Err(Errno::EINVAL));
    assert_eq!(access_mode_validate(i32::MAX), Err(Errno::EINVAL));
}

// ============================================================================
// faccessat (mode 同 access)
// ============================================================================

#[test]
fn test_faccessat_mode_reuses_access_logic() {
    assert_eq!(access_mode_validate(0o7), Ok(()));
    assert_eq!(access_mode_validate(0o10), Err(Errno::EINVAL));
}

// ============================================================================
// unlink
// ============================================================================

#[test]
fn test_unlink_null_path_rejected() {
    assert_eq!(unlink_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_unlink_valid() {
    assert_eq!(unlink_validate(0x1000), Ok(()));
    assert_eq!(unlink_validate(0xdead_beef), Ok(()));
}
