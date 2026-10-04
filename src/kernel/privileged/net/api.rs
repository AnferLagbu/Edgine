//! 网络子系统 API 层
//!
//! 基于 smoltcp 协议栈的网络初始化和 Socket 操作入口。
//!
//! ## 调用方契约
//! - `syscall::mod` —— `sys_socket/sys_connect/sys_accept/sys_sendto/sys_recvfrom` 系统调用
//! - `proc::api` —— 进程创建/销毁时关联 socket fd
//! - `egdf::proto_net` —— 网卡设备注册/注销
//! - `freg::recovery` —— 网络子系统纳入恢复域
//!
//! ## 内部接口
//! - `init.rs` —— 初始化状态机, DHCP, Socket API (`poll_network/tcp`_*/udp_*)
//! - `smoltcp_impl.rs` —— `EGDFNetDevice` + `NetworkStack` + Device trait 实现
//! - `types.rs` —— 公共状态 (`NET_READY` / `NET_CONFIGURED`)
//!
//! ## 安全约束
//! - 所有 static mut 变量在 `NET_LOCK`: Mutex<()> 保护下访问
//! - `poll_network()` 使用 `try_lock()` 避免 ISR 上下文阻塞
//! - Socket 操作在 #[cfg(feature = "net")] 控制下, `kernel_test` 模式不链接
//!
//! ## 性能特征
//! - `poll_network()`: 单次轮询, 无阻塞; ISR 安全
//! - Socket 创建: O(1) 数组扫描 (`MAX_SOCKETS` = 8)
//! - DHCP: 异步状态机, 不阻塞内核主循环

// ============================================================================
// 契约 trait: NetworkDevice — 所有网卡驱动必须实现
// ============================================================================

/// 网卡设备抽象。
///
/// egdf 注册网络设备时,驱动必须提供此 trait 的实现。
pub trait NetworkDevice: Send + Sync {
    /// 设备名称 (MAC 地址格式)
    fn name(&self) -> &'static str;

    /// 发送以太网帧
    ///
    /// # Errors
    /// 当发送失败 (如设备忙、链路断开或缓冲不足) 时返回 `Err(())`.
    fn transmit(&self, buf: &[u8]) -> Result<(), ()>;

    /// 轮询接收 (非阻塞, ISR 安全)
    fn poll(&self);
}

// ============================================================================
// 契约: 初始化
// ============================================================================

#[cfg(not(feature = "kernel_test"))]
use super::init::InitState;

/// 轮询网络设备 (由内核主循环或定时器调用)
///
/// # 安全约束
/// - ISR 安全: 使用 `try_lock()` 避免阻塞
/// - `kernel_test` 模式下不可用 (无真实硬件)
#[cfg(not(feature = "kernel_test"))]
pub fn poll_network() {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe { super::init::poll_network() }
}

/// 查询网络是否已完全初始化
#[cfg(not(feature = "kernel_test"))]
pub fn is_network_initialized() -> bool {
    super::init::is_network_initialized()
}

/// 查询网络是否已完成 DHCP 配置
#[cfg(not(feature = "kernel_test"))]
pub fn is_network_configured() -> bool {
    super::init::is_network_configured()
}

/// 查询当前初始化状态
#[cfg(not(feature = "kernel_test"))]
pub fn get_init_state() -> InitState {
    super::init::get_init_state()
}

// ============================================================================
// 高层 API (D1.1 收尾)
//
// 设计原则:
// - 与 init.rs 内部状态机解耦, 通过 NET_LOCK 间接访问
// - 全部 #[cfg(not(feature = "kernel_test"))], host-test 不链接
// - 返回值用类型化结构 (而非裸 [u8; N]) 以保证 API 自文档化
// ============================================================================

/// 主动触发网络初始化 (非阻塞; 失败返回 false)
///
/// # 调用方
/// 启动期显式调用, 替代依赖自动初始化的隐式行为.
///
/// # 行为
/// - 状态机 Uninitialized → `HardwareProbed` → `InterfaceReady`
/// - DHCP 配置是异步的, 此函数仅触发握手, 不等待结果
/// - 若已初始化, 立即返回 true
#[cfg(not(feature = "kernel_test"))]
pub fn init_network_now() -> bool {
    super::init::trigger_init()
}

/// 查询设备 MAC 地址
///
/// # 返回
/// - `Some(mac)` — 已初始化, 6 字节大端 MAC
/// - `None`      — 网络未就绪
#[cfg(not(feature = "kernel_test"))]
pub fn get_mac_address() -> Option<[u8; 6]> {
    super::init::get_mac_address()
}

/// 查询当前 IPv4 地址
///
/// # 返回
/// - `Some(ip)` — DHCP/静态配置成功
/// - `None`     — 仍在配置中或失败
#[cfg(not(feature = "kernel_test"))]
pub fn get_ipv4_address() -> Option<[u8; 4]> {
    super::init::get_ipv4_address()
}

/// 查询默认网关 IPv4
#[cfg(not(feature = "kernel_test"))]
pub fn get_default_gateway() -> Option<[u8; 4]> {
    super::init::get_default_gateway()
}

/// 查询 DNS 服务器 (最多 3 个)
#[cfg(not(feature = "kernel_test"))]
pub fn get_dns_servers() -> [Option<[u8; 4]>; 3] {
    super::init::get_dns_servers()
}

/// 简单 DNS 解析 (主机名 → IPv4)
///
/// # 实现
/// - 优先查静态 hosts 表 (`/etc/hosts` 风格, 内置)
///
/// # 局限
/// - 不发起 DNS UDP 查询 (后续 D 阶段可换 smoltcp wire/dns 升级)
/// - 仅支持 IPv4 单地址 (无 AAAA / CNAME / SRV)
#[cfg(not(feature = "kernel_test"))]
pub fn dns_resolve(name: &str) -> Option<[u8; 4]> {
    super::init::dns_resolve(name)
}

/// 显式关闭网络栈 (释放 DHCP 租约 + 关闭 socket)
///
/// # Safety
/// - 调用方必须保证: 关闭前所有用户态 socket fd 已 close
/// - 调用后, get_* 系列 API 全部返回 None/false
#[cfg(not(feature = "kernel_test"))]
pub fn shutdown_network() {
    super::init::shutdown_network();
}

/// 网络状态快照 (给观测 / 调试用, 单次复制, 无锁)
#[cfg(not(feature = "kernel_test"))]
pub fn status_snapshot() -> super::init::NetStatus {
    super::init::NetStatus::capture()
}
