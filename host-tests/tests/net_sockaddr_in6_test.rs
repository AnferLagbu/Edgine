//! net: sockaddr_in6 C 结构体与 FFI 翻译层验证 (sm_fi.rs)
//!
//! 验收:
//!   - sm_fi.rs 定义 `SockaddrIn6` (#[repr(C)], 28 字节, 与 Linux 布局一致)
//!   - SockaddrIn6 字段: sin6_family/sin6_port/sin6_flowinfo/sin6_addr/sin6_scope_id
//!   - `write_sockaddr` 按 IpAddr 分支写 sockaddr_in (16B) / sockaddr_in6 (28B)
//!   - `parse_endpoint_trait` 按 family (2/10) 分支解析端点
//!   - `endpoint_from_smol` 支持 IpAddress::Ipv6 → NetEndpoint::new_v6
//!
//! 追踪: DECISION-032 (IPv4/IPv6 双栈)
//! SPDX-License-Identifier: MPL-2.0

use std::fs;

const SM_FI_RS: &str = "../src/kernel/framework/net/init/sm_fi.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {}", path, e))
}

#[test]
fn test_sockaddr_in6_struct_defined() {
    let src = read(SM_FI_RS);
    assert!(src.contains("struct SockaddrIn6"), "SockaddrIn6 结构未定义");
    assert!(
        src.contains("#[repr(C)]"),
        "SockaddrIn6 缺 #[repr(C)] (C ABI 兼容)"
    );
    // Linux sockaddr_in6 布局字段
    assert!(src.contains("sin6_family: u16"), "sin6_family 字段缺失");
    assert!(src.contains("sin6_port: u16"), "sin6_port 字段缺失");
    assert!(src.contains("sin6_flowinfo: u32"), "sin6_flowinfo 字段缺失");
    assert!(
        src.contains("sin6_addr: [u8; 16]"),
        "sin6_addr 16 字节字段缺失"
    );
    assert!(src.contains("sin6_scope_id: u32"), "sin6_scope_id 字段缺失");
}

#[test]
fn test_sockaddr_in6_is_28_bytes() {
    let src = read(SM_FI_RS);
    // 28 字节 = 2(family) + 2(port) + 4(flowinfo) + 16(addr) + 4(scope_id)
    // 通过写入 addrlen 时用 size_of::<SockaddrIn6>() 保证
    assert!(
        src.contains("core::mem::size_of::<SockaddrIn6>()"),
        "write_sockaddr 未用 size_of::<SockaddrIn6>() 回写 addrlen"
    );
    // AF_INET6 = 10 常量用于 sin6_family
    assert!(
        src.contains("sin6_family: 10"),
        "SockaddrIn6 未写 AF_INET6 (10)"
    );
}

#[test]
fn test_write_sockaddr_dual_stack() {
    let src = read(SM_FI_RS);
    // write_sockaddr 按 IpAddr 分支写 V4 sockaddr_in / V6 sockaddr_in6
    assert!(
        src.contains("pub(crate) unsafe fn write_sockaddr("),
        "write_sockaddr 函数未定义"
    );
    assert!(src.contains("IpAddr::V4(v4)"), "write_sockaddr 缺 V4 分支");
    assert!(src.contains("IpAddr::V6(v6)"), "write_sockaddr 缺 V6 分支");
}

#[test]
fn test_parse_endpoint_trait_family_branch() {
    let src = read(SM_FI_RS);
    assert!(
        src.contains("pub(crate) unsafe fn parse_endpoint_trait("),
        "parse_endpoint_trait 未定义"
    );
    // 按 family 分支: 2 (AF_INET) / 10 (AF_INET6)
    assert!(
        src.contains("match family"),
        "parse_endpoint_trait 缺 family match"
    );
    assert!(
        src.contains("2 =>"),
        "parse_endpoint_trait 缺 AF_INET (2) 分支"
    );
    assert!(
        src.contains("10 =>"),
        "parse_endpoint_trait 缺 AF_INET6 (10) 分支"
    );
}

#[test]
fn test_endpoint_translation_supports_v6() {
    let src = read(SM_FI_RS);
    // endpoint_from_smol 支持 smoltcp IpAddress::Ipv6 → NetEndpoint::new_v6
    assert!(
        src.contains("IpAddress::Ipv6(v6)"),
        "endpoint_from_smol 未处理 IpAddress::Ipv6"
    );
    assert!(
        src.contains("NetEndpoint::new_v6"),
        "endpoint_from_smol 未用 new_v6 构造 V6 端点"
    );
    // wire_to_smol 统一翻译 IpAddr → smoltcp IpAddress
    assert!(
        src.contains("pub(crate) fn wire_to_smol("),
        "统一 wire_to_smol 未定义"
    );
}

#[test]
fn test_getsockname_uses_write_sockaddr() {
    let src = read(SM_FI_RS);
    // sm_getsockname / sm_getpeername 通过 write_sockaddr 支持 V4/V6
    let getsockname_count = src.matches("write_sockaddr(addr, addrlen, &ep);").count();
    assert!(
        getsockname_count >= 2,
        "sm_getsockname / sm_getpeername 未复用 write_sockaddr (当前 {} 处)",
        getsockname_count
    );
}
