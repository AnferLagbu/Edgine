//! net: IPv6 地址抽象类型定义验证 (iface_trait.rs)
//!
//! 验收:
//!   - iface_trait.rs 定义 `Ipv6Addr(pub [u8; 16])` 新类型
//!   - iface_trait.rs 定义 `enum IpAddr { V4, V6 }` 统一地址类型
//!   - iface_trait.rs 定义 `Ipv6Cidr` (地址 + 前缀长度)
//!   - 提供 `new_v4` / `new_v6` / `into_ip_addr` 双栈迁移辅助
//!   - 提供 `is_loopback` / `is_multicast` / `is_unspecified` 判定
//!   - 内置单元测试覆盖 IPv6 构造/转换/match
//!
//! 追踪: DECISION-032 (IPv4/IPv6 双栈)
//! SPDX-License-Identifier: MPL-2.0

use std::fs;

const IFACE_TRAIT_RS: &str = "../src/kernel/framework/net/iface_trait.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {}", path, e))
}

#[test]
fn test_ipv6_addr_type_defined() {
    let src = read(IFACE_TRAIT_RS);
    assert!(
        src.contains("pub struct Ipv6Addr(pub [u8; 16])"),
        "Ipv6Addr 新类型未定义"
    );
    assert!(
        src.contains("pub const UNSPECIFIED: Self = Self([0; 16])"),
        "Ipv6Addr::UNSPECIFIED (::) 未定义"
    );
    assert!(
        src.contains("pub const LOOPBACK: Self"),
        "Ipv6Addr::LOOPBACK (::1) 未定义"
    );
}

#[test]
fn test_ip_addr_enum_defined() {
    let src = read(IFACE_TRAIT_RS);
    assert!(
        src.contains("pub enum IpAddr"),
        "统一地址类型 IpAddr 未定义"
    );
    assert!(src.contains("V4(Ipv4Addr)"), "IpAddr::V4 变体未定义");
    assert!(src.contains("V6(Ipv6Addr)"), "IpAddr::V6 变体未定义");
    // 判定方法
    assert!(src.contains("pub const fn is_v4"), "is_v4 未定义");
    assert!(src.contains("pub const fn is_v6"), "is_v6 未定义");
    assert!(src.contains("pub const fn as_v4"), "as_v4 未定义");
    assert!(src.contains("pub const fn as_v6"), "as_v6 未定义");
}

#[test]
fn test_ipv6_cidr_defined() {
    let src = read(IFACE_TRAIT_RS);
    assert!(src.contains("pub struct Ipv6Cidr"), "Ipv6Cidr 未定义");
    assert!(
        src.contains("pub const fn new(address: Ipv6Addr, prefix_len: u8)"),
        "Ipv6Cidr::new 未定义"
    );
}

#[test]
fn test_dual_stack_helpers_defined() {
    let src = read(IFACE_TRAIT_RS);
    // 迁移辅助: NetEndpoint::new_v4 / new_v6
    assert!(
        src.contains("pub const fn new_v4"),
        "NetEndpoint::new_v4 未定义"
    );
    assert!(
        src.contains("pub const fn new_v6"),
        "NetEndpoint::new_v6 未定义"
    );
    // Ipv4Addr / Ipv6Addr 提升为 IpAddr
    assert!(
        src.contains("pub const fn into_ip_addr(self) -> IpAddr"),
        "into_ip_addr 未定义"
    );
}

#[test]
fn test_ipv6_predicates_defined() {
    let src = read(IFACE_TRAIT_RS);
    assert!(
        src.contains("pub const fn is_unspecified"),
        "is_unspecified 未定义"
    );
    assert!(
        src.contains("pub const fn is_loopback"),
        "is_loopback 未定义"
    );
    assert!(
        src.contains("pub const fn is_multicast"),
        "is_multicast 未定义"
    );
}

#[test]
fn test_ipv6_unit_tests_exist() {
    let src = read(IFACE_TRAIT_RS);
    // iface_trait.rs 内联单元测试覆盖 IPv6 构造/转换/match
    assert!(
        src.contains("fn test_ipv6_addr_constructors"),
        "缺 test_ipv6_addr_constructors"
    );
    assert!(
        src.contains("fn test_ipv6_addr_conversions"),
        "缺 test_ipv6_addr_conversions"
    );
    assert!(src.contains("fn test_ipv6_cidr"), "缺 test_ipv6_cidr");
    assert!(src.contains("fn test_ip_addr_enum"), "缺 test_ip_addr_enum");
}

#[test]
fn test_net_endpoint_uses_ip_addr() {
    let src = read(IFACE_TRAIT_RS);
    // NetEndpoint.addr 必须是 IpAddr (双栈改造核心)
    assert!(
        src.contains("pub addr: IpAddr"),
        "NetEndpoint.addr 未升级为 IpAddr"
    );
}
