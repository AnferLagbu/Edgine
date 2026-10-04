//! Socket 系统调用服务层参数验证测试
//!
//! 覆盖 functions/net/syscall.rs 的 10 个 pure-scalar 验证逻辑:
//! - socket/bind/listen/accept/connect
//! - sendto/recvfrom
//! - setsockopt/getsockopt
//! - shutdown

use edgine_tests::*;

// ============================================================================
// socket
// ============================================================================

#[test]
fn test_socket_af_inet_stream() {
    assert_eq!(socket_validate(AF_INET, SOCK_STREAM, 0), Ok(()));
}

#[test]
fn test_socket_af_inet_dgram() {
    assert_eq!(socket_validate(AF_INET, SOCK_DGRAM, 0), Ok(()));
}

#[test]
fn test_socket_af_unsupported() {
    // AF_UNIX = 1, 暂不支持
    assert_eq!(socket_validate(1, SOCK_STREAM, 0), Err(Errno::EINVAL));
    // AF_INET6 = 10
    assert_eq!(socket_validate(10, SOCK_STREAM, 0), Err(Errno::EINVAL));
    // AF_UNSPEC = 0
    assert_eq!(socket_validate(0, SOCK_STREAM, 0), Err(Errno::EINVAL));
}

#[test]
fn test_socket_invalid_type() {
    // SOCK_RAW = 3 不支持
    assert_eq!(socket_validate(AF_INET, 3, 0), Err(Errno::EINVAL));
    // SOCK_SEQPACKET = 5 不支持
    assert_eq!(socket_validate(AF_INET, 5, 0), Err(Errno::EINVAL));
    assert_eq!(socket_validate(AF_INET, -1, 0), Err(Errno::EINVAL));
}

#[test]
fn test_socket_protocol_must_be_zero() {
    // 暂只支持 protocol=0
    assert_eq!(socket_validate(AF_INET, SOCK_STREAM, 1), Err(Errno::EINVAL));
    assert_eq!(socket_validate(AF_INET, SOCK_DGRAM, 6), Err(Errno::EINVAL));
}

// ============================================================================
// bind
// ============================================================================

#[test]
fn test_bind_negative_fd_rejected() {
    assert_eq!(bind_validate(-1, 0x1000, 16), Err(Errno::EBADF));
    assert_eq!(bind_validate(-100, 0x1000, 16), Err(Errno::EBADF));
}

#[test]
fn test_bind_null_addr_rejected() {
    assert_eq!(bind_validate(3, 0, 16), Err(Errno::EFAULT));
}

#[test]
fn test_bind_valid_args() {
    assert_eq!(bind_validate(3, 0x7fff_ffff_e000, 16), Ok(()));
    assert_eq!(bind_validate(0, 0x1000, 16), Ok(()));
}

// ============================================================================
// listen
// ============================================================================

#[test]
fn test_listen_negative_fd_rejected() {
    assert_eq!(listen_validate(-1, 5), Err(Errno::EBADF));
}

#[test]
fn test_listen_negative_backlog_rejected() {
    assert_eq!(listen_validate(3, -1), Err(Errno::EINVAL));
    assert_eq!(listen_validate(3, -100), Err(Errno::EINVAL));
}

#[test]
fn test_listen_valid_backlog() {
    assert_eq!(listen_validate(3, 0), Ok(()));
    assert_eq!(listen_validate(3, 5), Ok(()));
    assert_eq!(listen_validate(3, 128), Ok(()));
    assert_eq!(listen_validate(3, i32::MAX), Ok(()));
}

// ============================================================================
// accept
// ============================================================================

#[test]
fn test_accept_negative_fd_rejected() {
    assert_eq!(accept_validate(-1, 0), Err(Errno::EBADF));
}

#[test]
fn test_accept_null_addr_allowed() {
    // POSIX 允许 addr=0 (不关心对端地址)
    assert_eq!(accept_validate(3, 0), Ok(()));
}

#[test]
fn test_accept_valid_addr() {
    assert_eq!(accept_validate(3, 0x7fff_ffff_e000), Ok(()));
}

// ============================================================================
// connect
// ============================================================================

#[test]
fn test_connect_negative_fd_rejected() {
    assert_eq!(connect_validate(-1, 0x1000, 16), Err(Errno::EBADF));
}

#[test]
fn test_connect_null_addr_rejected() {
    assert_eq!(connect_validate(3, 0, 16), Err(Errno::EFAULT));
}

#[test]
fn test_connect_valid_args() {
    assert_eq!(connect_validate(3, 0x7fff_ffff_e000, 16), Ok(()));
}

// ============================================================================
// sendto
// ============================================================================

#[test]
fn test_sendto_negative_fd_rejected() {
    assert_eq!(sendto_validate(-1, 0x1000, 10, 0), Err(Errno::EBADF));
}

#[test]
fn test_sendto_null_buf_with_len_rejected() {
    assert_eq!(sendto_validate(3, 0, 10, 0), Err(Errno::EFAULT));
}

#[test]
fn test_sendto_zero_len_udp_like() {
    // len=0 不要求 buf 有效 (sendto 探测路径)
    assert_eq!(sendto_validate(3, 0, 0, 0x2000), Ok(()));
}

#[test]
fn test_sendto_valid_args() {
    assert_eq!(sendto_validate(3, 0x1000, 100, 0x2000), Ok(()));
    // dest_ptr=0 (TCP 发送)
    assert_eq!(sendto_validate(3, 0x1000, 100, 0), Ok(()));
}

// ============================================================================
// recvfrom
// ============================================================================

#[test]
fn test_recvfrom_negative_fd_rejected() {
    assert_eq!(recvfrom_validate(-1, 0x1000, 10), Err(Errno::EBADF));
}

#[test]
fn test_recvfrom_null_buf_rejected() {
    assert_eq!(recvfrom_validate(3, 0, 10), Err(Errno::EFAULT));
}

#[test]
fn test_recvfrom_zero_len_rejected() {
    assert_eq!(recvfrom_validate(3, 0x1000, 0), Err(Errno::EFAULT));
}

#[test]
fn test_recvfrom_valid_args() {
    assert_eq!(recvfrom_validate(3, 0x7fff_ffff_e000, 4096), Ok(()));
}

// ============================================================================
// setsockopt
// ============================================================================

#[test]
fn test_setsockopt_negative_fd_rejected() {
    assert_eq!(setsockopt_validate(-1, SOL_SOCKET, SO_REUSEADDR, 0x1000), Err(Errno::EBADF));
}

#[test]
fn test_setsockopt_null_val_rejected() {
    assert_eq!(setsockopt_validate(3, SOL_SOCKET, SO_REUSEADDR, 0), Err(Errno::EFAULT));
}

#[test]
fn test_setsockopt_valid_args() {
    assert_eq!(setsockopt_validate(3, SOL_SOCKET, SO_REUSEADDR, 0x1000), Ok(()));
}

// ============================================================================
// getsockopt
// ============================================================================

#[test]
fn test_getsockopt_negative_fd_rejected() {
    assert_eq!(getsockopt_validate(-1, SOL_SOCKET, SO_REUSEADDR, 0x1000), Err(Errno::EBADF));
}

#[test]
fn test_getsockopt_null_val_rejected() {
    assert_eq!(getsockopt_validate(3, SOL_SOCKET, SO_REUSEADDR, 0), Err(Errno::EFAULT));
}

#[test]
fn test_getsockopt_valid_args() {
    assert_eq!(getsockopt_validate(3, SOL_SOCKET, SO_REUSEADDR, 0x1000), Ok(()));
}

// ============================================================================
// shutdown
// ============================================================================

#[test]
fn test_shutdown_negative_fd_rejected() {
    assert_eq!(shutdown_validate(-1, 2), Err(Errno::EBADF));
}

#[test]
fn test_shutdown_how_ignored() {
    // how 任意值均允许
    assert_eq!(shutdown_validate(3, 0), Ok(()));
    assert_eq!(shutdown_validate(3, 1), Ok(()));
    assert_eq!(shutdown_validate(3, 2), Ok(()));
    assert_eq!(shutdown_validate(3, 100), Ok(()));
}

// ============================================================================
// getsockname / getpeername
// ============================================================================

#[test]
fn test_sockname_validate_fd() {
    assert_eq!(sockname_validate(-1, 0x1000, 0x2000), Err(Errno::EBADF));
    assert_eq!(sockname_validate(0, 0x1000, 0x2000), Ok(()));
    assert_eq!(sockname_validate(63, 0x1000, 0x2000), Ok(()));
}

#[test]
fn test_sockname_validate_addr() {
    assert_eq!(sockname_validate(0, 0, 0x2000), Err(Errno::EFAULT));
    assert_eq!(sockname_validate(0, 0x1000, 0x2000), Ok(()));
}

#[test]
fn test_sockname_validate_addrlen() {
    assert_eq!(sockname_validate(0, 0x1000, 0), Err(Errno::EFAULT));
    assert_eq!(sockname_validate(0, 0x1000, 0x2000), Ok(()));
}

#[test]
fn test_peername_validate_fd() {
    assert_eq!(peername_validate(-1, 0x1000, 0x2000), Err(Errno::EBADF));
    assert_eq!(peername_validate(3, 0x1000, 0x2000), Ok(()));
}

#[test]
fn test_peername_validate_addr_null() {
    assert_eq!(peername_validate(0, 0, 0x2000), Err(Errno::EFAULT));
    assert_eq!(peername_validate(0, 0x1000, 0), Err(Errno::EFAULT));
}

#[test]
fn test_rusage_validate_who() {
    assert_eq!(rusage_validate(-1, 0x1000), Err(Errno::EINVAL));
    assert_eq!(rusage_validate(0, 0x1000), Ok(()));
    assert_eq!(rusage_validate(1, 0x1000), Ok(()));
    assert_eq!(rusage_validate(2, 0x1000), Ok(()));
    assert_eq!(rusage_validate(3, 0x1000), Err(Errno::EINVAL));
}

#[test]
fn test_rusage_validate_buf() {
    assert_eq!(rusage_validate(0, 0), Err(Errno::EFAULT));
    assert_eq!(rusage_validate(0, 0x1000), Ok(()));
}

// ============================================================================
// sendmsg / recvmsg
// ============================================================================

#[test]
fn test_sendmsg_validate_fd() {
    assert_eq!(sendmsg_validate(-1, 0x1000, 0), Err(Errno::EBADF));
    assert_eq!(sendmsg_validate(0, 0x1000, 0), Ok(()));
}

#[test]
fn test_sendmsg_validate_msg_null() {
    assert_eq!(sendmsg_validate(0, 0, 0), Err(Errno::EFAULT));
}

#[test]
fn test_sendmsg_validate_iovlen_zero() {
    // 假设 iovlen=0 写在 msg+24, 设为 0 -> EINVAL
    // 但 msg_ptr 必须先有效: 此测试不依赖实际读, 用 dummy 范围.
    // 实际场景: functions 先 check_user_buf(msg,56) 才能 read u64.
    // 这里简化为: 假定 msg_ptr=0x1000 范围不可读 -> EFAULT
    // 无法测 EINVAL 真实路径, 跳过.
}

#[test]
fn test_recvmsg_validate_fd() {
    assert_eq!(recvmsg_validate(-1, 0x1000, 0), Err(Errno::EBADF));
    assert_eq!(recvmsg_validate(3, 0x1000, 0), Ok(()));
}

#[test]
fn test_recvmsg_validate_msg_null() {
    assert_eq!(recvmsg_validate(0, 0, 0), Err(Errno::EFAULT));
}
