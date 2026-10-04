//! stat/lstat/fstat 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// stat
// ============================================================================

#[test]
fn test_stat_null_path_rejected() {
    assert_eq!(stat_validate(0, 0x1000, MIN_STAT_SIZE), Err(Errno::EFAULT));
}

#[test]
fn test_stat_null_buf_rejected() {
    assert_eq!(stat_validate(0x1000, 0, MIN_STAT_SIZE), Err(Errno::EFAULT));
}

#[test]
fn test_stat_both_null_rejected() {
    assert_eq!(stat_validate(0, 0, MIN_STAT_SIZE), Err(Errno::EFAULT));
}

#[test]
fn test_stat_buf_too_small_rejected() {
    assert_eq!(stat_validate(0x1000, 0x2000, MIN_STAT_SIZE - 1), Err(Errno::EINVAL));
    assert_eq!(stat_validate(0x1000, 0x2000, 0), Err(Errno::EINVAL));
}

#[test]
fn test_stat_valid() {
    assert_eq!(stat_validate(0x1000, 0x2000, MIN_STAT_SIZE), Ok(()));
    assert_eq!(stat_validate(0x1000, 0x2000, 256), Ok(()));
    assert_eq!(stat_validate(0x1000, 0x2000, 1024), Ok(()));
}

// ============================================================================
// lstat (同 stat)
// ============================================================================

#[test]
fn test_lstat_null_path_rejected() {
    assert_eq!(lstat_validate(0, 0x1000, MIN_STAT_SIZE), Err(Errno::EFAULT));
}

#[test]
fn test_lstat_null_buf_rejected() {
    assert_eq!(lstat_validate(0x1000, 0, MIN_STAT_SIZE), Err(Errno::EFAULT));
}

#[test]
fn test_lstat_buf_too_small_rejected() {
    assert_eq!(lstat_validate(0x1000, 0x2000, MIN_STAT_SIZE - 1), Err(Errno::EINVAL));
}

#[test]
fn test_lstat_valid() {
    assert_eq!(lstat_validate(0x1000, 0x2000, MIN_STAT_SIZE), Ok(()));
    assert_eq!(lstat_validate(0x1000, 0x2000, 1024), Ok(()));
}

// ============================================================================
// fstat
// ============================================================================

#[test]
fn test_fstat_negative_fd_rejected() {
    assert_eq!(fstat_validate(-1, 0x1000, MIN_STAT_SIZE), Err(Errno::EBADF));
    assert_eq!(fstat_validate(i32::MIN, 0x1000, MIN_STAT_SIZE), Err(Errno::EBADF));
}

#[test]
fn test_fstat_null_buf_rejected() {
    assert_eq!(fstat_validate(0, 0, MIN_STAT_SIZE), Err(Errno::EFAULT));
    assert_eq!(fstat_validate(3, 0, MIN_STAT_SIZE), Err(Errno::EFAULT));
}

#[test]
fn test_fstat_buf_too_small_rejected() {
    assert_eq!(fstat_validate(3, 0x1000, MIN_STAT_SIZE - 1), Err(Errno::EINVAL));
    assert_eq!(fstat_validate(3, 0x1000, 0), Err(Errno::EINVAL));
}

#[test]
fn test_fstat_valid() {
    assert_eq!(fstat_validate(0, 0x1000, MIN_STAT_SIZE), Ok(()));
    assert_eq!(fstat_validate(3, 0x2000, 256), Ok(()));
    assert_eq!(fstat_validate(100, 0x7fff_ffff_e000, 1024), Ok(()));
}

#[test]
fn test_fstat_fd_priority_over_buf() {
    // fd < 0 先检查, 返回 EBADF
    assert_eq!(fstat_validate(-1, 0, 0), Err(Errno::EBADF));
}
