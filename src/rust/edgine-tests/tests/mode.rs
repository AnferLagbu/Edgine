//! mode (umask/chmod/fchmod/mkdir/rmdir) 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// umask
// ============================================================================

#[test]
fn test_umask_zero() {
    assert_eq!(umask_validate(0), Ok(()));
}

#[test]
fn test_umask_typical() {
    assert_eq!(umask_validate(0o022), Ok(()));
    assert_eq!(umask_validate(0o077), Ok(()));
    assert_eq!(umask_validate(0o007), Ok(()));
}

#[test]
fn test_umask_max() {
    assert_eq!(umask_validate(0o777), Ok(()));
}

#[test]
fn test_umask_overflow_rejected() {
    assert_eq!(umask_validate(0o1000), Err(Errno::EINVAL));
    assert_eq!(umask_validate(0o7777), Err(Errno::EINVAL));
    assert_eq!(umask_validate(u32::MAX), Err(Errno::EINVAL));
}

// ============================================================================
// chmod / fchmod
// ============================================================================

#[test]
fn test_chmod_mode_typical() {
    assert_eq!(chmod_mode_validate(0o644), Ok(()));
    assert_eq!(chmod_mode_validate(0o755), Ok(()));
    assert_eq!(chmod_mode_validate(0o777), Ok(()));
    assert_eq!(chmod_mode_validate(0o600), Ok(()));
}

#[test]
fn test_chmod_mode_with_special_bits() {
    // setuid/setgid/sticky
    assert_eq!(chmod_mode_validate(0o4755), Ok(()));
    assert_eq!(chmod_mode_validate(0o2755), Ok(()));
    assert_eq!(chmod_mode_validate(0o1755), Ok(()));
    assert_eq!(chmod_mode_validate(0o7777), Ok(()));
}

#[test]
fn test_chmod_mode_overflow_rejected() {
    assert_eq!(chmod_mode_validate(0o10000), Err(Errno::EINVAL));
    assert_eq!(chmod_mode_validate(u32::MAX), Err(Errno::EINVAL));
}

#[test]
fn test_fchmod_valid() {
    assert_eq!(fchmod_validate(0, 0o644), Ok(()));
    assert_eq!(fchmod_validate(3, 0o755), Ok(()));
    assert_eq!(fchmod_validate(100, 0o777), Ok(()));
}

#[test]
fn test_fchmod_negative_fd() {
    assert_eq!(fchmod_validate(-1, 0o644), Err(Errno::EBADF));
    assert_eq!(fchmod_validate(i32::MIN, 0o644), Err(Errno::EBADF));
}

#[test]
fn test_fchmod_mode_overflow() {
    assert_eq!(fchmod_validate(3, 0o10000), Err(Errno::EINVAL));
}

#[test]
fn test_fchmod_both_invalid() {
    // fd 错误优先 (顺序: fd<0 → EBADF)
    assert_eq!(fchmod_validate(-1, 0o10000), Err(Errno::EBADF));
}

// ============================================================================
// mkdir / rmdir
// ============================================================================

#[test]
fn test_mkdir_null_rejected() {
    assert_eq!(mkdir_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_mkdir_valid() {
    assert_eq!(mkdir_validate(0x1000), Ok(()));
    assert_eq!(mkdir_validate(0x7fff_ffff_e000), Ok(()));
}

#[test]
fn test_rmdir_null_rejected() {
    assert_eq!(rmdir_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_rmdir_valid() {
    assert_eq!(rmdir_validate(0x1000), Ok(()));
    assert_eq!(rmdir_validate(0x7fff_ffff_e000), Ok(()));
}
