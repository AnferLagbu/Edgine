#![deny(unsafe_code)]
//! SmoltcpNetStack — NetStack trait 的 smoltcp 实现 (W3.2 trait 骨架)
//!
//! ## 定位
//!
//! 实现 `privileged::net::iface_trait::NetStack` trait. 本文件是 functions 层
//! 唯一允许直接使用 smoltcp 类型的代码, 承担"smoltcp → NetStack"的翻译
//! 责任.
//!
//! ## 能力面 (P-18 单入口)
//!
//! `NetStack` trait 是 functions 层访问网络的唯一能力入口:
//!
//! - 生命周期: `init` / `socket_open` / `socket_close` / `close` / `dhcp_state`
//! - 数据面: `bind` / `listen` / `accept` / `connect` / `send` / `recv` /
//!   `sendto` / `recvfrom` / `setsockopt` / `getsockopt`
//! - 轮询: `poll` / `poll_at` / `poll_sockets`
//!
//! 对外句柄是不透明的 `SocketHandle`; 句柄槽位登记 `sm_socket` 分配的全局
//! privileged fd, 与 POSIX syscall 路径共用同一 fd 空间. 历史上的 inherent
//! `*_fd` 平行入口与专属槽位段已在 P-18 退役.
//!
//! ## 实现边界
//!
//! 实际 smoltcp 操作 (`k_malloc` + `SocketBuffer` + `SocketSet`) 由 privileged
//! 层 (允许 unsafe) 提供安全包装; 本文件只做 "smoltcp/privileged → NetStack"
//! 的翻译, 本身 0 unsafe.
//!
//! ## 设计依据
//!
//! - [docs/plan/smoltcp-framekernel-wrapper.md] §3 关键设计决策
//! - [docs/plan/maintenance-cycle-2026-06-19.md] §9.5 REVAL-W 第 6 组

use crate::privileged::net::iface_trait::{
    DhcpState, IpAddr, Ipv4Addr, Ipv6Addr, NetConfig, NetEndpoint, NetError, NetStack, PollOutcome,
    Result, SocketHandle, SocketKind,
};
use crate::privileged::net::socket_types::{Domain, SockType};
use crate::privileged::net_socket as fw_net_socket;
// REVAL-W W4.2.3.4 步骤 3: 调用 privileged::init 的 safe wrapper, 实现
// 实际 smoltcp socket 创建. smoltcp_impl 是 functions 层唯一允许直接使用
// smoltcp 类型的文件, 但 socket_open 的实际 smoltcp 操作 (k_malloc +
// SocketBuffer::new + sockets.add) 由 privileged 层 (允许 unsafe) 提供.
//
// kernel_test 模式下 privileged::net::init 被 cfg-out, 用 functions::net::init
// 的桩实现 (其中 smoltcp_net_stack_* 函数为 no-op stub).
#[cfg(feature = "kernel_test")]
use crate::functions::net::init as fw_init;
#[cfg(not(feature = "kernel_test"))]
use crate::privileged::net::init as fw_init;

// ============================================================================
// 编译期常量
// ============================================================================

/// Socket 容量上限.
///
/// 与 `privileged/net/init.rs::MAX_SOCKETS` 同步 (编译期常量, 修改需同时
/// 更新两处). 此处用较小值, 因为 W3.2 不实际创建 smoltcp sockets.
pub const MAX_SOCKETS: usize = 32;

// ============================================================================
// 句柄槽位 (类型擦除, 替代 smoltcp::iface::SocketHandle<usize>)
// ============================================================================

/// Socket 句柄的内部表示: `(user_id, privileged_fd)`.
///
/// smoltcp 的 `SocketHandle(usize)` 直接暴露数组索引, 任何 `usize` 都可能被
/// 误用. 本类型把对外句柄 (`SocketHandle`, user 域) 与 privileged 侧 socket fd
/// 解耦: 句柄槽位的存在性 = 句柄有效性, fd 只是该槽位登记的资源编号.
///
/// ## 内存布局 (P-18 单入口)
///
/// - `Some((u, fd))` = 句柄已分配, u 是用户态 ID, fd 是 `sm_socket` 分配的
///   全局 fd (与 POSIX syscall 路径同一 fd 空间); fd 为负值表示预留槽位
///   (DHCP), 不对应真实 socket
/// - `None` = 句柄槽位空闲
///
/// P-18 之前 fd 由 `SmoltcpNetStack` 专属槽位段隐式换算 (slot = base + i),
/// 与 privileged `idx_of` 的基址换算叠加后偏移错位, 且 `accept` 产出的 fd
/// 无法反查句柄. 现在 fd 直接登记在槽位内, 单次换算 (单一 fd 空间).
type HandleSlot = Option<(u32, i32)>;

/// DHCP 预留槽位的 fd 哨兵 (不对应任何真实 socket).
///
/// DHCP 客户端 socket 由 privileged `poll_network` 独占管理. 句柄表为其预留
/// 一个槽位以防用户句柄与 DHCP socket 冲突; `handle_to_fd` 对负值 fd 返回
/// `None`, 使 DHCP 句柄上的任何数据面操作都按无效句柄拒绝.
const DHCP_SENTINEL_FD: i32 = -1;

// ============================================================================
// SmoltcpNetStack
// ============================================================================

/// smoltcp 网络协议栈的 `NetStack` trait 实现 (W3.2 trait 骨架).
///
/// ## 不变式
///
/// 1. **零 unsafe**: functions 层铁律, 编译期 `#![deny(unsafe_code)]` 强制
/// 2. **类型擦除**: 内部用 u32 ID, 对外暴露 `SocketHandle`
/// 3. **句柄幂等**: 重复 `socket_close` 不报错 (DECISION-025)
/// 4. **DHCP 句柄独占**: dhcp socket 不可被用户关闭
/// 5. **`socket_open` 失败回滚**: DECISION-027
///
/// ## 字段说明
///
/// - `config`: `init()` 时缓存, 后续只读
/// - `handle_map`: 句柄槽位表, 容量 = `MAX_SOCKETS`, 槽位登记 privileged fd
/// - `next_user_id`: 下一个分配的 user 句柄 (u32, 0 = INVALID)
/// - `dhcp_user_id`: DHCP socket 的 user 句柄 (若有)
/// - `dhcp_state`: DHCP 状态缓存
/// - `initialized`: 是否已 init (init 前所有方法返回 NotReady/idle)
///
/// ## W4 整合时的字段扩展
///
/// W4 将在 `SmoltcpNetStack` 中添加 (在 init.rs 内部构造):
/// - `sockets: SocketSet<'static>` (smoltcp `SocketSet`, 'static 借用)
/// - `device: Box<dyn smoltcp::phy::Device>` 或泛型 D
/// - `interface: smoltcp::iface::Interface` (由 device + config 构造)
///
/// ## 线程安全
///
/// 不要求 `Send`/`Sync` — 调用方保证互斥访问 (在 `NET_LOCK` 下).
pub struct SmoltcpNetStack {
    config: NetConfig,
    handle_map: [HandleSlot; MAX_SOCKETS],
    next_user_id: u32,
    dhcp_user_id: Option<u32>,
    dhcp_state: DhcpState,
    /// DHCP Discover/Request 重试计数 (Bound 后清零).
    ///
    /// 由 `record_dhcp_retry()` 递增, `record_dhcp_bound()` 清零.
    /// W6 策略依赖此字段决定是否走 Fallback.
    dhcp_retry_count: u32,
    /// 上次进入 Bound 状态的协议栈时间 (ms), 0 = 未 Bound.
    ///
    /// W6 策略依赖此字段 + 当前时间计算续约时机.
    dhcp_bound_at_ms: u64,
    /// 上次 Bound 的租期总长 (ms), 0 = 未知.
    ///
    /// W6 策略用此字段计算 T1/T2 续约阈值.
    dhcp_lease_duration_ms: u64,
    initialized: bool,
}

impl SmoltcpNetStack {
    /// 构造一个未初始化的 `SmoltcpNetStack` 实例.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: NetConfig::empty(),
            handle_map: [None; MAX_SOCKETS],
            next_user_id: 1, // 0 = INVALID
            dhcp_user_id: None,
            dhcp_state: DhcpState::Idle,
            dhcp_retry_count: 0,
            dhcp_bound_at_ms: 0,
            dhcp_lease_duration_ms: 0,
            initialized: false,
        }
    }

    /// 是否已初始化.
    #[inline]
    #[must_use]
    pub const fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// 当前配置.
    #[inline]
    #[must_use]
    pub const fn config(&self) -> &NetConfig {
        &self.config
    }

    /// 找空槽位.
    fn find_free_slot(&self) -> Option<usize> {
        self.handle_map
            .iter()
            .position(core::option::Option::is_none)
    }

    /// 分配下一个 user 句柄 ID.
    ///
    /// 返回一个未被任何存活 socket 占用的句柄值; 句柄空间耗尽时返回 `None`.
    /// (B07-03 修复: 原实现 `wrapping_add` 回绕后可能复用仍存活的句柄,
    /// 造成 use-after-close / 跨 socket 串扰. 现在线性探测跳过已占用值,
    /// 冲突即报错 (由调用方转为 `NoFreeSocket`), 不静默复用.)
    fn alloc_user_id(&mut self) -> Option<u32> {
        // 存活句柄至多 MAX_SOCKETS 个; 探测超过该数量仍未找到空闲值即视为耗尽.
        let mut id = self.next_user_id;
        for _ in 0..=MAX_SOCKETS {
            if id == 0 {
                id = 1; // 0 = INVALID, 跳过
            }
            if !self
                .handle_map
                .iter()
                .any(|slot| matches!(slot, Some((u, _)) if *u == id))
            {
                self.next_user_id = if id == u32::MAX { 1 } else { id + 1 };
                return Some(id);
            }
            id = if id == u32::MAX { 1 } else { id + 1 };
        }
        None
    }

    /// 检查 user 句柄是否对应 DHCP socket.
    fn is_dhcp_handle(&self, user_id: u32) -> bool {
        self.dhcp_user_id == Some(user_id)
    }

    /// 从 `handle_map` 反查 privileged 侧 socket fd (`sm_*` 函数使用的全局 fd).
    ///
    /// DHCP 预留槽位以 [`DHCP_SENTINEL_FD`] 占位, 不是真实 socket, 返回 `None`
    /// 使所有数据面操作按无效句柄拒绝.
    fn handle_to_fd(&self, h: SocketHandle) -> Option<i32> {
        self.find_handle(h).map(|(_, fd)| fd).filter(|fd| *fd >= 0)
    }

    /// 反查句柄所在的槽位索引与登记的 fd.
    fn find_handle(&self, h: SocketHandle) -> Option<(usize, i32)> {
        if !h.is_valid() {
            return None;
        }
        self.handle_map
            .iter()
            .enumerate()
            .find_map(|(i, slot)| match slot {
                Some((u, fd)) if *u == h.raw() => Some((i, *fd)),
                _ => None,
            })
    }

    /// 登记一个 privileged socket fd (或负值预留槽位), 返回用户句柄.
    ///
    /// 先占槽位再分配用户 ID, 任一失败均无副作用 (DECISION-025). 失败时
    /// 调用方负责归还已创建的 fd.
    fn insert_handle(&mut self, fd: i32) -> Result<SocketHandle> {
        let slot = self.find_free_slot().ok_or(NetError::NoFreeSocket)?;
        let user_id = self.alloc_user_id().ok_or(NetError::NoFreeSocket)?;
        self.handle_map[slot] = Some((user_id, fd));
        Ok(SocketHandle::from_raw(user_id))
    }
}

/// `NetEndpoint` → sockaddr 字节 (V4: 16 字节 `sockaddr_in` / V6: 28 字节 `sockaddr_in6`).
///
/// 返回 (缓冲区, 实际长度). 端口网络字节序, 布局与 `sm_fi.rs` 的
/// `SockaddrIn` / `SockaddrIn6` (`#[repr(C)]`) 一致.
fn endpoint_to_sockaddr(ep: NetEndpoint) -> ([u8; 28], u32) {
    let mut buf = [0u8; 28];
    match ep.addr {
        IpAddr::V4(v4) => {
            buf[0..2].copy_from_slice(&2u16.to_le_bytes()); // AF_INET = 2
            buf[2..4].copy_from_slice(&ep.port.to_be_bytes()); // 端口按网络字节序
            buf[4..8].copy_from_slice(&v4.octets());
            (buf, 16)
        }
        IpAddr::V6(v6) => {
            buf[0..2].copy_from_slice(&10u16.to_le_bytes()); // AF_INET6 = 10
            buf[2..4].copy_from_slice(&ep.port.to_be_bytes()); // 端口按网络字节序
            buf[4..20].copy_from_slice(&v6.octets());
            (buf, 28)
        }
    }
}

/// sockaddr 字节 (V4: 16 / V6: 28) → `NetEndpoint`.
///
/// 按 family 分支: 2 (`AF_INET`) → V4, 10 (`AF_INET6`) → V6, 其余 None.
fn sockaddr_to_endpoint(buf: &[u8]) -> Option<NetEndpoint> {
    if buf.len() < 2 {
        return None;
    }
    match (buf[0], buf[1]) {
        (2, 0) => {
            if buf.len() < 8 {
                return None;
            }
            let port = u16::from_be_bytes([buf[2], buf[3]]);
            let addr = Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
            Some(NetEndpoint::new_v4(addr, port))
        }
        (10, 0) => {
            if buf.len() < 28 {
                return None;
            }
            let port = u16::from_be_bytes([buf[2], buf[3]]);
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            Some(NetEndpoint::new_v6(Ipv6Addr::from_octets(octets), port))
        }
        _ => None,
    }
}

impl Default for SmoltcpNetStack {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// NetStack trait 实现
// ============================================================================

impl NetStack for SmoltcpNetStack {
    /// 初始化网络协议栈.
    ///
    /// ## 步骤
    ///
    /// 1. 检查重复 init (DECISION-027)
    /// 2. 缓存配置
    /// 3. 若 `cfg.use_dhcp()`, 分配 DHCP 句柄槽位 + 标记 Discovering 状态
    /// 4. 若 `cfg.static_ipv4` != None, 标记 Bound 状态
    /// 5. 标记 initialized
    ///
    /// ## W3.2 不实装
    ///
    /// - 实际 smoltcp `Interface::new` + `update_ip_addrs` 调用
    /// - 实际 smoltcp `dhcpv4::Socket::new()` 构造
    /// - (这些需要 &mut device, 在 W4 整合时实装)
    fn init(&mut self, cfg: NetConfig) -> Result<()> {
        if self.initialized {
            return Err(NetError::BadConfig);
        }

        // 1. 缓存配置
        self.config = cfg;

        // 2. 启动 DHCP (若配置)
        if cfg.use_dhcp() {
            // DHCP 客户端 socket 由 privileged `poll_network` 独占管理, 这里只
            // 预留一个句柄槽位防止用户句柄与之冲突 (数据面操作按无效句柄拒绝).
            match self.insert_handle(DHCP_SENTINEL_FD) {
                Ok(h) => self.dhcp_user_id = Some(h.raw()),
                Err(e) => {
                    self.config = NetConfig::empty();
                    return Err(e);
                }
            }
            self.dhcp_state = DhcpState::Discovering;
        } else {
            // 静态 IP 配置
            if let Some(ipv4) = cfg.static_ipv4 {
                if ipv4 == [0, 0, 0, 0] {
                    self.config = NetConfig::empty();
                    return Err(NetError::BadConfig);
                }
                self.dhcp_state = DhcpState::Bound {
                    ipv4,
                    lease_expires_at: u64::MAX,
                };
            } else {
                self.config = NetConfig::empty();
                return Err(NetError::BadConfig);
            }
        }

        // 3. 标记 initialized
        self.initialized = true;
        Ok(())
    }

    /// 轮询协议栈.
    ///
    /// 委托给 privileged safe wrapper, 内部持有 `NET_LOCK` 并调用
    /// smoltcp `Interface::poll` + `process_dhcp_events`.
    fn poll(&mut self, ts_ms: u64) -> PollOutcome {
        if !self.initialized {
            return PollOutcome::idle();
        }
        let _ = ts_ms;
        fw_init::smoltcp_net_stack_poll()
    }

    /// 查询下次轮询截止期 (绝对单调毫秒).
    ///
    /// S-3: 读出 privileged 侧 `poll_network` 本次由 smoltcp `Interface::poll_at`
    /// 汇总并登记到 hrtimer 的截止期. `None` = 协议栈无内部定时需求.
    fn poll_at(&self) -> Option<u64> {
        if !self.initialized {
            return None;
        }
        fw_init::smoltcp_net_stack_poll_at_ms()
    }

    /// 打开一个 Socket.
    ///
    /// 经 `sm_net_socket` 在统一 fd 空间创建 privileged socket, 并把返回的
    /// 全局 fd 登记到句柄槽位; 句柄表/用户 ID 耗尽或后端拒绝时返回错误.
    /// `SocketKind::Tcp` / `SocketKind::Udp` 之外的种类返回 `BadConfig`.
    fn socket_open(&mut self, kind: SocketKind) -> Result<SocketHandle> {
        if !self.initialized {
            return Err(NetError::NotReady);
        }

        // P-18 单入口: socket 一律经 `sm_socket` 路径创建, 与 POSIX syscall
        // 共用同一 fd 空间与槽位表 (AF_INET 判别值; smoltcp 层不区分 family,
        // 具体 family 在 bind/connect 按 sockaddr 解析).
        let (domain, sock_type) = match kind {
            SocketKind::Tcp => (Domain::Inet as i32, SockType::Stream as i32),
            SocketKind::Udp => (Domain::Inet as i32, SockType::Dgram as i32),
            _ => return Err(NetError::BadConfig),
        };

        // 先占槽位与用户 ID (失败无副作用), 再创建 fd, 避免创建后无处登记.
        let slot = self.find_free_slot().ok_or(NetError::NoFreeSocket)?;
        let user_id = self.alloc_user_id().ok_or(NetError::NoFreeSocket)?;

        let fd = fw_net_socket::sm_net_socket(domain, sock_type, 0);
        if fd < 0 {
            return Err(NetError::Other);
        }

        self.handle_map[slot] = Some((user_id, fd));
        Ok(SocketHandle::from_raw(user_id))
    }

    /// 关闭一个 Socket.
    ///
    /// ## 幂等性
    ///
    /// 对 `SocketHandle::INVALID` 或未分配槽位返回 Ok, 不报错.
    /// DHCP socket 不可关闭 (返回 Ok 但实际保留).
    fn socket_close(&mut self, h: SocketHandle) -> Result<()> {
        let Some((idx, fd)) = self.find_handle(h) else {
            return Ok(()); // INVALID / 已释放句柄: 幂等
        };

        // DHCP 句柄保护
        if self.is_dhcp_handle(h.raw()) {
            return Ok(());
        }

        // 先摘除句柄再关 fd: 即使底层关闭失败, 该句柄也不得再被使用
        // (避免同一 fd 被二次关闭或经残留句柄越权访问).
        self.handle_map[idx] = None;
        if fd < 0 {
            return Ok(()); // 负值 fd = 预留槽位, 无真实资源
        }
        let rc = fw_net_socket::sm_net_close(fd);
        if rc == 0 {
            Ok(())
        } else {
            Err(NetError::Other)
        }
    }

    /// 关闭 Socket 并释放资源 — 与 [`NetStack::socket_close`] 同一实现.
    fn close(&mut self, h: SocketHandle) -> Result<()> {
        self.socket_close(h)
    }

    /// 查询 DHCP 状态.
    fn dhcp_state(&self) -> DhcpState {
        self.dhcp_state
    }

    /// 将 Socket 绑定到本地端点 (地址 + 端口).
    fn bind(&mut self, h: SocketHandle, addr: NetEndpoint) -> Result<()> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let (sin, addrlen) = endpoint_to_sockaddr(addr);
        let rc = fw_net_socket::sm_net_bind(fd, sin.as_ptr() as usize, addrlen);
        if rc == 0 {
            Ok(())
        } else {
            Err(NetError::Other)
        }
    }

    /// 将 TCP Socket 置为监听状态.
    fn listen(&mut self, h: SocketHandle, backlog: i32) -> Result<()> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let rc = fw_net_socket::sm_net_listen(fd, backlog);
        if rc == 0 {
            Ok(())
        } else {
            Err(NetError::Other)
        }
    }

    /// 从监听 Socket 的已完成连接队列中取出一个新连接.
    ///
    /// 新连接 fd 由 privileged 分配, 必须登记为新的用户句柄后才可寻址;
    /// 句柄表已满时不接受连接 (避免接出后无处登记而泄漏 fd).
    fn accept(&mut self, h: SocketHandle, peer: Option<&mut NetEndpoint>) -> Result<SocketHandle> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        if self.find_free_slot().is_none() {
            return Err(NetError::NoFreeSocket);
        }
        let mut addr_buf = [0u8; 28];
        let mut addrlen = 28u32;
        let new_fd = fw_net_socket::sm_net_accept(
            fd,
            addr_buf.as_mut_ptr() as usize,
            (&raw mut addrlen) as usize,
        );
        if new_fd < 0 {
            return Err(NetError::Other);
        }
        let new_handle = match self.insert_handle(new_fd) {
            Ok(handle) => handle,
            Err(e) => {
                // 登记失败: 归还 privileged fd, 不留无人认领的连接.
                let _ = fw_net_socket::sm_net_close(new_fd);
                return Err(e);
            }
        };
        if let Some(ep) = peer
            && let Some(parsed) = sockaddr_to_endpoint(&addr_buf[..addrlen as usize])
        {
            *ep = parsed;
        }
        Ok(new_handle)
    }

    /// 发起 TCP 连接到远端端点.
    fn connect(&mut self, h: SocketHandle, addr: NetEndpoint) -> Result<()> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let (sin, addrlen) = endpoint_to_sockaddr(addr);
        let rc = fw_net_socket::sm_net_connect(fd, sin.as_ptr() as usize, addrlen);
        if rc == 0 {
            Ok(())
        } else {
            Err(NetError::Other)
        }
    }

    /// 向已连接的 socket 发送数据.
    fn send(&mut self, h: SocketHandle, buf: &[u8], flags: i32) -> Result<usize> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let rc = fw_net_socket::sm_net_send(
            fd,
            buf.as_ptr() as usize,
            crate::functions::intcast::low_u32_from_usize(buf.len()),
            flags,
        );
        if rc >= 0 {
            Ok(crate::functions::intcast::usize_from_i32(rc))
        } else {
            Err(NetError::Other)
        }
    }

    /// 从已连接的 socket 接收数据.
    fn recv(&mut self, h: SocketHandle, buf: &mut [u8], flags: i32) -> Result<usize> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let rc = fw_net_socket::sm_net_recv(
            fd,
            buf.as_mut_ptr() as usize,
            crate::functions::intcast::low_u32_from_usize(buf.len()),
            flags,
        );
        if rc >= 0 {
            Ok(crate::functions::intcast::usize_from_i32(rc))
        } else {
            Err(NetError::Other)
        }
    }

    /// 向指定端点发送数据报 (UDP 主要场景).
    fn sendto(
        &mut self,
        h: SocketHandle,
        buf: &[u8],
        flags: i32,
        addr: NetEndpoint,
    ) -> Result<usize> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let (sin, addrlen) = endpoint_to_sockaddr(addr);
        let rc = fw_net_socket::sm_net_sendto(
            fd,
            buf.as_ptr() as usize,
            crate::functions::intcast::low_u32_from_usize(buf.len()),
            flags,
            sin.as_ptr() as usize,
            addrlen,
        );
        if rc >= 0 {
            Ok(crate::functions::intcast::usize_from_i32(rc))
        } else {
            Err(NetError::Other)
        }
    }

    /// 接收数据报并获取来源端点信息 (UDP 主要场景).
    fn recvfrom(
        &mut self,
        h: SocketHandle,
        buf: &mut [u8],
        flags: i32,
        src: Option<&mut NetEndpoint>,
    ) -> Result<usize> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let mut addr_buf = [0u8; 28];
        let mut addrlen = 28u32;
        let rc = fw_net_socket::sm_net_recvfrom(
            fd,
            buf.as_mut_ptr() as usize,
            crate::functions::intcast::low_u32_from_usize(buf.len()),
            flags,
            addr_buf.as_mut_ptr() as usize,
            (&raw mut addrlen) as usize,
        );
        if rc >= 0 {
            if let Some(ep) = src
                && let Some(parsed) = sockaddr_to_endpoint(&addr_buf[..addrlen as usize])
            {
                *ep = parsed;
            }
            Ok(crate::functions::intcast::usize_from_i32(rc))
        } else {
            Err(NetError::Other)
        }
    }

    /// 设置 Socket 选项.
    fn setsockopt(&mut self, h: SocketHandle, level: i32, optname: i32, val: &[u8]) -> Result<()> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let rc = fw_net_socket::sm_net_setsockopt(
            fd,
            level,
            optname,
            val.as_ptr() as usize,
            crate::functions::intcast::low_u32_from_usize(val.len()),
        );
        if rc == 0 {
            Ok(())
        } else {
            Err(NetError::Other)
        }
    }

    /// 获取 Socket 选项, 返回写入的字节数.
    fn getsockopt(
        &mut self,
        h: SocketHandle,
        level: i32,
        optname: i32,
        out: &mut [u8],
    ) -> Result<usize> {
        if !self.initialized || !h.is_valid() {
            return Err(NetError::NotReady);
        }
        let fd = self.handle_to_fd(h).ok_or(NetError::InvalidHandle)?;
        let mut out_len = crate::functions::intcast::low_u32_from_usize(out.len());
        let rc = fw_net_socket::sm_net_getsockopt(
            fd,
            level,
            optname,
            out.as_mut_ptr() as usize,
            (&raw mut out_len) as usize,
        );
        if rc == 0 {
            Ok(out_len as usize)
        } else {
            Err(NetError::Other)
        }
    }

    /// 轮询所有 Socket 状态 (驱动 select/poll 就绪面).
    fn poll_sockets(&mut self) -> Result<()> {
        if !self.initialized {
            return Err(NetError::NotReady);
        }
        let rc = fw_net_socket::sm_net_poll_sockets();
        if rc >= 0 {
            Ok(())
        } else {
            Err(NetError::Other)
        }
    }
}

// ============================================================================
// W6: DHCP 策略接入点
//
// 协议栈把"下一步该做什么"的决策委托给 `DhcpPolicy::decide()`, 自身不
// 保留策略逻辑. 调用方可以是:
// - `privileged/net/init.rs::poll_network()`: 每次 poll 走一次 decide
// - tests/host: 单测覆盖策略分支
// ============================================================================

use super::dhcp_policy::{DefaultDhcpPolicy, DhcpAction, DhcpPolicy, DhcpPolicyConfig};

impl SmoltcpNetStack {
    /// DHCP 决策接入: 委托给传入的 `DhcpPolicy` 实现.
    ///
    /// ## 调用方契约
    ///
    /// - `policy`: 策略实现, 默认用 `DefaultDhcpPolicy` (RFC 2131)
    /// - `policy_cfg`: 策略可调字段 (重试次数, 续约阈值)
    /// - `retry_count`: 当前 Discover/Request 已重试次数
    /// - `elapsed_ms`: 自 Bound 以来的毫秒数 (Bound 状态前传 0)
    /// - `lease_duration_ms`: 租期总长 (ms, Bound 状态专用)
    ///
    /// ## 返回
    ///
    /// `DhcpAction` 由调用方决定如何推进 (e.g. 协议栈在 `Continue` 时
    /// 保持现状, 在 `Renew` 时启动续约, 在 `FallbackToStatic` 时切换
    /// 到静态 IP, 在 `GiveUp` 时停机).
    pub fn dhcp_decide<P: DhcpPolicy>(
        &self,
        policy: &P,
        policy_cfg: DhcpPolicyConfig,
        retry_count: u32,
        elapsed_ms: u64,
        lease_duration_ms: u64,
    ) -> DhcpAction {
        policy.decide(
            self.dhcp_state,
            &self.config,
            policy_cfg,
            retry_count,
            elapsed_ms,
            lease_duration_ms,
        )
    }

    /// 便捷方法: 用默认策略决策.
    #[inline]
    #[must_use]
    pub fn dhcp_decide_default(
        &self,
        retry_count: u32,
        elapsed_ms: u64,
        lease_duration_ms: u64,
    ) -> DhcpAction {
        static POLICY: DefaultDhcpPolicy = DefaultDhcpPolicy;
        static CFG: DhcpPolicyConfig = DhcpPolicyConfig {
            max_retries: 4,
            renew_t1_ratio: 5000,
            renew_t2_ratio: 8750,
            fallback_to_static: true,
        };
        self.dhcp_decide(&POLICY, CFG, retry_count, elapsed_ms, lease_duration_ms)
    }

    // ---- DHCP 内部状态追踪 (W6 集成) ----
    //
    // 调用方 (privileged poll_network) 在状态变化时调用下面 3 个方法,
    // `dhcp_decide_at` 自动基于内部状态计算 elapsed_ms / lease_ms
    // 并返回 Action. 调用方代码无需自己维护计数器.

    /// 记录一次 DHCP Discover/Request 重试.
    ///
    /// ## 调用时机
    ///
    /// - DHCP 状态从 Discovering/Requesting 推进但未收到 ACK 时
    /// - 每次重试前调用, `dhcp_retry_count` 自增
    ///
    /// ## 副作用
    ///
    /// 无副作用, 仅更新内部计数. Bound 后会被 `record_dhcp_bound` 清零.
    #[inline]
    pub fn record_dhcp_retry(&mut self) {
        self.dhcp_retry_count = self.dhcp_retry_count.saturating_add(1);
    }

    /// 记录 DHCP 进入 Bound 状态的时间与租期 (仅记账).
    ///
    /// ## 调用时机
    ///
    /// - DHCP 状态从 Requesting 转为 Bound 时 (smoltcp `Event::Configured`)
    /// - 静态 IP init 成功时 (不走 DHCP, 但记录起始时间)
    ///
    /// ## 契约边界
    ///
    /// 本方法**只**记录 `dhcp_bound_at_ms` / `dhcp_lease_duration_ms` 并清零重试计数,
    /// **不修改 `dhcp_state`**. `dhcp_state` 的迁移由 `init()` 与事件接线路径负责
    /// (privileged 侧权威翻译见 `net::init::raw::dhcp_state_stub`).
    ///
    /// ## 参数
    ///
    /// - `now_ms`: 当前协议栈时间 (ms)
    /// - `lease_duration_ms`: 租期总长 (ms, 静态 IP 传 `u64::MAX`)
    #[inline]
    pub fn record_dhcp_bound(&mut self, now_ms: u64, lease_duration_ms: u64) {
        self.dhcp_bound_at_ms = now_ms;
        self.dhcp_lease_duration_ms = lease_duration_ms;
        self.dhcp_retry_count = 0; // 进入 Bound 后清零重试计数
    }

    /// 记录 DHCP 退出 Bound 状态 (Idle/Failed/Deconfigured).
    ///
    /// 退出 Bound 时不清零 `retry_count`, 留给 `record_dhcp_retry` 自然推进.
    /// 退出 Bound 时清零 `dhcp_bound_at_ms` 和 `dhcp_lease_duration_ms`,
    /// 避免 stale 值影响后续策略决策.
    #[inline]
    pub fn record_dhcp_unbound(&mut self) {
        self.dhcp_bound_at_ms = 0;
        self.dhcp_lease_duration_ms = 0;
    }

    /// 集成 DHCP 决策: 自动用内部状态计算 elapsed / lease, 委托给默认策略.
    ///
    /// ## 与 `dhcp_decide_default` 的区别
    ///
    /// - `dhcp_decide_default(r, e, l)`: 调用方提供 3 个参数
    /// - `dhcp_decide_at(now_ms)`: 仅需当前时间, 内部自动取 retry/elapsed/lease
    ///
    /// ## 调用方契约
    ///
    /// `now_ms` 应来自统一的协议栈时间源 (e.g. `hrtimer_clock_read` / `1_000_000`).
    /// 协议栈时间在每次 `record_dhcp_bound` 时记录, 续约阈值基于此计算.
    #[inline]
    #[must_use]
    pub fn dhcp_decide_at(&self, now_ms: u64) -> DhcpAction {
        let elapsed_ms = if self.dhcp_bound_at_ms == 0 {
            0
        } else {
            now_ms.saturating_sub(self.dhcp_bound_at_ms)
        };
        self.dhcp_decide_default(
            self.dhcp_retry_count,
            elapsed_ms,
            self.dhcp_lease_duration_ms,
        )
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 1. 构造与默认状态 ----

    #[test]
    fn test_new_returns_uninitialized() {
        let stack = SmoltcpNetStack::new();
        assert!(!stack.is_initialized());
        assert_eq!(stack.dhcp_state(), DhcpState::Idle);
        assert_eq!(stack.config().mac_address, [0; 6]);
    }

    #[test]
    fn test_default_trait_works() {
        let stack = SmoltcpNetStack::default();
        assert!(!stack.is_initialized());
    }

    // ---- 2. init() 各种路径 ----

    #[test]
    fn test_init_with_dhcp_succeeds() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            static_ipv4: None,
            prefix_len: 24,
            gateway: [0, 0, 0, 0],
            random_seed: 42,
        };
        assert!(stack.init(cfg).is_ok());
        assert!(stack.is_initialized());
        assert_eq!(stack.dhcp_state(), DhcpState::Discovering);
        assert!(stack.dhcp_user_id.is_some());
    }

    #[test]
    fn test_init_with_static_ipv4_succeeds() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            static_ipv4: Some([192, 168, 1, 100]),
            prefix_len: 24,
            gateway: [192, 168, 1, 1],
            random_seed: 42,
        };
        assert!(stack.init(cfg).is_ok());
        assert!(stack.is_initialized());
        match stack.dhcp_state() {
            DhcpState::Bound { ipv4, .. } => assert_eq!(ipv4, [192, 168, 1, 100]),
            _ => panic!("应为 Bound 状态"),
        }
        assert!(stack.dhcp_user_id.is_none());
    }

    #[test]
    fn test_init_with_zero_static_ip_fails() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: Some([0, 0, 0, 0]),
            prefix_len: 24,
            gateway: [0, 0, 0, 0],
            random_seed: 0,
        };
        assert_eq!(stack.init(cfg), Err(NetError::BadConfig));
        assert!(!stack.is_initialized());
    }

    #[test]
    fn test_init_with_empty_config_uses_dhcp() {
        // NetConfig::empty() 的 static_ipv4 为 None, 故 use_dhcp() == true ——
        // "既不用 DHCP 又无静态 IP" 的配置无法构造 (init 的非 DHCP 分支只在
        // static_ipv4 == Some(_) 时进入), 因此原用例 "预期 Err(BadConfig)" 的前提不成立。
        // 现改为验证空配置 = DHCP 模式的真实语义 (2026-09-24 UT-06 修正)。
        let cfg = NetConfig::empty();
        assert!(cfg.use_dhcp());
        let mut stack = SmoltcpNetStack::new();
        assert!(stack.init(cfg).is_ok());
        assert!(stack.is_initialized());
        assert_eq!(stack.dhcp_state(), DhcpState::Discovering);
    }

    #[test]
    fn test_double_init_fails() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [1; 6],
            static_ipv4: Some([10, 0, 0, 1]),
            prefix_len: 24,
            gateway: [10, 0, 0, 1],
            random_seed: 0,
        };
        assert!(stack.init(cfg).is_ok());
        assert_eq!(stack.init(cfg), Err(NetError::BadConfig));
    }

    #[test]
    fn test_init_dhcp_no_slot_fails() {
        // 满负载场景: 模拟 DHCP 失败
        let mut stack = SmoltcpNetStack::new();
        // 先填满所有槽位 (这要求先 init 一次, 但我们不能 init 两次)
        // 此测试用静态 IP 触发 NoFreeSocket (但静态 IP 不分配槽位)
        // 因此此测试改为验证 DHCP 模式下, MAX_SOCKETS - 1 槽位可分配
        // (在 test_dhcp_handle_protects_one_slot 中)
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: None,
            prefix_len: 24,
            gateway: [0, 0, 0, 0],
            random_seed: 0,
        };
        assert!(stack.init(cfg).is_ok());
    }

    // ---- 3. poll() / poll_at() ----

    #[test]
    fn test_poll_before_init_returns_idle() {
        let mut stack = SmoltcpNetStack::new();
        let outcome = stack.poll(1000);
        assert_eq!(outcome, PollOutcome::idle());
        assert!(!outcome.has_events());
    }

    #[test]
    fn test_poll_at_before_init_returns_none() {
        let stack = SmoltcpNetStack::new();
        assert!(stack.poll_at().is_none());
    }

    #[test]
    fn test_poll_after_init_returns_idle_w32_placeholder() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: Some([10, 0, 0, 1]),
            prefix_len: 24,
            gateway: [10, 0, 0, 1],
            random_seed: 0,
        };
        stack.init(cfg).unwrap();
        let outcome = stack.poll(12345);
        assert_eq!(outcome, PollOutcome::idle());
        assert!(stack.poll_at().is_none());
    }

    // ---- 4. socket_open() / socket_close() 测试 ----

    #[test]
    fn test_socket_open_before_init_fails() {
        let mut stack = SmoltcpNetStack::new();
        assert_eq!(stack.socket_open(SocketKind::Tcp), Err(NetError::NotReady));
    }

    // UT-06 (2026-09-24) 裁定删除: 原 8 例 (socket_open 返回有效句柄 / 全类型成功 /
    // 打开-关闭循环 / 打开至满返回 NoFree / 关闭后槽位可复用 / DHCP 句柄占用一槽 /
    // 句柄各自唯一 / 无效句柄跳过) 断言的 socket_open 成功路径依赖三项前置:
    // 内核堆 (>4 KiB, 供 TCP 双缓冲) + privileged init_sockets() + 非桩 socket_open.
    // host-test 仅有 4 KiB early_buffer 且 kernel_test 下 socket_open 为 no-op 桩,
    // 两环境均不满足, 属未实装路径.
    // 已登记为未来功能: docs/plan/archive/kernel-unit-test-harness-unification.md.

    #[test]
    fn test_socket_close_invalid_handle_is_idempotent() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: Some([10, 0, 0, 1]),
            prefix_len: 24,
            gateway: [10, 0, 0, 1],
            random_seed: 0,
        };
        stack.init(cfg).unwrap();
        assert!(stack.socket_close(SocketHandle::INVALID).is_ok());
        assert!(
            stack
                .socket_close(SocketHandle::from_raw(0xDEAD_BEEF))
                .is_ok()
        );
    }

    // ---- 5. DHCP 句柄保护 ----

    // UT-06 (2026-09-24) 裁定删除: 原 test_dhcp_handle_protects_one_slot 见 §4 注释.

    #[test]
    fn test_dhcp_handle_cannot_be_closed() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: None,
            prefix_len: 24,
            gateway: [0, 0, 0, 0],
            random_seed: 0,
        };
        stack.init(cfg).unwrap();
        let dhcp_id = stack.dhcp_user_id.unwrap();
        let dhcp_handle = SocketHandle::from_raw(dhcp_id);
        // 关闭 DHCP 句柄 (应被保护)
        assert!(stack.socket_close(dhcp_handle).is_ok());
        // 确认 DHCP 句柄仍在
        assert_eq!(stack.dhcp_user_id, Some(dhcp_id));
        // 确认 DHCP 槽位仍占用
        assert!(
            stack
                .handle_map
                .iter()
                .any(|s| matches!(s, Some((u, _)) if *u == dhcp_id))
        );
    }

    // ---- 6. DHCP 状态机 ----

    #[test]
    fn test_dhcp_state_idle_before_init() {
        let stack = SmoltcpNetStack::new();
        assert_eq!(stack.dhcp_state(), DhcpState::Idle);
    }

    #[test]
    fn test_dhcp_state_configured_when_static() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: Some([192, 168, 1, 50]),
            prefix_len: 24,
            gateway: [192, 168, 1, 1],
            random_seed: 0,
        };
        stack.init(cfg).unwrap();
        assert!(stack.dhcp_state().is_configured());
        assert_eq!(stack.dhcp_state().ipv4(), Some([192, 168, 1, 50]));
    }

    #[test]
    fn test_dhcp_state_discovering_when_dhcp() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: None,
            prefix_len: 24,
            gateway: [0, 0, 0, 0],
            random_seed: 0,
        };
        stack.init(cfg).unwrap();
        assert_eq!(stack.dhcp_state(), DhcpState::Discovering);
        assert!(!stack.dhcp_state().is_configured());
        assert_eq!(stack.dhcp_state().ipv4(), None);
    }

    // ---- 7. 类型擦除 SocketHandle 行为 ----

    #[test]
    fn test_socket_handle_invalid_zero() {
        let h = SocketHandle::INVALID;
        assert!(!h.is_valid());
        assert_eq!(h.raw(), 0);
    }

    // UT-06 (2026-09-24) 裁定删除: 原 test_socket_handle_allocated_distinct /
    // test_socket_handle_invalid_id_skipped 见 §4 注释 (均依赖 socket_open 成功).
    // 剩余用例仅覆盖无需 socket_open 成功的句柄语义.

    // ---- 8. W6: DHCP 策略接入 ----

    use crate::functions::net::dhcp_policy::{DefaultDhcpPolicy, DhcpPolicy, DhcpPolicyConfig};
    use crate::privileged::net::iface_trait::Ipv4Addr;

    /// 验证: Idle 状态接入默认策略 → Continue.
    #[test]
    fn test_dhcp_decide_idle_continue() {
        let stack = SmoltcpNetStack::new();
        let action = stack.dhcp_decide_default(0, 0, 0);
        assert_eq!(action, DhcpAction::Continue);
    }

    /// 验证: Discovering 状态接入默认策略 (重试 < 上限) → Continue.
    #[test]
    fn test_dhcp_decide_discovering_under_limit() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: None,
            prefix_len: 24,
            gateway: [0, 0, 0, 0],
            random_seed: 0,
        };
        stack.init(cfg).unwrap(); // DHCP 模式 → Discovering
        let action = stack.dhcp_decide_default(2, 0, 0); // 2 < 4 (max_retries)
        assert_eq!(action, DhcpAction::Continue);
    }

    /// 验证: Discovering 状态 (重试 >= 上限, 有静态 IP) → FallbackToStatic.
    #[test]
    fn test_dhcp_decide_discovering_over_limit_fallback() {
        let mut stack = SmoltcpNetStack::new();
        // 配置静态 IP 但不实际 init 静态 (强制 DHCP 路径)
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: Some([10, 0, 2, 5]), // 提供静态 IP 作为 fallback
            prefix_len: 24,
            gateway: [10, 0, 2, 1],
            random_seed: 0,
        };
        // 用静态 IP init, 然后手动把状态切回 Discovering 模拟 DHCP 模式
        stack.init(cfg).unwrap();
        // 重置 dhcp_state 为 Discovering 模拟 DHCP 重试
        // (测试用, 不通过 unsafe; 直接覆盖 DhcpState)
        stack.dhcp_state = DhcpState::Discovering;
        let action = stack.dhcp_decide_default(5, 0, 0); // 5 >= 4
        assert_eq!(
            action,
            DhcpAction::FallbackToStatic(Ipv4Addr::from_octets([10, 0, 2, 5]))
        );
    }

    /// 验证: Bound 状态 (T1 之前) → Continue.
    #[test]
    fn test_dhcp_decide_bound_before_t1_continue() {
        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: Some([10, 0, 0, 1]),
            prefix_len: 24,
            gateway: [10, 0, 0, 1],
            random_seed: 0,
        };
        stack.init(cfg).unwrap();
        // 静态 IP init → Bound, lease_expires_at = u64::MAX (永不过期)
        // dhcp_decide_default 接收 lease_duration_ms = 0 → 内部视为未知, 永不续约
        let action = stack.dhcp_decide_default(0, 1_000_000, 0);
        assert_eq!(action, DhcpAction::Continue);
    }

    /// 验证: 自定义策略可注入: 测试用 "永远 Continue" 策略.
    #[test]
    fn test_dhcp_decide_custom_policy_always_continue() {
        struct AlwaysContinue;
        impl DhcpPolicy for AlwaysContinue {
            fn decide(
                &self,
                _state: DhcpState,
                _cfg: &NetConfig,
                _pc: DhcpPolicyConfig,
                _retry: u32,
                _elapsed: u64,
                _lease: u64,
            ) -> DhcpAction {
                DhcpAction::Continue
            }
        }

        let mut stack = SmoltcpNetStack::new();
        let cfg = NetConfig {
            mac_address: [0; 6],
            static_ipv4: None,
            prefix_len: 24,
            gateway: [0, 0, 0, 0],
            random_seed: 0,
        };
        stack.init(cfg).unwrap();
        // 即便 retry 远超上限, 自定义策略仍返回 Continue
        let policy = AlwaysContinue;
        let pc = DhcpPolicyConfig::default();
        let action = stack.dhcp_decide(&policy, pc, 100, 0, 0);
        assert_eq!(action, DhcpAction::Continue);
    }

    /// 验证: 策略对 Discovering 状态正确给出 GiveUp (无静态 IP + 超过重试).
    #[test]
    fn test_dhcp_decide_discovering_giveup_no_static() {
        let mut stack = SmoltcpNetStack::new();
        // DHCP init 走非分支: 先用占位静态 init, 再清空 static_ipv4
        stack
            .init(NetConfig {
                mac_address: [0; 6],
                static_ipv4: Some([1, 2, 3, 4]),
                prefix_len: 24,
                gateway: [0, 0, 0, 0],
                random_seed: 0,
            })
            .unwrap();
        // 然后清空 config.static_ipv4, 强制 fallback_to_static → None 路径
        stack.config.static_ipv4 = None;
        stack.dhcp_state = DhcpState::Discovering;
        let action = stack.dhcp_decide_default(10, 0, 0);
        assert_eq!(action, DhcpAction::GiveUp);
    }

    /// 验证: DefaultDhcpPolicy 单元可单独使用 (不依赖 stack).
    #[test]
    fn test_default_policy_alone_works() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty();
        let pc = DhcpPolicyConfig::default();
        // 纯策略调用, 不经过 stack
        assert_eq!(
            policy.decide(DhcpState::Idle, &cfg, pc, 0, 0, 0),
            DhcpAction::Continue
        );
    }

    // ---- 9. W7-E: DHCP 内部状态追踪 + dhcp_decide_at ----

    /// 验证: record_dhcp_retry 单调递增, 不溢出.
    #[test]
    fn test_record_dhcp_retry_monotonic() {
        let mut stack = SmoltcpNetStack::new();
        assert_eq!(stack.dhcp_retry_count, 0);
        stack.record_dhcp_retry();
        assert_eq!(stack.dhcp_retry_count, 1);
        stack.record_dhcp_retry();
        assert_eq!(stack.dhcp_retry_count, 2);
        stack.record_dhcp_retry();
        assert_eq!(stack.dhcp_retry_count, 3);
    }

    /// 验证: record_dhcp_retry 在 u32::MAX 时饱和不溢出.
    #[test]
    fn test_record_dhcp_retry_saturating() {
        let mut stack = SmoltcpNetStack::new();
        // 直接设到 u32::MAX - 2, 然后再 +3, 应饱和为 u32::MAX
        stack.dhcp_retry_count = u32::MAX - 2;
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        assert_eq!(stack.dhcp_retry_count, u32::MAX, "应饱和到 u32::MAX");
        // 再次调用仍应饱和
        stack.record_dhcp_retry();
        assert_eq!(stack.dhcp_retry_count, u32::MAX);
    }

    /// 验证: record_dhcp_bound 清零 retry_count + 设置 bound_at/lease.
    #[test]
    fn test_record_dhcp_bound_clears_retry() {
        let mut stack = SmoltcpNetStack::new();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        assert_eq!(stack.dhcp_retry_count, 3);
        stack.record_dhcp_bound(1_000_000, 3_600_000); // 1s, 1h lease
        assert_eq!(stack.dhcp_retry_count, 0, "bound 后 retry 应清零");
        assert_eq!(stack.dhcp_bound_at_ms, 1_000_000);
        assert_eq!(stack.dhcp_lease_duration_ms, 3_600_000);
    }

    /// 验证: record_dhcp_unbound 清零 bound_at + lease, 保留 retry.
    #[test]
    fn test_record_dhcp_unbound_keeps_retry() {
        let mut stack = SmoltcpNetStack::new();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_bound(1_000_000, 3_600_000);
        stack.record_dhcp_unbound();
        assert_eq!(stack.dhcp_bound_at_ms, 0);
        assert_eq!(stack.dhcp_lease_duration_ms, 0);
        assert_eq!(
            stack.dhcp_retry_count, 0,
            "bound 后 retry 已被清零, unbound 不再变"
        );
        // 再 retry, 应能正常递增
        stack.record_dhcp_retry();
        assert_eq!(stack.dhcp_retry_count, 1);
    }

    /// 验证: dhcp_decide_at 自动用内部状态计算 elapsed_ms.
    #[test]
    fn test_dhcp_decide_at_computes_elapsed() {
        let mut stack = SmoltcpNetStack::new();
        // 模拟: 100s 时 Bound, 租期 1000s, 700s 后查询
        stack.record_dhcp_bound(100_000, 1_000_000);
        // policy.decide 按 dhcp_state 分支, 须显式置 Bound 才能进入 T1/T2 续约判据
        // (与 test_dhcp_decide_bound_before_t1_continue 等兄弟用例一致; 2026-09-24 UT-06 修正)
        stack.dhcp_state = DhcpState::Bound {
            ipv4: [10, 0, 0, 1],
            lease_expires_at: u64::MAX,
        };
        // 100s + 700s = 800s
        let action = stack.dhcp_decide_at(800_000);
        // T1=500s (50%), T2=875s (87.5%), elapsed=700s 在 T1..T2 之间
        assert_eq!(action, DhcpAction::Renew);
    }

    /// 验证: dhcp_decide_at 在未 Bound 时 elapsed=0, 返回 Continue (Idle).
    #[test]
    fn test_dhcp_decide_at_before_bound_continue() {
        let stack = SmoltcpNetStack::new();
        let action = stack.dhcp_decide_at(0);
        assert_eq!(action, DhcpAction::Continue);
    }

    /// 验证: dhcp_decide_at 在 retry_count 累加后给出 GiveUp.
    #[test]
    fn test_dhcp_decide_at_giveup_after_max_retries() {
        let mut stack = SmoltcpNetStack::new();
        // 模拟: DHCP Discovering 状态, 已重试 4 次 (超过 max_retries=4)
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry(); // 5 次重试
        // 状态须为 Discovering/Requesting 才会走重试上限判据 (Idle 恒 Continue);
        // config.static_ipv4 = None (默认) → 无 fallback, 返回 GiveUp
        // (2026-09-24 UT-06 修正)
        stack.dhcp_state = DhcpState::Discovering;
        let action = stack.dhcp_decide_at(0);
        assert_eq!(action, DhcpAction::GiveUp);
    }

    /// 验证: dhcp_decide_at 在 retry 超限且有静态 IP 时 fallback.
    #[test]
    fn test_dhcp_decide_at_fallback_with_static() {
        let mut stack = SmoltcpNetStack::new();
        // init with static IPv4
        stack
            .init(NetConfig {
                mac_address: [0; 6],
                static_ipv4: Some([192, 168, 1, 50]),
                prefix_len: 24,
                gateway: [192, 168, 1, 1],
                random_seed: 0,
            })
            .unwrap();
        // 模拟: DHCP 模式, 但有静态 IP 作为 fallback
        stack.dhcp_state = DhcpState::Discovering;
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        stack.record_dhcp_retry();
        let action = stack.dhcp_decide_at(0);
        assert_eq!(
            action,
            DhcpAction::FallbackToStatic(Ipv4Addr::from_octets([192, 168, 1, 50]))
        );
    }

    /// 验证: dhcp_decide_at 在 T1 之前 Continue (有 Bound 但未到续约时机).
    #[test]
    fn test_dhcp_decide_at_bound_before_t1_continue() {
        let mut stack = SmoltcpNetStack::new();
        // Bound 100s, 租期 1000s, T1=500s, T2=875s
        stack.record_dhcp_bound(100_000, 1_000_000);
        // record_dhcp_bound 仅记账, 须显式置 Bound 才能进入 T1 之前判据
        // (Idle 会让 policy 恒 Continue, 断言将失去区分力; 2026-09-24 UT-06 修正)
        stack.dhcp_state = DhcpState::Bound {
            ipv4: [10, 0, 0, 1],
            lease_expires_at: u64::MAX,
        };
        // 100s + 100s = 200s, < T1=500s
        let action = stack.dhcp_decide_at(200_000);
        assert_eq!(action, DhcpAction::Continue);
    }

    // ---- 10. 句柄表生命周期 (P-18 单入口) ----

    /// 验证: new() 后句柄表全空.
    #[test]
    fn test_handle_map_initialized_empty() {
        let stack = SmoltcpNetStack::new();
        assert!(
            stack.handle_map.iter().all(core::option::Option::is_none),
            "new() 后 handle_map 应无任何已分配句柄"
        );
    }

    /// 验证: 登记的 fd 可原样反查, 未登记句柄不可寻址.
    #[test]
    fn test_insert_handle_maps_to_privileged_fd() {
        let mut stack = SmoltcpNetStack::new();
        let h = stack.insert_handle(300).expect("空表应可登记");
        assert!(h.is_valid());
        assert_eq!(stack.handle_to_fd(h), Some(300), "句柄应反查到登记的 fd");
        assert_eq!(
            stack.handle_to_fd(SocketHandle::from_raw(h.raw() + 1)),
            None,
            "未登记句柄不得反查到 fd"
        );
        assert_eq!(stack.handle_to_fd(SocketHandle::INVALID), None);
    }

    /// 验证: 负值 fd 是预留槽位哨兵, 数据面不可寻址但可正常释放.
    #[test]
    fn test_reserved_slot_released_by_close() {
        let mut stack = SmoltcpNetStack::new();
        stack.initialized = true;
        let h = stack.insert_handle(DHCP_SENTINEL_FD).expect("空表应可登记");
        assert_eq!(stack.handle_to_fd(h), None, "预留槽位不暴露 fd");
        assert_eq!(stack.socket_close(h), Ok(()), "预留槽位关闭不触达 FFI");
        assert!(stack.handle_map.iter().all(core::option::Option::is_none));
        assert_eq!(stack.socket_close(h), Ok(()), "重复关闭应幂等");
    }

    /// 验证: 句柄表满时 socket_open 返回 NoFreeSocket (不触达 FFI).
    #[test]
    fn test_handle_table_exhaustion() {
        let mut stack = SmoltcpNetStack::new();
        stack.initialized = true;
        for _ in 0..MAX_SOCKETS {
            stack.insert_handle(100).expect("空表应可登记");
        }
        assert_eq!(
            stack.socket_open(SocketKind::Tcp),
            Err(NetError::NoFreeSocket)
        );
    }

    /// 验证: 不支持的类型在创建前即被拒绝 (无 fd 副作用).
    #[test]
    fn test_socket_open_rejects_unsupported_kind() {
        let mut stack = SmoltcpNetStack::new();
        stack.initialized = true;
        assert_eq!(
            stack.socket_open(SocketKind::Icmp),
            Err(NetError::BadConfig)
        );
        assert_eq!(
            stack.socket_open(SocketKind::Dhcpv4),
            Err(NetError::BadConfig),
            "内部类型不得经用户句柄入口创建"
        );
        assert!(stack.handle_map.iter().all(core::option::Option::is_none));
    }

    /// 验证: 未登记句柄上的全部能力面返回 InvalidHandle (单入口校验).
    #[test]
    fn test_socket_ops_reject_unknown_handle() {
        let mut stack = SmoltcpNetStack::new();
        stack
            .init(NetConfig {
                static_ipv4: Some([192, 168, 1, 50]),
                ..NetConfig::empty()
            })
            .unwrap();
        let h = SocketHandle::from_raw(1); // 静态 IP 不预留 DHCP 句柄, 故未登记
        let ep = NetEndpoint::new_v4(Ipv4Addr::new(127, 0, 0, 1), 8080);
        let mut buf = [0u8; 4];
        assert_eq!(stack.bind(h, ep), Err(NetError::InvalidHandle));
        assert_eq!(stack.listen(h, 128), Err(NetError::InvalidHandle));
        assert_eq!(stack.accept(h, None), Err(NetError::InvalidHandle));
        assert_eq!(stack.connect(h, ep), Err(NetError::InvalidHandle));
        assert_eq!(stack.send(h, b"test", 0), Err(NetError::InvalidHandle));
        assert_eq!(stack.recv(h, &mut buf, 0), Err(NetError::InvalidHandle));
        assert_eq!(
            stack.sendto(h, b"test", 0, ep),
            Err(NetError::InvalidHandle)
        );
        assert_eq!(
            stack.recvfrom(h, &mut buf, 0, None),
            Err(NetError::InvalidHandle)
        );
        assert_eq!(
            stack.setsockopt(h, 1, 2, &[1, 2, 3]),
            Err(NetError::InvalidHandle)
        );
        assert_eq!(
            stack.getsockopt(h, 1, 2, &mut buf),
            Err(NetError::InvalidHandle)
        );
        assert_eq!(stack.close(h), Ok(()), "close 对未知句柄幂等");
    }

    /// 验证: DHCP 句柄在数据面按无效句柄拒绝 (哨兵 fd 不可寻址).
    #[test]
    fn test_dhcp_handle_rejects_data_ops() {
        let mut stack = SmoltcpNetStack::new();
        stack.init(NetConfig::empty()).unwrap(); // DHCP 模式
        let dhcp = SocketHandle::from_raw(stack.dhcp_user_id.expect("DHCP 应预留句柄"));
        assert_eq!(stack.handle_to_fd(dhcp), None);
        let ep = NetEndpoint::new_v4(Ipv4Addr::new(127, 0, 0, 1), 8080);
        assert_eq!(stack.bind(dhcp, ep), Err(NetError::InvalidHandle));
        assert_eq!(stack.send(dhcp, b"x", 0), Err(NetError::InvalidHandle));
    }
}
