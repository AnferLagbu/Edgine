//! 静态 DNS 解析 + 真实 DNS 客户端查询 (B04-09 拆分 Step D; S2 接线 2026-10)
//!
//! 原 init.rs 内联定义: `HostEntry` / `STATIC_HOSTS` / `dns_resolve` /
//! `parse_ipv4_literal`. 抽出为独立子模块后, init.rs 通过
//! `pub use dns::*` re-export.
//!
//! S2 (docs/plan/archive/net-feature-alignment.md): `dns_resolve` 升级为三级解析 —— IPv4 字面量
//! → 静态 hosts → smoltcp `dns::Socket` 真实查询 (阻塞有界等待). 未初始化/
//! 无 server/查询失败均回退 `None`, 旧输入族的既有语义逐字保持.

// I-46: hosts 表里 10.0.2.x 引用集中常量, 避免散落硬编码
use core::sync::atomic::Ordering;

use smoltcp::socket::dns as smoltcp_dns;
use smoltcp::wire::{DnsQueryType, IpAddress, Ipv4Address, Ipv6Address};

use super::dns_wait;
use super::raw;
use super::state::{G_DNS, G_DNS6_PRESENT, NET_STATE};
use crate::privileged::net::iface_trait::{IpAddr, Ipv4Addr, Ipv6Addr};
use crate::privileged::net::types::{FALLBACK_DNS, FALLBACK_GATEWAY, FALLBACK_IPV4};
use crate::privileged::proc::{
    BlockReason, has_deliverable_signal, process_get_current_pid, scheduler_block,
    scheduler_schedule,
};
use crate::privileged::sync::in_irq_context;
use crate::privileged::timer::hrtimer_clock_read;

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

/// 简单 DNS 解析: 三级 —— IPv4 字面量 / 静态 hosts / 真实 DNS 查询 (双栈)
///
/// # 实现
/// - 精确匹配主机名 (不区分大小写 — ASCII tolower)
/// - 大小写不敏感: "Router" / "ROUTER" / "router" 都匹配
/// - 静态表未命中时发起 smoltcp DNS 查询 (仅进程上下文, 见 `dns_query_net` 契约)
/// - DNS 双栈 (docs/plan/dns-dual-stack.md Phase 2): 真实查询采 **IPv6 优先** ——
///   先 AAAA, 成功返 `IpAddr::V6`, 否则 A 返 `IpAddr::V4`; 静态 hosts / IPv4
///   字面量短路返 `IpAddr::V4`
/// - 未初始化/无 server/失败均返 `None`
///
/// # 局限 (SIMPLIFIED 口径)
/// - 不支持 search domain / 缓存; IPv6 字面量文本暂不解析 (仅 IPv4 字面量短路)
#[must_use]
pub fn dns_resolve(name: &str) -> Option<IpAddr> {
    for entry in STATIC_HOSTS {
        if entry.name.eq_ignore_ascii_case(name) {
            return Some(IpAddr::V4(Ipv4Addr(entry.ip)));
        }
    }
    // 数字字面量解析: "10.0.2.15" 直接返 (避免对 IP 字符串做 DNS 浪费)
    if let Some(ip) = parse_ipv4_literal(name) {
        return Some(IpAddr::V4(Ipv4Addr(ip)));
    }
    // 第三级: 真实 DNS 查询, IPv6 优先 (AAAA 成功返 V6, 否则 A 返 V4).
    // SIMPLIFIED: 顺序双查询 (先 AAAA 后 A) 而非并发; 影响面: AAAA 超时窗口后
    //   才发 A, 最坏 2× 有界自旋; 何时扩展: E2 异步化时改并发双查询省延迟.
    //   选顺序: AAAA 无记录时 smoltcp 立即返 Failed (非满窗超时), 实践中延迟
    //   与并发等价, 且复用既有单查询有界等待/取消机制 (外科手术, 低风险).
    if let Some(addr) = dns_query_net(name, DnsQueryType::Aaaa) {
        return Some(addr);
    }
    dns_query_net(name, DnsQueryType::A)
}

/// 把 DHCP 租约/回退的 DNS server 写入 smoltcp `dns::Socket`.
///
/// `leases` 为 `dhcp_dns_servers()` 输出 (全 0 = 无); IPv6 侧取 `raw::dns6_servers()`
/// (`DHCPv6` 下发). 合并排序委托 [`merge_dns_servers`] (IPv6 优先). 调用方须持
/// `NET_STATE` 锁并传入同一锁域内的 `sockets`.
pub(crate) fn apply_dns_servers(
    sockets: &mut smoltcp::iface::SocketSet<'_>,
    handle: Option<smoltcp::iface::SocketHandle>,
    leases: [[u8; 4]; 3],
) {
    let Some(handle) = handle else {
        return;
    };
    let (buf, n) = merge_dns_servers(&raw::dns6_servers(), leases);
    sockets
        .get_mut::<smoltcp_dns::Socket>(handle)
        .update_servers(&buf[..n]);
}

/// 合并 IPv6 + IPv4 DNS 服务器为 smoltcp 喂入列表 (纯函数, IPv6 优先, D3).
///
/// 返回 `(定长缓冲, 有效个数)`. 排序: IPv6 槽 (`dns6`) 排前, 其次非零 IPv4 租约
/// (`leases`), 两侧皆空落 [`FALLBACK_DNS`] (QEMU slirp 惯例). 缓冲容量 6 =
/// 3 IPv6 + 3 IPv4. smoltcp `DNS_MAX_SERVER_COUNT=1` 会在 `update_servers` 截断到
/// 首个 server, 故 **排序即优先级** —— 有 IPv6 server 时优先用它 (D3). server
/// 地址族与查询类型无关 (一个 IPv6 server 亦可应答 A 记录).
pub(crate) fn merge_dns_servers(
    dns6: &[Option<[u8; 16]>; 3],
    leases: [[u8; 4]; 3],
) -> ([IpAddress; 6], usize) {
    let mut buf = [IpAddress::Ipv4(Ipv4Address::UNSPECIFIED); 6];
    let mut n = 0usize;
    // IPv6 优先: DHCPv6 下发的 server 排在最前 (截断到 1 时即优先 IPv6).
    for slot in dns6 {
        if let Some(ip) = slot
            && n < buf.len()
        {
            buf[n] = IpAddress::Ipv6(Ipv6Address::from_octets(*ip));
            n += 1;
        }
    }
    for ip in leases {
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
    (buf, n)
}

/// 写入 `DHCPv6` 下发的 IPv6 DNS 服务器并重新合并喂入 smoltcp (DNS 双栈 Phase 1).
///
/// `servers` 为 `DHCPv6` 选项 23 解析出的 IPv6 地址列表 (最多取前 3 个); 空列表
/// 清空 IPv6 槽 (回退纯 IPv4). IPv4 侧沿用最近 DHCP 租约 (`raw::dhcp_dns_servers()`),
/// 与 [`apply_dns_servers`] 同源. 调用方须持 `NET_STATE` 锁并传入同一锁域内的
/// `sockets`. IPv6 优先合并语义见 [`apply_dns_servers`].
pub(crate) fn apply_dns6_servers(
    sockets: &mut smoltcp::iface::SocketSet<'_>,
    handle: Option<smoltcp::iface::SocketHandle>,
    servers: &[[u8; 16]],
) {
    let mut slots = [None; 3];
    for (slot, ip) in slots.iter_mut().zip(servers.iter()) {
        *slot = Some(*ip);
    }
    raw::set_dns6_servers(slots);
    // lock-free 短路观测标志: 有任一 IPv6 server 即置位 (供 dns_query_net 无锁判).
    G_DNS6_PRESENT.store(slots.iter().any(Option::is_some), Ordering::Release);
    apply_dns_servers(sockets, handle, raw::dhcp_dns_servers());
}

/// 真实 DNS 查询 (进程上下文真异步阻塞, boot/IRQ 回退有界自旋).
///
/// `qtype` 指定查询类型 (`A` / `Aaaa`); 结果按地址族映射为 `IpAddr`
/// (`Ipv4 -> V4` / `Ipv6 -> V6`). DNS 双栈 (Phase 2) 由 `dns_resolve` 先 AAAA
/// 后 A 顺序调用, IPv6 优先.
///
/// # 契约
/// - **进程上下文** (`pid != 0` 且非 IRQ): 提交查询后登记 `dns_wait` 电平重扫
///   等待表 → `scheduler_block` 让出 CPU, 由 `poll_network` 每 tick 消费 smoltcp
///   结果并唤醒 (E2 B2, 免 smoltcp async). 等待期间绝不持 `NET_STATE`.
/// - **boot/IRQ 上下文**: 回退有界自旋 (`poll_network` + spin), 不得阻塞 (无
///   进程可调度 / 中断上下文). `poll_network` 自行 `NET_STATE.try_lock`, 嵌套必死锁.
/// - 短路路径全部无锁: `NET_READY=false` (host 单测/boot 前) 与无 server
///   (`G_DNS[0]==0` 且 `G_DNS6_PRESENT=false`) 直接返 `None`, 不碰 socket.
fn dns_query_net(name: &str, qtype: DnsQueryType) -> Option<IpAddr> {
    if !crate::privileged::net::NET_READY.load(Ordering::Acquire) {
        return None;
    }
    if G_DNS[0].load(Ordering::Acquire) == 0 && !G_DNS6_PRESENT.load(Ordering::Acquire) {
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
            .start_query(stack.iface.context(), name, qtype)
            .ok()?
    };

    // 阶段 2: 进程上下文真异步阻塞; boot/IRQ 回退有界自旋.
    let pid = process_get_current_pid();
    if pid != 0 && !in_irq_context() {
        dns_query_blocking(query, pid)
    } else {
        dns_query_spin(query)
    }
}

/// 进程上下文阻塞查询 (E2 B2): 登记电平重扫等待表 → block 让出 CPU →
/// `poll_network` 每 tick 消费 smoltcp 结果并唤醒 → 醒来读表返回.
///
/// 结果由 poll 侧消费 (smoltcp `get_query_result` 完成即释放槽, 无非消费 peek),
/// 本函数只读 `dns_wait` 落表的结果. 丢唤醒由下一 tick 电平重扫自愈.
fn dns_query_blocking(query: smoltcp_dns::QueryHandle, pid: u32) -> Option<IpAddr> {
    let deadline = hrtimer_clock_read() + dns_wait::DNS_QUERY_TIMEOUT_NS;
    // 登记 (锁内). 表满 → 取消查询释放 smoltcp 槽, 回退 None.
    let idx = {
        let _guard = NET_STATE.lock();
        // SAFETY: 持 NET_STATE 锁 (dns_wait 唯一互斥域).
        unsafe { dns_wait::register(pid, query, deadline) }
    };
    let Some(idx) = idx else {
        let _guard = NET_STATE.lock();
        cancel_query_locked(query);
        return None;
    };
    loop {
        // 先让出 CPU (不持锁), 醒来后再锁内读表重判.
        scheduler_block(BlockReason::WaitingForIo);
        scheduler_schedule();
        let _guard = NET_STATE.lock();
        // SAFETY: 持 NET_STATE 锁.
        let (state, result) = unsafe { dns_wait::status(idx) };
        match state {
            dns_wait::DnsWaitState::Done => {
                // SAFETY: 持 NET_STATE 锁.
                unsafe { dns_wait::free(idx) };
                return result;
            }
            dns_wait::DnsWaitState::Failed | dns_wait::DnsWaitState::Timeout => {
                // SAFETY: 持 NET_STATE 锁.
                unsafe { dns_wait::free(idx) };
                return None;
            }
            dns_wait::DnsWaitState::Pending => {
                // 信号 / 死线自兜底 (poll 侧亦会到点取消): 取消查询 + 摘槽.
                if has_deliverable_signal(pid) || hrtimer_clock_read() >= deadline {
                    // SAFETY: 持 NET_STATE 锁; 状态 Pending ⇒ smoltcp 槽仍占用,
                    // cancel_query 不会命中已释放槽.
                    unsafe {
                        if let Some(h) = dns_wait::handle_of(idx) {
                            cancel_query_locked(h);
                        }
                        dns_wait::free(idx);
                    }
                    return None;
                }
                // 否则继续阻塞 (guard 随本轮结束释放, 下一轮重新 block).
            }
        }
    }
}

/// boot/IRQ 上下文回退: 有界轮次内 `poll_network` + 自旋, 自行消费结果.
///
/// 复用原同步范式 (无进程可调度时不能 block). 不登记 `dns_wait`, 故与 poll
/// 侧排空互不干扰. 超时显式取消查询释放槽 (否则 `DNS_QUERY_SLOTS` 槽泄漏).
fn dns_query_spin(query: smoltcp_dns::QueryHandle) -> Option<IpAddr> {
    for _round in 0..DNS_QUERY_MAX_ROUNDS {
        // SAFETY: boot/IRQ 上下文; NET_STATE 由 poll_network 内部 try_lock
        // 自保护; device/socket_set 在 NET_READY=true 后已初始化 (调用方契约).
        unsafe { super::poll_network() };
        for _ in 0..DNS_QUERY_SPIN {
            core::hint::spin_loop();
        }
        let _guard = NET_STATE.lock();
        let handle = raw::dns_handle()?;
        // SAFETY: 持 NET_STATE 锁; SOCKET_SET 已初始化 (NET_READY=true 契约).
        let sockets = unsafe { &mut *raw::socket_set() };
        let socket = sockets.get_mut::<smoltcp_dns::Socket>(handle);
        match socket.get_query_result(query) {
            Ok(addrs) => {
                return addrs
                    .iter()
                    .map(|ip| match ip {
                        IpAddress::Ipv4(v4) => IpAddr::V4(Ipv4Addr(v4.octets())),
                        IpAddress::Ipv6(v6) => IpAddr::V6(Ipv6Addr(v6.octets())),
                    })
                    .next();
            }
            Err(smoltcp_dns::GetQueryResultError::Pending) => {}
            Err(smoltcp_dns::GetQueryResultError::Failed) => {
                // slot 已由 get_query_result 释放 (smoltcp 契约)
                return None;
            }
        }
    }
    // 超时: 显式释放查询槽.
    let _guard = NET_STATE.lock();
    cancel_query_locked(query);
    None
}

/// 锁内取消查询释放 smoltcp 槽 (调用方须持 `NET_STATE`; 槽须仍占用).
fn cancel_query_locked(query: smoltcp_dns::QueryHandle) {
    if let Some(handle) = raw::dns_handle() {
        // SAFETY: 调用方持 NET_STATE 锁; SOCKET_SET 已初始化 (NET_READY 契约).
        let sockets = unsafe { &mut *raw::socket_set() };
        sockets
            .get_mut::<smoltcp_dns::Socket>(handle)
            .cancel_query(query);
    }
}

/// 解析 IPv4 字面量 "a.b.c.d" (无错处理; 不合法返 None)
#[must_use]
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
            octets[idx] = crate::privileged::glue::low_u8_from_u32(cur);
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
    octets[3] = crate::privileged::glue::low_u8_from_u32(cur);
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
#[must_use]
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
            crate::privileged::glue::low_u8_from_u32(v)
        }
        None => 24,
    };
    Some((ip, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    const V6_A: [u8; 16] = [
        0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88,
    ];
    const V6_B: [u8; 16] = [
        0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x44,
    ];

    fn v6(ip: [u8; 16]) -> IpAddress {
        IpAddress::Ipv6(Ipv6Address::from_octets(ip))
    }

    fn v4(a: [u8; 4]) -> IpAddress {
        IpAddress::Ipv4(Ipv4Address::new(a[0], a[1], a[2], a[3]))
    }

    /// D3: IPv6 server 排在 IPv4 之前 (smoltcp 截断到 1 时即优先 IPv6).
    #[test]
    fn merge_ipv6_before_ipv4() {
        let dns6 = [Some(V6_A), None, None];
        let leases = [[10, 0, 2, 3], [0; 4], [0; 4]];
        let (buf, n) = merge_dns_servers(&dns6, leases);
        assert_eq!(n, 2);
        assert_eq!(buf[0], v6(V6_A));
        assert_eq!(buf[1], v4([10, 0, 2, 3]));
    }

    /// 纯 IPv4 租约: 跳过全 0 槽, 保序.
    #[test]
    fn merge_ipv4_only_skips_zero() {
        let dns6 = [None; 3];
        let leases = [[10, 0, 2, 3], [0; 4], [8, 8, 8, 8]];
        let (buf, n) = merge_dns_servers(&dns6, leases);
        assert_eq!(n, 2);
        assert_eq!(buf[0], v4([10, 0, 2, 3]));
        assert_eq!(buf[1], v4([8, 8, 8, 8]));
    }

    /// 纯 IPv6 server (无 IPv4 租约): 多槽保序.
    #[test]
    fn merge_ipv6_only() {
        let dns6 = [Some(V6_A), Some(V6_B), None];
        let leases = [[0; 4]; 3];
        let (buf, n) = merge_dns_servers(&dns6, leases);
        assert_eq!(n, 2);
        assert_eq!(buf[0], v6(V6_A));
        assert_eq!(buf[1], v6(V6_B));
    }

    /// 两侧皆空: 落 FALLBACK_DNS (slirp 惯例), 单个 server.
    #[test]
    fn merge_empty_falls_back() {
        let (buf, n) = merge_dns_servers(&[None; 3], [[0; 4]; 3]);
        assert_eq!(n, 1);
        assert_eq!(buf[0], v4(FALLBACK_DNS));
    }

    /// 满容量: 3 IPv6 + 3 IPv4 = 6, 不越界且 IPv6 仍排前.
    #[test]
    fn merge_full_capacity() {
        let dns6 = [Some(V6_A), Some(V6_B), Some(V6_A)];
        let leases = [[10, 0, 2, 3], [8, 8, 8, 8], [1, 1, 1, 1]];
        let (buf, n) = merge_dns_servers(&dns6, leases);
        assert_eq!(n, 6);
        assert_eq!(buf[0], v6(V6_A));
        assert_eq!(buf[3], v4([10, 0, 2, 3]));
        assert_eq!(buf[5], v4([1, 1, 1, 1]));
    }
}
