//! 静态 DNS 解析 (B04-09 拆分 Step D, 2026-08-25)
//!
//! 原 init.rs 内联定义: `HostEntry` / `STATIC_HOSTS` / `dns_resolve` /
//! `parse_ipv4_literal`. 抽出为独立子模块后, init.rs 通过
//! `pub use dns::*` re-export.

// I-46: hosts 表里 10.0.2.x 引用集中常量, 避免散落硬编码
use crate::framework::net::types::{FALLBACK_GATEWAY, FALLBACK_IPV4};

/// 静态 hosts 表条目: 主机名 → IPv4
#[derive(Debug, Clone, Copy)]
struct HostEntry {
    name: &'static str,
    ip: [u8; 4],
}

/// 内置静态 hosts (D1.2 起步, D 阶段后续可换 smoltcp wire/dns 升级)
const STATIC_HOSTS: &[HostEntry] = &[
    HostEntry {
        name: "localhost",
        ip: [127, 0, 0, 1],
    },
    HostEntry {
        name: "router",
        ip: FALLBACK_GATEWAY,
    },
    HostEntry {
        name: "host",
        ip: FALLBACK_IPV4,
    },
    HostEntry {
        name: "qemu-gateway",
        ip: FALLBACK_GATEWAY,
    },
    HostEntry {
        name: "edgine-gateway",
        ip: FALLBACK_GATEWAY,
    },
];

/// 简单 DNS 解析 (静态 hosts 表)
///
/// # 实现
/// - 精确匹配主机名 (不区分大小写 — ASCII tolower)
/// - 大小写不敏感: "Router" / "ROUTER" / "router" 都匹配
///
/// # 局限 (D 阶段后续工作)
/// - 不发起 DNS UDP 查询
/// - 不支持通配 (`*.example.com`)
/// - 不支持 AAAA (IPv6)
pub fn dns_resolve(name: &str) -> Option<[u8; 4]> {
    for entry in STATIC_HOSTS {
        if entry.name.eq_ignore_ascii_case(name) {
            return Some(entry.ip);
        }
    }
    // 数字字面量解析: "10.0.2.15" 直接返 (避免对 IP 字符串做 DNS 浪费)
    if let Some(ip) = parse_ipv4_literal(name) {
        return Some(ip);
    }
    None
}

/// 解析 IPv4 字面量 "a.b.c.d" (无错处理; 不合法返 None)
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn parse_ipv4_literal(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut idx = 0usize;
    let mut cur: u32 = 0;
    let mut has_digit = false;
    for &b in s.as_bytes() {
        if b == b'.' {
            if !has_digit || idx >= 3 || cur > 255 {
                return None;
            }
            octets[idx] = cur as u8;
            idx += 1;
            cur = 0;
            has_digit = false;
        } else if b.is_ascii_digit() {
            cur = cur * 10 + u32::from(b - b'0');
            has_digit = true;
        } else {
            return None;
        }
    }
    if !has_digit || idx != 3 || cur > 255 {
        return None;
    }
    octets[3] = cur as u8;
    Some(octets)
}

/// 解析 CIDR 字面量 "a.b.c.d/prefix" (可选 prefix, 默认 24)
///
/// 返回 `(ipv4, prefix_len)`. 用于 `cmd.rs::eg_net_static_ip` 复用,
/// 消除 init.rs 中重复的 IPv4 文本解析实现 (B04-09 优化拆分 Step G).
///
/// # 语法
/// - `"a.b.c.d"` → prefix 默认 24
/// - `"a.b.c.d/16"` → prefix = 16
/// - 前缀非 0-32 数字 / IP 不合法 → `None`
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn parse_cidr(s: &str) -> Option<([u8; 4], u8)> {
    let (ip_part, prefix_part) = match s.split_once('/') {
        Some((ip, pfx)) => (ip, Some(pfx)),
        None => (s, None),
    };
    let ip = parse_ipv4_literal(ip_part)?;
    let prefix = match prefix_part {
        Some(pfx) => {
            if pfx.is_empty() || !pfx.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let v: u32 = pfx.parse().ok()?;
            if v > 32 {
                return None;
            }
            v as u8
        }
        None => 24,
    };
    Some((ip, prefix))
}
