//! brk 系统调用服务层参数验证测试
//!
//! 覆盖 functions/mm/brk.rs 的 brk_syscall 验证:
//! - 0 表示查询当前 brk
//! - 非 0 必须在用户空间地址范围

use edgine_tests::*;

#[test]
fn test_brk_zero_query_current() {
    // 0 表示查询当前 brk
    assert_eq!(brk_validate(0), Ok(()));
}

#[test]
fn test_brk_typical_user_addr() {
    // 用户空间典型地址
    assert_eq!(brk_validate(0x1000), Ok(()));
    assert_eq!(brk_validate(0x10000), Ok(()));
    assert_eq!(brk_validate(0x4000_0000), Ok(()));
}

#[test]
fn test_brk_upper_user_limit() {
    // 用户空间地址上界 0x7FFF_FFFF_FFFF
    assert_eq!(brk_validate(0x0000_7FFF_FFFF_FFFF), Ok(()));
}

#[test]
fn test_brk_kernel_addr_rejected() {
    // 内核地址范围
    assert_eq!(brk_validate(0x0000_8000_0000_0000), Err(Errno::ENOMEM));
    assert_eq!(brk_validate(0xFFFF_0000_0000_0000), Err(Errno::ENOMEM));
    assert_eq!(brk_validate(u64::MAX), Err(Errno::ENOMEM));
}
