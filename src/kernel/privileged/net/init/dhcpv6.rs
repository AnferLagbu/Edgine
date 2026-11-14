#![deny(missing_docs)]
//! `DHCPv6` 机制封装 (P6b Phase 4): UDP 字节管道 + 地址落表 + 反转钩子.
//!
//! smoltcp 0.14 无 `DHCPv6` socket, 故客户端状态机由 functions 层自持
//! (`functions::net::dhcpv6`). 本模块只提供 privileged 侧机制:
//! - 创建绑定 546 端口、加入 `ff02::1:2` 组播的 UDP socket;
//! - 以 [`Dhcpv6PollCtx`] 向 functions 暴露 safe 的收/发/落表字节级 API
//!   (F2 类型擦除: 不泄漏任何 smoltcp 类型);
//! - 持擦除 `fn` 钩子 (仿 `net_register_functions_driver`), 由 `poll_network`
//!   在 `NET_STATE` 锁内回调, 驱动 functions 客户端 (依赖反转, 避免 F3 循环).
//!
//! 本模块全部走 smoltcp safe API + raw 安全访问器, 无 unsafe.

use alloc::boxed::Box;

use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::raw as raw_socket;
use smoltcp::socket::udp;
use smoltcp::wire::{
    IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, IpProtocol, IpVersion, Ipv6Address, Ipv6Cidr,
};

use super::{NetworkStack, raw};
use crate::privileged::sync::OnceLock;

/// `DHCPv6` 客户端监听端口 (RFC 8415 §7.1).
pub const DHCPV6_CLIENT_PORT: u16 = 546;
/// `DHCPv6` 服务器/中继监听端口 (RFC 8415 §7.1).
pub const DHCPV6_SERVER_PORT: u16 = 547;
/// `All_DHCP_Relay_Agents_and_Servers` 组播地址 `ff02::1:2` (RFC 8415 §7.1).
pub const ALL_DHCPV6_MULTICAST: [u8; 16] =
    [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0x02];

/// `DHCPv6` UDP socket 收发缓冲字节数.
const DHCPV6_BUF_SIZE: usize = 2048;
/// `DHCPv6` UDP socket 收发元数据槽数.
const DHCPV6_META_COUNT: usize = 4;
/// RA 捕获 raw `ICMPv6` socket 收发缓冲字节数 (覆盖标准 MTU 内的 `ICMPv6` 报文).
const RA_BUF_SIZE: usize = 1500;
/// RA 捕获 raw `ICMPv6` socket 收发元数据槽数.
const RA_META_COUNT: usize = 4;

/// 创建 `DHCPv6` UDP socket 并加入组播组, 返回 socket 句柄.
///
/// 绑定 546 端口 (任意地址), hop limit 置 255 (RFC 8415 §7.1: 链路本地组播
/// 不被路由转发), 加入 `ff02::1:2`. 缓冲经 `Box::leak` 取 `'static`
/// (同 `net_init_open_sockets` 的 DNS 查询槽先例).
///
/// 调用方须持有 `NET_STATE` 锁, 且 `iface` 已完成初始化.
pub fn open_socket(sockets: &mut SocketSet<'static>, iface: &mut Interface) -> SocketHandle {
    let rx_meta = Box::leak(Box::new([udp::PacketMetadata::EMPTY; DHCPV6_META_COUNT]));
    let tx_meta = Box::leak(Box::new([udp::PacketMetadata::EMPTY; DHCPV6_META_COUNT]));
    let rx_payload = Box::leak(alloc::vec![0u8; DHCPV6_BUF_SIZE].into_boxed_slice());
    let tx_payload = Box::leak(alloc::vec![0u8; DHCPV6_BUF_SIZE].into_boxed_slice());
    let mut sock = udp::Socket::new(
        udp::PacketBuffer::new(&mut rx_meta[..], rx_payload),
        udp::PacketBuffer::new(&mut tx_meta[..], tx_payload),
    );
    // 绑定失败仅发生于端口已占用; 本 socket 由本函数唯一创建, 546 不会冲突.
    let _ = sock.bind(IpListenEndpoint {
        addr: None,
        port: DHCPV6_CLIENT_PORT,
    });
    sock.set_hop_limit(Some(255));
    let handle = sockets.add(sock);
    // 组表满时加入失败仍保留 socket: 单播 Renew 可用, 组播发现降级 (重传兜底).
    let _ = iface.join_multicast_group(Ipv6Address::from_octets(ALL_DHCPV6_MULTICAST));
    handle
}

/// 创建 RA 捕获 raw `ICMPv6` socket, 返回 socket 句柄 (P6b Phase 5).
///
/// 绑定 `IpVersion::Ipv6` + `IpProtocol::Icmpv6`, 捕获全部 `ICMPv6` 报文 (含 RA);
/// functions 侧 `functions::net::dhcpv6::ra::parse` 过滤 type 134 并提取 M/O 标志.
/// raw socket 投递的是重序列化后的**完整 IPv6 报文** (40 字节头 + `ICMPv6` 载荷),
/// 且不影响 smoltcp 内部 `ICMPv6` 处理 (SLAAC/ND 照常).
/// 缓冲经 `Box::leak` 取 `'static` (同 [`open_socket`]).
///
/// 调用方须持有 `NET_STATE` 锁.
pub fn open_ra_socket(sockets: &mut SocketSet<'static>) -> SocketHandle {
    let rx_meta = Box::leak(Box::new([raw_socket::PacketMetadata::EMPTY; RA_META_COUNT]));
    let tx_meta = Box::leak(Box::new([raw_socket::PacketMetadata::EMPTY; RA_META_COUNT]));
    let rx_payload = Box::leak(alloc::vec![0u8; RA_BUF_SIZE].into_boxed_slice());
    let tx_payload = Box::leak(alloc::vec![0u8; RA_BUF_SIZE].into_boxed_slice());
    let sock = raw_socket::Socket::new(
        Some(IpVersion::Ipv6),
        Some(IpProtocol::Icmpv6),
        raw_socket::PacketBuffer::new(&mut rx_meta[..], rx_payload),
        raw_socket::PacketBuffer::new(&mut tx_meta[..], tx_payload),
    );
    sockets.add(sock)
}

/// `DHCPv6` 轮询上下文: 向 functions 客户端暴露 safe 字节级机制.
///
/// 由 [`drive_dhcpv6`] 在 `NET_STATE` 锁内构造并交给注册钩子, 生命周期限于
/// 单次 poll. 字段私有, 所有方法不泄漏 smoltcp 类型 (F2 类型擦除).
pub struct Dhcpv6PollCtx<'a> {
    sockets: &'a mut SocketSet<'static>,
    iface: &'a mut Interface,
    udp_handle: SocketHandle,
    ra_handle: Option<SocketHandle>,
    now_ms: u64,
    mac: [u8; 6],
}

impl Dhcpv6PollCtx<'_> {
    /// 当前单调时钟 (毫秒), 由 privileged 供给.
    #[inline]
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// 网卡 MAC 地址 (用于派生 `DUID-LL`).
    #[inline]
    #[must_use]
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// 从 `DHCPv6` UDP socket 收一条报文, 返回 (字节数, 源 IPv6 地址).
    ///
    /// 无报文可读时返回 `None`; 源地址非 IPv6 时地址置 `None` (`DHCPv6` 仅 IPv6).
    pub fn recv(&mut self, buf: &mut [u8]) -> Option<(usize, Option<[u8; 16]>)> {
        let sock = self.sockets.get_mut::<udp::Socket>(self.udp_handle);
        let (n, meta) = sock.recv_slice(buf).ok()?;
        let src = match meta.endpoint.addr {
            IpAddress::Ipv6(a) => Some(a.octets()),
            IpAddress::Ipv4(_) => None,
        };
        Some((n, src))
    }

    /// 从 RA 捕获 raw `ICMPv6` socket 收一条**完整 IPv6 报文**, 返回字节数.
    ///
    /// 无报文可读 (或句柄未就绪) 时返回 `None`. 投递内容含 40 字节 IPv6 头 +
    /// `ICMPv6` 载荷, 由 functions 侧 `ra::parse` 过滤 RA 并提取 M/O 标志.
    /// 捕获全部 `ICMPv6` (非 RA 报文由解析侧丢弃), 不影响 smoltcp 内部 SLAAC/ND.
    pub fn recv_ra(&mut self, buf: &mut [u8]) -> Option<usize> {
        let handle = self.ra_handle?;
        let sock = self.sockets.get_mut::<raw_socket::Socket>(handle);
        sock.recv_slice(buf).ok()
    }

    /// 向指定 IPv6 目的地址发送 `DHCPv6` 报文 (UDP 目的端口 547).
    ///
    /// 发送失败 (缓冲满) 时静默丢弃, 由客户端重传机制兜底.
    pub fn send(&mut self, data: &[u8], dest: [u8; 16]) {
        let sock = self.sockets.get_mut::<udp::Socket>(self.udp_handle);
        let endpoint = IpEndpoint {
            addr: IpAddress::Ipv6(Ipv6Address::from_octets(dest)),
            port: DHCPV6_SERVER_PORT,
        };
        let _ = sock.send_slice(data, endpoint);
    }

    /// 安装或移除一个 /128 IPv6 地址 (`DHCPv6` `IA_NA` 地址落表).
    ///
    /// `remove == false` 安装 (地址池满时告警); `remove == true` 移除匹配地址.
    pub fn apply_addr(&mut self, ipv6: [u8; 16], remove: bool) {
        let addr = Ipv6Address::from_octets(ipv6);
        self.iface.update_ip_addrs(|addrs| {
            if remove {
                let mut idx = addrs.len();
                while idx > 0 {
                    idx -= 1;
                    if let IpCidr::Ipv6(v6) = addrs[idx]
                        && v6.address() == addr
                        && v6.prefix_len() == 128
                    {
                        let _ = addrs.swap_remove(idx);
                    }
                }
            } else {
                let cidr = IpCidr::Ipv6(Ipv6Cidr::new(addr, 128));
                if addrs.push(cidr).is_err() {
                    crate::klog_warn!(Net, "DHCPv6: iface 地址池已满, {} 未落 iface", cidr);
                }
            }
        });
    }
}

/// 擦除的 `DHCPv6` 轮询钩子 (functions 侧注册, 仿 `net_register_functions_driver`).
static DHCPV6_POLL_HOOK: OnceLock<fn(&mut Dhcpv6PollCtx<'_>)> = OnceLock::new();

/// 注册 `DHCPv6` 轮询钩子 (由 `functions::net::init` 调用一次).
///
/// 钩子在 `poll_network` 的 `NET_STATE` 锁内被回调, 驱动 functions 客户端.
pub fn net_register_dhcpv6_poll_hook(hook: fn(&mut Dhcpv6PollCtx<'_>)) {
    let _ = DHCPV6_POLL_HOOK.set(hook);
}

/// 在 `NET_STATE` 锁内驱动 `DHCPv6` 客户端 (反转钩子).
///
/// 读取 `DHCPv6` UDP 句柄与 RA 捕获句柄, 构造 [`Dhcpv6PollCtx`] 回调注册钩子.
/// UDP 句柄未就绪或钩子未注册时直接返回 (RA 句柄缺失仅降级为无 RA 触发).
/// 调用方须持有 `NET_STATE` 锁.
pub fn drive_dhcpv6(sockets: &mut SocketSet<'static>, stack: &mut NetworkStack, now_ms: u64) {
    let Some(udp_handle) = raw::dhcpv6_udp_handle() else {
        return;
    };
    let Some(hook) = DHCPV6_POLL_HOOK.get() else {
        return;
    };
    let ra_handle = raw::dhcpv6_ra_handle();
    let mac = stack.mac;
    let mut ctx = Dhcpv6PollCtx {
        sockets,
        iface: &mut stack.iface,
        udp_handle,
        ra_handle,
        now_ms,
        mac,
    };
    hook(&mut ctx);
}
