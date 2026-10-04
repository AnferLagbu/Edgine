//! Socket Syscall 12 dispatch 预校验测试 (D1.4)
//!
//! 覆盖 `functions::net::syscall.rs` 的 12 个 dispatch 的"参数预校验"逻辑:
//! - fd 范围 (EBADF)
//! - 指针非空 (EFAULT/EINVAL)
//! - 长度非零 (EFAULT)
//! - backlog 范围 (EINVAL)
//! - iov 范围 (EINVAL)
//!
//! 真正的 fw:: 委托调用 (raw::check_user_buf / read_u64_from_user) 需要
//! QEMU 集成测试覆盖, host 端只覆盖纯标量验证。

use edgine_tests::*;

// ============================================================================
// socket_syscall
// ============================================================================

#[test]
fn test_socket_syscall_accepts_any_args() {
    // socket 自身不做预校验, 即使非法 domain/type 也 Ok(()); 由底层拒绝
    assert_eq!(socket_syscall_validate(2, 1, 0), Ok(()));   // AF_INET, STREAM
    assert_eq!(socket_syscall_validate(10, 3, 0), Ok(()));  // 非法, 但 OK
    assert_eq!(socket_syscall_validate(-1, -1, -1), Ok(()));
}

// ============================================================================
// bind_syscall
// ============================================================================

#[test]
fn test_bind_syscall_valid_fd() {
    assert_eq!(bind_syscall_validate(0, 0x1000, 16), Ok(()));
    assert_eq!(bind_syscall_validate(1, 0, 0), Ok(()));  // addr_ptr==0 不在校验内
    assert_eq!(bind_syscall_validate(100, 0xFFFF, 0), Ok(()));
}

#[test]
fn test_bind_syscall_negative_fd_rejected() {
    assert_eq!(bind_syscall_validate(-1, 0x1000, 16), Err(Errno::EBADF));
    assert_eq!(bind_syscall_validate(-100, 0x1000, 16), Err(Errno::EBADF));
    assert_eq!(bind_syscall_validate(i32::MIN, 0, 0), Err(Errno::EBADF));
}

// ============================================================================
// listen_syscall
// ============================================================================

#[test]
fn test_listen_syscall_valid() {
    assert_eq!(listen_syscall_validate(0, 0), Ok(()));
    assert_eq!(listen_syscall_validate(0, 1), Ok(()));
    assert_eq!(listen_syscall_validate(0, 128), Ok(()));
    assert_eq!(listen_syscall_validate(0, i32::MAX), Ok(()));
}

#[test]
fn test_listen_syscall_negative_fd() {
    assert_eq!(listen_syscall_validate(-1, 10), Err(Errno::EBADF));
}

#[test]
fn test_listen_syscall_negative_backlog() {
    assert_eq!(listen_syscall_validate(0, -1), Err(Errno::EINVAL));
    assert_eq!(listen_syscall_validate(0, -100), Err(Errno::EINVAL));
    assert_eq!(listen_syscall_validate(0, i32::MIN), Err(Errno::EINVAL));
}

#[test]
fn test_listen_syscall_fd_checked_first() {
    // 负 fd 在 backlog 之前检查
    assert_eq!(listen_syscall_validate(-1, -1), Err(Errno::EBADF));
}

// ============================================================================
// accept_syscall
// ============================================================================

#[test]
fn test_accept_syscall_valid() {
    assert_eq!(accept_syscall_validate(0, 0, 0), Ok(()));
    assert_eq!(accept_syscall_validate(0, 0x1000, 0x2000), Ok(()));
    assert_eq!(accept_syscall_validate(0, 0, 0x100), Ok(()));  // 任何 ptr 都行
}

#[test]
fn test_accept_syscall_negative_fd() {
    assert_eq!(accept_syscall_validate(-1, 0, 0), Err(Errno::EBADF));
    assert_eq!(accept_syscall_validate(i32::MIN, 0, 0), Err(Errno::EBADF));
}

// ============================================================================
// connect_syscall
// ============================================================================

#[test]
fn test_connect_syscall_valid() {
    assert_eq!(connect_syscall_validate(0, 0x1000, 16), Ok(()));
    assert_eq!(connect_syscall_validate(0, 0, 0), Ok(()));  // addr_ptr==0 不在校验内
}

#[test]
fn test_connect_syscall_negative_fd() {
    assert_eq!(connect_syscall_validate(-1, 0x1000, 16), Err(Errno::EBADF));
}

// ============================================================================
// sendto_syscall
// ============================================================================

#[test]
fn test_sendto_syscall_valid() {
    assert_eq!(sendto_syscall_validate(0, 0x1000, 100, 0, 0x2000, 16), Ok(()));
    assert_eq!(sendto_syscall_validate(0, 0, 0, 0, 0, 0), Ok(()));
    assert_eq!(sendto_syscall_validate(0, 0x1000, 100, 0xFFFF, 0x2000, 16), Ok(()));
}

#[test]
fn test_sendto_syscall_negative_fd() {
    assert_eq!(sendto_syscall_validate(-1, 0x1000, 100, 0, 0x2000, 16), Err(Errno::EBADF));
}

// ============================================================================
// recvfrom_syscall
// ============================================================================

#[test]
fn test_recvfrom_syscall_valid() {
    assert_eq!(recvfrom_syscall_validate(0, 0x1000, 100, 0, 0, 0), Ok(()));
    assert_eq!(recvfrom_syscall_validate(0, 0x1000, 1, 0, 0x2000, 0x2008), Ok(()));
}

#[test]
fn test_recvfrom_syscall_negative_fd() {
    assert_eq!(recvfrom_syscall_validate(-1, 0x1000, 100, 0, 0, 0), Err(Errno::EBADF));
}

#[test]
fn test_recvfrom_syscall_null_buf() {
    assert_eq!(recvfrom_syscall_validate(0, 0, 100, 0, 0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_recvfrom_syscall_zero_len() {
    assert_eq!(recvfrom_syscall_validate(0, 0x1000, 0, 0, 0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_recvfrom_syscall_fd_checked_first() {
    // 负 fd 在 buf 之前检查
    assert_eq!(recvfrom_syscall_validate(-1, 0, 0, 0, 0, 0), Err(Errno::EBADF));
}

// ============================================================================
// setsockopt_syscall
// ============================================================================

#[test]
fn test_setsockopt_syscall_valid() {
    assert_eq!(setsockopt_syscall_validate(0, 1, 9, 0x1000, 4), Ok(()));
    assert_eq!(setsockopt_syscall_validate(0, 0, 0, 0, 0), Ok(()));
    assert_eq!(setsockopt_syscall_validate(0, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF), Ok(()));
}

#[test]
fn test_setsockopt_syscall_negative_fd() {
    assert_eq!(setsockopt_syscall_validate(-1, 1, 9, 0x1000, 4), Err(Errno::EBADF));
}

// ============================================================================
// getsockopt_syscall
// ============================================================================

#[test]
fn test_getsockopt_syscall_valid() {
    assert_eq!(getsockopt_syscall_validate(0, 1, 9, 0x1000, 0x2000), Ok(()));
    assert_eq!(getsockopt_syscall_validate(0, 0, 0, 0, 0), Ok(()));
}

#[test]
fn test_getsockopt_syscall_negative_fd() {
    assert_eq!(getsockopt_syscall_validate(-1, 1, 9, 0x1000, 0x2000), Err(Errno::EBADF));
}

// ============================================================================
// shutdown_syscall
// ============================================================================

#[test]
fn test_shutdown_syscall_valid() {
    assert_eq!(shutdown_syscall_validate(0, 0), Ok(()));
    assert_eq!(shutdown_syscall_validate(0, 1), Ok(()));
    assert_eq!(shutdown_syscall_validate(0, 2), Ok(()));
    assert_eq!(shutdown_syscall_validate(0, -1), Ok(()));  // how 不校验
}

#[test]
fn test_shutdown_syscall_negative_fd() {
    assert_eq!(shutdown_syscall_validate(-1, 0), Err(Errno::EBADF));
    assert_eq!(shutdown_syscall_validate(-1, 2), Err(Errno::EBADF));
}

// ============================================================================
// sendmsg_syscall
// ============================================================================

#[test]
fn test_sendmsg_syscall_valid() {
    assert_eq!(sendmsg_syscall_validate(0, 0x1000, 0), Ok(()));
    assert_eq!(sendmsg_syscall_validate(0, 0x1000, 0xFFFF), Ok(()));
}

#[test]
fn test_sendmsg_syscall_negative_fd() {
    assert_eq!(sendmsg_syscall_validate(-1, 0x1000, 0), Err(Errno::EBADF));
}

#[test]
fn test_sendmsg_syscall_null_msg() {
    assert_eq!(sendmsg_syscall_validate(0, 0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_sendmsg_syscall_fd_checked_first() {
    assert_eq!(sendmsg_syscall_validate(-1, 0, 0), Err(Errno::EBADF));
}

// ============================================================================
// recvmsg_syscall
// ============================================================================

#[test]
fn test_recvmsg_syscall_valid() {
    assert_eq!(recvmsg_syscall_validate(0, 0x1000, 0), Ok(()));
    assert_eq!(recvmsg_syscall_validate(0, 0x1000, 0xFFFF), Ok(()));
}

#[test]
fn test_recvmsg_syscall_negative_fd() {
    assert_eq!(recvmsg_syscall_validate(-1, 0x1000, 0), Err(Errno::EBADF));
}

#[test]
fn test_recvmsg_syscall_null_msg() {
    assert_eq!(recvmsg_syscall_validate(0, 0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_recvmsg_syscall_fd_checked_first() {
    assert_eq!(recvmsg_syscall_validate(-1, 0, 0), Err(Errno::EBADF));
}

// ============================================================================
// msg_iov_validate (sendmsg/recvmsg 内部)
// ============================================================================

#[test]
fn test_msg_iov_valid() {
    assert_eq!(msg_iov_validate(0x1000, 1), Ok(()));
    assert_eq!(msg_iov_validate(0x2000, 100), Ok(()));
    assert_eq!(msg_iov_validate(0xFFFF, 1024), Ok(()));  // 最大
}

#[test]
fn test_msg_iov_zero_iovlen() {
    assert_eq!(msg_iov_validate(0x1000, 0), Err(Errno::EINVAL));
}

#[test]
fn test_msg_iov_too_many_iovlen() {
    assert_eq!(msg_iov_validate(0x1000, 1025),  Err(Errno::EINVAL));
    assert_eq!(msg_iov_validate(0x1000, 2048),  Err(Errno::EINVAL));
    assert_eq!(msg_iov_validate(0x1000, u64::MAX), Err(Errno::EINVAL));
}

#[test]
fn test_msg_iov_null_ptr() {
    assert_eq!(msg_iov_validate(0, 1),    Err(Errno::EINVAL));
    assert_eq!(msg_iov_validate(0, 100),  Err(Errno::EINVAL));
}

#[test]
fn test_msg_iov_overflow_protected() {
    // iovlen = u64::MAX/2 + 1, 乘 16 应溢出 → 返 EINVAL (这里用较小值也能触发 overflow)
    // u64::MAX / 16 + 1 即会让 checked_mul 返 None
    let overflow_iovlen = (u64::MAX / 16) + 1;
    assert_eq!(msg_iov_validate(0x1000, overflow_iovlen), Err(Errno::EINVAL));
}
