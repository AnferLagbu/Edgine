//! 静态 DNS 解析 + 真实 DNS 客户端查询 (B04-09 拆分 Step D; S2 接线 2026-10)
//!
//! 原 init.rs 内联定义: `HostEntry` / `STATIC_HOSTS` / `dns_resolve` /
//! `parse_ipv4_literal`. 抽出为独立子模块后, init.rs 通过
//! `pub use dns::*` re-export.
//!
//! S2 (net-feature-alignment.md): `dns_resolve` 升级为三级解析 —— IPv4 字面量
//! → 静态 hosts → smoltcp `dns::Socket` 真实查询 (阻塞有界等待). 未初始化/
//! 无 server/查询失败均回退 `None`, 旧输入族的既有语义逐字保持.

// I-46: hosts 表里 10.0.2.x 引用集中常量, 避免散落硬编码
use core::sync::atomic::Ordering;

use smoltcp::socket::dns as smoltcp_dns;
use smoltcp::wire::{DnsQueryType, IpAddress, Ipv4Address};

use super::raw;
use super::state::{G_DNS, NET_STATE};
use crate::privileged::net::types::{FALLBACK_DNS, FALLBACK_GATEWAY, FALLBACK_IPV4};

/// DNS 并发查询槽数 (借给 smoltcp `dns::Socket` 的定长查询表容量).
pub(crate) const DNS_QUERY_SLOTS: usize = 2;

/// 阻塞查询有界等待参数 (复用 boot DHCP 等待环范式 `poll_network`+spin).
const DNS_QUERY_MAX_ROUNDS: u32 = 200;
const DNS_QUERY_SPIN: u32 = 50_000;

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

/// 简单 DNS 解析: 三级 —— IPv4 字面量 / 静态 hosts / 真实 DNS 查询
///
/// # 实现
/// - 精确匹配主机名 (不区分大小写 — ASCII tolower)
/// - 大小写不敏感: "Router" / "ROUTER" / "router" 都匹配
/// - 静态表未命中时发起 smoltcp DNS 查询 (仅进程上下文, 见
///   `dns_query_net` 契约); 未初始化/无 server/失败均返 `None`
///
/// # 局限 (S2 SIMPLIFIED 口径)
/// - 查询仅 IPv4 (Type::A), 不支持 AAAA / search domain / 缓存
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
    // 第三级: 真实 DNS 查询 (S2, net-feature-alignment.md)
    dns_query_net(name)
}

/// 把 DHCP 租约/回退的 DNS server 写入 smoltcp `dns::Socket`.
///
/// `leases` 为 `dhcp_dns_servers()` 输出 (全 0 = 无); 空时落 [`FALLBACK_DNS`]
/// (QEMU slirp 惯例). 调用方须持 `NET_STATE` 锁并传入同一锁域内的 `sockets`.
pub(crate) fn apply_dns_servers(
    sockets: &mut smoltcp::iface::SocketSet<'_>,
    handle: Option<smoltcp::iface::SocketHandle>,
    leases: [[u8; 4]; 3],
) {
    let Some(handle) = handle else {
        return;
    };
    let mut buf = [IpAddress::Ipv4(Ipv4Address::UNSPECIFIED); 3];
    let mut n = 0usize;
    for ip in &leases {
        if ip.iter().any(|&b| b != 0) && n < buf.len() {
            buf[n] = IpAddress::Ipv4(Ipv4Address::new(ip[0], ip[1], ip[2], ip[3]));
            n += 1;
        }
    }
    if n == 0 {
        // static fallback / 租约未带 option 6: slirp 惯例 nameserver (I-46 集中常量)
        buf[0] = IpAddress::Ipv4(Ipv4Address::new(
            FALLBACK_DNS[0],
            FALLBACK_DNS[1],
            FALLBACK_DNS[2],
            FALLBACK_DNS[3],
        ));
        n = 1;
    }
    sockets
        .get_mut::<smoltcp_dns::Socket>(handle)
        .update_servers(&buf[..n]);
}

/// 真实 DNS 查询 (阻塞有界等待, 仅进程上下文).
///
/// # 契约
/// - **仅限进程上下文**: 有界轮次内 `poll_network` + 自旋等待, 不得在 IRQ/
///   持锁路径调用 (`poll_network` 自行 `NET_STATE.try_lock`, 嵌套必死锁).
/// - 短路路径全部无锁: `NET_READY=false` (host 单测/boot 前) 与无 server
///   (`G_DNS[0]==0`, 含 cmd 手工 static ip 路径) 直接返 `None`, 不碰 socket.
///
/// SIMPLIFIED: 同步阻塞语义, 无异步/缓存/AAAA; 影响面: 单次查询串行且最坏
/// 占用有界自旋窗口; 何时扩展: 用户态真实消费者 (getaddrinfo 类 syscall) 出现
/// 时改 waker→wait_queue 阻塞 (复用 P5 形状).
fn dns_query_net(name: &str) -> Option<[u8; 4]> {
    if !crate::privileged::net::NET_READY.load(Ordering::Acquire) {
        return None;
    }
    if G_DNS[0].load(Ordering::Acquire) == 0 {
        return None;
    }

    // 阶段 1: 锁内提交查询 (start_query 触 socket, 须 NET_STATE 保护)
    let query = {
        let _guard = NET_STATE.lock();
        let handle = raw::dns_handle()?;
        let stack = raw::stack_mut()?;
        // SAFETY: 调用方持有 NET_STATE 锁; SOCKET_SET 在 init_sockets 后已
        // 初始化, 锁域内无并发可变借用 (同 process_dhcp_events 契约).
        let sockets = unsafe { &mut *raw::socket_set() };
        let socket = sockets.get_mut::<smoltcp_dns::Socket>(handle);
        // smoltcp 0.14: start_query 的 cx 是 iface::Context (= InterfaceInner,
        // rand 源: txid/源端口), 经 Interface::context() 取, 非 core::task;
        // 查询类型公开路径为 wire::DnsQueryType (socket::dns 不再导出 Type).
        socket
            .start_query(stack.iface.context(), name, DnsQueryType::A)
            .ok()?
    };

    // 阶段 2: 锁外驱动等待 (P-27 锁域慢 IO 教训: 等待期间绝不持 NET_STATE)
    for _round in 0..DNS_QUERY_MAX_ROUNDS {
        // SAFETY: 进程上下文非中断; NET_STATE 由 poll_network 内部 try_lock
        // 自保护; device/socket_set 在 NET_READY=true 后已初始化 (调用方契约).
        unsafe { super::poll_network() };
        for _ in 0..DNS_QUERY_SPIN {
            core::hint::spin_loop();
        }
        let _guard = NET_STATE.lock();
        let Some(handle) = raw::dns_handle() else {
            return None;
        };
        // SAFETY: 持 NET_STATE 锁; SOCKET_SET 已初始化 (NET_READY=true 契约).
        let sockets = unsafe { &mut *raw::socket_set() };
        let socket = sockets.get_mut::<smoltcp_dns::Socket>(handle);
        match socket.get_query_result(query) {
            Ok(addrs) => {
                // SIMPLIFIED: 仅取首个 IPv4 地址; 影响面: AAAA/IPv6 应答被忽略
                // (查询本就限定 Type::A, 正常不会出 v6 记录); 何时扩展: 双栈
                // 解析需求出现时返多地址.
                return addrs.iter().find_map(|ip| match ip {
                    IpAddress::Ipv4(v4) => Some(v4.octets()),
                    IpAddress::Ipv6(_) => None,
                });
            }
            Err(smoltcp_dns::GetQueryResultError::Pending) => {}
            Err(smoltcp_dns::GetQueryResultError::Failed) => {
                // slot 已由 get_query_result 释放 (smoltcp 契约)
                return None;
            }
        }
    }

    // 超时: 显式释放查询槽 (否则 DNS_QUERY_SLOTS 个槽泄漏后永久 NoFreeSlot)
    let _guard = NET_STATE.lock();
    if let Some(handle) = raw::dns_handle() {
        // SAFETY: 持 NET_STATE 锁; SOCKET_SET 已初始化 (NET_READY=true 契约).
        let sockets = unsafe { &mut *raw::socket_set() };
        sockets
            .get_mut::<smoltcp_dns::Socket>(handle)
            .cancel_query(query);
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
