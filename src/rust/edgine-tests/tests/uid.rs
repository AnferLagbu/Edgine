//! UID/GID 系统调用服务层参数验证测试
//!
//! 覆盖 functions/sgeg/uid.rs 的标量验证:
//! - 读类 (get*) 始终成功
//! - 写类 (set*) 简化: 总是先做规则校验, 框架层做最终决定

use edgine_tests::*;

// ============================================================================
// 读类
// ============================================================================

#[test]
fn test_setuid_any_u32_valid() {
    // functions 层不做限制, 始终接受 u32 参数
    // 真实权限校验由 privileged 决定 (root / euid 一致才允许)
    assert_eq!(setuid_validate(0), Ok(()));
    assert_eq!(setuid_validate(1), Ok(()));
    assert_eq!(setuid_validate(1000), Ok(()));
    assert_eq!(setuid_validate(u32::MAX), Ok(()));
}

#[test]
fn test_setreuid_valid_args() {
    // POSIX 允许 (uid_t)-1 = 0xFFFFFFFF 表示不变
    assert_eq!(setreuid_validate(0, 0), Ok(()));
    assert_eq!(setreuid_validate(1000, 0), Ok(()));
    assert_eq!(setreuid_validate(0xFFFFFFFF, 0xFFFFFFFF), Ok(()));
    assert_eq!(setreuid_validate(0xFFFFFFFF, 0), Ok(()));
    assert_eq!(setreuid_validate(0, 0xFFFFFFFF), Ok(()));
}
