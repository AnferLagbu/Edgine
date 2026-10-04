//! getrlimit 系统调用服务层参数验证测试

use edgine_tests::*;

#[test]
fn test_getrlimit_null_rejected() {
    assert_eq!(getrlimit_validate(0, 0), Err(Errno::EINVAL));
    assert_eq!(getrlimit_validate(3, 0), Err(Errno::EINVAL));
}

#[test]
fn test_getrlimit_negative_resource_rejected() {
    assert_eq!(getrlimit_validate(-1, 0x1000), Err(Errno::EINVAL));
    assert_eq!(getrlimit_validate(i32::MIN, 0x1000), Err(Errno::EINVAL));
}

#[test]
fn test_getrlimit_too_large_resource_rejected() {
    assert_eq!(getrlimit_validate(17, 0x1000), Err(Errno::EINVAL));
    assert_eq!(getrlimit_validate(100, 0x1000), Err(Errno::EINVAL));
    assert_eq!(getrlimit_validate(i32::MAX, 0x1000), Err(Errno::EINVAL));
}

#[test]
fn test_getrlimit_valid_resources() {
    // POSIX 资源类型
    for r in 0..=16 {
        assert_eq!(getrlimit_validate(r, 0x1000), Ok(()));
    }
}

#[test]
fn test_getrlimit_null_priority_over_resource() {
    // 顺序: rlim_ptr == 0 → EINVAL
    assert_eq!(getrlimit_validate(0, 0), Err(Errno::EINVAL));
}
