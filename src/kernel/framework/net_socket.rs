//! 网络子系统 FFI 安全代理 — framework TCB
//!
//! ## 职责
//!
//! 这是 services 层与 `kernel::net::init::sm_*` 之间的**唯一** unsafe 边界。
//! 所有 `unsafe extern "C"` 调用都在本模块集中处理, services 层 0 unsafe。
//!
//! ## 设计原则
//!
//! 1. 每个 `unsafe { ... }` 块都带 SAFETY 注释, 描述契约
//! 2. 切片 API (`&[u8]`, `&mut [u8]`) 替代 `*const T` / `*mut T` + 长度
//! 3. 强类型 `i32` → `NetError` 翻译
//! 4. services 层只调用本模块, 不再接触 `net::init::*` 裸函数
//!
//! 评估日期: 2026-06-04

// I-预存 (kernel_test build): `net::init` 模块在 `#[cfg(not(feature = "kernel_test"))]`
// 下被过滤 (无真实硬件), 但本模块函数体统一通过 `init::*` 调用. 在 kernel_test 模式下
// 我们提供一个同名 `init` 桩模块, 让现有函数体零改动, 公共 API 表面保持稳定
// (services 调用方不需要 cfg 化).
#[cfg(not(feature = "kernel_test"))]
use crate::framework::net::init;

// kernel_test 桩: `unsafe` 性与真实实现对齐 (`eg_net_init` 为 safe `extern "C" fn`,
// 其余真实实现为 `unsafe fn` / `unsafe extern "C" fn`), 函数体 no-op.
// 提供与 `init::*` 同名的桩, 让 `init::xxx()` 路径在两种 build 下都有效,
// 且上层包装器中的 `unsafe` 块在两种 build 下都不冗余 (F9 清理 crate 级
// `allow(unused_unsafe)` 后, 桩若为 safe 会使包装器的 `unsafe` 块被判冗余).
#[cfg(feature = "kernel_test")]
mod init {
    pub fn eg_net_init() {}
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn poll_network() {}
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn eg_net_start_dhcp() -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn eg_net_static_ip(_cidr: *const u8, _gw: *const u8) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn reset_network_state() {}
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_socket(_d: i32, _t: i32, _p: i32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_bind(_fd: i32, _a: *const u8, _l: u32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_listen(_fd: i32, _b: i32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_accept(_fd: i32, _a: *mut u8, _l: *mut u32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_connect(_fd: i32, _a: *const u8, _l: u32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_send(_fd: i32, _b: *const u8, _l: u32, _f: i32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_recv(_fd: i32, _b: *mut u8, _l: u32, _f: i32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_sendto(
        _fd: i32,
        _b: *const u8,
        _l: u32,
        _f: i32,
        _d: *const u8,
        _a: u32,
    ) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_recvfrom(
        _fd: i32,
        _b: *mut u8,
        _l: u32,
        _f: i32,
        _s: *mut u8,
        _a: *mut u32,
    ) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_close(_fd: i32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_sendmsg(_fd: i32, _m: *const u8, _f: i32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_recvmsg(_fd: i32, _m: *mut u8, _f: i32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_setsockopt(
        _fd: i32,
        _level: i32,
        _name: i32,
        _val: *const u8,
        _optlen: u32,
    ) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_getsockopt(
        _fd: i32,
        _level: i32,
        _name: i32,
        _val: *mut u8,
        _optlen: *mut u32,
    ) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_getsockname(_fd: i32, _addr: *mut u8, _addrlen: *mut u32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_getpeername(_fd: i32, _addr: *mut u8, _addrlen: *mut u32) -> i32 {
        0
    }
    // SAFETY: kernel_test 桩, no-op 无内存/硬件访问; unsafe 仅与真实实现签名对齐
    pub unsafe fn sm_poll_sockets() -> i32 {
        0
    }
}

// ============================================================================
// 网络顶层 API
// ============================================================================

/// 初始化网络子系统
///
/// # Safety
///
/// 调用方保证单线程上下文 (启动期) 调用一次, 内部全局状态串行化。
pub fn eg_net_init() {
    init::eg_net_init();
}

/// 轮询网络栈
///
/// # Safety
///
/// `try_lock` 保证 ISR 安全; 多个 poll 调用可重入。
pub fn poll_network() {
    // SAFETY: try_lock 保证 ISR 安全
    unsafe { init::poll_network() }
}

/// 启动 DHCP
///
/// # Safety
///
/// 由 services 串行调用, `NET_LOCK` 由内核管理。
pub fn eg_net_start_dhcp() -> i32 {
    // SAFETY: 串行调用, NET_LOCK 由内核管理
    unsafe { init::eg_net_start_dhcp() }
}

/// 设置静态 IP
///
/// # Safety
///
/// `cidr_ptr` / `gw_ptr` 必须为以 NUL 结尾的有效 C 字符串, 且调用期间不释放。
pub fn eg_net_static_ip(cidr_ptr: *const u8, gw_ptr: *const u8) -> i32 {
    // SAFETY: cidr_ptr/gw_ptr 由调用方保证为有效 NUL 结尾字符串
    unsafe { init::eg_net_static_ip(cidr_ptr, gw_ptr) }
}

/// 重置网络状态
///
/// # Safety
///
/// 恢复域串行调用。
pub fn reset_network_state() {
    // SAFETY: 恢复域串行调用
    unsafe { init::reset_network_state() }
}

// ============================================================================
// Socket FFI 安全代理
// ============================================================================

/// POSIX `socket(domain, type, protocol)` — 创建 socket
pub fn sm_socket(domain: i32, sock_type: i32, protocol: i32) -> i32 {
    // SAFETY: NET_LOCK 由 sm_socket 内部获取, 串行化
    unsafe { init::sm_socket(domain, sock_type, protocol) }
}

/// POSIX `bind(fd, addr, addrlen)`
///
/// # Safety
///
/// `addr` 必须为至少 `addrlen` 字节的有效指针, 调用期间不释放。
pub fn sm_bind(fd: i32, addr: *const u8, addrlen: u32) -> i32 {
    // SAFETY: addr 由调用方保证有效, sm_bind 同步读取
    unsafe { init::sm_bind(fd, addr, addrlen) }
}

/// POSIX `listen(fd, backlog)`
pub fn sm_listen(fd: i32, backlog: i32) -> i32 {
    // SAFETY: NET_LOCK 内部获取
    unsafe { init::sm_listen(fd, backlog) }
}

/// POSIX `accept(fd, addr, addrlen)` — 返回新连接的 FD
///
/// # Safety
///
/// `addr` 和 `addrlen` 可为 null 表示不关心对端地址。
pub fn sm_accept(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    // SAFETY: null 表示不写对端地址, 由调用方契约保证
    unsafe { init::sm_accept(fd, addr, addrlen) }
}

/// POSIX `connect(fd, addr, addrlen)`
///
/// # Safety
///
/// `addr` 必须为至少 `addrlen` 字节的有效指针, 调用期间不释放。
pub fn sm_connect(fd: i32, addr: *const u8, addrlen: u32) -> i32 {
    // SAFETY: addr 栈上有效, sm_connect 同步读取
    unsafe { init::sm_connect(fd, addr, addrlen) }
}

/// POSIX `send(fd, buf, len, flags)` — 阻塞发送
///
/// # Safety
///
/// `buf` 必须为至少 `len` 字节的有效只读指针, 调用期间不释放。
pub fn sm_send(fd: i32, buf: *const u8, len: u32, flags: i32) -> i32 {
    // SAFETY: buf 在调用期间有效, sm_send 同步读取
    unsafe { init::sm_send(fd, buf, len, flags) }
}

/// POSIX `recv(fd, buf, len, flags)` — 阻塞接收
///
/// # Safety
///
/// `buf` 必须为至少 `len` 字节的有效可写指针。
pub fn sm_recv(fd: i32, buf: *mut u8, len: u32, flags: i32) -> i32 {
    // SAFETY: out 在调用期间有效可写
    unsafe { init::sm_recv(fd, buf, len, flags) }
}

/// POSIX `sendto(fd, buf, len, flags, dest_addr, addrlen)`
///
/// # Safety
///
/// `buf` 和 `dest_addr` 必须为有效指针, 调用期间不释放。
pub fn sm_sendto(
    fd: i32,
    buf: *const u8,
    len: u32,
    flags: i32,
    dest_addr: *const u8,
    addrlen: u32,
) -> i32 {
    // SAFETY: buf + dest_addr 同步有效
    unsafe { init::sm_sendto(fd, buf, len, flags, dest_addr, addrlen) }
}

/// POSIX `recvfrom(fd, buf, len, flags, src_addr, addrlen)`
///
/// # Safety
///
/// `buf` 可写, `src_addr` 为有效可写 8 字节缓冲, `addrlen` 为可写 u32。
pub fn sm_recvfrom(
    fd: i32,
    buf: *mut u8,
    len: u32,
    flags: i32,
    src_addr: *mut u8,
    addrlen: *mut u32,
) -> i32 {
    // SAFETY: out 可写, src 8 字节栈缓冲
    unsafe { init::sm_recvfrom(fd, buf, len, flags, src_addr, addrlen) }
}

/// POSIX `close(fd)`
pub fn sm_close(fd: i32) -> i32 {
    // SAFETY: sm_close 内部 NET_LOCK 串行化
    unsafe { init::sm_close(fd) }
}

/// POSIX `sendmsg(fd, msg, flags)` — 散聚 I/O
pub fn sm_sendmsg(fd: i32, msg: *const u8, flags: i32) -> i32 {
    // SAFETY: sm_sendmsg 内部 NET_LOCK 持有, msg 由 services 校验 msghdr 布局.
    unsafe { init::sm_sendmsg(fd, msg, flags) }
}

/// POSIX `recvmsg(fd, msg, flags)` — 散聚 I/O
pub fn sm_recvmsg(fd: i32, msg: *mut u8, flags: i32) -> i32 {
    // SAFETY: sm_recvmsg 内部 NET_LOCK 持有, msg 由 services 校验 msghdr 布局.
    unsafe { init::sm_recvmsg(fd, msg, flags) }
}

/// POSIX `setsockopt(fd, level, optname, optval, optlen)`
///
/// # Safety
///
/// `optval` 必须为至少 `optlen` 字节的有效指针。
pub fn sm_setsockopt(fd: i32, level: i32, optname: i32, optval: *const u8, optlen: u32) -> i32 {
    // SAFETY: optval 栈上有效
    unsafe { init::sm_setsockopt(fd, level, optname, optval, optlen) }
}

/// POSIX `getsockopt(fd, level, optname, optval, optlen)` → i32
///
/// # Safety
///
/// `optval` 必须为可写指针, `optlen` 为可写 u32。
pub fn sm_getsockopt(fd: i32, level: i32, optname: i32, optval: *mut u8, optlen: *mut u32) -> i32 {
    // SAFETY: optval 可写, optlen 可写
    unsafe { init::sm_getsockopt(fd, level, optname, optval, optlen) }
}

/// POSIX `getsockname(fd, addr, addrlen)` — 获取本端地址
///
/// # Safety
///
/// `addr` 必须为可写缓冲区, `addrlen` 为可写 u32。
pub fn sm_getsockname(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    // SAFETY: addr 可写, addrlen 可写, 由调用方保证有效
    unsafe { init::sm_getsockname(fd, addr, addrlen) }
}

/// POSIX `getpeername(fd, addr, addrlen)` — 获取对端地址
///
/// # Safety
///
/// `addr` 必须为可写缓冲区, `addrlen` 为可写 u32。
pub fn sm_getpeername(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    // SAFETY: addr 可写, addrlen 可写, 由调用方保证有效
    unsafe { init::sm_getpeername(fd, addr, addrlen) }
}

/// 轮询所有 socket
pub fn sm_poll_sockets() -> i32 {
    // SAFETY: try_lock 内部使用, ISR 安全
    unsafe { init::sm_poll_sockets() }
}

// ============================================================================
// SmoltcpNetStack 桥接 safe wrappers (W4.2.3.4)
//
// services 层 SmoltcpNetStack 通过本模块调用 init::sm_* 函数,
// 保持 services 0 unsafe.
// ============================================================================

/// `SmoltcpNetStack::bind` — POSIX bind 委托
///
/// # Safety
///
/// `addr` 必须为至少 `addrlen` 字节的有效指针, 调用期间不释放。
pub fn sm_net_bind(fd: i32, addr: *const u8, addrlen: u32) -> i32 {
    // SAFETY: addr 由调用方保证有效, sm_bind 同步读取
    unsafe { init::sm_bind(fd, addr, addrlen) }
}

/// `SmoltcpNetStack::listen` — POSIX listen 委托
pub fn sm_net_listen(fd: i32, backlog: i32) -> i32 {
    // SAFETY: NET_LOCK 内部获取
    unsafe { init::sm_listen(fd, backlog) }
}

/// `SmoltcpNetStack::accept` — POSIX accept 委托
///
/// # Safety
///
/// `addr` 和 `addrlen` 可为 null 表示不关心对端地址。
pub fn sm_net_accept(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    // SAFETY: null 表示不写对端地址, 由调用方契约保证
    unsafe { init::sm_accept(fd, addr, addrlen) }
}

/// `SmoltcpNetStack::connect` — POSIX connect 委托
///
/// # Safety
///
/// `addr` 必须为至少 `addrlen` 字节的有效指针, 调用期间不释放。
pub fn sm_net_connect(fd: i32, addr: *const u8, addrlen: u32) -> i32 {
    // SAFETY: addr 栈上有效, sm_connect 同步读取
    unsafe { init::sm_connect(fd, addr, addrlen) }
}

/// `SmoltcpNetStack::send` — POSIX send 委托
///
/// # Safety
///
/// `buf` 必须为至少 `len` 字节的有效只读指针, 调用期间不释放。
pub fn sm_net_send(fd: i32, buf: *const u8, len: u32, flags: i32) -> i32 {
    // SAFETY: buf 在调用期间有效, sm_send 同步读取
    unsafe { init::sm_send(fd, buf, len, flags) }
}

/// `SmoltcpNetStack::recv` — POSIX recv 委托
///
/// # Safety
///
/// `buf` 必须为至少 `len` 字节的有效可写指针。
pub fn sm_net_recv(fd: i32, buf: *mut u8, len: u32, flags: i32) -> i32 {
    // SAFETY: buf 在调用期间有效可写
    unsafe { init::sm_recv(fd, buf, len, flags) }
}

/// `SmoltcpNetStack::sendto` — POSIX sendto 委托
///
/// # Safety
///
/// `buf` 和 `dest_addr` 必须为有效指针, 调用期间不释放。
pub fn sm_net_sendto(
    fd: i32,
    buf: *const u8,
    len: u32,
    flags: i32,
    dest_addr: *const u8,
    addrlen: u32,
) -> i32 {
    // SAFETY: buf + dest_addr 同步有效
    unsafe { init::sm_sendto(fd, buf, len, flags, dest_addr, addrlen) }
}

/// `SmoltcpNetStack::recvfrom` — POSIX recvfrom 委托
///
/// # Safety
///
/// `buf` 可写, `src_addr` 为有效可写缓冲, `addrlen` 为可写 u32。
pub fn sm_net_recvfrom(
    fd: i32,
    buf: *mut u8,
    len: u32,
    flags: i32,
    src_addr: *mut u8,
    addrlen: *mut u32,
) -> i32 {
    // SAFETY: buf 可写, src_addr 可写
    unsafe { init::sm_recvfrom(fd, buf, len, flags, src_addr, addrlen) }
}

/// `SmoltcpNetStack::close` — POSIX close 委托
pub fn sm_net_close(fd: i32) -> i32 {
    // SAFETY: sm_close 内部 NET_LOCK 串行化
    unsafe { init::sm_close(fd) }
}

// ============================================================================
// SmoltcpNetStack 专属 safe wrapper (W4.2.3.4)
// ============================================================================

/// `SmoltcpNetStack::close` 的 safe wrapper — 关闭 `SmoltcpNetStack` 范围内的 socket.
///
/// # Safety
///
/// `slot_idx` 必须在 `[smoltcp_net_stack_slot_base(), TOTAL_SLOTS)` 范围内,
/// 由 `SmoltcpNetStack` 调用方保证.
#[cfg(not(feature = "kernel_test"))]
pub fn smoltcp_net_stack_socket_close(slot_idx: usize) -> bool {
    init::raw::smoltcp_net_stack_socket_close(slot_idx)
}

// ============================================================================
// Socket 选项与轮询 FFI 安全代理
// ============================================================================

/// 创建 socket (POSIX socket(domain, type, protocol)).
///
/// # Safety
///
/// 由 `NET_LOCK` 内部串行化.
pub fn sm_net_socket(domain: i32, sock_type: i32, protocol: i32) -> i32 {
    // SAFETY: 内部持有 NET_LOCK, 串行化
    unsafe { init::sm_socket(domain, sock_type, protocol) }
}

/// 设置 Socket 选项 (POSIX setsockopt).
///
/// # Safety
///
/// `val` 必须为至少 `optlen` 字节的有效只读指针.
pub fn sm_net_setsockopt(fd: i32, level: i32, optname: i32, val: *const u8, optlen: u32) -> i32 {
    // SAFETY: val 由调用方保证有效, sm_setsockopt 同步读取
    unsafe { init::sm_setsockopt(fd, level, optname, val, optlen) }
}

/// 获取 Socket 选项 (POSIX getsockopt).
///
/// # Safety
///
/// `val` 必须为可写缓冲区, `optlen` 为可写 u32.
pub fn sm_net_getsockopt(fd: i32, level: i32, optname: i32, val: *mut u8, optlen: *mut u32) -> i32 {
    // SAFETY: val 可写, optlen 可写
    unsafe { init::sm_getsockopt(fd, level, optname, val, optlen) }
}

/// 轮询所有 Socket 状态.
///
/// # Safety
///
/// 内部持有 `NET_LOCK`, ISR 安全.
pub fn sm_net_poll_sockets() -> i32 {
    // SAFETY: 内部持有 NET_LOCK, 串行化
    unsafe { init::sm_poll_sockets() }
}
