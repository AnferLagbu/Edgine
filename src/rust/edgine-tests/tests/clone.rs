//! clone 服务层参数验证测试
//!
//! 覆盖 services/proc/clone.rs 的纯标量验证逻辑:
//! - CLONE_VM/CLONE_THREAD 必须配 CLONE_SIGHAND
//! - child_stack 16 字节对齐

use edgine_tests::*;

#[test]
fn test_clone_no_flags_is_valid() {
    // 无 flag (等同 fork)
    assert_eq!(clone_validate(0, 0), Ok(()));
    assert_eq!(clone_validate(0, 0x7fff_ffff_f000), Ok(()));
}

#[test]
fn test_clone_vm_requires_sighand() {
    // CLONE_VM 必须配 CLONE_SIGHAND
    assert_eq!(clone_validate(CLONE_VM, 0), Err(Errno::EINVAL));
    assert_eq!(clone_validate(CLONE_VM | CLONE_SIGHAND, 0), Ok(()));
}

#[test]
fn test_clone_thread_requires_sighand() {
    // CLONE_THREAD 必须配 CLONE_SIGHAND
    assert_eq!(clone_validate(CLONE_THREAD, 0), Err(Errno::EINVAL));
    assert_eq!(clone_validate(CLONE_THREAD | CLONE_SIGHAND, 0), Ok(()));
}

#[test]
fn test_clone_thread_full_thread_set() {
    // 完整线程创建: VM + FS + FILES + SIGHAND + THREAD
    let flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
    assert_eq!(clone_validate(flags, 0x7fff_ffff_f000), Ok(()));
}

#[test]
fn test_clone_stack_alignment_required() {
    // child_stack 16 字节对齐 (x86_64 ABI)
    assert_eq!(clone_validate(0, 0x7fff_ffff_f000), Ok(()));  // 16 字节对齐
    assert_eq!(clone_validate(0, 0x7fff_ffff_f001), Err(Errno::EINVAL));  // 不对齐
    assert_eq!(clone_validate(0, 0x7fff_ffff_f008), Err(Errno::EINVAL));  // 不对齐
    assert_eq!(clone_validate(0, 0x7fff_ffff_f010), Ok(()));  // 16 字节对齐
    assert_eq!(clone_validate(0, 0x7fff_ffff_f020), Ok(()));  // 32 字节对齐
}

#[test]
fn test_clone_zero_stack_allowed() {
    // child_stack = 0 表示不修改栈, 允许
    assert_eq!(clone_validate(0, 0), Ok(()));
    assert_eq!(clone_validate(CLONE_VM | CLONE_SIGHAND, 0), Ok(()));
}

#[test]
fn test_clone_with_parent_settid() {
    // CLONE_PARENT_SETTID 可与基础 flag 组合
    assert_eq!(clone_validate(CLONE_PARENT_SETTID, 0), Ok(()));
    assert_eq!(clone_validate(CLONE_VM | CLONE_SIGHAND | CLONE_PARENT_SETTID, 0x1000), Ok(()));
}

#[test]
fn test_clone_vm_without_thread_is_valid() {
    // 仅 CLONE_VM (无 CLONE_THREAD) — 创建独立进程但共享地址空间
    assert_eq!(clone_validate(CLONE_VM | CLONE_SIGHAND, 0x1000), Ok(()));
}

#[test]
fn test_errno_from_ret() {
    assert_eq!(errno_from_ret_value(-1), Errno::EPERM);
    assert_eq!(errno_from_ret_value(-2), Errno::ENOENT);
    assert_eq!(errno_from_ret_value(-9), Errno::EBADF);
    assert_eq!(errno_from_ret_value(-12), Errno::ENOMEM);
    assert_eq!(errno_from_ret_value(-14), Errno::EFAULT);
    assert_eq!(errno_from_ret_value(-22), Errno::EINVAL);
    assert_eq!(errno_from_ret_value(-3), Errno::ESRCH);
}

#[test]
fn test_errno_from_ret_unknown_defaults_to_einval() {
    // 未知错误码默认 EINVAL
    assert_eq!(errno_from_ret_value(-9999), Errno::EINVAL);
}
