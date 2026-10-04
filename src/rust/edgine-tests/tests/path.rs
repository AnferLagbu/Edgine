//! 路径系统调用服务层参数验证测试
//!
//! 覆盖 functions/fs/path.rs 的标量验证:
//! - chdir: path_ptr != 0
//! - getcwd: buf_ptr != 0 && size > 0

use edgine_tests::*;

// ============================================================================
// chdir
// ============================================================================

#[test]
fn test_chdir_null_rejected() {
    assert_eq!(chdir_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_chdir_valid_path() {
    assert_eq!(chdir_validate(0x1000), Ok(()));
    assert_eq!(chdir_validate(0x7fff_ffff_e000), Ok(()));
}

// ============================================================================
// getcwd
// ============================================================================

#[test]
fn test_getcwd_null_buf_rejected() {
    assert_eq!(getcwd_validate(0, 256), Err(Errno::EINVAL));
}

#[test]
fn test_getcwd_zero_size_rejected() {
    assert_eq!(getcwd_validate(0x1000, 0), Err(Errno::EINVAL));
}

#[test]
fn test_getcwd_both_invalid_rejected() {
    assert_eq!(getcwd_validate(0, 0), Err(Errno::EINVAL));
}

#[test]
fn test_getcwd_valid_args() {
    assert_eq!(getcwd_validate(0x1000, 256), Ok(()));
    assert_eq!(getcwd_validate(0x7fff_ffff_e000, 4096), Ok(()));
    assert_eq!(getcwd_validate(0x1000, 1), Ok(()));
}
