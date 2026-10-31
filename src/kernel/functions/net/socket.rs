#![deny(unsafe_code)]
//! Socket 子系统 — functions 层安全代理
//!
//! ## 定位 (P-18 单入口)
//!
//! 本文件是 `NetStack` trait 的**唯一能力入口**消费方: 全部操作以
//! [`SocketHandle`] (不透明句柄) 为寻址键, 经 `SmoltcpNetStack` 的 trait
//! 实现翻译到 privileged `sm_net_*` 安全代理. 历史上的 inherent `*_fd`
//! 平行入口已退役 —— 同一能力不再有两份翻译与两套自校验.
//!
//! ## 覆盖能力
//!
//! - [x] socket/bind/listen/accept/connect (TCP 客户端/服务器)
//! - [x] send/recv (TCP 字节流)
//! - [x] sendto/recvfrom (UDP 数据报)
//! - [x] 关闭/设置选项/获取选项 (close/setsockopt/getsockopt)
//! - [x] `poll_sockets`
//!
//! 评估日期: 2026-06-04

use super::net_stack;
use crate::privileged::net::iface_trait::{Ipv4Addr, NetEndpoint, NetStack, SocketKind};

/// 网络栈 Socket 句柄 (由 `NetStack::socket_open` 分配, 经 `close` 释放).
pub use crate::privileged::net::iface_trait::SocketHandle;

// ============================================================================
// 错误
// ============================================================================

/// Socket 错误 (TD-08: 改为统一 `KernelError` 的 type alias, 单一来源).
///
/// 历史: `SocketError` 自带 17 个字段 (与 `UnixSocketError` 高度重叠).
/// 现在所有共享错误统一在 `functions::error::KernelError`, `SocketError` 仅保留别名.
/// 子系统特有错误 (无 — INET socket 全部错误都在 `KernelError`) 用 0 字段表达.
pub use crate::functions::error::KernelError as SocketError;

/// functions 层结果类型
pub type SocketResult<T> = Result<T, SocketError>;

/// 将 privileged 层的 `NetError` 精确映射为 `KernelError`
fn map_net_error(e: crate::privileged::net::iface_trait::NetError) -> SocketError {
    use crate::privileged::net::iface_trait::NetError;
    match e {
        NetError::NoFreeSocket => SocketError::ProcessFileLimit,
        NetError::InvalidHandle => SocketError::BadFd,
        NetError::BadConfig => SocketError::InvalidArgument,
        NetError::NotReady => SocketError::NotReady,
        NetError::Timeout => SocketError::WouldBlock,
        NetError::BufferTooSmall => SocketError::NoMemory,
        NetError::Other => SocketError::Io,
    }
}

// ============================================================================
// 协议 / 类型
// ============================================================================

// DECISION-J (2026-09-13): wire 类型 (Domain/SockType/SockAddrIn) 迁回
// `privileged::net::socket_types` — 用户态 ABI 协议类型由 privileged TCB raw
// 桥接 (net/syscall.rs) 从用户内存构造, 属机制的安全导出面。此处 re-export
// 保持 API 兼容 (functions→privileged 合法方向)。
pub use crate::privileged::net::socket_types::{Domain, SockAddrIn, SockType};

// ============================================================================
// Socket API
// ============================================================================

/// POSIX `socket(domain, type, protocol)`
///
/// 成功返回新 socket 的句柄, 失败返回 `SocketError`。
///
/// # Errors
///
/// 当协议族/类型组合不被内核 smoltcp 后端支持时返回
/// `SocketError::AddrFamilyNotSupported`; 网络栈尚未就绪时返回
/// `SocketError::NotReady`; 资源耗尽等底层创建失败时返回对应的 `SocketError`。
pub fn socket(domain: Domain, sock_type: SockType, _protocol: i32) -> SocketResult<SocketHandle> {
    let kind = socket_kind(domain, sock_type)?;
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.socket_open(kind).map_err(map_net_error)
}

/// 协议族 + socket 类型 → 抽象层 `SocketKind` (先校验再触达协议栈).
fn socket_kind(domain: Domain, sock_type: SockType) -> SocketResult<SocketKind> {
    if domain == Domain::Unix {
        // UDS 走独立的 `functions::net::unix` 子系统, 不经 smoltcp 后端.
        return Err(SocketError::AddrFamilyNotSupported);
    }
    Ok(match sock_type {
        SockType::Stream => SocketKind::Tcp,
        SockType::Dgram => SocketKind::Udp,
    })
}

/// POSIX `bind(handle, addr, addrlen)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效、地址非法或地址已被占用等底层错误被映射为对应的 `SocketError`。
pub fn bind(h: SocketHandle, addr: SockAddrIn) -> SocketResult<()> {
    let ep = NetEndpoint::new_v4(Ipv4Addr::from_octets(addr.ip), addr.port);
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.bind(h, ep).map_err(map_net_error)
}

/// POSIX `listen(handle, backlog)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效、未绑定或非流式 socket 等底层错误被映射为对应的 `SocketError`。
pub fn listen(h: SocketHandle, backlog: i32) -> SocketResult<()> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.listen(h, backlog).map_err(map_net_error)
}

/// POSIX `accept(handle, addr, addrlen)` — 返回新连接的句柄
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄非监听 socket、完成队列为空或句柄表已满等底层错误被映射为对应的 `SocketError`。
pub fn accept(h: SocketHandle) -> SocketResult<SocketHandle> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.accept(h, None).map_err(map_net_error)
}

/// POSIX `connect(handle, addr, addrlen)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效或连接失败等底层错误被映射为对应的 `SocketError`。
pub fn connect(h: SocketHandle, addr: SockAddrIn) -> SocketResult<()> {
    let ep = NetEndpoint::new_v4(Ipv4Addr::from_octets(addr.ip), addr.port);
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.connect(h, ep).map_err(map_net_error)
}

/// POSIX `send(handle, buf, len, flags)` → 实际发送字节数
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效或发送失败等底层错误被映射为对应的 `SocketError`。
pub fn send(h: SocketHandle, buf: &[u8]) -> SocketResult<usize> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.send(h, buf, 0).map_err(map_net_error)
}

/// POSIX `recv(handle, buf, len, flags)` → 实际接收字节数
///
/// 写入 `out` 切片, 返回字节数; `WouldBlock` 表示无数据可读。
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效或接收失败 (含无数据可读时的 `WouldBlock`) 等底层错误被映射为对应的 `SocketError`。
pub fn recv(h: SocketHandle, out: &mut [u8]) -> SocketResult<usize> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.recv(h, out, 0).map_err(map_net_error)
}

/// POSIX `sendto(handle, buf, len, flags, dest_addr, addrlen)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效或发送失败等底层错误被映射为对应的 `SocketError`。
pub fn sendto(h: SocketHandle, buf: &[u8], dest: SockAddrIn) -> SocketResult<usize> {
    let ep = NetEndpoint::new_v4(Ipv4Addr::from_octets(dest.ip), dest.port);
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.sendto(h, buf, 0, ep).map_err(map_net_error)
}

/// POSIX `recvfrom(handle, buf, len, flags, src_addr, addrlen)` → (字节数, 源地址)
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效或接收失败等底层错误被映射为对应的 `SocketError`; 源地址非 IPv4 时返回 `SocketError::AddrFamilyNotSupported`。
pub fn recvfrom(h: SocketHandle, out: &mut [u8]) -> SocketResult<(usize, SockAddrIn)> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    let mut ep = NetEndpoint::UNSPECIFIED;
    let n = s
        .recvfrom(h, out, 0, Some(&mut ep))
        .map_err(map_net_error)?;
    // 当前 functions 层 SockAddrIn 仅承载 IPv4; IPv6 源地址在 Phase 4 引入 sockaddr_in6 后支持
    let addr = match ep.addr.as_v4() {
        Some(v4) => SockAddrIn::new(ep.port, v4.octets()),
        None => return Err(SocketError::AddrFamilyNotSupported),
    };
    Ok((n, addr))
}

/// POSIX `close(handle)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 底层关闭失败时返回对应的 `SocketError`。
pub fn close(h: SocketHandle) -> SocketResult<()> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.close(h).map_err(map_net_error)
}

/// POSIX `setsockopt(handle, level, optname, optval, optlen)`
///
/// # 参数
/// - `level`: 协议层 (e.g. `1` = `SOL_SOCKET`)
/// - `optname`: 选项名
/// - `val`: 选项值 (u32)
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效或选项不被支持等底层错误被映射为对应的 `SocketError`。
pub fn setsockopt(h: SocketHandle, level: i32, optname: i32, val: u32) -> SocketResult<()> {
    let val_bytes = val.to_ne_bytes();
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.setsockopt(h, level, optname, &val_bytes)
        .map_err(map_net_error)
}

/// POSIX `getsockopt(handle, level, optname, optval, optlen)` → u32
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 句柄无效或底层 `getsockopt` 失败时返回对应的 `SocketError`。
pub fn getsockopt(h: SocketHandle, level: i32, optname: i32) -> SocketResult<u32> {
    let mut buf = [0u8; 4];
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.getsockopt(h, level, optname, &mut buf)
        .map_err(map_net_error)?;
    Ok(u32::from_ne_bytes(buf))
}

/// 轮询所有 socket (驱动事件分发)
///
/// 由 timer ISR 或专用网络任务周期性调用。
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 底层轮询失败时返回对应的 `SocketError`。
pub fn poll_all() -> SocketResult<i32> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    // 保持既有语义: 轮询期间持有 functions 层网络栈互斥锁
    let mut s = s.lock();
    s.poll_sockets().map_err(map_net_error)?;
    Ok(0)
}

/// POSIX `poll` 单 fd 就绪快照 (D8b): 委托 privileged `net_socket::sm_socket_poll`.
///
/// 返回 revents 位掩码 (POLLIN/POLLOUT/POLLHUP/POLLERR/POLLNVAL); 非阻塞、无副作用.
/// 这是终态 `File::poll` 的数值 fd 前影 (见 `docs/design/fd-object-model-design.md` 第四节);
/// `net_socket` 为 privileged 顶层安全代理, 与 `open.rs` 直调 `sm_close` 同路径.
#[must_use]
pub fn poll_fd(fd: i32, events: i16) -> i16 {
    crate::privileged::net_socket::sm_socket_poll(fd, events)
}

// ============================================================================
// 便利: 字符串 IP 解析
// ============================================================================

/// 解析 "a.b.c.d" 格式 IP 字符串
#[must_use]
pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut parts = s.split('.');
    let mut out = [0u8; 4];
    for octet in &mut out {
        let p = parts.next()?;
        let v: u32 = p.parse().ok()?;
        if v > 255 {
            return None;
        }
        *octet = crate::functions::intcast::low_u8_from_u32(v);
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// 构造 `SockAddrIn` from IP 字符串 + 端口
#[must_use]
pub fn endpoint_from_str(ip: &str, port: u16) -> Option<SockAddrIn> {
    let bytes = parse_ipv4(ip)?;
    Some(SockAddrIn::new(port, bytes))
}
