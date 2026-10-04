//! open/close/creat 系统调用服务层参数验证测试

use edgine_tests::*;

// ============================================================================
// open
// ============================================================================

#[test]
fn test_open_null_path_rejected() {
    assert_eq!(open_path_validate(0), Err(Errno::EFAULT));
}

#[test]
fn test_open_valid_path() {
    assert_eq!(open_path_validate(0x1000), Ok(()));
    assert_eq!(open_path_validate(0xdead_beef), Ok(()));
}

#[test]
fn test_open_flags_access_modes() {
    // 三种访问模式都允许
    assert_eq!(open_flags_validate(O_RDONLY), Ok(()));
    assert_eq!(open_flags_validate(O_WRONLY), Ok(()));
    assert_eq!(open_flags_validate(O_RDWR), Ok(()));
}

#[test]
fn test_open_flags_invalid_access() {
    // 访问模式 0o3 不存在
    assert_eq!(open_flags_validate(0o3), Err(Errno::EINVAL));
}

#[test]
fn test_open_flags_negative_rejected() {
    assert_eq!(open_flags_validate(-1), Err(Errno::EINVAL));
}

#[test]
fn test_open_creat_requires_writable() {
    // O_CREAT 必须配合 O_WRONLY/O_RDWR
    assert_eq!(open_flags_validate(O_CREAT | O_RDONLY), Err(Errno::EINVAL));
    assert_eq!(open_flags_validate(O_CREAT | O_WRONLY), Ok(()));
    assert_eq!(open_flags_validate(O_CREAT | O_RDWR), Ok(()));
}

#[test]
fn test_open_trunc_requires_writable() {
    assert_eq!(open_flags_validate(O_TRUNC | O_RDONLY), Err(Errno::EINVAL));
    assert_eq!(open_flags_validate(O_TRUNC | O_WRONLY), Ok(()));
}

#[test]
fn test_open_directory_creat_conflict() {
    // O_DIRECTORY 与 O_CREAT 不能同用
    assert_eq!(open_flags_validate(O_DIRECTORY | O_CREAT), Err(Errno::EINVAL));
    assert_eq!(open_flags_validate(O_DIRECTORY | O_WRONLY), Ok(()));
}

#[test]
fn test_open_combined_flags() {
    // 常见组合
    assert_eq!(open_flags_validate(O_CREAT | O_WRONLY | O_TRUNC), Ok(()));
    assert_eq!(open_flags_validate(O_CREAT | O_RDWR | O_EXCL), Ok(()));
    assert_eq!(open_flags_validate(O_RDWR | O_APPEND), Ok(()));
    assert_eq!(open_flags_validate(O_RDONLY | O_CLOEXEC), Ok(()));
}

// ============================================================================
// open mode
// ============================================================================

#[test]
fn test_open_mode_creat_required() {
    // 无 O_CREAT, mode 必须为 0
    assert_eq!(open_mode_validate(O_RDONLY, 0), Ok(()));
    assert_eq!(open_mode_validate(O_RDONLY, 0o644), Err(Errno::EINVAL));
    assert_eq!(open_mode_validate(O_WRONLY, 0o644), Err(Errno::EINVAL));
}

#[test]
fn test_open_mode_creat_with_mode() {
    assert_eq!(open_mode_validate(O_CREAT | O_WRONLY, 0), Ok(()));
    assert_eq!(open_mode_validate(O_CREAT | O_WRONLY, 0o644), Ok(()));
    assert_eq!(open_mode_validate(O_CREAT | O_WRONLY, 0o777), Ok(()));
    assert_eq!(open_mode_validate(O_CREAT | O_WRONLY, 0o7777), Ok(()));
}

#[test]
fn test_open_mode_too_large() {
    assert_eq!(open_mode_validate(O_CREAT | O_WRONLY, 0o10_000), Err(Errno::EINVAL));
    assert_eq!(open_mode_validate(O_CREAT | O_WRONLY, 0o100_000), Err(Errno::EINVAL));
}

#[test]
fn test_open_mode_negative() {
    assert_eq!(open_mode_validate(O_CREAT | O_WRONLY, -1), Err(Errno::EINVAL));
}

// ============================================================================
// close
// ============================================================================

#[test]
fn test_close_negative_fd_rejected() {
    assert_eq!(close_fd_validate(-1), Err(Errno::EBADF));
    assert_eq!(close_fd_validate(i32::MIN), Err(Errno::EBADF));
}

#[test]
fn test_close_valid_fd() {
    assert_eq!(close_fd_validate(0), Ok(()));
    assert_eq!(close_fd_validate(3), Ok(()));
    assert_eq!(close_fd_validate(255), Ok(()));
    assert_eq!(close_fd_validate(i32::MAX), Ok(()));
}
