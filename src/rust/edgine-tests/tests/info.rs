//! 信息查询系统调用服务层参数验证测试
//!
//! 覆盖 services/proc/info.rs 的纯标量验证逻辑:
//! - getpgid: pid >= 0
//! - uname: buf != 0
//! - gettimeofday: tv != 0

use edgine_tests::*;

// ============================================================================
// getpgid
// ============================================================================

#[test]
fn test_getpgid_zero_means_current() {
    // pid == 0 表示当前进程
    assert_eq!(getpgid_validate(0), Ok(()));
}

#[test]
fn test_getpgid_positive_pid() {
    // pid > 0 查找特定进程
    assert_eq!(getpgid_validate(1), Ok(()));
    assert_eq!(getpgid_validate(100), Ok(()));
    assert_eq!(getpgid_validate(0x7FFF), Ok(()));
    assert_eq!(getpgid_validate(i32::MAX), Ok(()));
}

#[test]
fn test_getpgid_negative_rejected() {
    // pid < 0 非法
    assert_eq!(getpgid_validate(-1), Err(Errno::EINVAL));
    assert_eq!(getpgid_validate(-100), Err(Errno::EINVAL));
    assert_eq!(getpgid_validate(i32::MIN), Err(Errno::EINVAL));
}

// ============================================================================
// uname
// ============================================================================

#[test]
fn test_uname_null_buf_rejected() {
    // NULL 缓冲区
    assert_eq!(uname_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_uname_valid_buf() {
    // 合法用户空间地址
    assert_eq!(uname_validate(0x7fff_ffff_f000), Ok(()));
    assert_eq!(uname_validate(0x1000), Ok(()));
    assert_eq!(uname_validate(0x100), Ok(()));
}

#[test]
fn test_uname_kernel_address() {
    // 内核空间地址 (框架内 check_user_buf 会拒绝, services 仅查 0)
    // services 层只检查 NULL, 不查范围
    assert_eq!(uname_validate(0xffff_8000_0000_0000), Ok(()));
}

// ============================================================================
// gettimeofday
// ============================================================================

#[test]
fn test_gettimeofday_null_rejected() {
    // NULL timeval
    assert_eq!(gettimeofday_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_gettimeofday_valid_tv() {
    // 合法用户空间地址
    assert_eq!(gettimeofday_validate(0x7fff_ffff_f000), Ok(()));
    assert_eq!(gettimeofday_validate(0x1000), Ok(()));
}

#[test]
fn test_gettimeofday_zero_len_buffer() {
    // 验证: 框架层 check_user_buf 会检查 16 字节可写, services 仅查 NULL
    // services 0 是 EFAULT, 非 0 通过
    assert_eq!(gettimeofday_validate(0), Err(Errno::EFAULT));
    assert_ne!(gettimeofday_validate(1), Err(Errno::EFAULT));
}

// ============================================================================
// 集成场景
// ============================================================================

#[test]
fn test_info_scenarios_init() {
    // 系统初始化查询
    assert_eq!(uname_validate(0x7fff_ffff_f000), Ok(()));
    assert_eq!(gettimeofday_validate(0x7fff_ffff_f008), Ok(()));
}

#[test]
fn test_info_scenarios_shell() {
    // shell getpid/gettid (恒成功)
    // 测试不报错,仅验证代理存在
    // (实际调用需要 kernel context, 此处只测试 pure validation)
    assert_eq!(getpgid_validate(0), Ok(()));
    assert_eq!(getpgid_validate(1), Ok(()));
}

#[test]
fn test_info_validation_independence() {
    // 各验证独立: 错误互不影响
    assert_eq!(getpgid_validate(0), Ok(()));
    assert_eq!(uname_validate(0), Err(Errno::EFAULT));
    assert_eq!(gettimeofday_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_info_negative_pids() {
    // pid 边界: -1, i32::MIN
    assert_eq!(getpgid_validate(-1), Err(Errno::EINVAL));
    assert_eq!(getpgid_validate(i32::MIN), Err(Errno::EINVAL));
}

#[test]
fn test_info_high_pids() {
    // pid 边界: i32::MAX
    assert_eq!(getpgid_validate(i32::MAX), Ok(()));
}
