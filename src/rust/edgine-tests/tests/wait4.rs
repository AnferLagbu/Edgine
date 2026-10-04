//! wait4 服务层参数验证测试
//!
//! 覆盖 functions/proc/wait4.rs 的纯标量验证逻辑:
//! - pid 范围合法 (-32768..=32767)
//! - options 标志组合 (WNOHANG | WUNTRACED | WCONTINUED)

use edgine_tests::*;

#[test]
fn test_wait4_pid_specific() {
    // pid > 0: 等待特定子进程
    assert_eq!(wait4_validate(1, 0), Ok(()));
    assert_eq!(wait4_validate(1234, 0), Ok(()));
    assert_eq!(wait4_validate(0x7FFF, 0), Ok(())); // PID_MAX
}

#[test]
fn test_wait4_pid_any() {
    // pid == -1: 等待任意子进程 (POSIX wait())
    assert_eq!(wait4_validate(-1, 0), Ok(()));
}

#[test]
fn test_wait4_pid_group() {
    // pid == 0: 同进程组
    assert_eq!(wait4_validate(0, 0), Ok(()));
    // pid < -1: 进程组 |pid|
    assert_eq!(wait4_validate(-100, 0), Ok(()));
    assert_eq!(wait4_validate(-0x8000, 0), Ok(())); // PID_MIN
}

#[test]
fn test_wait4_pid_out_of_range() {
    // pid > PID_MAX
    assert_eq!(wait4_validate(0x8000, 0), Err(Errno::EINVAL));
    assert_eq!(wait4_validate(i32::MAX, 0), Err(Errno::EINVAL));
    // pid < PID_MIN
    assert_eq!(wait4_validate(-0x8001, 0), Err(Errno::EINVAL));
    assert_eq!(wait4_validate(i32::MIN, 0), Err(Errno::EINVAL));
}

#[test]
fn test_wait4_options_wnohang() {
    // WNOHANG: 非阻塞
    assert_eq!(wait4_validate(-1, WNOHANG), Ok(()));
    assert_eq!(wait4_validate(0, WNOHANG), Ok(()));
    assert_eq!(wait4_validate(123, WNOHANG), Ok(()));
}

#[test]
fn test_wait4_options_combinations() {
    // 合法组合
    assert_eq!(wait4_validate(-1, WNOHANG | WUNTRACED), Ok(()));
    assert_eq!(wait4_validate(-1, WNOHANG | WCONTINUED), Ok(()));
    assert_eq!(wait4_validate(-1, WUNTRACED | WCONTINUED), Ok(()));
    assert_eq!(wait4_validate(-1, WNOHANG | WUNTRACED | WCONTINUED), Ok(()));
}

#[test]
fn test_wait4_options_invalid_flags_rejected() {
    // 非法 options 标志
    assert_eq!(wait4_validate(-1, 0x10), Err(Errno::EINVAL));
    assert_eq!(wait4_validate(-1, 0x100), Err(Errno::EINVAL));
    assert_eq!(wait4_validate(-1, 0xFF), Err(Errno::EINVAL));
}

#[test]
fn test_wait4_zero_options_blocking() {
    // options = 0: 阻塞等待
    assert_eq!(wait4_validate(-1, 0), Ok(()));
}

#[test]
fn test_wait4_typical_user_scenarios() {
    // POSIX wait(): 等待任意子进程
    assert_eq!(wait4_validate(-1, 0), Ok(()));

    // 等待特定子进程, 非阻塞 (用于轮询)
    assert_eq!(wait4_validate(42, WNOHANG), Ok(()));

    // 等待进程组, 阻塞
    assert_eq!(wait4_validate(0, 0), Ok(()));

    // WUNTRACED (调试器场景)
    assert_eq!(wait4_validate(42, WUNTRACED), Ok(()));

    // 完整 WCONTINUED (SIGCONT 报告)
    assert_eq!(wait4_validate(42, WUNTRACED | WCONTINUED), Ok(()));
}

#[test]
fn test_wait4_boundary_pid_values() {
    // 边界值
    assert_eq!(wait4_validate(0, 0), Ok(()));            // 同进程组
    assert_eq!(wait4_validate(-1, 0), Ok(()));           // 任意
    assert_eq!(wait4_validate(0x7FFF, 0), Ok(()));       // PID_MAX
    assert_eq!(wait4_validate(-0x8000, 0), Ok(()));      // PID_MIN

    // 越界
    assert_eq!(wait4_validate(0x8000, 0), Err(Errno::EINVAL));
    assert_eq!(wait4_validate(-0x8001, 0), Err(Errno::EINVAL));
}
