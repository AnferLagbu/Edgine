#![deny(unsafe_code)]
//! 网络子系统 — functions 层安全代理
//!
//! 封装 smoltcp 协议栈的 safe 入口, 提供 socket / DHCP / 路由 / Netfilter
//! 等用户态可见的策略层 API. 0 unsafe, 全部硬件交互走 privileged.
//!
//! 历史: 2026-06 之前 v2.7 状态评估已过时, 当前 Phase 2.4 网络栈收尾
//! 已完成, IPv4/IPv6 双栈支持 (DECISION-032) 已实装. 详细进度见
//! 进度跟踪文档 (docs/plan/progress-active-tasks.md).

use crate::functions::net::smoltcp_impl::SmoltcpNetStack;
use crate::privileged::net_socket as fw_net_socket;
use crate::privileged::sync::{Mutex, OnceLock};

/// 全局 `SmoltcpNetStack` 实例, 由 `init()` 初始化, socket.rs 通过 `net_stack()` 访问
///
/// M3 修复: 使用 Mutex 替代 `IrqSpinLock`, 因为网络操作 (socket/bind/listen 等)
/// 都在进程上下文执行, 不在中断上下文, 可以使用睡眠锁减少 CPU 空转。
static NET_STACK_INSTANCE: OnceLock<Mutex<SmoltcpNetStack>> = OnceLock::new();

/// 获取全局 `SmoltcpNetStack` 实例的引用 (需在 `init()` 之后调用)
///
/// 返回 `None` 表示网络子系统未初始化，调用方应返回 `NotReady` 错误而非 panic。
pub fn net_stack() -> Option<&'static Mutex<SmoltcpNetStack>> {
    NET_STACK_INSTANCE.get()
}

// ============================================================================
// 状态类型 (统一定义, 避免 kernel_test stub 重复)
// ============================================================================

/// 网络初始化状态 (与 `kernel::net::init::InitState` 对齐)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InitState {
    Uninitialized = 0,
    HardwareProbed = 1,
    InterfaceReady = 2,
    FullyInitialized = 3,
    Failed = 255,
}

// I-预存 (kernel_test build): 同 net_socket.rs 处理 — 用 cfg-gate `use` 别名
// + 桩模块, 让函数体保持 `init::*` 调用, 不扩散 cfg 到 fn body.
#[cfg(not(feature = "kernel_test"))]
use crate::privileged::net::init;

// kernel_test 桩: 对齐真实 `net::init` 暴露的类型与函数签名.
// 同时为 smoltcp_impl.rs 的 `fw_init::` 调用提供 kernel_test no-op stub.
#[cfg(feature = "kernel_test")]
mod init {
    // 复用外层 InitState, 避免类型重复定义
    pub use super::InitState;

    pub fn is_network_initialized() -> bool {
        false
    }
    pub fn is_network_configured() -> bool {
        false
    }
    pub fn get_init_state() -> InitState {
        InitState::Uninitialized
    }
    // I-预存 (kernel_test build): 为 smoltcp_impl.rs 的 fw_init:: 调用
    // 提供 no-op stub, 避免编译失败. 测试模式下不创建真实 socket.
    use crate::privileged::net::iface_trait::SocketKind;
    pub fn smoltcp_net_stack_socket_open(_kind: SocketKind, _slot_idx: usize) -> Option<u32> {
        None
    }
    pub fn smoltcp_net_stack_slot_base() -> usize {
        0
    }
    pub fn smoltcp_net_stack_poll() -> crate::privileged::net::iface_trait::PollOutcome {
        crate::privileged::net::iface_trait::PollOutcome::idle()
    }
    pub fn smoltcp_net_stack_close(_slot_idx: usize) {}
}

/// REVAL-W 第 6 组 W6 (2026-06-25): DHCP 策略 trait 抽象
/// (何时重试/续约/fallback), 机制与策略分离.
pub mod dhcp_policy;
pub mod netfilter;
pub mod route;
/// REVAL-W 第 6 组 W3.2 (2026-06-24): NetStack trait 的 smoltcp 实现
/// 设计: docs/plan/smoltcp-framekernel-wrapper.md §3.2
pub mod smoltcp_impl;
pub mod socket;
pub mod syscall;
/// T6-9: 网络子系统公共类型 (原 privileged/net/types.rs)
pub mod types;
pub mod unix;
/// T6-9: Socket 等待队列 (原 `privileged/net/wait_queue.rs`)
pub mod wait_queue;

pub use socket::{
    Domain, SockAddrIn, SockType, SocketError, SocketResult, accept, bind, close, connect,
    endpoint_from_str, getsockopt, listen, parse_ipv4, poll_all, recv, recvfrom, send, sendto,
    setsockopt, socket,
};

pub use unix::{
    FD_BASE as UNIX_FD_BASE, PATH_MAX as UNIX_PATH_MAX, SockAddrUn, UnixResult, UnixSocketError,
    accept as unix_accept, bind as unix_bind, close as unix_close, connect as unix_connect,
    is_uds_fd, listen as unix_listen, recv as unix_recv, recvfrom as unix_recvfrom,
    send as unix_send, sendto as unix_sendto, socket as unix_socket, unlink as unix_unlink,
};

// ============================================================================
// 错误
// ============================================================================

/// 网络操作错误 — TD-20: 收敛到 `KernelError`, 1 字段 net 特有 + 1 共享包装.
///
/// 字段说明:
///   - `NotConfigured`: DHCP 未配置 (网络层语义, 不在 POSIX 通用集)
///   - `Kernel(KernelError)`: 共享错误 (NotReady/InvalidArgument/NotFound/Io
///     等) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// DHCP 未配置
    NotConfigured,
    /// 共享 `KernelError` 包装
    Kernel(crate::functions::error::KernelError),
}

impl NetError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::NotConfigured => E::ENETDOWN,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    pub fn from_i32(rc: i32) -> Self {
        use crate::functions::error::KernelError as K;
        match rc {
            -1 => Self::Kernel(K::NotReady),
            -2 => Self::Kernel(K::NoSuchProcess),
            -5 => Self::Kernel(K::Fault),
            -19 => Self::Kernel(K::NoDevice),
            -22 => Self::Kernel(K::InvalidArgument),
            -101 => Self::NotConfigured,
            _ => Self::Kernel(K::Other(rc)),
        }
    }
}

/// functions 层结果类型别名
pub type NetResult<T> = Result<T, NetError>;

use crate::privileged::syscall::Errno;

// ============================================================================
// 状态转换
// ============================================================================

// 非 kernel_test: init::InitState 是 privileged 类型, 需要 From 转换
// kernel_test: init::InitState 就是 super::InitState, From 是恒等转换
#[cfg(not(feature = "kernel_test"))]
impl From<init::InitState> for InitState {
    fn from(s: init::InitState) -> Self {
        match s {
            init::InitState::Uninitialized => Self::Uninitialized,
            init::InitState::HardwareProbed => Self::HardwareProbed,
            init::InitState::InterfaceReady => Self::InterfaceReady,
            init::InitState::FullyInitialized => Self::FullyInitialized,
            init::InitState::Failed => Self::Failed,
        }
    }
}

// ============================================================================
// 顶层 API
// ============================================================================

/// 初始化网络子系统
///
/// 探测网卡 (e1000 / virtio-net), 启动协议栈, 启动 DHCP 异步获取 IP。
/// 如果无 NIC 则进入 "`NoNetwork`" 状态。
pub fn init() {
    // 初始化全局 SmoltcpNetStack 实例
    // M3 修复: 使用 Mutex 替代 IrqSpinLock
    NET_STACK_INSTANCE.get_or_init(|slot| {
        slot.write(Mutex::new(SmoltcpNetStack::new()));
    });
    fw_net_socket::eg_net_init();
}

/// 轮询网络栈 (驱动 TX/RX、定时器、DHCP)
///
/// 在 timer ISR 或网络任务中调用, 内部 `try_lock` 避免阻塞。
/// 若网络锁已被持有则直接返回, 不会等待。
pub fn poll() {
    fw_net_socket::poll_network();
}

/// 启动 DHCP (异步, 由 timer ISR 驱动 poll 完成)
///
/// 调用后 DHCP Discover 会在下一个 timer tick 发出。
/// 用户态通过 `is_configured()` 轮询等待完成。
///
/// # Errors
///
/// 当底层 DHCP 启动失败时返回 `Err(NetError)`, 例如网络栈尚未初始化或网卡不支持 DHCP。
pub fn start_dhcp() -> NetResult<()> {
    let rc = fw_net_socket::eg_net_start_dhcp();
    if rc == 0 {
        Ok(())
    } else {
        Err(NetError::from_i32(rc))
    }
}

/// 设置静态 IP (格式: "10.0.2.15/24,10.0.2.2")
///
/// # 参数
/// - `cidr_str`: CIDR 字符串, 如 "192.168.1.100/24"
/// - `gw_str`: 网关地址字符串, 如 "192.168.1.1"
///
/// # 返回
/// 成功返回 `Ok(())`, 失败返回 `NetError`
///
/// # Errors
///
/// 当 CIDR 或网关字符串格式非法、无法解析, 或底层静态 IP 配置失败时返回 `Err(NetError)`。
pub fn static_ip(cidr_str: &str, gw_str: &str) -> NetResult<()> {
    // 复制到 C 字符串 (添加 NUL 终止符)
    let mut cidr_c = alloc::vec::Vec::with_capacity(cidr_str.len() + 1);
    cidr_c.extend_from_slice(cidr_str.as_bytes());
    cidr_c.push(0);

    let mut gw_c = alloc::vec::Vec::with_capacity(gw_str.len() + 1);
    gw_c.extend_from_slice(gw_str.as_bytes());
    gw_c.push(0);

    let rc = fw_net_socket::eg_net_static_ip(cidr_c.as_ptr(), gw_c.as_ptr());
    if rc == 0 {
        Ok(())
    } else {
        Err(NetError::from_i32(rc))
    }
}

// ============================================================================
// 状态查询
// ============================================================================

/// 网络是否已完全初始化
pub fn is_initialized() -> bool {
    init::is_network_initialized()
}

/// 网络是否已完成 DHCP/Static IP 配置
pub fn is_configured() -> bool {
    init::is_network_configured()
}

/// 当前初始化状态
pub fn state() -> InitState {
    #[cfg(feature = "kernel_test")]
    {
        // kernel_test 模式: init 是 functions::net::init 子模块, 返回 super::InitState, 无需转换
        init::get_init_state()
    }
    #[cfg(not(feature = "kernel_test"))]
    {
        // 非 kernel_test: init 是 privileged::net::init, 需 From 转换
        InitState::from(init::get_init_state())
    }
}

/// 重置网络状态 (恢复机制)
pub fn reset_state() {
    fw_net_socket::reset_network_state();
}
