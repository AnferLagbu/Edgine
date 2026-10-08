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
use crate::privileged::net::{
    SOCKET_WAIT_QUEUES, WAITER_ACCEPT, WAITER_CONNECT, WAITER_READ, WAITER_WRITE,
};
use crate::privileged::proc::{
    BlockReason, process_get_current_pid, scheduler_block, scheduler_schedule,
};
use crate::privileged::sync::in_irq_context;
use core::sync::atomic::AtomicU16;
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Duration;
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
const E_INPROGRESS: i32 = 115;
const E_NODEV: i32 = 19;
const E_NOPROTOOPT: i32 = 92;

// ============================================================================
// D8: socket 选项常量 (Linux asm-generic 值域)
// ============================================================================
const SOL_SOCKET: i32 = 1;
const IPPROTO_TCP: i32 = 6;
const SO_REUSEADDR: i32 = 2;
const SO_TYPE: i32 = 3;
const SO_ERROR: i32 = 4;
const SO_KEEPALIVE: i32 = 9;
const SO_REUSEPORT: i32 = 15;
const SO_PASSCRED: i32 = 16;
const TCP_NODELAY: i32 = 1;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;

// ============================================================================
// D8b: poll 事件位 (Linux <poll.h> 值域; 与 functions/fs/file_ops.rs 一致)
// ============================================================================
const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;
const POLLERR: i16 = 8;
const POLLHUP: i16 = 16;
const POLLNVAL: i16 = 32;

// ============================================================================
// 方案 C: fd → 槽位索引单点换算 (Smoltcp 段, base = FdPlan::SMOLTCP.base)
// ============================================================================

/// 把用户态 fd 换算为 Smoltcp 段内部槽位索引。
///
/// 本模块全部 `raw::*` 静态表以**槽位索引空间**运作 (紧凑 [0, MAX_SM_FD)),
/// fd 数值 (含基址偏移) 仅经本函数进入表。这是 `fd_alloc::idx_of` 在本子树的
/// 唯一切换面: fd 对象模型终态工程 (全局 FdPlan 退役, per-process fd 表接管)
/// 时整体改写本函数的调用点即可。
#[inline]
fn sm_slot(fd: i32) -> Option<usize> {
    match crate::privileged::proc::fd_alloc::idx_of(fd) {
        Some((crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp, slot)) => Some(slot),
        _ => None,
    }
}

/// 登记当前进程为 `slot` 上方向 `want` 的等待者 (须在持 `NET_STATE` 时调用).
/// 返 false 表示等待者槽满 → 调用方应回退非阻塞语义 (见 `WAITER_SLOTS` SIMPLIFIED).
#[inline]
fn net_add_waiter(slot: usize, pid: u32, want: u8) -> bool {
    SOCKET_WAIT_QUEUES
        .get(slot)
        .is_some_and(|q| q.add_waiter(pid, want))
}

/// 清除当前进程在 `slot` 上的等待登记 (IO 返回前自摘; 无登记则 no-op).
/// 因 `poll_network` 采用非破坏 `collect_waiters` + 电平重扫, 等待者必须由
/// 消费者在成功返回/放弃阻塞时自行摘除, 否则槽位永久泄漏.
#[inline]
fn net_clear_waiter(slot: usize, pid: u32) {
    if pid != 0 {
        if let Some(q) = SOCKET_WAIT_QUEUES.get(slot) {
            q.remove_waiter(pid);
        }
    }
}

/// 分配一个 Smoltcp 段 FD 并换算为槽位索引, 返回 `(fd, slot)`.
///
/// 分配失败与换算失败统一返回 `None`; 换算属防御分支 (alloc 段与 sm_slot 换算
/// 同源, 必然成功), 一旦失败先归还 FD 编号避免位图泄漏. 调用点以单一 let-else
/// 处理 `-E_NFILE`, 分配-回滚逻辑收敛于此不再重复.
///
/// 调用方须持有 `NET_STATE` 锁 (与两处调用点既有约定一致).
#[inline]
fn sm_alloc_slot() -> Option<(i32, usize)> {
    let fd = crate::privileged::proc::fd_alloc::alloc_fd(
        crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp,
    )?;
    if let Some(slot) = sm_slot(fd) {
        return Some((fd, slot));
    }
    crate::privileged::proc::fd_alloc::free_fd(
        crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp,
        fd,
    );
    None
}

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

/// 把 trait 本地端点翻译为 smoltcp `IpListenEndpoint` (D4/D5 复用).
///
/// 通配地址 (`0.0.0.0` / `::`) 必须映射为 `addr: None`: 若置 `Some(unspecified)`,
/// smoltcp 会以 unspecified 作为发送源地址, 且 `accepts()` 会因 `addr != dst`
/// 拒绝所有入向报文 (通配语义丢失). 故仅在指定地址时置 `Some`.
fn endpoint_to_listen(ep: crate::privileged::net::iface_trait::NetEndpoint) -> IpListenEndpoint {
    let wildcard = match ep.addr {
        crate::privileged::net::iface_trait::IpAddr::V4(v4) => v4.is_unspecified(),
        crate::privileged::net::iface_trait::IpAddr::V6(v6) => v6.is_unspecified(),
    };
    IpListenEndpoint {
        addr: if wildcard {
            None
        } else {
            Some(wire_to_smol(ep.addr))
        },
        port: ep.port,
    }
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

        // V2: 使用集中分配器获取 FD (方案 C: sm_alloc_slot 分配 + 槽位换算一体,
        // 换算防御分支内部归还 FD 编号)
        let Some((fd, fd_idx)) = sm_alloc_slot() else {
            return -E_NFILE;
        };

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

/// 设置 socket 非阻塞标志 (D10 / DECISION-092).
///
/// `nonblock` = true 置非阻塞, false 恢复阻塞 (写 per-slot `blocking` 标志).
/// fd 非法 / 非 socket (`fd_type`=0) 返回 `-E_BADF`, 否则 `0`. 内部持 `NET_STATE`
/// 锁串行化; 供 `socket_syscall` (剥离 `SOCK_NONBLOCK`) 与 `functions` 侧
/// `fcntl(F_SETFL, O_NONBLOCK)` 经 `net_socket` 安全代理调用. 阻塞睡眠本身归 P5b.
pub fn sm_set_nonblocking(fd: i32, nonblock: bool) -> i32 {
    let _guard = NET_STATE.lock();
    let Some(slot) = sm_slot(fd) else {
        return -E_BADF;
    };
    if raw::fd_type(slot) == 0 {
        return -E_BADF;
    }
    raw::set_blocking(slot, !nonblock);
    0
}

/// 读取 socket 非阻塞标志 (D10). 供 `fcntl(F_GETFL)` 回填 `O_NONBLOCK`.
///
/// 返回 `1` (非阻塞) / `0` (阻塞); fd 非法 / 非 socket 返回 `-E_BADF`.
pub fn sm_get_nonblocking(fd: i32) -> i32 {
    let _guard = NET_STATE.lock();
    let Some(slot) = sm_slot(fd) else {
        return -E_BADF;
    };
    if raw::fd_type(slot) == 0 {
        return -E_BADF;
    }
    // 非阻塞 = !blocking; 返回 1 (非阻塞) / 0 (阻塞).
    i32::from(!raw::is_blocking(slot))
}

// ============================================================================
// D2: 临时端口 (ephemeral port) 分配器
//
// 区间 49152-65535 (IANA 动态/私有端口段), 共 16384 个端口.
// 分配在 NET_STATE 锁下串行进行, 分配结果由调用方写入 D1 local 端点表.
// ============================================================================

/// 临时端口区间下界 (IANA dynamic port range 起点).
const EPHEMERAL_START: u16 = 49152;
/// 临时端口区间上界 (含).
const EPHEMERAL_END: u16 = 65535;
/// 临时端口总数 (16384).
const EPHEMERAL_COUNT: u32 = (EPHEMERAL_END - EPHEMERAL_START) as u32 + 1;

/// 临时端口分配游标 (指向"最近一次分配的候选值").
static EPHEMERAL_CURSOR: AtomicU16 = AtomicU16::new(EPHEMERAL_START);

/// 判断某本地端口是否已被任一 FD 占用 (扫 D1 local 端点表).
///
/// 调用方须持有 `NET_STATE` 锁.
fn local_port_in_use(port: u16) -> bool {
    (0..MAX_SM_FD).any(|i| raw::socket_local_endpoint(i).is_some_and(|ep| ep.port == port))
}

/// 判断某本地端点 (地址族 + 端口) 是否已被任一 FD 占用 (扫 D1 local 端点表).
///
/// 供 D3 (TCP `bind`) 冲突检测使用. 调用方须持有 `NET_STATE` 锁.
// SIMPLIFIED: 只比较地址族与端口, 不比较具体地址 (如 `0.0.0.0:80` 与 `1.2.3.4:80`
// 视为冲突). 影响面: 同族同端口不同地址的 bind 亦被拒绝, 比 POSIX 严格;
// 扩展时机: 若需精确 POSIX 重叠判定 (通配地址与具体地址的包含关系), 改为逐地址比较.
fn local_endpoint_in_use(ep: crate::privileged::net::iface_trait::NetEndpoint) -> bool {
    (0..MAX_SM_FD).any(|i| {
        raw::socket_local_endpoint(i)
            .is_some_and(|e| e.port == ep.port && e.addr.is_v4() == ep.addr.is_v4())
    })
}

/// 分配下一个可用临时端口; 全区间占满返回 `None`.
///
/// 调用方须持有 `NET_STATE` 锁 (游标读改写与 D1 表扫描均在该锁下串行).
// SIMPLIFIED: 游标用 load + store 而非 fetch_add — u16 的 fetch_add 越过 65535
// 会静默回绕到 0 (落在动态端口区间外), 需额外分支修正; 且分配本身必须在
// NET_STATE 锁下串行, 无需 fetch_add 的原子读改写.
// 影响面: 仅本函数; 扩展时机: 若未来需无锁并发分配, 改用 fetch_update.
fn next_ephemeral() -> Option<u16> {
    for _ in 0..EPHEMERAL_COUNT {
        let cur = EPHEMERAL_CURSOR.load(Ordering::Relaxed);
        // EPHEMERAL_END == u16::MAX, 故 `>=` 与 `==` 等价 (clippy absurd_extreme_comparisons).
        let candidate = if cur == EPHEMERAL_END {
            EPHEMERAL_START
        } else {
            cur + 1
        };
        EPHEMERAL_CURSOR.store(candidate, Ordering::Relaxed);
        if !local_port_in_use(candidate) {
            return Some(candidate);
        }
    }
    None
}

/// POSIX `bind(fd, addr, addrlen)` 内核实现。
///
/// # Safety
/// - `addr` 必须是有效的 sockaddr 指针, 含 `_addrlen` 字节已初始化。
/// - 由 `sys_bind` 系统调用分发, 调用方验证权限。
/// - `NET_LOCK` 持有。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sm_bind(fd: i32, addr: *const u8, addrlen: u32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();
        sm_bind_locked(fd, addr, addrlen)
    }
}

/// `sm_bind` 的锁自由内核实现, 要求调用方已持有 `NET_STATE` 锁。
///
/// # Safety
/// - `addr` 必须是有效的 sockaddr 指针, 含 `_addrlen` 字节已初始化。
/// - 调用方必须持有 `NET_STATE` 锁 (非可重入自旋锁, 重复加锁会死锁)。
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
unsafe fn sm_bind_locked(fd: i32, addr: *const u8, _addrlen: u32) -> i32 {
    unsafe {
        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(slot) {
            Some(h) => h,
            None => return -E_BADF,
        };

        let sockets = &mut *socket_set();

        match raw::fd_type(slot) {
            2 => {
                let sock = sockets.get_mut::<udp::Socket>(handle);
                // D1: 解析为 trait 端点, 便于写入本地端点表.
                let mut ep = match parse_endpoint_trait(addr) {
                    Some(ep) => ep,
                    None => return -E_INVAL,
                };
                // D2: port == 0 表示请求内核自动分配临时端口.
                if ep.port == 0 {
                    match next_ephemeral() {
                        Some(port) => ep.port = port,
                        None => return -E_ADDRINUSE,
                    }
                }
                // 通配绑定 (`[::]` / `0.0.0.0`) 必须映射为 `addr: None`:
                // 若置 `Some(::)`, smoltcp 会以 unspecified 作为发送源地址,
                // 且 `UdpSocket::accepts` 会因 `addr != dst` 拒绝所有入向报文
                // (通配语义丢失). 故仅在指定地址时置 `Some`.
                let wildcard = match ep.addr {
                    crate::privileged::net::iface_trait::IpAddr::V4(v4) => v4.is_unspecified(),
                    crate::privileged::net::iface_trait::IpAddr::V6(v6) => v6.is_unspecified(),
                };
                let endpoint = IpListenEndpoint {
                    addr: if wildcard {
                        None
                    } else {
                        Some(wire_to_smol(ep.addr))
                    },
                    port: ep.port,
                };
                match sock.bind(endpoint) {
                    Ok(()) => {
                        // D1: 记录本地端点 (保留通配族地址), 供端口冲突检测.
                        raw::set_socket_local_endpoint(slot, Some(ep));
                        0
                    }
                    Err(_) => -E_ADDRINUSE,
                }
            }
            1 => {
                // D3: TCP 不调用 smoltcp bind (tcp::Socket 无此方法), 仅登记本地端点,
                // 供 listen (D4) / connect (D5) 复用; 通配地址在 D1 表内保留族.
                let mut ep = match parse_endpoint_trait(addr) {
                    Some(ep) => ep,
                    None => return -E_INVAL,
                };
                // D2: port == 0 表示请求内核自动分配临时端口.
                if ep.port == 0 {
                    match next_ephemeral() {
                        Some(port) => ep.port = port,
                        None => return -E_ADDRINUSE,
                    }
                }
                // D3: 冲突检测 (同族同端口已占用则拒绝).
                if local_endpoint_in_use(ep) {
                    return -E_ADDRINUSE;
                }
                raw::set_socket_local_endpoint(slot, Some(ep));
                0
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
pub unsafe extern "C" fn sm_listen(fd: i32, backlog: i32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();
        sm_listen_locked(fd, backlog)
    }
}

/// `sm_listen` 的锁自由内核实现, 要求调用方已持有 `NET_STATE` 锁。
///
/// # Safety
/// 调用方必须持有 `NET_STATE` 锁 (非可重入自旋锁, 重复加锁会死锁)。
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
unsafe fn sm_listen_locked(fd: i32, _backlog: i32) -> i32 {
    unsafe {
        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(slot) {
            Some(h) => h,
            None => return -E_BADF,
        };

        if raw::fd_type(slot) != 1 {
            return -E_NOTSUPP;
        }

        // D4: 本地端点优先取 D1 表; 缺省时按 D2 分配临时端口并回写.
        let local_ep = match raw::socket_local_endpoint(slot) {
            Some(ep) if ep.port != 0 => ep,
            _ => {
                let port = match next_ephemeral() {
                    Some(p) => p,
                    None => return -E_ADDRINUSE,
                };
                let ep = crate::privileged::net::iface_trait::NetEndpoint::new(
                    crate::privileged::net::iface_trait::IpAddr::V4(
                        crate::privileged::net::iface_trait::Ipv4Addr::UNSPECIFIED,
                    ),
                    port,
                );
                raw::set_socket_local_endpoint(slot, Some(ep));
                ep
            }
        };

        let local = endpoint_to_listen(local_ep);
        let sockets = &mut *socket_set();
        let sock = sockets.get_mut::<tcp::Socket>(handle);
        match sock.listen(local) {
            Ok(()) => 0,
            Err(_) => -E_INVAL,
        }
    }
}

/// POSIX `accept(fd, addr, addrlen)` 内核实现。
///
/// # Safety
/// - `addr`/`addrlen` 可为 NULL; 非 NULL 时 `addr` 须指向至少 16 字节 (V4) /
///   28 字节 (V6) 可写内存, `addrlen` 须指向有效 u32。
/// - `NET_LOCK` 持有; 由 `sys_accept` 分发, 调用方验证权限。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sm_accept(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    // P5c (DECISION-095): 监听槽暂无已完成握手的连接 (sm_accept_locked 返 -E_AGAIN)
    // 且 fd 阻塞时, 登记 WAITER_ACCEPT 等待者 → 释放 NET_STATE → block+schedule →
    // 回顶重查; poll_network 检测监听槽迁移到 Established/CloseWait 时唤醒. 非阻塞 /
    // 中断路径保留旧 eager -E_AGAIN 语义 (显式 O_NONBLOCK 或不可挂起场景).
    let pid = process_get_current_pid();
    let can_block = pid != 0 && !in_irq_context();
    unsafe {
        loop {
            let guard = NET_STATE.lock();
            let r = sm_accept_locked(fd, addr, addrlen);
            if r != -E_AGAIN {
                if let Some(slot) = sm_slot(fd) {
                    net_clear_waiter(slot, pid);
                }
                return r;
            }
            // -E_AGAIN 唯一来源: 监听槽无可交付连接 (其余错误直接返回, 不挂起).
            let Some(slot) = sm_slot(fd) else {
                return r;
            };
            if !raw::is_blocking(slot) || !can_block {
                return r;
            }
            if !net_add_waiter(slot, pid, WAITER_ACCEPT) {
                return -E_AGAIN; // 等待者槽满, 保守回退 (见 WAITER_SLOTS SIMPLIFIED)
            }
            drop(guard);
            scheduler_block(BlockReason::WaitingForIo);
            scheduler_schedule();
            // poll_network 唤醒 → 回循环顶重锁重查监听槽状态.
        }
    }
}

/// `sm_accept` 的锁自由内核实现, 要求调用方已持有 `NET_STATE` 锁。
///
/// # Safety
/// - `addr`/`addrlen` 同 [`sm_accept`]。
/// - 调用方必须持有 `NET_STATE` 锁 (非可重入自旋锁, 重复加锁会死锁)。
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
unsafe fn sm_accept_locked(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32 {
    unsafe {
        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }
        let listen_slot = slot;
        let conn_handle = match raw::socket_handle(listen_slot) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if raw::fd_type(listen_slot) != 1 {
            return -E_NOTSUPP;
        }

        let sockets = &mut *socket_set();

        // D6 门控 1: 监听 socket 上存在已完成三次握手的连接.
        // 可接受状态: Established (正常) 与 CloseWait (对端在 accept 前已发 FIN ——
        // 握手完成但数据/FIN 已到达, POSIX accept 仍应交付该连接; 若只认
        // Established, 客户端 send 后立即 shutdown(SHUT_WR) 的场景会让监听槽永久
        // 卡在 CloseWait, accept 永远 EAGAIN).
        let listen_state = sockets.get::<tcp::Socket>(conn_handle).state();
        if listen_state != tcp::State::Established && listen_state != tcp::State::CloseWait {
            return -E_AGAIN;
        }

        // 监听槽 L 的本地端点 (D4 listen 已回写 D1 表); 缺失或端口为 0 视为异常.
        let listener_local = match raw::socket_local_endpoint(listen_slot) {
            Some(ep) if ep.port != 0 => ep,
            _ => return -E_INVAL,
        };

        // D6 门控 2: 活跃 socket 数上限 (与 sm_socket I-47 同口径).
        let active: usize = (0..MAX_SM_FD).filter(|&i| raw::fd_type(i) != 0).count();
        if active >= get_max_sockets() {
            return -E_NFILE;
        }

        // 步骤 2: 为新连接分配 FD 编号 (槽 n; 方案 C: sm_alloc_slot 分配 + 换算
        // 一体, 换算防御分支内部归还 FD 编号避免位图泄漏).
        let Some((conn_fd, conn_slot)) = sm_alloc_slot() else {
            return -E_NFILE;
        };

        // 步骤 3: 在槽 n 建新监听 socket (新 handle + 新缓冲).
        // socket_open_stub 失败时已内部归还缓冲, 此处只需归还 FD 编号.
        let kind = crate::privileged::net::iface_trait::SocketKind::Tcp;
        let new_handle = if let Some(h) = raw::socket_open_stub(sockets, kind, conn_slot) {
            h
        } else {
            crate::privileged::proc::fd_alloc::free_fd(
                crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp,
                conn_fd,
            );
            return -E_NOMEM;
        };

        // 步骤 4: 新 socket 立即进入 Listen, 交接前监听即就位 (消除监听槽空闲窗口).
        // listener_local.port != 0 (门控保证) 且新 socket 处于 Closed, listen 必然
        // 成功; Err 分支为防御性回滚 (自建内联清理, sm_close 自锁不可复用).
        if sockets
            .get_mut::<tcp::Socket>(new_handle)
            .listen(endpoint_to_listen(listener_local))
            .is_err()
        {
            sockets.remove(new_handle);
            if !raw::tcp_rx_buf(conn_slot).is_null() {
                crate::privileged::mm::k_free(raw::tcp_rx_buf(conn_slot));
            }
            if !raw::tcp_tx_buf(conn_slot).is_null() {
                crate::privileged::mm::k_free(raw::tcp_tx_buf(conn_slot));
            }
            raw::set_tcp_rx_buf(conn_slot, core::ptr::null_mut());
            raw::set_tcp_tx_buf(conn_slot, core::ptr::null_mut());
            raw::set_socket_handle(conn_slot, None);
            raw::set_fd_type(conn_slot, 0);
            crate::privileged::proc::fd_alloc::free_fd(
                crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp,
                conn_fd,
            );
            return -E_INVAL;
        }

        // 步骤 5: 索引交换 (O(1) 值交换, 无内存拷贝).
        // 槽 L 与槽 n 互换 handle 与 TCP 缓冲: 槽 n 承接已连接会话, 槽 L 承接新监听.
        // 两槽 fd_type 恒为 1; D1 local 两槽均写监听本地端点.
        let listener_bufs = (raw::tcp_rx_buf(listen_slot), raw::tcp_tx_buf(listen_slot));
        let fresh_bufs = (raw::tcp_rx_buf(conn_slot), raw::tcp_tx_buf(conn_slot));
        raw::set_socket_handle(conn_slot, Some(conn_handle));
        raw::set_socket_handle(listen_slot, Some(new_handle));
        raw::set_tcp_rx_buf(conn_slot, listener_bufs.0);
        raw::set_tcp_tx_buf(conn_slot, listener_bufs.1);
        raw::set_tcp_rx_buf(listen_slot, fresh_bufs.0);
        raw::set_tcp_tx_buf(listen_slot, fresh_bufs.1);
        raw::set_socket_local_endpoint(conn_slot, Some(listener_local));
        raw::set_socket_local_endpoint(listen_slot, Some(listener_local));

        // 步骤 6: 从已连接会话回写对端地址 (沿用 getsockname/getpeername 直写约定).
        if let Some(remote) = sockets.get::<tcp::Socket>(conn_handle).remote_endpoint() {
            if let Some(ep) = endpoint_from_smol(remote) {
                write_sockaddr(addr, addrlen, &ep);
            }
        }

        // 步骤 7: 返回承载已连接会话的新 fd; 监听槽 L 持续可用.
        conn_fd
    }
}

/// POSIX `connect(fd, addr, addrlen)` 内核实现。
///
/// # Safety
/// `addr` 必须指向有效的 sockaddr 结构, 至少 `_addrlen` 字节。
/// `NET_LOCK` 持有。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sm_connect(fd: i32, addr: *const u8, addrlen: u32) -> i32 {
    // P5c (DECISION-095, A2): TCP connect 发起 SYN 后进入 SynSent, 需等握手完成.
    // 阻塞 fd → 登记 WAITER_CONNECT 挂起, poll_network 检测 Established(成功)/Closed
    // (失败) 唤醒; 非阻塞 / 中断路径 → 发起后返 -E_INPROGRESS (POSIX 异步 connect).
    // UDP connect 为同步登记 (sm_connect_locked 内即时完成), 不涉及等待.
    // Phase A: 单发发起 (含参数/NET_CONFIGURED/UDP 同步/连接错误映射), 复用 locked.
    let rc = unsafe {
        let _guard = NET_STATE.lock();
        sm_connect_locked(fd, addr, addrlen)
    };
    if rc != 0 {
        return rc; // 发起即失败 (E_BADF/E_NODEV/E_INVAL/E_CONNREFUSED)
    }
    // Phase B: 仅 TCP 可能处于握手进行中; UDP (fd_type!=1) 同步完成直接返 0.
    let pid = process_get_current_pid();
    let can_block = pid != 0 && !in_irq_context();
    unsafe {
        loop {
            let guard = NET_STATE.lock();
            let Some(slot) = sm_slot(fd) else {
                return 0;
            };
            if raw::fd_type(slot) != 1 {
                net_clear_waiter(slot, pid);
                return 0; // UDP: connect 同步登记完成
            }
            let Some(handle) = raw::socket_handle(slot) else {
                net_clear_waiter(slot, pid);
                return 0;
            };
            let st = (&*socket_set()).get::<tcp::Socket>(handle).state();
            match st {
                tcp::State::Established => {
                    net_clear_waiter(slot, pid);
                    return 0;
                }
                tcp::State::Closed => {
                    // SIMPLIFIED: smoltcp 不提供 connect 失败精确原因 (RST vs 超时),
                    // 统一近似 ECONNREFUSED; 精确 refused/timeout 区分归 P5d.
                    net_clear_waiter(slot, pid);
                    return -E_CONNREFUSED;
                }
                tcp::State::SynSent | tcp::State::SynReceived => {
                    if !raw::is_blocking(slot) || !can_block {
                        return -E_INPROGRESS; // 非阻塞 / 不可挂起: 异步进行中
                    }
                    if !net_add_waiter(slot, pid, WAITER_CONNECT) {
                        return -E_INPROGRESS; // 等待者槽满: 保守回退非阻塞语义
                    }
                    drop(guard);
                    scheduler_block(BlockReason::WaitingForIo);
                    scheduler_schedule();
                    // poll_network 唤醒 → 回顶重查握手结果.
                }
                _ => {
                    // 其他态 (罕见) 视为完成.
                    net_clear_waiter(slot, pid);
                    return 0;
                }
            }
        }
    }
}

/// `sm_connect` 的锁自由内核实现, 要求调用方已持有 `NET_STATE` 锁。
///
/// # Safety
/// - `addr` 必须指向有效的 sockaddr 结构, 至少 `_addrlen` 字节。
/// - 调用方必须持有 `NET_STATE` 锁 (非可重入自旋锁, 重复加锁会死锁)。
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
unsafe fn sm_connect_locked(fd: i32, addr: *const u8, _addrlen: u32) -> i32 {
    unsafe {
        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(slot) {
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

        // D9: UDP connect — 登记对端到 D1 remote 表; 未 bind 时按 D2 分配临时端口并 bind.
        if raw::fd_type(slot) == 2 {
            let net_remote = match endpoint_from_smol(endpoint) {
                Some(ep) => ep,
                None => return -E_INVAL,
            };
            raw::set_socket_remote_endpoint(slot, Some(net_remote));

            // 已显式 bind (port != 0) 则无需再分配临时端口.
            let need_bind = match raw::socket_local_endpoint(slot) {
                Some(ep) => ep.port == 0,
                None => true,
            };
            if !need_bind {
                return 0;
            }
            let Some(port) = next_ephemeral() else {
                raw::set_socket_remote_endpoint(slot, None);
                return -E_ADDRINUSE;
            };
            let sockets = &mut *socket_set();
            let sock = sockets.get_mut::<udp::Socket>(handle);
            return if let Ok(()) = sock.bind(IpListenEndpoint { addr: None, port }) {
                raw::set_socket_local_endpoint(
                    slot,
                    Some(crate::privileged::net::iface_trait::NetEndpoint::new(
                        crate::privileged::net::iface_trait::IpAddr::V4(
                            crate::privileged::net::iface_trait::Ipv4Addr::UNSPECIFIED,
                        ),
                        port,
                    )),
                );
                0
            } else {
                raw::set_socket_remote_endpoint(slot, None);
                -E_ADDRINUSE
            };
        }

        if raw::fd_type(slot) != 1 {
            return -E_NOTSUPP;
        }

        // D5: 本地端点取 D1 表; 缺省时按 D2 分配临时端口并回写.
        // 必须在 `stack_mut()` 之前完成 D1/D2 写入: `stack_mut()` 返回
        // `&'static mut NetworkStack`, 与 `set_socket_local_endpoint` 路径对
        // `NET_STATE` 的访问构成别名, 故先写后借.
        let local_ep = match raw::socket_local_endpoint(slot) {
            Some(ep) if ep.port != 0 => ep,
            _ => {
                let port = match next_ephemeral() {
                    Some(p) => p,
                    None => return -E_ADDRINUSE,
                };
                let ep = crate::privileged::net::iface_trait::NetEndpoint::new(
                    crate::privileged::net::iface_trait::IpAddr::V4(
                        crate::privileged::net::iface_trait::Ipv4Addr::UNSPECIFIED,
                    ),
                    port,
                );
                raw::set_socket_local_endpoint(slot, Some(ep));
                ep
            }
        };
        let local = endpoint_to_listen(local_ep);

        let stack = match raw::stack_mut() {
            Some(s) => s,
            None => return -E_NODEV,
        };

        let sockets = &mut *socket_set();
        let sock = sockets.get_mut::<tcp::Socket>(handle);

        match sock.connect(stack.iface.context(), endpoint, local) {
            Ok(()) => 0,
            Err(tcp::ConnectError::Unaddressable) => -E_INVAL,
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
    // SIMPLIFIED: 未处理 MSG_DONTWAIT / MSG_NOSIGNAL (归 P5d); 仅据 fd 阻塞位
    // 决定是否挂起; 影响面=单次 send 的 flag 级非阻塞; 何时扩展=flags 语义完善轮.
    // P5b (DECISION-094): TCP 发送缓冲满 (can_send=false) 且连接存活时, 阻塞 fd
    // 登记 WAITER_WRITE 等待者 → 释放 NET_STATE → block+schedule → 回顶重查.
    let pid = process_get_current_pid();
    let can_block = pid != 0 && !in_irq_context();
    unsafe {
        loop {
            let guard = NET_STATE.lock();

            let Some(slot) = sm_slot(fd) else {
                return -E_BADF;
            };
            if raw::fd_type(slot) == 0 {
                return -E_BADF;
            }
            let handle = match raw::socket_handle(slot) {
                Some(h) => h,
                None => return -E_BADF,
            };
            if buf.is_null() || len == 0 {
                return -E_INVAL;
            }
            let blocking = raw::is_blocking(slot);

            let sockets = &mut *socket_set();
            let data = core::slice::from_raw_parts(buf, len as usize);

            // Some(r) = 本次可完成 (返 r); None = TCP 可发送性未就绪且需阻塞.
            // 非阻塞 / 中断路径 / idle 保持旧 eager 语义 (不改变既有返回值).
            let done: Option<i32> = match raw::fd_type(slot) {
                1 => {
                    let sock = sockets.get_mut::<tcp::Socket>(handle);
                    if !sock.is_open() {
                        Some(-E_CONNRESET)
                    } else if sock.can_send() || !(blocking && can_block) {
                        Some(sock.send_slice(data).map_or(-E_CONNRESET, |n| n as i32))
                    } else {
                        None // 阻塞 fd + 连接存活 + 缓冲满 → 挂起等可写
                    }
                }
                2 => {
                    // D9: UDP send 依赖 connect 登记的对端 (D1 remote 表); 未 connect → ENOTCONN.
                    let remote = match raw::socket_remote_endpoint(slot) {
                        Some(ep) => ep,
                        None => return -E_NOTCONN,
                    };
                    let smol = IpEndpoint {
                        addr: wire_to_smol(remote.addr),
                        port: remote.port,
                    };
                    let sock = sockets.get_mut::<udp::Socket>(handle);
                    // UDP 数据报: 维持原语义, 不阻塞 (send 侧无"半包等待"概念).
                    Some(match sock.send_slice(data, smol) {
                        Ok(()) => len as i32,
                        Err(_) => -E_CONNRESET,
                    })
                }
                _ => Some(-E_NOTSUPP),
            };

            if let Some(r) = done {
                net_clear_waiter(slot, pid);
                return r;
            }
            // None 仅在 (blocking && can_block && TCP 缓冲满) 产生 → 登记等待者并挂起.
            if !net_add_waiter(slot, pid, WAITER_WRITE) {
                return -E_AGAIN; // 等待者槽满, 保守回退 (见 WAITER_SLOTS SIMPLIFIED)
            }
            drop(guard);
            scheduler_block(BlockReason::WaitingForIo);
            scheduler_schedule();
            // 被 poll_network 唤醒 → 回循环顶重锁重查 can_send.
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
    // SIMPLIFIED: 未处理 MSG_DONTWAIT / MSG_PEEK (归 P5d); 仅据 fd 阻塞位决定挂起.
    // P5b (DECISION-094): TCP 空缓冲且连接存活 / UDP 无数据报时, 阻塞 fd 登记
    // WAITER_READ → 释放 NET_STATE → block+schedule → 回顶重查. 连接关闭 (EOF) 由
    // poll_network 的 dead 分支唤醒, 避免阻塞挂死.
    let pid = process_get_current_pid();
    let can_block = pid != 0 && !in_irq_context();
    unsafe {
        loop {
            let guard = NET_STATE.lock();

            let Some(slot) = sm_slot(fd) else {
                return -E_BADF;
            };
            if raw::fd_type(slot) == 0 {
                return -E_BADF;
            }
            let handle = match raw::socket_handle(slot) {
                Some(h) => h,
                None => return -E_BADF,
            };
            if buf.is_null() || len == 0 {
                return -E_INVAL;
            }
            let blocking = raw::is_blocking(slot);

            let sockets = &mut *socket_set();
            let data = core::slice::from_raw_parts_mut(buf, len as usize);

            // Some(r) = 本次可完成; None = 未就绪且需阻塞. 非阻塞/中断路径保持旧 eager 语义.
            let done: Option<i32> = match raw::fd_type(slot) {
                1 => {
                    let sock = sockets.get_mut::<tcp::Socket>(handle);
                    if sock.can_recv() {
                        Some(sock.recv_slice(data).map_or(0, |n| n as i32))
                    } else if !sock.is_open() {
                        Some(-E_CONNRESET) // 无数据且连接关闭 (沿用旧 eager 语义: 未细分 EOF/RST)
                    } else if blocking && can_block {
                        None // 连接存活且空 → 阻塞等可读
                    } else {
                        Some(0) // 非阻塞 empty+open: 旧行为返 0
                    }
                }
                2 => {
                    let sock = sockets.get_mut::<udp::Socket>(handle);
                    if sock.can_recv() {
                        Some(
                            sock.recv_slice(data)
                                .map_or(-E_AGAIN, |(n, _meta)| n as i32),
                        )
                    } else if blocking && can_block {
                        None
                    } else {
                        Some(-E_AGAIN) // 非阻塞空: 旧行为返 -E_AGAIN
                    }
                }
                _ => Some(-E_NOTSUPP),
            };

            if let Some(r) = done {
                net_clear_waiter(slot, pid);
                return r;
            }
            if !net_add_waiter(slot, pid, WAITER_READ) {
                return -E_AGAIN; // 等待者槽满, 保守回退 (见 WAITER_SLOTS SIMPLIFIED)
            }
            drop(guard);
            scheduler_block(BlockReason::WaitingForIo);
            scheduler_schedule();
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
    // SIMPLIFIED: 未处理 MSG_DONTWAIT (归 P5d). P5b: TCP sendto 与 sm_send 同阻塞语义;
    // UDP sendto 为数据报不阻塞 (维持旧 eager). 方向 WAITER_WRITE.
    let pid = process_get_current_pid();
    let can_block = pid != 0 && !in_irq_context();
    unsafe {
        loop {
            let guard = NET_STATE.lock();

            let Some(slot) = sm_slot(fd) else {
                return -E_BADF;
            };
            if raw::fd_type(slot) == 0 {
                return -E_BADF;
            }
            let handle = match raw::socket_handle(slot) {
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
            let blocking = raw::is_blocking(slot);

            let sockets = &mut *socket_set();
            let data = core::slice::from_raw_parts(buf, len as usize);

            let done: Option<i32> = match raw::fd_type(slot) {
                2 => {
                    let sock = sockets.get_mut::<udp::Socket>(handle);
                    // UDP sendto: 数据报 eager, 不阻塞.
                    Some(match sock.send_slice(data, endpoint) {
                        Ok(()) => len as i32,
                        Err(_) => -E_CONNRESET,
                    })
                }
                1 => {
                    let sock = sockets.get_mut::<tcp::Socket>(handle);
                    if !sock.is_open() {
                        Some(-E_CONNRESET)
                    } else if sock.can_send() || !(blocking && can_block) {
                        Some(sock.send_slice(data).map_or(-E_CONNRESET, |n| n as i32))
                    } else {
                        None
                    }
                }
                _ => Some(-E_NOTSUPP),
            };

            if let Some(r) = done {
                net_clear_waiter(slot, pid);
                return r;
            }
            if !net_add_waiter(slot, pid, WAITER_WRITE) {
                return -E_AGAIN;
            }
            drop(guard);
            scheduler_block(BlockReason::WaitingForIo);
            scheduler_schedule();
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
    // SIMPLIFIED: 未处理 MSG_DONTWAIT / MSG_PEEK (归 P5d). P5b: 与 sm_recv 同阻塞
    // 语义 (方向 WAITER_READ), UDP 额外写回对端 sockaddr.
    let pid = process_get_current_pid();
    let can_block = pid != 0 && !in_irq_context();
    unsafe {
        loop {
            let guard = NET_STATE.lock();

            let Some(slot) = sm_slot(fd) else {
                return -E_BADF;
            };
            if raw::fd_type(slot) == 0 {
                return -E_BADF;
            }
            let handle = match raw::socket_handle(slot) {
                Some(h) => h,
                None => return -E_BADF,
            };
            if buf.is_null() || len == 0 {
                return -E_INVAL;
            }
            let blocking = raw::is_blocking(slot);

            let sockets = &mut *socket_set();
            let data = core::slice::from_raw_parts_mut(buf, len as usize);

            let done: Option<i32> = match raw::fd_type(slot) {
                2 => {
                    let sock = sockets.get_mut::<udp::Socket>(handle);
                    if sock.can_recv() {
                        Some(sock.recv_slice(data).map_or(-E_AGAIN, |(n, meta)| {
                            // 通过 endpoint_from_smol 将 smoltcp IpEndpoint 翻译为 NetEndpoint,
                            // 再写入 sockaddr_in 供用户态读取对端地址.
                            if let Some(ep) = endpoint_from_smol(meta.endpoint) {
                                write_sockaddr(addr, addrlen, &ep);
                            }
                            n as i32
                        }))
                    } else if blocking && can_block {
                        None
                    } else {
                        Some(-E_AGAIN)
                    }
                }
                1 => {
                    let sock = sockets.get_mut::<tcp::Socket>(handle);
                    if sock.can_recv() {
                        Some(sock.recv_slice(data).map_or(0, |n| n as i32))
                    } else if !sock.is_open() {
                        Some(-E_CONNRESET)
                    } else if blocking && can_block {
                        None
                    } else {
                        Some(0)
                    }
                }
                _ => Some(-E_NOTSUPP),
            };

            if let Some(r) = done {
                net_clear_waiter(slot, pid);
                return r;
            }
            if !net_add_waiter(slot, pid, WAITER_READ) {
                return -E_AGAIN;
            }
            drop(guard);
            scheduler_block(BlockReason::WaitingForIo);
            scheduler_schedule();
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
        // 方案 C: 此处仅校验 fd 归属与类型, 实际收发委托 sm_send/sm_recv
        // (其内部自行完成槽位换算与二次校验)。
        if sm_slot(fd).is_none_or(|slot| raw::fd_type(slot) == 0) {
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
        // 方案 C: 同 sm_sendmsg, 仅校验归属与类型, 拆分委托 sm_recv。
        if sm_slot(fd).is_none_or(|slot| raw::fd_type(slot) == 0) {
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

        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(slot) {
            Some(h) => h,
            None => return -E_BADF,
        };

        let stype = raw::fd_type(slot);
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
        if !raw::tcp_rx_buf(slot).is_null() {
            crate::privileged::mm::k_free(raw::tcp_rx_buf(slot));
            raw::set_tcp_rx_buf(slot, core::ptr::null_mut());
        }
        if !raw::tcp_tx_buf(slot).is_null() {
            crate::privileged::mm::k_free(raw::tcp_tx_buf(slot));
            raw::set_tcp_tx_buf(slot, core::ptr::null_mut());
        }
        if !raw::udp_rx_buf(slot).is_null() {
            crate::privileged::mm::k_free(raw::udp_rx_buf(slot));
            raw::set_udp_rx_buf(slot, core::ptr::null_mut());
        }
        if !raw::udp_tx_buf(slot).is_null() {
            crate::privileged::mm::k_free(raw::udp_tx_buf(slot));
            raw::set_udp_tx_buf(slot, core::ptr::null_mut());
        }
        raw::set_socket_handle(slot, None);
        raw::set_fd_type(slot, 0);
        // D1: 清空 local 端点表槽位.
        raw::set_socket_local_endpoint(slot, None);
        // D9: 清空 remote 端点表槽位 (UDP connect 登记的对端).
        raw::set_socket_remote_endpoint(slot, None);
        // G10: 归还 FD 编号, 否则 socket/accept 循环会耗尽 MAX_SM_FD 个 FD 位.
        crate::privileged::proc::fd_alloc::free_fd(
            crate::privileged::proc::fd_alloc::FdSubsystem::Smoltcp,
            fd,
        );
        0
    }
}

/// POSIX `shutdown(fd, how)` 内核实现 — 半关闭, 区别于 `sm_close` 的整体回收.
///
/// - `SHUT_WR(1)` / `SHUT_RDWR(2)`: 对 TCP/UDP socket 调 `sock.close()`, 触发
///   smoltcp 主动关闭 (发送 FIN, 停止接收新数据), 近似 POSIX 发送侧半关语义;
///   socket 句柄与 FD 编号**保留**, 不回收缓冲/端点表 (与 `sm_close` 的关键差异).
/// - `SHUT_RD(0)`: no-op 返 `0` (见下方 SIMPLIFIED).
/// - 其他 `how`: `-EINVAL`.
///
/// # Safety
/// `fd` 须为合法 socket FD; 内部持 `NET_STATE` 锁串行化.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sm_shutdown(fd: i32, how: i32) -> i32 {
    unsafe {
        let _guard = NET_STATE.lock();
        sm_shutdown_locked(fd, how)
    }
}

/// `sm_shutdown` 的锁自由内核实现, 要求调用方已持有 `NET_STATE` 锁。
///
/// # Safety
/// 调用方必须持有 `NET_STATE` 锁 (非可重入自旋锁, 重复加锁会死锁)。
unsafe fn sm_shutdown_locked(fd: i32, how: i32) -> i32 {
    unsafe {
        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }

        // 非法 how: POSIX 约定返回 EINVAL.
        if how != 0 && how != 1 && how != 2 {
            return -E_INVAL;
        }

        // SHUT_RD(0): smoltcp 无"仅停接收、保持发送"的直接对应物, no-op 返 0.
        // SIMPLIFIED: 未真正切断接收路径; 影响面为 recv 仍可读到对端残余数据;
        // 何时需扩展: P5 阻塞/唤醒链路接入后在 rx 侧补屏蔽.
        if how == 0 {
            return 0;
        }

        // SHUT_WR(1) / SHUT_RDWR(2): 取句柄触发主动关闭, socket/FD 保留.
        let Some(handle) = raw::socket_handle(slot) else {
            return -E_BADF;
        };
        let stype = raw::fd_type(slot);
        let sockets = &mut *socket_set();
        match stype {
            1 => sockets.get_mut::<tcp::Socket>(handle).close(),
            2 => sockets.get_mut::<udp::Socket>(handle).close(),
            _ => {}
        }
        0
    }
}

/// POSIX `poll` 的单 socket 就绪快照 (D8b: `File::poll` 的数值 fd 前影).
///
/// 非阻塞、无副作用: 只查询收发就绪与关闭态, 返回 `events` 请求位的就绪子集
/// (POLLHUP/POLLERR 可无条件附加). 不睡眠 (阻塞等待归 P5 等待队列).
///
/// - TCP (`fd_type`=1): `can_recv()` → POLLIN; `can_send()` → POLLOUT;
///   `state()==Closed` → POLLHUP|POLLERR.
/// - UDP (`fd_type`=2): `can_recv()`/`can_send()` 同理.
/// - fd 非法 / 非 socket (`fd_type`=0) → POLLNVAL.
///
/// # Safety
/// `fd` 须为合法 FD; 内部持 `NET_STATE` 锁串行化.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sm_socket_poll(fd: i32, events: i16) -> i16 {
    unsafe {
        let _guard = NET_STATE.lock();
        sm_socket_poll_locked(fd, events)
    }
}

/// `sm_socket_poll` 的锁自由内核实现, 要求调用方已持有 `NET_STATE` 锁。
///
/// # Safety
/// 调用方必须持有 `NET_STATE` 锁 (非可重入自旋锁, 重复加锁会死锁)。
unsafe fn sm_socket_poll_locked(fd: i32, events: i16) -> i16 {
    unsafe {
        let Some(slot) = sm_slot(fd) else {
            return POLLNVAL;
        };
        let stype = raw::fd_type(slot);
        if stype == 0 {
            return POLLNVAL;
        }
        let Some(handle) = raw::socket_handle(slot) else {
            return POLLNVAL;
        };

        let mut revents: i16 = 0;
        let sockets = &mut *socket_set();
        match stype {
            1 => {
                let sock = sockets.get::<tcp::Socket>(handle);
                // SIMPLIFIED: 监听 socket 的"待 accept 连接"就绪 (POLLIN) 未单独
                // 识别 (smoltcp listen socket `can_recv()` 恒 false); 影响面: poll
                // 一个监听 fd 不会因有新连接而报 POLLIN; 何时需扩展: 待 P5 接入
                // accept 就绪路径后补 (需判 `state()==Listen` + 完成队列非空).
                if events & POLLIN != 0 && sock.can_recv() {
                    revents |= POLLIN;
                }
                if events & POLLOUT != 0 && sock.can_send() {
                    revents |= POLLOUT;
                }
                if sock.state() == tcp::State::Closed {
                    revents |= POLLHUP | POLLERR;
                }
            }
            2 => {
                let sock = sockets.get::<udp::Socket>(handle);
                if events & POLLIN != 0 && sock.can_recv() {
                    revents |= POLLIN;
                }
                if events & POLLOUT != 0 && sock.can_send() {
                    revents |= POLLOUT;
                }
            }
            _ => return POLLNVAL,
        }
        revents
    }
}

/// POSIX `setsockopt` 内核实现 (D8: 精简集).
///
/// 已支持选项:
/// - `SO_PASSCRED` (`SOL_SOCKET`/16): 路由到 UDS 服务层 (`uds_setsockopt`).
/// - `SO_REUSEADDR`(2)/`SO_REUSEPORT`(15): 接受但忽略 (本内核无端口复用调度需求).
/// - `SO_KEEPALIVE` (`SOL_SOCKET`/9, 仅 TCP): 置/清 keep-alive (7200s 缺省间隔).
/// - `TCP_NODELAY` (`IPPROTO_TCP`/6, 仅 TCP): `val != 0` 关闭 Nagle.
/// 其余 (`level`, `optname`): `-ENOPROTOOPT`.
///
/// # Safety
/// `optval` 必须是 syscall 层提供的有效内核指针 (4 字节 i32), `optlen` 为其长度。
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_setsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *const u8,
    optlen: u32,
) -> i32 {
    // SAFETY: optval 为 syscall 层传入的内核栈指针 (含 4 字节); NET_STATE 锁保护 socket 集合.
    unsafe {
        // v2 SO_PASSCRED 路由: level==1 (SOL_SOCKET), optname==16 (SO_PASSCRED)
        if level == SOL_SOCKET && optname == SO_PASSCRED {
            if optlen < 4 {
                return -E_INVAL;
            }
            let val = core::ptr::read_unaligned(optval as *const i32);
            // 第二十六批: UDS 策略经注册钩子委托 (未注册 fail-closed, 早期
            // 启动无用户态进程, -ENOPROTOOPT 窗口安全; 符号与全文件
            // 负 errno 返回惯例一致)
            return match UDS_SETOPT_HOOK.get() {
                Some(&hook) => hook(fd, val != 0),
                None => -E_NOPROTOOPT,
            };
        }

        // D8: SOL_SOCKET 层的 REUSEADDR/REUSEPORT 接受但忽略.
        if level == SOL_SOCKET && (optname == SO_REUSEADDR || optname == SO_REUSEPORT) {
            return 0;
        }

        // D8: TCP_NODELAY (IPPROTO_TCP) 与 SO_KEEPALIVE (SOL_SOCKET) 需 TCP socket.
        let need_tcp = (level == IPPROTO_TCP && optname == TCP_NODELAY)
            || (level == SOL_SOCKET && optname == SO_KEEPALIVE);
        if !need_tcp {
            return -E_NOPROTOOPT;
        }
        if optlen < 4 {
            return -E_INVAL;
        }
        let val = core::ptr::read_unaligned(optval as *const i32);

        let _guard = NET_STATE.lock();
        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) != 1 {
            // 仅 TCP socket 支持 Nagle / keep-alive.
            return -E_NOPROTOOPT;
        }
        let Some(handle) = raw::socket_handle(slot) else {
            return -E_BADF;
        };
        let sockets = &mut *socket_set();
        let sock = sockets.get_mut::<tcp::Socket>(handle);
        if level == IPPROTO_TCP {
            // TCP_NODELAY: val != 0 关闭 Nagle.
            sock.set_nagle_enabled(val == 0);
        } else {
            // SO_KEEPALIVE: 开启用 7200s 缺省间隔 (对齐 Linux tcp_keepalive_time).
            sock.set_keep_alive(if val != 0 {
                Some(Duration::from_secs(7200))
            } else {
                None
            });
        }
        0
    }
}

/// POSIX `getsockopt` 内核实现 (D8: 精简集).
///
/// 已支持选项 (均写回 i32 到 `optval`, `*optlen` 置 4):
/// - `SO_TYPE` (`SOL_SOCKET`/3): `SOCK_STREAM(1)` / `SOCK_DGRAM(2)`.
/// - `SO_ERROR` (`SOL_SOCKET`/4): 近似恒 0 (见下方 SIMPLIFIED).
/// - `TCP_NODELAY` (`IPPROTO_TCP`/1, 仅 TCP): Nagle 禁用时 1, 启用时 0.
/// 其余 (`level`, `optname`): `-ENOPROTOOPT`.
///
/// # Safety
/// `optval` 必须是 syscall 层提供的可写内核指针 (≥ 4 字节), `optlen` 为可写内核 u32 指针。
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub unsafe extern "C" fn sm_getsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *mut u8,
    optlen: *mut u32,
) -> i32 {
    // SAFETY: optval/optlen 为 syscall 层传入的内核栈指针 (NET_STATE 锁保护 socket 集合).
    unsafe {
        if optval.is_null() || optlen.is_null() {
            return -E_INVAL;
        }
        // 出参缓冲由 syscall 层预置 (内核栈, 固定 4 字节); 不足视为 EINVAL.
        if core::ptr::read_unaligned(optlen) < 4 {
            return -E_INVAL;
        }

        let out: i32 = {
            let _guard = NET_STATE.lock();
            let Some(slot) = sm_slot(fd) else {
                return -E_BADF;
            };
            let stype = raw::fd_type(slot);
            if stype == 0 {
                return -E_BADF;
            }
            match (level, optname) {
                (SOL_SOCKET, SO_TYPE) => {
                    if stype == 1 {
                        SOCK_STREAM
                    } else {
                        SOCK_DGRAM
                    }
                }
                // P5c (DECISION-095): 非阻塞 connect 语义闭环 — sticky so_error
                // 从 smoltcp TCP 连接状态派生 (Established/优雅关闭态→0; SynSent/
                // SynReceived→EINPROGRESS; Closed→ECONNREFUSED; 监听→0). UDP 无异步
                // connect, 恒 0.
                // SIMPLIFIED: smoltcp 无 sticky so_error 字段, 且 Closed 不区分
                // "connect 失败" vs "从未连接"; Edgine 中 socket 关闭即从集合移除,
                // state==Closed 窗口实际仅对应 connect 失败, 故近似 ECONNREFUSED.
                // 精确 refused/timeout 区分归 P5d.
                (SOL_SOCKET, SO_ERROR) => {
                    if stype == 1 {
                        raw::socket_handle(slot).map_or(0, |h| {
                            let s = (&*socket_set()).get::<tcp::Socket>(h);
                            match s.state() {
                                tcp::State::SynSent | tcp::State::SynReceived => E_INPROGRESS,
                                tcp::State::Closed => E_CONNREFUSED,
                                _ => 0,
                            }
                        })
                    } else {
                        0
                    }
                }
                (IPPROTO_TCP, TCP_NODELAY) => {
                    if stype != 1 {
                        return -E_NOPROTOOPT;
                    }
                    let Some(handle) = raw::socket_handle(slot) else {
                        return -E_BADF;
                    };
                    let sockets = &mut *socket_set();
                    let sock = sockets.get::<tcp::Socket>(handle);
                    i32::from(!sock.nagle_enabled())
                }
                _ => return -E_NOPROTOOPT,
            }
        };
        core::ptr::write_unaligned(optval as *mut i32, out);
        core::ptr::write_unaligned(optlen, 4);
        0
    }
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

        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(slot) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if addr.is_null() || addrlen.is_null() {
            return -E_INVAL;
        }
        let stype = raw::fd_type(slot);
        let sockets = &mut *socket_set();

        let endpoint_opt: Option<IpEndpoint> = match stype {
            1 => {
                let sock = sockets.get::<tcp::Socket>(handle);
                // D9: TCP 未 connect 时 smoltcp local_endpoint() 为 None,
                // 回退 D1 本地端点表 (bind 时登记, 使 getsockname 可用).
                match sock.local_endpoint() {
                    Some(ep) => Some(ep),
                    None => raw::socket_local_endpoint(slot).map(|nep| IpEndpoint {
                        addr: wire_to_smol(nep.addr),
                        port: nep.port,
                    }),
                }
            }
            2 => {
                let sock = sockets.get::<udp::Socket>(handle);
                let ep = sock.endpoint();
                match ep.addr {
                    Some(addr) => Some(IpEndpoint {
                        addr,
                        port: ep.port,
                    }),
                    // D9/U7: 通配绑定时 smoltcp 存 None, 按 D1 登记的族如实回填
                    // (避免将 IPv6 通配误回为 IPv4 unspecified); D1 也缺省时退 V4.
                    None => Some(match raw::socket_local_endpoint(slot) {
                        Some(nep) => IpEndpoint {
                            addr: wire_to_smol(nep.addr),
                            port: ep.port,
                        },
                        None => IpEndpoint {
                            addr: IpAddress::Ipv4(Ipv4Address::UNSPECIFIED),
                            port: ep.port,
                        },
                    }),
                }
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

        let Some(slot) = sm_slot(fd) else {
            return -E_BADF;
        };
        if raw::fd_type(slot) == 0 {
            return -E_BADF;
        }
        let handle = match raw::socket_handle(slot) {
            Some(h) => h,
            None => return -E_BADF,
        };
        if addr.is_null() || addrlen.is_null() {
            return -E_INVAL;
        }
        let stype = raw::fd_type(slot);
        let sockets = &mut *socket_set();

        let endpoint_opt: Option<IpEndpoint> = match stype {
            1 => {
                let sock = sockets.get::<tcp::Socket>(handle);
                sock.remote_endpoint()
            }
            2 => {
                // D9: UDP 对端取 D1 remote 表 (connect 时登记); 未 connect → ENOTCONN.
                return match raw::socket_remote_endpoint(slot) {
                    Some(ep) => {
                        // SAFETY: write_sockaddr 按 ep.addr 分支写 sockaddr, addr 已校验非空.
                        write_sockaddr(addr, addrlen, &ep);
                        0
                    }
                    None => -E_NOTCONN,
                };
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

// ============================================================================
// P1 契约测试 (host-test): D1 端点表 / D2 临时端口分配器 / G10 FD 归还
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::net::iface_trait::{
        Ipv4Addr as TraitIpv4Addr, NetEndpoint as TraitEndpoint,
    };
    use crate::privileged::proc::fd_alloc::{FdPlan, FdSubsystem, alloc_fd, fd_at, free_fd};

    /// D1: local 端点表写入/读取/清空 往返一致.
    #[test]
    fn test_local_endpoint_table_roundtrip() {
        // allocate() 重置全部表, 避免索引越界与跨用例污染; 持锁串行化.
        let mut guard = NET_STATE.lock();
        guard.allocate();

        let ep = TraitEndpoint::new_v4(TraitIpv4Addr::new(127, 0, 0, 1), 12345);
        raw::set_socket_local_endpoint(0, Some(ep));
        assert_eq!(raw::socket_local_endpoint(0), Some(ep));

        raw::set_socket_local_endpoint(0, None);
        assert_eq!(raw::socket_local_endpoint(0), None);
    }

    /// D10 (P5a): blocking 标志 `allocate()` 默认阻塞, raw accessor 跨槽位往返一致.
    ///
    /// 全程持单次 `NET_STATE` 锁 (非可重入), 只验证 per-slot 阻塞表存储 (P5a 数据
    /// 结构本体); `sm_set_nonblocking`/`fcntl`/`socket_syscall` 为自锁 glue, 其
    /// 行为待 P5b 阻塞睡眠接入后经 e2e 验证.
    #[test]
    fn test_blocking_flag_default_and_roundtrip() {
        let mut guard = NET_STATE.lock();
        guard.allocate();
        // allocate() 默认阻塞 (POSIX); 抽查首/尾槽位.
        assert!(raw::is_blocking(0), "allocate 后应默认阻塞");
        assert!(raw::is_blocking(MAX_SM_FD - 1), "末槽位应默认阻塞");
        // 跨槽位 set/get 往返 (非阻塞仅翻 per-slot 标志, 不触 smoltcp).
        raw::set_blocking(0, false);
        assert!(!raw::is_blocking(0), "置非阻塞后应读到 false");
        assert!(raw::is_blocking(1), "相邻槽位不受影响");
        raw::set_blocking(0, true);
        assert!(raw::is_blocking(0));
    }

    /// D2: 连续分配返回互不相同的端口, 且均落在动态端口区间内.
    #[test]
    fn test_next_ephemeral_unique_and_in_range() {
        let mut guard = NET_STATE.lock();
        guard.allocate();

        let a = next_ephemeral().expect("应能分配临时端口");
        assert!(
            (EPHEMERAL_START..=EPHEMERAL_END).contains(&a),
            "端口 {a} 越界"
        );

        // 登记 a 为已占用, 下次分配必须避开它.
        let ep = TraitEndpoint::new_v4(TraitIpv4Addr::new(0, 0, 0, 0), a);
        raw::set_socket_local_endpoint(0, Some(ep));

        let b = next_ephemeral().expect("应能分配第二个临时端口");
        assert!((EPHEMERAL_START..=EPHEMERAL_END).contains(&b));
        assert_ne!(a, b, "已占用端口不应被再次分配");

        raw::set_socket_local_endpoint(0, None);
    }

    /// D2: 游标到达区间上界后回绕到区间下界.
    #[test]
    fn test_next_ephemeral_wraps_to_start() {
        let mut guard = NET_STATE.lock();
        guard.allocate();

        EPHEMERAL_CURSOR.store(EPHEMERAL_END, Ordering::Relaxed);
        let p = next_ephemeral().expect("回绕后应能分配");
        assert_eq!(p, EPHEMERAL_START);
    }

    /// G10: 归还 FD 编号后可重新分配 (修复 accept/socket 循环 FD 泄漏).
    #[test]
    fn test_free_fd_allows_realloc() {
        let first = alloc_fd(FdSubsystem::Smoltcp).expect("首次分配应成功");
        // 方案 C 回归: Smoltcp 段 fd 不得落入 VFS [0, 64) (命名空间重叠防线)
        assert!(
            FdPlan::SMOLTCP.contains(first) && !FdPlan::VFS.contains(first),
            "fd {first} 应落在 Smoltcp 段且不与 VFS 段重叠"
        );
        assert!(free_fd(FdSubsystem::Smoltcp, first), "归还已分配 FD 应成功");

        let second = alloc_fd(FdSubsystem::Smoltcp).expect("归还后应能重新分配");
        // 不与 first 比较具体编号: 其他并行用例可能也在分配同一子系统.
        free_fd(FdSubsystem::Smoltcp, second);
    }

    /// D4/D5: `endpoint_to_listen` 把通配地址映射为 `None`, 具体地址保留, 端口透传.
    #[test]
    fn test_endpoint_to_listen_wildcard_mapping() {
        let wildcard = TraitEndpoint::new_v4(TraitIpv4Addr::UNSPECIFIED, 80);
        let l = endpoint_to_listen(wildcard);
        assert_eq!(l.addr, None, "通配地址应映射为 None");
        assert_eq!(l.port, 80);

        let specific = TraitEndpoint::new_v4(TraitIpv4Addr::new(127, 0, 0, 1), 8080);
        let l = endpoint_to_listen(specific);
        assert!(l.addr.is_some(), "具体地址应保留");
        assert_eq!(l.port, 8080);
    }

    /// D3: 冲突检测按 (地址族, 端口) 判定; 端口不同不冲突.
    #[test]
    fn test_local_endpoint_in_use_conflict() {
        let mut guard = NET_STATE.lock();
        guard.allocate();

        let ep = TraitEndpoint::new_v4(TraitIpv4Addr::new(0, 0, 0, 0), 80);
        raw::set_socket_local_endpoint(0, Some(ep));

        // 同族同端口 (地址不同) 视为冲突.
        assert!(local_endpoint_in_use(TraitEndpoint::new_v4(
            TraitIpv4Addr::new(1, 2, 3, 4),
            80
        )));
        // 端口不同不冲突.
        assert!(!local_endpoint_in_use(TraitEndpoint::new_v4(
            TraitIpv4Addr::new(0, 0, 0, 0),
            81
        )));

        raw::set_socket_local_endpoint(0, None);
    }

    /// D3/D4/D5 契约: TCP bind 登记本地端点 + 冲突检测 → listen 状态迁移 →
    /// connect 在网络未配置时返回 `-E_NODEV`. connect 成功路径由 QEMU 端到端覆盖.
    #[test]
    fn test_tcp_bind_listen_connect_contract() {
        // 全程持单次 NET_STATE 锁 (非可重入), 故只调用 *_locked 变体.
        let mut guard = NET_STATE.lock();
        guard.allocate();
        raw::init_sockets();

        // host-test 下 kmalloc 仅 4 KiB early_buffer, 无法经受保护路径分配 TCP
        // 缓冲; 故以 Box::leak 自建 'static 缓冲并直接装配 socket, 绕开 k_malloc.
        let mk_tcp_fd = |fd_idx: usize| {
            // SAFETY: 持 NET_STATE 锁, SocketSet 已初始化.
            let sockets = unsafe { &mut *raw::socket_set() };
            // Box::leak 交出独占所有权并把生命周期提升为 'static, 仅此处持有.
            let rx: &'static mut [u8] = alloc::boxed::Box::leak(
                alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
            );
            let tx: &'static mut [u8] = alloc::boxed::Box::leak(
                alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
            );
            let handle = sockets.add(tcp::Socket::new(
                tcp::SocketBuffer::new(rx),
                tcp::SocketBuffer::new(tx),
            ));
            raw::set_socket_handle(fd_idx, Some(handle));
            raw::set_fd_type(fd_idx, 1);
        };

        // 127.0.0.1:8080 的 sockaddr (NE family + BE port).
        let mut sa = [0u8; 8];
        sa[0..2].copy_from_slice(&(2u16).to_ne_bytes());
        sa[2..4].copy_from_slice(&8080u16.to_be_bytes());
        sa[4..8].copy_from_slice(&[127, 0, 0, 1]);

        let fd = alloc_fd(FdSubsystem::Smoltcp).expect("分配 TCP FD");
        let slot = sm_slot(fd).expect("已分配 FD 应可换算槽位");
        mk_tcp_fd(slot);
        // SAFETY: 持 NET_STATE 锁, sockaddr 为本地栈数组且含 8 字节.
        assert_eq!(
            unsafe { sm_bind_locked(fd, sa.as_ptr(), 8) },
            0,
            "TCP bind 应成功"
        );
        assert_eq!(
            raw::socket_local_endpoint(slot).map(|e| e.port),
            Some(8080),
            "bind 后应登记本地端点"
        );

        // D3 冲突检测: 同族同端口再 bind 应失败.
        let fd2 = alloc_fd(FdSubsystem::Smoltcp).expect("分配第二个 TCP FD");
        mk_tcp_fd(sm_slot(fd2).expect("第二个 FD 应可换算槽位"));
        // SAFETY: 同上.
        assert_eq!(
            unsafe { sm_bind_locked(fd2, sa.as_ptr(), 8) },
            -E_ADDRINUSE,
            "同族同端口 bind 应冲突"
        );

        // D4: listen 后 socket 进入 Listen 状态.
        // SAFETY: 同上.
        assert_eq!(unsafe { sm_listen_locked(fd, 1) }, 0, "TCP listen 应成功");
        let handle = raw::socket_handle(slot).expect("fd 应有 handle");
        // SAFETY: 持 NET_STATE 锁, SocketSet 已初始化.
        let state = unsafe { (&*socket_set()).get::<tcp::Socket>(handle).state() };
        assert_eq!(state, tcp::State::Listen, "listen 后应处于 Listen 状态");

        // D4 缺省分支: 未 bind 直接 listen, 应自动分配临时端口并回写 D1 端点表.
        let fd3 = alloc_fd(FdSubsystem::Smoltcp).expect("分配第三个 TCP FD");
        mk_tcp_fd(sm_slot(fd3).expect("第三个 FD 应可换算槽位"));
        // SAFETY: 同上.
        assert_eq!(unsafe { sm_listen_locked(fd3, 1) }, 0, "缺省 listen 应成功");
        let port3 = raw::socket_local_endpoint(sm_slot(fd3).expect("第三个 FD 应可换算槽位"))
            .expect("listen 应回写本地端点")
            .port;
        assert!(
            (EPHEMERAL_START..=EPHEMERAL_END).contains(&port3),
            "缺省端口 {port3} 应落在动态区间"
        );

        // D5: 网络未配置时 connect 返回 -E_NODEV.
        let mut dst = [0u8; 8];
        dst[0..2].copy_from_slice(&(2u16).to_ne_bytes());
        dst[2..4].copy_from_slice(&9999u16.to_be_bytes());
        dst[4..8].copy_from_slice(&[127, 0, 0, 1]);
        // SAFETY: 同上.
        assert_eq!(unsafe { sm_connect_locked(fd, dst.as_ptr(), 8) }, -E_NODEV);
    }

    /// D6 契约: TCP `accept` 的门控与错误分支.
    ///
    /// 成功交接路径 (Established → 新建监听 → 索引交换 → 回写对端) 需要真实
    /// socket 缓冲, host-test 下 early_buffer 仅 4 KiB 无法承载, 故由 QEMU
    /// 端到端 (accept → recv → send) 覆盖; 此处仅锁定确定性错误分支.
    #[test]
    fn test_tcp_accept_contract() {
        let mut guard = NET_STATE.lock();
        guard.allocate();
        raw::init_sockets();

        // 与 D3/D4/D5 契约一致: host-test 下以 Box::leak 自建 'static 缓冲装配 socket.
        let mk_tcp_fd = |fd_idx: usize| {
            // SAFETY: 持 NET_STATE 锁, SocketSet 已初始化.
            let sockets = unsafe { &mut *raw::socket_set() };
            let rx: &'static mut [u8] = alloc::boxed::Box::leak(
                alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
            );
            let tx: &'static mut [u8] = alloc::boxed::Box::leak(
                alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
            );
            let handle = sockets.add(tcp::Socket::new(
                tcp::SocketBuffer::new(rx),
                tcp::SocketBuffer::new(tx),
            ));
            raw::set_socket_handle(fd_idx, Some(handle));
            raw::set_fd_type(fd_idx, 1);
        };

        // 无效 fd: 负数 / 超出 Smoltcp 段上界 / 未使用空槽 (经 fd_at 取段内数值) → -E_BADF.
        // SAFETY: 持 NET_STATE 锁, 地址指针为 null (accept 容忍 NULL).
        assert_eq!(
            unsafe { sm_accept_locked(-1, core::ptr::null_mut(), core::ptr::null_mut()) },
            -E_BADF
        );
        assert_eq!(
            unsafe {
                sm_accept_locked(
                    FdPlan::SMOLTCP.end_exclusive(),
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                )
            },
            -E_BADF
        );
        // allocate() 后未使用的槽 fd_type == 0.
        assert_eq!(
            unsafe {
                sm_accept_locked(
                    fd_at(FdSubsystem::Smoltcp, MAX_SM_FD - 1),
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                )
            },
            -E_BADF
        );

        // 非 TCP 类型 (fd_type != 1) → -E_NOTSUPP.
        let other_fd = alloc_fd(FdSubsystem::Smoltcp).expect("分配非 TCP 类型 FD");
        let other_slot = sm_slot(other_fd).expect("已分配 FD 应可换算槽位");
        mk_tcp_fd(other_slot);
        raw::set_fd_type(other_slot, 2);
        // SAFETY: 同上.
        assert_eq!(
            unsafe { sm_accept_locked(other_fd, core::ptr::null_mut(), core::ptr::null_mut()) },
            -E_NOTSUPP
        );

        // TCP fd 但未完成三次握手 (Closed) → -E_AGAIN.
        let tcp_fd = alloc_fd(FdSubsystem::Smoltcp).expect("分配 TCP FD");
        mk_tcp_fd(sm_slot(tcp_fd).expect("已分配 FD 应可换算槽位"));
        // SAFETY: 同上.
        assert_eq!(
            unsafe { sm_accept_locked(tcp_fd, core::ptr::null_mut(), core::ptr::null_mut()) },
            -E_AGAIN
        );
    }

    /// D9: remote 端点表写入/读取/清空 往返一致 (UDP connect 登记对端的基础).
    #[test]
    fn test_remote_endpoint_table_roundtrip() {
        let mut guard = NET_STATE.lock();
        guard.allocate();

        let ep = TraitEndpoint::new_v4(TraitIpv4Addr::new(10, 0, 0, 1), 53);
        raw::set_socket_remote_endpoint(0, Some(ep));
        assert_eq!(raw::socket_remote_endpoint(0), Some(ep));

        raw::set_socket_remote_endpoint(0, None);
        assert_eq!(raw::socket_remote_endpoint(0), None);
    }

    /// D7: `shutdown` 半关闭契约 — 非法 fd / 非法 how 错误分支, SHUT_RD no-op 返 0,
    /// SHUT_WR 保留 socket (与 close 的关键差异: 不回收 handle/fd_type/FD 编号).
    #[test]
    fn test_shutdown_contract() {
        let mut guard = NET_STATE.lock();
        guard.allocate();
        raw::init_sockets();

        // 建一个 TCP socket (host-test 下 Box::leak 自建 'static 缓冲, 绕开 k_malloc).
        // SAFETY: 持 NET_STATE 锁, SocketSet 已初始化.
        let sockets = unsafe { &mut *raw::socket_set() };
        let rx: &'static mut [u8] = alloc::boxed::Box::leak(
            alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
        );
        let tx: &'static mut [u8] = alloc::boxed::Box::leak(
            alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
        );
        let handle = sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(rx),
            tcp::SocketBuffer::new(tx),
        ));
        let fd = alloc_fd(FdSubsystem::Smoltcp).expect("分配 TCP FD");
        let slot = sm_slot(fd).expect("FD 应可换算槽位");
        raw::set_socket_handle(slot, Some(handle));
        raw::set_fd_type(slot, 1);

        // 非法 fd → -E_BADF.
        assert_eq!(unsafe { sm_shutdown_locked(-1, 1) }, -E_BADF);
        // 合法 socket 但非法 how (非 0/1/2) → -E_INVAL.
        assert_eq!(unsafe { sm_shutdown_locked(fd, 5) }, -E_INVAL);
        // SHUT_RD(0) → no-op 返 0, socket 保留.
        assert_eq!(unsafe { sm_shutdown_locked(fd, 0) }, 0);
        assert!(raw::fd_type(slot) == 1 && raw::socket_handle(slot).is_some());
        // SHUT_WR(1) → 返 0, 且 socket/FD **未被回收** (区别于 close).
        assert_eq!(unsafe { sm_shutdown_locked(fd, 1) }, 0);
        assert_eq!(raw::fd_type(slot), 1, "SHUT_WR 不应改 fd_type");
        assert!(
            raw::socket_handle(slot).is_some(),
            "SHUT_WR 不应 remove socket"
        );
    }

    /// D8b: `sm_socket_poll` 错误/状态分支 — 非法 fd 与空槽 → POLLNVAL;
    /// TCP 新建 socket (state Closed) → POLLHUP|POLLERR. 真实收发就绪由 QEMU e2e 覆盖.
    #[test]
    fn test_socket_poll_contract() {
        let mut guard = NET_STATE.lock();
        guard.allocate();
        raw::init_sockets();

        // 非法 fd (负数) → POLLNVAL.
        // SAFETY: 调用 *_locked 变体要求持 NET_STATE 锁 (本测试已持 guard); 负 fd 经 sm_slot 返 None, 不解引用 socket_set.
        assert_eq!(unsafe { sm_socket_poll_locked(-1, POLLIN) }, POLLNVAL);
        // 段内未使用空槽 (fd_type == 0) → POLLNVAL.
        // SAFETY: 持 NET_STATE 锁 (guard); 空槽 fd_type==0 于取 handle 前早返回 POLLNVAL, 不触碰 socket_set.
        assert_eq!(
            unsafe { sm_socket_poll_locked(fd_at(FdSubsystem::Smoltcp, MAX_SM_FD - 1), POLLIN) },
            POLLNVAL
        );

        // 建 TCP socket (新建态 Closed), poll 应报 POLLHUP|POLLERR.
        // SAFETY: 持 NET_STATE 锁, SocketSet 已初始化.
        let sockets = unsafe { &mut *raw::socket_set() };
        let rx: &'static mut [u8] = alloc::boxed::Box::leak(
            alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
        );
        let tx: &'static mut [u8] = alloc::boxed::Box::leak(
            alloc::vec![0u8; super::super::TCP_BUF_SIZE].into_boxed_slice(),
        );
        let handle = sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(rx),
            tcp::SocketBuffer::new(tx),
        ));
        let fd = alloc_fd(FdSubsystem::Smoltcp).expect("分配 TCP FD");
        let slot = sm_slot(fd).expect("FD 应可换算槽位");
        raw::set_socket_handle(slot, Some(handle));
        raw::set_fd_type(slot, 1);

        let rev = unsafe { sm_socket_poll_locked(fd, POLLIN | POLLOUT) };
        assert_ne!(rev & POLLHUP, 0, "Closed TCP 应报 POLLHUP");
        assert_ne!(rev & POLLERR, 0, "Closed TCP 应报 POLLERR");
    }
}
