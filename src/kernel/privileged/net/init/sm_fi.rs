//! Socket FFI 公共 API (sm_* 函数)
//!
//! 从 init.rs 拆分, 集中 POSIX socket FFI 实现:
//! `sm_socket、sm_bind、sm_listen、sm_accept、sm_connect、sm_send`、
//! `sm_recv、sm_sendto、sm_recvfrom、sm_sendmsg、sm_recvmsg、sm_close`、
//! `sm_setsockopt、sm_getsockopt、sm_getsockname、sm_getpeername`、
//! `sm_poll_sockets` 等函数.
//!
//! ## 依赖
//!
//! 通过 `use super::*` 访问 init.rs 的私有项 (`NET_STATE`, raw 模块, `socket_set`,
//! `parse_endpoint` 等). Rust 模块系统允许子模块访问父模块所有项.

use super::{
    MAX_SM_FD, NET_STATE, Ordering, get_max_sockets, is_network_initialized, process_dhcp_events,
    raw, socket_set,
};
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, Ipv6Address};

// ============================================================================
// POSIX errno 常量 (i32)
// ============================================================================
const E_BADF: i32 = 9;
const E_AGAIN: i32 = 11;
const E_NOMEM: i32 = 12;
const E_FAULT: i32 = 14;
const E_INVAL: i32 = 22;
const E_NFILE: i32 = 23;
const E_NOTSUPP: i32 = 95;
const E_AFNOSUPPORT: i32 = 97;
const E_ADDRINUSE: i32 = 98;
const E_CONNRESET: i32 = 104;
const E_NOTCONN: i32 = 107;
const E_CONNREFUSED: i32 = 111;
const E_NODEV: i32 = 19;
const E_NOPROTOOPT: i32 = 92;

// ============================================================================
// UDS setsockopt 策略注册契约 (DECISION-K 统一模式: 机制 init 后注册策略)
// ============================================================================

/// UDS `SO_PASSCRED` setsockopt 策略钩子 — functions UDS 实现注册, privileged 消费
///
/// privileged `sm_setsockopt` (机制 FFI) 识别 `SO_PASSCRED` 路由需求后,
/// 经由此钩子委托 `functions::net::unix::uds_setsockopt` (策略实现),
/// privileged 不反向依赖 functions 具象 UDS 模块 (第二十六批反转).
static UDS_SETOPT_HOOK: crate::privileged::sync::OnceLock<fn(i32, bool) -> i32> =
    crate::privileged::sync::OnceLock::new();

/// 注册 UDS setsockopt 策略钩子 (由 `functions::net::unix::uds_init` 调用)
///
/// # Errors
/// 钩子已被注册过时返回 Err (幂等语义由调用方忽略重复注册)。
pub fn register_uds_setsockopt_hook(
    hook: fn(i32, bool) -> i32,
) -> Result<(), fn(i32, bool) -> i32> {
    UDS_SETOPT_HOOK.set(hook)
}

// ============================================================================
// W4.4: smoltcp wire 类型 ↔ NetStack trait 抽象类型的翻译 helper
//
// 仅在 privileged 边界 (raw::qemu_net_skel 一类适配器, 或 boot 阶段从 MAC/IP
// 字面量构造 Interface::update_ip_addrs) 使用, functions 层访问地址一律走
// Ipv4Addr / Ipv4Cidr / NetEndpoint. 此处的 smoltcp wire 类型导入仅服务于
// 翻译函数本身.
//
// ## 与 W3.2 SmoltcpNetStack 的职责划分
//
// - SmoltcpNetStack::init / socket_open / dhcp_state: 服务层 trait API,
//   不暴露 smoltcp wire 类型.
// - 本模块的 wire_to_* / *_to_wire: 框架层内部适配器, 仅在
//   qemu_net_skel / update_ip_addrs 等 privileged 内部使用.
// ============================================================================

/// 把 trait 抽象的 `IpAddr` 翻译成 smoltcp 的 `IpAddress` (双栈, DECISION-032).
#[inline(always)]
pub(crate) fn wire_to_smol(a: crate::privileged::net::iface_trait::IpAddr) -> IpAddress {
    match a {
        crate::privileged::net::iface_trait::IpAddr::V4(v4) => {
            let o = v4.octets();
            IpAddress::Ipv4(Ipv4Address::new(o[0], o[1], o[2], o[3]))
        }
        crate::privileged::net::iface_trait::IpAddr::V6(v6) => {
            IpAddress::Ipv6(Ipv6Address::from_octets(v6.octets()))
        }
    }
}

/// 把 trait 抽象的 `NetEndpoint` 翻译成 smoltcp 的 `IpEndpoint`.
#[inline]
pub(crate) fn endpoint_to_smol(e: crate::privileged::net::iface_trait::NetEndpoint) -> IpEndpoint {
    IpEndpoint {
        addr: wire_to_smol(e.addr),
        port: e.port,
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 把 smoltcp 的 `IpEndpoint` 翻译回 trait 抽象的 `NetEndpoint`.
pub(crate) fn endpoint_from_smol(
    ep: IpEndpoint,
) -> Option<crate::privileged::net::iface_trait::NetEndpoint> {
    match ep.addr {
        IpAddress::Ipv4(v4) => Some(crate::privileged::net::iface_trait::NetEndpoint::new_v4(
            crate::privileged::net::iface_trait::Ipv4Addr::from_octets(v4.octets()),
            ep.port,
        )),
        IpAddress::Ipv6(v6) => Some(crate::privileged::net::iface_trait::NetEndpoint::new_v6(
            crate::privileged::net::iface_trait::Ipv6Addr::from_octets(v6.octets()),
            ep.port,
        )),
    }
}

/// 将 `NetEndpoint` 写入 sockaddr C 结构体 (供 recvfrom/getsockname/getpeername 返回地址).
///
/// 双栈 (DECISION-032): V4 写 `SockaddrIn` (16 字节), V6 写 `SockaddrIn6` (28 字节).
/// 调用方提供 `addr` 指针与 `addrlen` 指针:
/// - `addr` 为 NULL: 跳过写入 (调用方不关心对端地址)
/// - `addrlen` 为 NULL: 仅写 addr, 不回写长度
/// - 正常情况: 写入 sockaddr 并将 addrlen 更新为实际结构体大小 (16 / 28)
///
/// # Safety
/// `addr` 非空时必须指向至少 28 字节可写内存 (V6 路径); `addrlen` 非空时必须指向有效 u32.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
pub(crate) unsafe fn write_sockaddr(
    addr: *mut u8,
    addrlen: *mut u32,
    ep: &crate::privileged::net::iface_trait::NetEndpoint,
) {
    unsafe {
        if addr.is_null() {
            return;
        }
        match ep.addr {
            crate::privileged::net::iface_trait::IpAddr::V4(v4) => {
                let sin = SockaddrIn {
                    sin_family: 2, // AF_INET
                    sin_port: ep.port.to_be(),
                    sin_addr: v4.octets(),
                    sin_zero: [0; 8],
                };
                core::ptr::write(addr as *mut SockaddrIn, sin);
                if !addrlen.is_null() {
                    core::ptr::write(addrlen, core::mem::size_of::<SockaddrIn>() as u32);
                }
            }
            crate::privileged::net::iface_trait::IpAddr::V6(v6) => {
                let sin6 = SockaddrIn6 {
                    sin6_family: 10, // AF_INET6
                    sin6_port: ep.port.to_be(),
                    sin6_flowinfo: 0,
                    sin6_addr: v6.octets(),
                    sin6_scope_id: 0,
                };
                core::ptr::write(addr as *mut SockaddrIn6, sin6);
                if !addrlen.is_null() {
                    core::ptr::write(addrlen, core::mem::size_of::<SockaddrIn6>() as u32);
                }
            }
        }
    }
}

#[repr(C)]
struct SockaddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

/// POSIX `sockaddr_in6` (28 字节, `#[repr(C)]`, 与 Linux 布局一致).
#[repr(C)]
struct SockaddrIn6 {
    sin6_family: u16,
    sin6_port: u16,
    sin6_flowinfo: u32,
    sin6_addr: [u8; 16],
    sin6_scope_id: u32,
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
/// 从 sockaddr C 结构体解析端点 (W4.4 trait 翻译版本, 双栈).
///
/// 按 `sin_family` 分支: 2 (`AF_INET`) → `SockaddrIn` → V4 端点;
/// 10 (`AF_INET6`) → `SockaddrIn6` → V6 端点; 其余族返回 None.
///
/// 解析后**先**填充 trait 抽象的 `NetEndpoint`, 调用方按需通过
/// `endpoint_to_smol()` 翻译回 smoltcp `IpEndpoint`. 这一层翻译是
/// W4.4 目标: 让 sock 路径不直接持有 smoltcp wire 类型.
///
/// # Safety
/// `addr` 必须指向有效的 sockaddr 结构体, 至少含对应族所需的已初始化字节。
pub(crate) unsafe fn parse_endpoint_trait(
    addr: *const u8,
) -> Option<crate::privileged::net::iface_trait::NetEndpoint> {
    unsafe {
        if addr.is_null() {
            return None;
        }
        // 读取族字段 (前 2 字节, 主机字节序)
        let family = core::ptr::read_unaligned(addr as *const u16);
        match family {
            2 => {
                let sin = &*(addr as *const SockaddrIn);
                let octets = sin.sin_addr;
                let port = u16::from_be(sin.sin_port);
                Some(crate::privileged::net::iface_trait::NetEndpoint::new_v4(
                    crate::privileged::net::iface_trait::Ipv4Addr::from_octets(octets),
                    port,
                ))
            }
            10 => {
                let sin6 = &*(addr as *const SockaddrIn6);
                let octets = sin6.sin6_addr;
                let port = u16::from_be(sin6.sin6_port);
                Some(crate::privileged::net::iface_trait::NetEndpoint::new_v6(
                    crate::privileged::net::iface_trait::Ipv6Addr::from_octets(octets),
                    port,
                ))
            }
            _ => None,
        }
    }
}

/// 从 sockaddr C 结构体解析端点为 smoltcp `IpEndpoint` (双栈, DECISION-032).
///
/// 与 `parse_endpoint_trait` 的区别: 本函数返回 smoltcp wire 类型 `IpEndpoint`,
/// 供 `sm_bind`/`sm_connect`/`sm_sendto` 直接传给 smoltcp socket API;
/// `parse_endpoint_trait` 返回 trait 抽象 `NetEndpoint`, 供上层 (functions) 使用.
/// 两者按 `sin_family` 分支逻辑相同 (2=AF_INET / 10=AF_INET6).
///
/// # Safety
/// 同 `parse_endpoint_trait`.
pub(crate) unsafe fn parse_endpoint(addr: *const u8) -> Option<IpEndpoint> {
    unsafe { parse_endpoint_trait(addr).map(endpoint_to_smol) }
}

// ============================================================================
// Socket FFI 实现
// ============================================================================

/// POSIX `socket(domain, type, protocol)` 内核实现。
///
/// # Safety
/// - 由 `sys_socket` 系统调用分发, 参数由 syscall 层校验 (cred 检查)。
/// - 必须 `NET_LOCK` 持有。
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_socket(domain: i32, sock_type: i32, _protocol: i32) -> i32 {
    unsafe {
        if !is_network_initialized() {
            return -E_NODEV;
        }

        let _guard = NET_STATE.lock();

        // I-47: 检查活动 socket 上限 (≤ G_MAX_SOCKETS ≤ MAX_SOCKETS).
        // 运行时可通过 set_max_sockets 调整, 编译期上限 MAX_SOCKETS 静态保证.
        let active: usize = (0..MAX_SM_FD).filter(|&i| raw::fd_type(i) != 0).count();
        if active >= get_max_sockets() {
            return -E_NFILE;
        }

        // V2: 使用集中分配器获取 FD
        let fd = match crate::privileged::proc::fd_alloc::alloc_fd(
            crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp,
        ) {
            Some(f) => f,
            None => return -E_NFILE,
        };
        let fd_idx = fd as usize;

        // REVAL-W W4.2.3.3 (2026-06-25): sm_socket 路径迁移到 raw::socket_open_stub.
        // 删除 75 行重复 socket 构造代码, 统一走 raw 模块 (与 SmoltcpNetStack 共享).
        // 0 行为变更: sm_socket 仍返回 fd, k_malloc 失败仍返回 -E_NOMEM.
        // 双栈 (DECISION-032): AF_INET(2) 与 AF_INET6(10) 均创建同一 smoltcp socket
        // (smoltcp 层不区分 family, bind/connect 时按 sockaddr 族解析).
        let is_af = domain == 2 || domain == 10;
        if is_af && sock_type == 1 {
            // TCP — 委托 raw::socket_open_stub
            let sockets = &mut *socket_set();
            let kind = crate::privileged::net::iface_trait::SocketKind::Tcp;
            if raw::socket_open_stub(sockets, kind, fd_idx).is_none() {
                return -E_NOMEM;
            }
            fd
        } else if is_af && sock_type == 2 {
            // UDP — 委托 raw::socket_open_stub
            let sockets = &mut *socket_set();
            let kind = crate::privileged::net::iface_trait::SocketKind::Udp;
            if raw::socket_open_stub(sockets, kind, fd_idx).is_none() {
                return -E_NOMEM;
            }
            fd
        } else {
            -E_AFNOSUPPORT
        }
    }
}

/// POSIX `bind(fd, addr, addrlen)` 内核实现。
///
/// # Safety
/// - `addr` 必须是有效的 sockaddr 指针, 含 `_addrlen` 字节已初始化。
/// - 由 `sys_bind` 系统调用分发, 调用方验证权限。
/// - `NET_LOCK` 持有。
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_bind(fd: i32, addr: *const u8, _addrlen: u32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };

        let sockets = &mut *socket_set();

        match raw::fd_type(fd as usize) {
            2 => {
                let sock = sockets.get_mut::<udp::Socket>(handle);
                let endpoint = match parse_endpoint(addr) {
                    Some(ep) => IpListenEndpoint {
                        addr: Some(ep.addr),
                        port: ep.port,
                    },
                    None => return -E_INVAL,
                };
                match sock.bind(endpoint) {
                    Ok(()) => 0,
                    Err(_) => -E_ADDRINUSE,
                }
            }
            _ => -E_NOTSUPP,
        }
    }
}

/// POSIX `listen(fd, backlog)` 内核实现。
///
/// # Safety
/// `NET_LOCK` 持有; 由 `sys_listen` 分发, 调用方验证权限。
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_listen(fd: i32, _backlog: i32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };

        if raw::fd_type(fd as usize) != 1 {
            return -E_NOTSUPP;
        }

        let sockets = &mut *socket_set();
        let sock = sockets.get_mut::<tcp::Socket>(handle);

        let local = IpListenEndpoint {
            addr: None,
            port: 0,
        };
        match sock.listen(local) {
            Ok(()) => 0,
            Err(_) => -E_ADDRINUSE,
        }
    }
}

/// POSIX `accept(fd, addr, addrlen)` 内核实现。
///
/// # Safety
/// - `addr`/`_addrlen` 必须是有效的 sockaddr 指针 (此处忽略)。
/// - `NET_LOCK` 持有; 由 `sys_accept` 分发, 调用方验证权限。
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_accept(fd: i32, _addr: *mut u8, _addrlen: *mut u32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };

        if raw::fd_type(fd as usize) != 1 {
            return -E_NOTSUPP;
        }

        let sockets = &mut *socket_set();
        let sock = sockets.get_mut::<tcp::Socket>(handle);

        if sock.is_active() { fd } else { -E_AGAIN }
    }
}

/// POSIX `connect(fd, addr, addrlen)` 内核实现。
///
/// # Safety
/// `addr` 必须指向有效的 sockaddr 结构, 至少 `_addrlen` 字节。
/// `NET_LOCK` 持有。
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_connect(fd: i32, addr: *const u8, _addrlen: u32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };

        if !crate::privileged::net::NET_CONFIGURED.load(Ordering::Acquire) {
            return -E_NODEV;
        }

        let endpoint = match parse_endpoint(addr) {
            Some(ep) => ep,
            None => return -E_INVAL,
        };

        if raw::fd_type(fd as usize) != 1 {
            return -E_NOTSUPP;
        }

        let stack = match raw::stack_mut() {
            Some(s) => s,
            None => return -E_NODEV,
        };

        let sockets = &mut *socket_set();
        let sock = sockets.get_mut::<tcp::Socket>(handle);

        let local = IpListenEndpoint {
            addr: None,
            port: 0,
        };
        match sock.connect(stack.iface.context(), endpoint, local) {
            Ok(()) => 0,
            Err(_) => -E_CONNREFUSED,
        }
    }
}

/// POSIX `send(fd, buf, len, flags)` 内核实现。
///
/// # Safety
/// `buf` 必须指向至少 `len` 字节的有效可读内存, 内存必须在调用期间保持有效。
/// `NET_LOCK` 持有; 由 `sys_send` 分发, cred 校验已通过。
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_send(fd: i32, buf: *const u8, len: u32, _flags: i32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if buf.is_null() || len == 0 {
            return -E_INVAL;
        }

        let sockets = &mut *socket_set();
        let data = core::slice::from_raw_parts(buf, len as usize);

        match raw::fd_type(fd as usize) {
            1 => {
                let sock = sockets.get_mut::<tcp::Socket>(handle);
                sock.send_slice(data).map_or(-E_CONNRESET, |n| n as i32)
            }
            2 => {
                // UDP 无目的地址: 依赖 socket 已 "连接" (经 endpoint 绑定)
                // 简化处理, 返回 ENOTCONN; 请改用 sendto
                -E_NOTCONN
            }
            _ => -E_NOTSUPP,
        }
    }
}

/// POSIX `recv(fd, buf, len, flags)` 内核实现。
///
/// # Safety
/// `buf` 必须指向至少 `len` 字节的有效可写内存, 内存必须在调用期间保持有效。
/// `NET_LOCK` 持有; 由 `sys_recv` 分发。
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_recv(fd: i32, buf: *mut u8, len: u32, _flags: i32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if buf.is_null() || len == 0 {
            return -E_INVAL;
        }

        let sockets = &mut *socket_set();
        let data = core::slice::from_raw_parts_mut(buf, len as usize);

        match raw::fd_type(fd as usize) {
            1 => {
                let sock = sockets.get_mut::<tcp::Socket>(handle);
                sock.recv_slice(data).map_or_else(
                    |_| if sock.is_open() { 0 } else { -E_CONNRESET },
                    |n| n as i32,
                )
            }
            2 => {
                let sock = sockets.get_mut::<udp::Socket>(handle);
                sock.recv_slice(data)
                    .map_or(-E_AGAIN, |(n, _meta)| n as i32)
            }
            _ => -E_NOTSUPP,
        }
    }
}

/// POSIX `sendto(fd, buf, len, flags, addr, addrlen)` 内核实现。
///
/// # Safety
/// `buf`/`addr` 必须是有效指针, 内存至少含 `len`/`_addrlen` 字节。
/// `NET_LOCK` 持有; 由 `sys_sendto` 分发。
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_sendto(
    fd: i32,
    buf: *const u8,
    len: u32,
    _flags: i32,
    addr: *const u8,
    _addrlen: u32,
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if buf.is_null() || len == 0 {
            return -E_INVAL;
        }

        let endpoint = match parse_endpoint(addr) {
            Some(ep) => ep,
            None => return -E_INVAL,
        };

        let sockets = &mut *socket_set();
        let data = core::slice::from_raw_parts(buf, len as usize);

        match raw::fd_type(fd as usize) {
            2 => {
                let sock = sockets.get_mut::<udp::Socket>(handle);
                match sock.send_slice(data, endpoint) {
                    Ok(()) => len as i32,
                    Err(_) => -E_CONNRESET,
                }
            }
            1 => {
                let sock = sockets.get_mut::<tcp::Socket>(handle);
                sock.send_slice(data).map_or(-E_CONNRESET, |n| n as i32)
            }
            _ => -E_NOTSUPP,
        }
    }
}

/// POSIX `recvfrom(fd, buf, len, flags, addr, addrlen)` 内核实现。
///
/// # Safety
/// `buf` 必须是有效可写指针, 至少 `len` 字节; `addr`/`addrlen` 可选地写入对端地址。
/// `NET_LOCK` 持有; 由 `sys_recvfrom` 分发。
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_recvfrom(
    fd: i32,
    buf: *mut u8,
    len: u32,
    _flags: i32,
    addr: *mut u8,
    addrlen: *mut u32,
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if buf.is_null() || len == 0 {
            return -E_INVAL;
        }

        let sockets = &mut *socket_set();
        let data = core::slice::from_raw_parts_mut(buf, len as usize);

        match raw::fd_type(fd as usize) {
            2 => {
                let sock = sockets.get_mut::<udp::Socket>(handle);
                sock.recv_slice(data).map_or(-E_AGAIN, |(n, meta)| {
                    // 通过 endpoint_from_smol 将 smoltcp IpEndpoint 翻译为 NetEndpoint,
                    // 再写入 sockaddr_in 供用户态读取对端地址.
                    if let Some(ep) = endpoint_from_smol(meta.endpoint) {
                        write_sockaddr(addr, addrlen, &ep);
                    }
                    n as i32
                })
            }
            1 => {
                let sock = sockets.get_mut::<tcp::Socket>(handle);
                sock.recv_slice(data).map_or_else(
                    |_| if sock.is_open() { 0 } else { -E_CONNRESET },
                    |n| n as i32,
                )
            }
            _ => -E_NOTSUPP,
        }
    }
}

/// POSIX `sendmsg(fd, msghdr, flags)` 内核实现 (SG 拼接, 栈缓冲 4KB 上限).
///
/// # Safety
/// `msg` 必须是有效用户指针, 含完整 `Msghdr { msg_iov, msg_iovlen, ... }`.
/// 调用方 (functions) 须先校验可读范围.
/// `NET_LOCK` 持有; 由 `sys_sendmsg` 分发.
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_sendmsg(fd: i32, msg: *const u8, _flags: i32) -> i32 {
    unsafe {
        if msg.is_null() {
            return -E_FAULT;
        }
        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        // 读 Msghdr
        // SAFETY: msg 由 functions 校验可读 56 字节 (u64 Linux x86_64 / aarch64 布局).
        let msg_iov_ptr = core::ptr::read_unaligned(msg.add(16) as *const u64);
        let msg_iovlen_us = core::ptr::read_unaligned(msg.add(24) as *const u64) as usize;
        if msg_iovlen_us == 0 || msg_iovlen_us > 1024 {
            return -E_INVAL;
        }
        if msg_iov_ptr == 0 {
            return -E_INVAL;
        }
        // 拼接 iov 到 IobRegion (按需 alloc, 突破 4KB 栈限制; 性能瓶颈解除).
        // 先总容量, 再一次 alloc.
        let mut total: usize = 0;
        let mut lens: [usize; 1024] = [0usize; 1024];
        let mut bases: [u64; 1024] = [0u64; 1024];
        for i in 0..msg_iovlen_us {
            // SAFETY: msg_iov + i*Iovec(16) 可读 16 字节 (functions 校验 iov 范围).
            let iov_base =
                core::ptr::read_unaligned((msg_iov_ptr as *const u8).add(i * 16) as *const u64);
            let iov_len =
                core::ptr::read_unaligned((msg_iov_ptr as *const u8).add(i * 16 + 8) as *const u64)
                    as usize;
            bases[i] = iov_base;
            lens[i] = iov_len;
            if iov_base == 0 || iov_len == 0 {
                continue;
            }
            total = match total.checked_add(iov_len) {
                Some(v) => v,
                None => return -E_INVAL,
            };
        }
        if total == 0 {
            return 0;
        }
        let region = match crate::privileged::iobuf::IobRegion::alloc(total) {
            Some(r) => r,
            None => return -E_NOMEM,
        };
        let mut off: usize = 0;
        for i in 0..msg_iovlen_us {
            if bases[i] == 0 || lens[i] == 0 {
                continue;
            }
            // SAFETY: iov_base 由 functions 校验 lens[i] 字节可读; region 容量 >= total >= off+lens[i].
            core::ptr::copy_nonoverlapping(
                bases[i] as *const u8,
                region.as_mut_ptr().add(off),
                lens[i],
            );
            off += lens[i];
        }
        let rc = sm_send(fd, region.as_mut_ptr(), total as u32, 0);
        rc
    }
}

/// POSIX `recvmsg(fd, msghdr, flags)` 内核实现 (SG 拆分, 栈缓冲 4KB 上限).
///
/// # Safety
/// `msg` 必须是有效可写用户指针, functions 校验.
/// `NET_LOCK` 持有; 由 `sys_recvmsg` 分发.
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_recvmsg(fd: i32, msg: *mut u8, _flags: i32) -> i32 {
    unsafe {
        if msg.is_null() {
            return -E_FAULT;
        }
        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let msg_iov_ptr = core::ptr::read_unaligned(msg.add(16) as *const u64);
        let msg_iovlen_us = core::ptr::read_unaligned(msg.add(24) as *const u64) as usize;
        if msg_iovlen_us == 0 || msg_iovlen_us > 1024 {
            return -E_INVAL;
        }
        if msg_iov_ptr == 0 {
            return -E_INVAL;
        }
        // 计算总可用 iov 容量 + 收集 iov (突破 4KB 栈限制).
        let mut cap: usize = 0;
        let mut lens: [usize; 1024] = [0usize; 1024];
        let mut bases: [u64; 1024] = [0u64; 1024];
        for i in 0..msg_iovlen_us {
            let iov_base =
                core::ptr::read_unaligned((msg_iov_ptr as *const u8).add(i * 16) as *const u64);
            let iov_len =
                core::ptr::read_unaligned((msg_iov_ptr as *const u8).add(i * 16 + 8) as *const u64)
                    as usize;
            bases[i] = iov_base;
            lens[i] = iov_len;
            if iov_base == 0 || iov_len == 0 {
                continue;
            }
            cap = match cap.checked_add(iov_len) {
                Some(v) => v,
                None => return -E_INVAL,
            };
        }
        if cap == 0 {
            return 0;
        }
        let region = match crate::privileged::iobuf::IobRegion::alloc(cap) {
            Some(r) => r,
            None => return -E_NOMEM,
        };
        let n = sm_recv(fd, region.as_mut_ptr(), cap as u32, 0);
        if n <= 0 {
            return n;
        }
        // 拆分回 iov
        let mut left = n as usize;
        let mut off = 0usize;
        for i in 0..msg_iovlen_us {
            if left == 0 {
                break;
            }
            if bases[i] == 0 || lens[i] == 0 {
                continue;
            }
            let cp = core::cmp::min(lens[i], left);
            // SAFETY: iov_base 由 functions 校验 cp 字节可写.
            core::ptr::copy_nonoverlapping(region.as_mut_ptr().add(off), bases[i] as *mut u8, cp);
            off += cp;
            left -= cp;
        }
        n
    }
}

/// POSIX `close(fd)` 内核实现。
///
/// # Safety
/// `NET_LOCK` 持有; 由 `sys_close` 分发, cred 校验已通过。
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_close(fd: i32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };

        let stype = raw::fd_type(fd as usize);
        let sockets = &mut *socket_set();

        match stype {
            1 => {
                let sock = sockets.get_mut::<tcp::Socket>(handle);
                sock.close();
            }
            2 => {
                let sock = sockets.get_mut::<udp::Socket>(handle);
                sock.close();
            }
            _ => {}
        }

        sockets.remove(handle);
        // TD-07: smoltcp socket 已 drop, buf 借用结束, 此时 k_free 安全.
        if !raw::tcp_rx_buf(fd as usize).is_null() {
            crate::privileged::mm::k_free(raw::tcp_rx_buf(fd as usize));
            raw::set_tcp_rx_buf(fd as usize, core::ptr::null_mut());
        }
        if !raw::tcp_tx_buf(fd as usize).is_null() {
            crate::privileged::mm::k_free(raw::tcp_tx_buf(fd as usize));
            raw::set_tcp_tx_buf(fd as usize, core::ptr::null_mut());
        }
        if !raw::udp_rx_buf(fd as usize).is_null() {
            crate::privileged::mm::k_free(raw::udp_rx_buf(fd as usize));
            raw::set_udp_rx_buf(fd as usize, core::ptr::null_mut());
        }
        if !raw::udp_tx_buf(fd as usize).is_null() {
            crate::privileged::mm::k_free(raw::udp_tx_buf(fd as usize));
            raw::set_udp_tx_buf(fd as usize, core::ptr::null_mut());
        }
        raw::set_socket_handle(fd as usize, None);
        raw::set_fd_type(fd as usize, 0);
        0
    }
}

/// POSIX `setsockopt` 内核实现 (当前空操作占位)。
///
/// v2: 识别 `SO_PASSCRED` (`level=SOL_SOCKET=1`, `optname=SO_PASSCRED=16`).
/// 路由到 UDS 服务层 (`uds_setsockopt`).
/// 其他 (level, optname): 0 (no-op).
///
/// # Safety
/// `_optval` 必须是有效指针, 含 `_optlen` 字节 (此处忽略)。
#[unsafe(no_mangle)]
#[expect(
    clippy::used_underscore_binding,
    reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_setsockopt(
    _fd: i32,
    _level: i32,
    _optname: i32,
    _optval: *const u8,
    _optlen: u32,
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
) -> i32 {
    unsafe {
        // v2 SO_PASSCRED 路由: level==1 (SOL_SOCKET), optname==16 (SO_PASSCRED)
        if _level == 1 && _optname == 16 {
            if _optlen < 4 {
                return -22; // EINVAL
            }
            let val = core::ptr::read_unaligned(_optval as *const i32);
            // 第二十六批: UDS 策略经注册钩子委托 (未注册 fail-closed, 早期
            // 启动无用户态进程, -ENOPROTOOPT 窗口安全; 符号与全文件
            // 负 errno 返回惯例一致)
            return match UDS_SETOPT_HOOK.get() {
                Some(&hook) => hook(_fd, val != 0),
                None => -E_NOPROTOOPT,
            };
        }
        0
    }
}

/// POSIX `getsockopt` 内核实现 (当前空操作占位)。
///
/// # Safety
/// `_optval` 必须是有效可写指针, `_optlen` 必须是有效可写 u32 指针 (此处忽略)。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sm_getsockopt(
    _fd: i32,
    _level: i32,
    _optname: i32,
    _optval: *mut u8,
    _optlen: *mut u32,
) -> i32 {
    0
}

/// POSIX `getsockname(fd, addr, addrlen)` 内核实现。
///
/// 真实实现: 写回 socket 的 local endpoint 到 `*addr`, 更新 `*addrlen`。
/// TCP 用 `local_endpoint()`, UDP 用 `endpoint()` (`IpListenEndpoint`).
///
/// # Safety
/// - `addr` 必须是可写 sockaddr 指针, 至少 `_addrlen` 字节.
/// - `_addrlen` 必须是可写 u32 指针 (写回实际长度).
/// - `NET_LOCK` 持有; 由 `sys_getsockname` 分发.
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_getsockname(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if addr.is_null() || addrlen.is_null() {
            return -E_INVAL;
        }
        let stype = raw::fd_type(fd as usize);
        let sockets = &mut *socket_set();

        let endpoint_opt: Option<IpEndpoint> = match stype {
            1 => {
                let sock = sockets.get::<tcp::Socket>(handle);
                sock.local_endpoint()
            }
            2 => {
                let sock = sockets.get::<udp::Socket>(handle);
                let ep = sock.endpoint();
                ep.addr.map_or(
                    Some(IpEndpoint {
                        addr: IpAddress::Ipv4(Ipv4Address::UNSPECIFIED),
                        port: ep.port,
                    }),
                    |addr| {
                        Some(IpEndpoint {
                            addr,
                            port: ep.port,
                        })
                    },
                )
            }
            _ => return -E_NOTSUPP,
        };

        let endpoint = match endpoint_opt {
            Some(e) => e,
            None => return -E_NOTCONN, // TCP 未 connect
        };
        // 双栈 (DECISION-032): 翻译为 NetEndpoint 后按 V4/V6 分支写 sockaddr_in / sockaddr_in6
        let ep = match endpoint_from_smol(endpoint) {
            Some(ep) => ep,
            None => return -E_AFNOSUPPORT,
        };
        // SAFETY: write_sockaddr 按 ep.addr 分支写对应 sockaddr 结构, addr 已校验非空且 ≥ 28 字节
        write_sockaddr(addr, addrlen, &ep);
        0
    }
}

/// POSIX `getpeername(fd, addr, addrlen)` 内核实现。
///
/// 真实实现: 写回 socket 的 remote endpoint 到 `*addr` (TCP 需已 connect).
///
/// # Safety
/// - `addr` 必须是可写 sockaddr 指针, 至少 `_addrlen` 字节.
/// - `_addrlen` 必须是可写 u32 指针 (写回实际长度).
/// - `NET_LOCK` 持有; 由 `sys_getpeername` 分发.
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_getpeername(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        if fd < 0 || fd as usize >= MAX_SM_FD || raw::fd_type(fd as usize) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(fd as usize) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if addr.is_null() || addrlen.is_null() {
            return -E_INVAL;
        }
        let stype = raw::fd_type(fd as usize);
        let sockets = &mut *socket_set();

        let endpoint_opt: Option<IpEndpoint> = match stype {
            1 => {
                let sock = sockets.get::<tcp::Socket>(handle);
                sock.remote_endpoint()
            }
            2 => {
                // UDP: remote 由 last_recv_meta 取, 但 Socket 没暴露, 暂返 ENOTCONN.
                return -E_NOTCONN;
            }
            _ => return -E_NOTSUPP,
        };

        let endpoint = match endpoint_opt {
            Some(e) => e,
            None => return -E_NOTCONN,
        };
        // 双栈 (DECISION-032): 翻译为 NetEndpoint 后按 V4/V6 分支写 sockaddr_in / sockaddr_in6
        let ep = match endpoint_from_smol(endpoint) {
            Some(ep) => ep,
            None => return -E_AFNOSUPPORT,
        };
        // SAFETY: write_sockaddr 按 ep.addr 分支写对应 sockaddr 结构, addr 已校验非空且 ≥ 28 字节
        write_sockaddr(addr, addrlen, &ep);
        0
    }
}

/// 轮询所有 socket 状态 (驱动 `select/poll` 内核实现)。
///
/// # Safety
/// `NET_LOCK` 持有; 由 `sys_poll`/`sys_select` 分发。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sm_poll_sockets() -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();

        let sockets = &mut *socket_set();
        process_dhcp_events(sockets);

        for i in 0..MAX_SM_FD {
            if raw::fd_type(i) != 1 {
                continue;
            }
            if let Some(handle) = raw::socket_handle(i) {
                let _sock = sockets.get_mut::<tcp::Socket>(handle);
            }
        }
        0
    }
}
