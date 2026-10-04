//! fchown / times / getitimer / setitimer / alarm 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// fchown
// ============================================================================

#[test]
fn test_fchown_negative_fd_rejected() {
    assert_eq!(fchown_fd_validate(-1), Err(Errno::EBADF));
    assert_eq!(fchown_fd_validate(i32::MIN), Err(Errno::EBADF));
}

#[test]
fn test_fchown_valid_fd() {
    assert_eq!(fchown_fd_validate(0), Ok(()));
    assert_eq!(fchown_fd_validate(255), Ok(()));
    assert_eq!(fchown_fd_validate(i32::MAX), Ok(()));
}

// ============================================================================
// times
// ============================================================================

#[test]
fn test_times_null_buf_ok() {
    assert_eq!(times_buf_validate(0), Ok(()));
}

#[test]
fn test_times_unaligned_buf_rejected() {
    assert_eq!(times_buf_validate(0x1001), Err(Errno::EINVAL));
    assert_eq!(times_buf_validate(0x1003), Err(Errno::EINVAL));
}

#[test]
fn test_times_aligned_buf_ok() {
    assert_eq!(times_buf_validate(0x1000), Ok(()));
    assert_eq!(times_buf_validate(0x1004), Ok(()));
    assert_eq!(times_buf_validate(0xdead_beef_dead_beec), Ok(()));
}

// ============================================================================
// getitimer
// ============================================================================

#[test]
fn test_getitimer_invalid_which_rejected() {
    assert_eq!(itimer_which_validate(-1), Err(Errno::EINVAL));
    assert_eq!(itimer_which_validate(4), Err(Errno::EINVAL));
    assert_eq!(itimer_which_validate(i32::MAX), Err(Errno::EINVAL));
}

#[test]
fn test_getitimer_valid_which() {
    assert_eq!(itimer_which_validate(0), Ok(()));
    assert_eq!(itimer_which_validate(1), Ok(()));
    assert_eq!(itimer_which_validate(2), Ok(()));
    assert_eq!(itimer_which_validate(3), Ok(()));
}

#[test]
fn test_getitimer_null_value_rejected() {
    assert_eq!(getitimer_value_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_getitimer_valid_value() {
    assert_eq!(getitimer_value_validate(0x1000), Ok(()));
}

// ============================================================================
// setitimer
// ============================================================================

#[test]
fn test_setitimer_invalid_which_rejected() {
    assert_eq!(itimer_which_validate(-1), Err(Errno::EINVAL));
    assert_eq!(itimer_which_validate(99), Err(Errno::EINVAL));
}

#[test]
fn test_setitimer_null_new_rejected() {
    assert_eq!(setitimer_new_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_setitimer_valid_new() {
    assert_eq!(setitimer_new_validate(0x1000), Ok(()));
}

// ============================================================================
// alarm
// ============================================================================

#[test]
fn test_alarm_any_value_ok() {
    assert_eq!(alarm_seconds_ok(0), Ok(()));
    assert_eq!(alarm_seconds_ok(60), Ok(()));
    assert_eq!(alarm_seconds_ok(u32::MAX), Ok(()));
}
