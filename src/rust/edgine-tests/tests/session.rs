//! 会话/进程组系统调用服务层参数验证测试
//!
//! 覆盖 services/proc/session.rs 的标量验证逻辑:
//! - setsid: 无参数, 总是返回成功
//! - getsid: pid >= 0
//! - setpgid: pid >= 0 && pgid >= 0

use edgine_tests::*;

// ============================================================================
// setsid
// ============================================================================

#[test]
fn test_setsid_always_ok() {
    // setsid 无参数, 简化实现始终返回成功
    assert_eq!(setsid_validate(), Ok(()));
}

// ============================================================================
// getsid
// ============================================================================

#[test]
fn test_getsid_zero_means_current() {
    // pid == 0 表示当前进程会话
    assert_eq!(getsid_validate(0), Ok(()));
}

#[test]
fn test_getsid_positive_pid() {
    assert_eq!(getsid_validate(1), Ok(()));
    assert_eq!(getsid_validate(100), Ok(()));
    assert_eq!(getsid_validate(i32::MAX), Ok(()));
}

#[test]
fn test_getsid_negative_rejected() {
    assert_eq!(getsid_validate(-1), Err(Errno::EINVAL));
    assert_eq!(getsid_validate(-100), Err(Errno::EINVAL));
    assert_eq!(getsid_validate(i32::MIN), Err(Errno::EINVAL));
}

// ============================================================================
// setpgid
// ============================================================================

#[test]
fn test_setpgid_valid_args() {
    // pid=0 pgid=0 表示当前进程加入当前组 (POSIX 合法)
    assert_eq!(setpgid_validate(0, 0), Ok(()));
    // 正数
    assert_eq!(setpgid_validate(1, 1), Ok(()));
    assert_eq!(setpgid_validate(100, 50), Ok(()));
    assert_eq!(setpgid_validate(i32::MAX, i32::MAX), Ok(()));
}

#[test]
fn test_setpgid_negative_pid_rejected() {
    assert_eq!(setpgid_validate(-1, 0), Err(Errno::EINVAL));
    assert_eq!(setpgid_validate(-100, 5), Err(Errno::EINVAL));
    assert_eq!(setpgid_validate(i32::MIN, 0), Err(Errno::EINVAL));
}

#[test]
fn test_setpgid_negative_pgid_rejected() {
    assert_eq!(setpgid_validate(0, -1), Err(Errno::EINVAL));
    assert_eq!(setpgid_validate(5, -100), Err(Errno::EINVAL));
    assert_eq!(setpgid_validate(0, i32::MIN), Err(Errno::EINVAL));
}

#[test]
fn test_setpgid_both_negative_rejected() {
    assert_eq!(setpgid_validate(-1, -1), Err(Errno::EINVAL));
    assert_eq!(setpgid_validate(-100, -100), Err(Errno::EINVAL));
}
