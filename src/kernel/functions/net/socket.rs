#![deny(unsafe_code)]
//! Socket 子系统 — functions 层安全代理
//!
//! ## 状态 (v2.8, 2026-06-04)
//!
//! Phase 2.4 net 2/4 子系统迁移: 封装 `kernel::net::init::sm_*` 12 个 FFI:
//! - [x] socket/bind/listen/accept/connect (TCP 客户端/服务器)
//! - [x] send/recv (TCP 字节流)
//! - [x] sendto/recvfrom (UDP 数据报)
//! - [x] 关闭/设置选项/获取选项 (close/setsockopt/getsockopt)
//! - [x] `poll_sockets`
//!
//! ## 迁移方法
//!
//! 1. `unsafe extern "C" fn sm_*(...) -> i32` → `safe fn (...) -> Result<_, SocketError>`
//! 2. 内部 unsafe 块带 SAFETY 注释, 委托给 smoltcp 协议栈
//! 3. `&[u8]` / `&mut [u8]` 切片替代 `*const u8` / `*mut u8` 裸指针
//! 4. Socket 类型 (`1=TCP`, `2=UDP`) 改为强类型枚举
//!
//! 评估日期: 2026-06-04

use super::net_stack;
use crate::privileged::net::iface_trait::{Ipv4Addr, NetEndpoint};

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
/// 成功返回新 socket 的 FD, 失败返回 `SocketError`。
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; 协议不支持或资源耗尽等底层创建失败时返回对应的 `SocketError`。
pub fn socket(domain: Domain, sock_type: SockType, _protocol: i32) -> SocketResult<i32> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.socket_create_fd(domain as i32, sock_type as i32)
        .map_err(map_net_error)
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
/// POSIX `bind(fd, addr, addrlen)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效、地址非法或地址已被占用等底层错误被映射为对应的 `SocketError`。
pub fn bind(fd: i32, addr: &SockAddrIn) -> SocketResult<()> {
    let ep = NetEndpoint::new_v4(Ipv4Addr::from_octets(addr.ip), addr.port);
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.bind_fd(fd, ep).map_err(map_net_error)
}

/// POSIX `listen(fd, backlog)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 未绑定或非流式 socket 等底层错误被映射为对应的 `SocketError`。
pub fn listen(fd: i32, backlog: i32) -> SocketResult<()> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.listen_fd(fd, backlog).map_err(map_net_error)
}

/// POSIX `accept(fd, addr, addrlen)` — 返回新连接的 FD
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 非监听状态或完成队列为空等底层错误被映射为对应的 `SocketError`。
pub fn accept(fd: i32) -> SocketResult<i32> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.accept_fd(fd).map_err(map_net_error)
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
/// POSIX `connect(fd, addr, addrlen)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效或连接失败等底层错误被映射为对应的 `SocketError`。
pub fn connect(fd: i32, addr: &SockAddrIn) -> SocketResult<()> {
    let ep = NetEndpoint::new_v4(Ipv4Addr::from_octets(addr.ip), addr.port);
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.connect_fd(fd, ep).map_err(map_net_error)
}

/// POSIX `send(fd, buf, len, flags)` → 实际发送字节数
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 非活动 socket 或发送失败等底层错误被映射为对应的 `SocketError`。
pub fn send(fd: i32, buf: &[u8]) -> SocketResult<usize> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.send_fd(fd, buf).map_err(map_net_error)
}

/// POSIX `recv(fd, buf, len, flags)` → 实际接收字节数
///
/// 写入 `out` 切片, 返回字节数; `WouldBlock` 表示无数据可读。
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效或接收失败 (含无数据可读时的 `WouldBlock`) 等底层错误被映射为对应的 `SocketError`。
pub fn recv(fd: i32, out: &mut [u8]) -> SocketResult<usize> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.recv_fd(fd, out).map_err(map_net_error)
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
/// POSIX `sendto(fd, buf, len, flags, dest_addr, addrlen)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效或发送失败等底层错误被映射为对应的 `SocketError`。
pub fn sendto(fd: i32, buf: &[u8], dest: &SockAddrIn) -> SocketResult<usize> {
    let ep = NetEndpoint::new_v4(Ipv4Addr::from_octets(dest.ip), dest.port);
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.sendto_fd(fd, buf, ep).map_err(map_net_error)
}

/// POSIX `recvfrom(fd, buf, len, flags, src_addr, addrlen)` → (字节数, 源地址)
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效或接收失败等底层错误被映射为对应的 `SocketError`; 源地址非 IPv4 时返回 `SocketError::AddrFamilyNotSupported`。
pub fn recvfrom(fd: i32, out: &mut [u8]) -> SocketResult<(usize, SockAddrIn)> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    let (n, ep) = s.recvfrom_fd(fd, out).map_err(map_net_error)?;
    // 当前 functions 层 SockAddrIn 仅承载 IPv4; IPv6 源地址在 Phase 4 引入 sockaddr_in6 后支持
    let addr = match ep.addr.as_v4() {
        Some(v4) => SockAddrIn::new(ep.port, v4.octets()),
        None => return Err(SocketError::AddrFamilyNotSupported),
    };
    Ok((n, addr))
}

/// POSIX `close(fd)`
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效或底层关闭失败时返回对应的 `SocketError`。
pub fn close(fd: i32) -> SocketResult<()> {
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let mut s = s.lock();
    s.close_fd(fd).map_err(map_net_error)
}

/// POSIX `setsockopt(fd, level, optname, optval, optlen)`
///
/// # 参数
/// - `level`: 协议层 (e.g. `1` = `SOL_SOCKET`)
/// - `optname`: 选项名
/// - `val`: 选项值 (u32)
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效或选项不被支持等底层错误被映射为对应的 `SocketError`。
pub fn setsockopt(fd: i32, level: i32, optname: i32, val: u32) -> SocketResult<()> {
    let val_bytes = val.to_ne_bytes();
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.setsockopt_fd(fd, level, optname, &val_bytes)
        .map_err(map_net_error)
}

/// POSIX `getsockopt(fd, level, optname, optval, optlen)` → u32
///
/// # Errors
///
/// 当网络栈尚未就绪时返回 `SocketError::NotReady`; `fd` 无效或底层 `getsockopt` 失败时返回对应的 `SocketError`。
pub fn getsockopt(fd: i32, level: i32, optname: i32) -> SocketResult<u32> {
    let mut buf = [0u8; 4];
    let s = net_stack().ok_or(SocketError::NotReady)?;
    let s = s.lock();
    s.getsockopt_fd(fd, level, optname, &mut buf)
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
    let s = s.lock();
    s.poll_all_fd().map_err(map_net_error)?;
    Ok(0)
}

// ============================================================================
// 便利: 字符串 IP 解析
// ============================================================================

/// 解析 "a.b.c.d" 格式 IP 字符串
pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut parts = s.split('.');
    let mut out = [0u8; 4];
    for i in 0..4 {
        let p = parts.next()?;
        let v: u32 = p.parse().ok()?;
        if v > 255 {
            return None;
        }
        out[i] = v as u8;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// 构造 `SockAddrIn` from IP 字符串 + 端口
pub fn endpoint_from_str(ip: &str, port: u16) -> Option<SockAddrIn> {
    let bytes = parse_ipv4(ip)?;
    Some(SockAddrIn::new(port, bytes))
}
