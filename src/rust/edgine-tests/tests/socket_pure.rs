//! Socket 子系统纯逻辑测试 (D1.3)
//!
//! 覆盖 `services::net::socket` 中不依赖 FFI 的 pure-scalar / pure-bytes 逻辑:
//! - SocketError::from_i32 (POSIX errno → 强类型)
//! - Domain::from_i32 (AF_INET 2, 其它 None)
//! - SockType::from_i32 (SOCK_STREAM 1, SOCK_DGRAM 2, 其它 None)
//! - SockAddrIn 编解码 (8 字节 sockaddr_in)
//! - parse_ipv4 / endpoint_from_str
//!
//! 不可 host 测的 FFI (socket/bind/listen/accept/connect/send/recv/sendto/recvfrom/
//! close/setsockopt/getsockopt/poll_all) 由 kernel_test feature 下的
//! 集成测试 (QEMU) 覆盖。

use edgine_tests::*;

// ============================================================================
// SocketError::from_i32
// ============================================================================

#[test]
fn test_socket_error_eperm() {
    assert_eq!(SocketError::from_i32(1), SocketError::PermissionDenied);
}

#[test]
fn test_socket_error_ebadf() {
    assert_eq!(SocketError::from_i32(9), SocketError::BadFd);
}

#[test]
fn test_socket_error_eagain_wouldblock() {
    assert_eq!(SocketError::from_i32(11), SocketError::WouldBlock);
}

#[test]
fn test_socket_error_enomem() {
    assert_eq!(SocketError::from_i32(12), SocketError::NoMemory);
}

#[test]
fn test_socket_error_efault() {
    assert_eq!(SocketError::from_i32(14), SocketError::Fault);
}

#[test]
fn test_socket_error_enodev() {
    assert_eq!(SocketError::from_i32(19), SocketError::NoDevice);
}

#[test]
fn test_socket_error_einval() {
    assert_eq!(SocketError::from_i32(22), SocketError::InvalidArgument);
}

#[test]
fn test_socket_error_enfile() {
    assert_eq!(SocketError::from_i32(23), SocketError::ProcessFileLimit);
}

#[test]
fn test_socket_error_enotsup() {
    assert_eq!(SocketError::from_i32(95), SocketError::NotSupported);
}

#[test]
fn test_socket_error_eafnosupport() {
    assert_eq!(SocketError::from_i32(97), SocketError::AddrFamilyNotSupported);
}

#[test]
fn test_socket_error_eaddrinuse() {
    assert_eq!(SocketError::from_i32(98), SocketError::AddrInUse);
}

#[test]
fn test_socket_error_eaddrnotavail() {
    assert_eq!(SocketError::from_i32(99), SocketError::AddrNotAvailable);
}

#[test]
fn test_socket_error_econnreset() {
    assert_eq!(SocketError::from_i32(104), SocketError::ConnectionReset);
}

#[test]
fn test_socket_error_enotconn() {
    assert_eq!(SocketError::from_i32(107), SocketError::NotConnected);
}

#[test]
fn test_socket_error_econnrefused() {
    assert_eq!(SocketError::from_i32(111), SocketError::ConnectionRefused);
}

#[test]
fn test_socket_error_unknown_falls_back_to_other() {
    // 任何未列出的 errno 都装入 Other
    assert_eq!(SocketError::from_i32(0),     SocketError::Other(0));
    assert_eq!(SocketError::from_i32(2),     SocketError::Other(2));
    assert_eq!(SocketError::from_i32(255),   SocketError::Other(255));
    assert_eq!(SocketError::from_i32(-1),    SocketError::Other(-1));
    assert_eq!(SocketError::from_i32(i32::MIN), SocketError::Other(i32::MIN));
}

// ============================================================================
// Domain::from_i32
// ============================================================================

#[test]
fn test_domain_inet() {
    assert_eq!(Domain::from_i32(2), Some(Domain::Inet));
    // repr
    assert_eq!(Domain::Inet as i32, 2);
}

#[test]
fn test_domain_unsupported_returns_none() {
    // AF_UNIX=1, AF_INET6=10, AF_PACKET=17
    assert_eq!(Domain::from_i32(0),  None);
    assert_eq!(Domain::from_i32(1),  None);
    assert_eq!(Domain::from_i32(3),  None);
    assert_eq!(Domain::from_i32(10), None);
    assert_eq!(Domain::from_i32(17), None);
    assert_eq!(Domain::from_i32(-1), None);
    assert_eq!(Domain::from_i32(i32::MAX), None);
}

// ============================================================================
// SockType::from_i32
// ============================================================================

#[test]
fn test_socktype_stream() {
    assert_eq!(SockType::from_i32(1), Some(SockType::Stream));
    assert_eq!(SockType::Stream as i32, 1);
}

#[test]
fn test_socktype_dgram() {
    assert_eq!(SockType::from_i32(2), Some(SockType::Dgram));
    assert_eq!(SockType::Dgram as i32, 2);
}

#[test]
fn test_socktype_unsupported_returns_none() {
    // SOCK_SEQPACKET=5, SOCK_RAW=3
    assert_eq!(SockType::from_i32(0),  None);
    assert_eq!(SockType::from_i32(3),  None);
    assert_eq!(SockType::from_i32(4),  None);
    assert_eq!(SockType::from_i32(5),  None);
    assert_eq!(SockType::from_i32(99), None);
    assert_eq!(SockType::from_i32(-1), None);
}

// ============================================================================
// SockAddrIn 字节序编解码
// ============================================================================

#[test]
fn test_sockaddr_in_new_and_eq() {
    let a = SockAddrIn::new(8080, [10, 0, 2, 15]);
    assert_eq!(a.port, 8080);
    assert_eq!(a.ip, [10, 0, 2, 15]);
    let b = SockAddrIn::new(8080, [10, 0, 2, 15]);
    assert_eq!(a, b);
}

#[test]
fn test_sockaddr_in_to_bytes_layout() {
    // struct sockaddr_in: sin_family(2) | sin_port(2) | sin_addr(4) | zero(0)
    // AF_INET = 2 (大端), port = 8080 (大端), ip = 10.0.2.15
    let addr = SockAddrIn::new(8080, [10, 0, 2, 15]);
    let bytes = sockaddr_in_to_bytes(&addr);
    // family
    assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 2);
    // port (BE)
    assert_eq!(u16::from_be_bytes([bytes[2], bytes[3]]), 8080);
    // ip
    assert_eq!(&bytes[4..8], &[10, 0, 2, 15]);
    // 总长 8
    assert_eq!(bytes.len(), 8);
}

#[test]
fn test_sockaddr_in_to_bytes_edge_ports() {
    // port 0 / 65535
    let a = sockaddr_in_to_bytes(&SockAddrIn::new(0, [0, 0, 0, 0]));
    assert_eq!(u16::from_be_bytes([a[2], a[3]]), 0);
    let b = sockaddr_in_to_bytes(&SockAddrIn::new(65535, [255, 255, 255, 255]));
    assert_eq!(u16::from_be_bytes([b[2], b[3]]), 65535);
    assert_eq!(&b[4..8], &[255, 255, 255, 255]);
}

#[test]
fn test_bytes_to_sockaddr_in_roundtrip() {
    let original = SockAddrIn::new(443, [192, 168, 1, 1]);
    let bytes = sockaddr_in_to_bytes(&original);
    let recovered = bytes_to_sockaddr_in(&bytes).expect("roundtrip must succeed");
    assert_eq!(recovered, original);
}

#[test]
fn test_bytes_to_sockaddr_in_wrong_family() {
    // 写入 AF_UNIX=1 应被拒
    let mut bytes = [0u8; 8];
    bytes[0..2].copy_from_slice(&(1u16).to_be_bytes());
    assert_eq!(bytes_to_sockaddr_in(&bytes), None);
    // AF_INET6=10 也应被拒
    let mut bytes2 = [0u8; 8];
    bytes2[0..2].copy_from_slice(&(10u16).to_be_bytes());
    assert_eq!(bytes_to_sockaddr_in(&bytes2), None);
}

#[test]
fn test_bytes_to_sockaddr_in_all_zero() {
    // 0 是 family=0 (AF_UNSPEC), 应被拒
    let bytes = [0u8; 8];
    assert_eq!(bytes_to_sockaddr_in(&bytes), None);
}

// ============================================================================
// parse_ipv4
// ============================================================================

#[test]
fn test_socket_parse_ipv4_valid() {
    assert_eq!(socket_parse_ipv4("0.0.0.0"),            Some([0, 0, 0, 0]));
    assert_eq!(socket_parse_ipv4("10.0.2.15"),          Some([10, 0, 2, 15]));
    assert_eq!(socket_parse_ipv4("127.0.0.1"),          Some([127, 0, 0, 1]));
    assert_eq!(socket_parse_ipv4("255.255.255.255"),    Some([255, 255, 255, 255]));
    assert_eq!(socket_parse_ipv4("1.2.3.4"),           Some([1, 2, 3, 4]));
}

#[test]
fn test_socket_parse_ipv4_invalid_count() {
    assert_eq!(socket_parse_ipv4(""),         None);
    assert_eq!(socket_parse_ipv4("10"),       None);
    assert_eq!(socket_parse_ipv4("10.0"),     None);
    assert_eq!(socket_parse_ipv4("10.0.2"),   None);
    assert_eq!(socket_parse_ipv4("10.0.2.15.1"), None);
    assert_eq!(socket_parse_ipv4("1.2.3.4.5"),   None);
}

#[test]
fn test_socket_parse_ipv4_out_of_range() {
    // 与 parse_ipv4_literal 不同: socket_parse_ipv4 用 .parse() 拒负数, 拒越界
    assert_eq!(socket_parse_ipv4("10.0.2.256"),       None);
    assert_eq!(socket_parse_ipv4("999.999.999.999"),  None);
    assert_eq!(socket_parse_ipv4("256.0.0.0"),        None);
}

#[test]
fn test_socket_parse_ipv4_non_digit() {
    assert_eq!(socket_parse_ipv4("a.b.c.d"),     None);
    assert_eq!(socket_parse_ipv4("10.0.2.15x"),  None);
    // 负数通过 .parse() 失败
    assert_eq!(socket_parse_ipv4("10.-1.2.15"),  None);
    // 前缀 0 不会拒绝 (u32 parse 允许 010=10)
    assert_eq!(socket_parse_ipv4("010.0.2.15"),  Some([10, 0, 2, 15]));
}

// ============================================================================
// endpoint_from_str
// ============================================================================

#[test]
fn test_endpoint_from_str_valid() {
    let a = endpoint_from_str("10.0.2.15", 8080).expect("valid");
    assert_eq!(a, SockAddrIn::new(8080, [10, 0, 2, 15]));

    let b = endpoint_from_str("127.0.0.1", 443).expect("valid");
    assert_eq!(b, SockAddrIn::new(443, [127, 0, 0, 1]));

    let c = endpoint_from_str("0.0.0.0", 0).expect("zero");
    assert_eq!(c, SockAddrIn::new(0, [0, 0, 0, 0]));
}

#[test]
fn test_endpoint_from_str_invalid() {
    assert_eq!(endpoint_from_str("not-an-ip", 80), None);
    assert_eq!(endpoint_from_str("10.0.2.256", 80), None);
    assert_eq!(endpoint_from_str("10.0.2", 80), None);
    assert_eq!(endpoint_from_str("10.0.2.15.1", 80), None);
    assert_eq!(endpoint_from_str("", 80), None);
}

// ============================================================================
// 类型大小 / 对齐假设 (供 syscall.rs 转换函数使用)
// ============================================================================

#[test]
fn test_sockaddr_in_size() {
    // struct sockaddr_in 标准布局 16 字节 (含 8 字节 padding),
    // 但 edgine 只用前 8 字节, 验证 SockAddrIn 自身为 6 字节
    use std::mem::size_of;
    assert_eq!(size_of::<SockAddrIn>(), 6);
    assert_eq!(size_of::<[u8; 8]>(),      8);
    // Domain / SockType 在 services 层标 #[repr(i32)], 大小为 4
    assert_eq!(size_of::<Domain>(),       4);
    assert_eq!(size_of::<SockType>(),     4);
    // SocketError 16 个无 payload 变体 + Other(i32) → 大小 8
    // (与服务层保持一致 — services 端未显式 #[repr(i32)], 跟随 Rust 默认)
    assert_eq!(size_of::<SocketError>(),  8);
}
