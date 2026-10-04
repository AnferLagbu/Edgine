//! 网络子系统参数验证测试 (D1.2)
//!
//! 覆盖 `framework::net::init` 的纯标量验证逻辑 (host-side 复刻):
//! - `parse_ipv4_literal` — IPv4 字面量解析
//! - `dns_resolve`       — 简单 DNS 解析 (静态 hosts 表 + IPv4 fallback)
//!
//! 测试运行: `cd src/rust/edgine-tests && cargo test --test net_dns`

use edgine_tests::*;

// ============================================================================
// parse_ipv4_literal
// ============================================================================

#[test]
fn test_parse_ipv4_literal_valid() {
    assert_eq!(parse_ipv4_literal("0.0.0.0"), Some([0, 0, 0, 0]));
    assert_eq!(parse_ipv4_literal("10.0.2.15"), Some([10, 0, 2, 15]));
    assert_eq!(parse_ipv4_literal("255.255.255.255"), Some([255, 255, 255, 255]));
    assert_eq!(parse_ipv4_literal("127.0.0.1"), Some([127, 0, 0, 1]));
    assert_eq!(parse_ipv4_literal("1.2.3.4"), Some([1, 2, 3, 4]));
}

#[test]
fn test_parse_ipv4_literal_invalid_too_few() {
    assert_eq!(parse_ipv4_literal(""), None);
    assert_eq!(parse_ipv4_literal("10"), None);
    assert_eq!(parse_ipv4_literal("10.0"), None);
    assert_eq!(parse_ipv4_literal("10.0.2"), None);
}

#[test]
fn test_parse_ipv4_literal_invalid_too_many() {
    assert_eq!(parse_ipv4_literal("10.0.2.15.1"), None);
    assert_eq!(parse_ipv4_literal("1.2.3.4.5"), None);
}

#[test]
fn test_parse_ipv4_literal_out_of_range() {
    assert_eq!(parse_ipv4_literal("10.0.2.256"), None);   // 越界
    assert_eq!(parse_ipv4_literal("999.999.999.999"), None);
    assert_eq!(parse_ipv4_literal("10.0.300.15"), None);
    assert_eq!(parse_ipv4_literal("256.0.0.0"), None);
}

#[test]
fn test_parse_ipv4_literal_malformed() {
    assert_eq!(parse_ipv4_literal("10..2.15"), None);
    assert_eq!(parse_ipv4_literal("10.0.2."), None);
    assert_eq!(parse_ipv4_literal(".10.0.2.15"), None);
    assert_eq!(parse_ipv4_literal("10.0.2.15 "), None);   // 尾随空格
    assert_eq!(parse_ipv4_literal("a.b.c.d"), None);
    assert_eq!(parse_ipv4_literal("10.0.2.15x"), None);
    assert_eq!(parse_ipv4_literal("10.-1.2.15"), None);
}

#[test]
fn test_parse_ipv4_literal_zero_padded() {
    // 允许前导 0 (parse_ipv4_literal 不走 strict 四段, 仅校验范围)
    assert_eq!(parse_ipv4_literal("010.000.002.015"), Some([10, 0, 2, 15]));
}

// ============================================================================
// dns_resolve
// ============================================================================

#[test]
fn test_dns_resolve_localhost() {
    assert_eq!(dns_resolve("localhost"), Some([127, 0, 0, 1]));
    assert_eq!(dns_resolve("LOCALHOST"), Some([127, 0, 0, 1]));  // 大小写不敏感
    assert_eq!(dns_resolve("LocalHost"), Some([127, 0, 0, 1]));
}

#[test]
fn test_dns_resolve_router() {
    assert_eq!(dns_resolve("router"), Some([10, 0, 2, 2]));
    assert_eq!(dns_resolve("ROUTER"), Some([10, 0, 2, 2]));
    assert_eq!(dns_resolve("Router"), Some([10, 0, 2, 2]));
}

#[test]
fn test_dns_resolve_aliases() {
    assert_eq!(dns_resolve("host"), Some([10, 0, 2, 15]));
    assert_eq!(dns_resolve("qemu-gateway"), Some([10, 0, 2, 2]));
    assert_eq!(dns_resolve("edgine-gateway"), Some([10, 0, 2, 2]));
}

#[test]
fn test_dns_resolve_unknown_falls_back_to_ip_literal() {
    // 未知主机名走 IPv4 字面量路径
    assert_eq!(dns_resolve("8.8.8.8"), Some([8, 8, 8, 8]));
    assert_eq!(dns_resolve("10.0.2.15"), Some([10, 0, 2, 15]));
    assert_eq!(dns_resolve("1.1.1.1"), Some([1, 1, 1, 1]));
}

#[test]
fn test_dns_resolve_returns_none_for_garbage() {
    assert_eq!(dns_resolve("nonexistent.example.com"), None);
    assert_eq!(dns_resolve(""), None);
    assert_eq!(dns_resolve("999.999.999.999"), None);
    assert_eq!(dns_resolve("hello world"), None);
    assert_eq!(dns_resolve("not-a-name"), None);
}

// ============================================================================
// 优先级: 静态 hosts 优先于 IPv4 literal
// ============================================================================

#[test]
fn test_dns_resolve_static_takes_precedence() {
    // "host" 在静态 hosts 里, 走 [10, 0, 2, 15]
    // 如果 "host" 也是合法 IPv4 literal, 应优先静态表
    assert_eq!(dns_resolve("host"), Some([10, 0, 2, 15]));
    // 验证: 静态表 [10, 0, 2, 15] 即使被当作 IPv4 literal 也是 [10, 0, 2, 15]
    assert_eq!(parse_ipv4_literal("10.0.2.15"), Some([10, 0, 2, 15]));
}
