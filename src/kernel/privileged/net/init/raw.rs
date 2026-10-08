//! 特权子模块 (Framekernel raw): 集中 static mut 访问 (B04-09 拆分 Step A)
//!
//! 原为 init.rs 内联 `pub(crate) mod raw { ... }` (1281-1914, 634 行).
//! 抽出为独立文件后, `super` 仍指向 init 模块, 引用方式不变.
//! 调用方契约: 本模块内所有函数要求调用方持有 `NET_STATE` 锁.

use super::{
    EGDFNetDevice, MAX_SM_FD, NET_STATE, NetState, NetworkStack, SOCKET_SET, SocketHandle,
    SocketSet, TCP_BUF_SIZE, TOTAL_SLOTS, UDP_BUF_SIZE, UDP_META_COUNT, dhcpv4, klog_init_msg,
    klog_net, klog_net_err, tcp, udp,
};
use crate::privileged::net::iface_trait::NetEndpoint;

/// 获取 `NetState` 可变引用 (调用方必须持有 `NET_STATE` 锁).
///
/// # Safety
///
/// 调用方必须持有 `NET_STATE` 的锁 (通过 `lock()` 或 `try_lock()`).
/// 返回的引用生命周期为 `'static` (因底层数据在 `static NET_STATE` 中),
/// 但调用方不得在锁释放后继续使用.
#[inline(always)]
unsafe fn state() -> &'static mut NetState {
    // SAFETY: NET_STATE 是 static, 数据永不移动;
    // 调用方持有锁保证互斥访问.
    unsafe { &mut *NET_STATE.data_ptr() }
}

/// 安全访问 stack (Framekernel 集中 unsafe 边界)
pub fn stack_mut() -> Option<&'static mut NetworkStack> {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().stack.as_mut() }
}

/// 安全访问 device
pub fn device_mut() -> Option<&'static mut EGDFNetDevice> {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().device.as_mut() }
}

/// 安全设置 device
pub fn set_device(d: Option<EGDFNetDevice>) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().device = d;
    }
}

/// 安全设置 stack
pub fn set_stack(s: Option<NetworkStack>) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().stack = s;
    }
}

/// 安全读取 `dhcp_handle`
pub fn dhcp_handle() -> Option<SocketHandle> {
    // SAFETY: SocketHandle 是 Copy, 调用方持有锁.
    unsafe { state().dhcp_handle }
}

/// 安全设置 `dhcp_handle`
pub fn set_dhcp_handle(h: Option<SocketHandle>) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().dhcp_handle = h;
    }
}

/// 安全清空网络全局状态
pub fn clear_all() {
    // SAFETY: 调用方持有 NET_STATE 锁, 串行重置流程.
    let s = unsafe { state() };
    s.device = None;
    s.stack = None;
    s.dhcp_handle = None;
}

// ========================================================================
// FD_TYPES / SOCKET_TABLE / buffer accessor 函数
//
// 集中访问 NetState 中的 FD 表、socket 表和 buffer 指针数组.
// 所有函数要求调用方持有 NET_STATE 锁.
// ========================================================================

/// 读取 fd 类型 (0=free, 1=tcp, 2=udp)
pub fn fd_type(fd: usize) -> u8 {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().fd_types[fd] }
}

/// 写入 fd 类型
pub fn set_fd_type(fd: usize, val: u8) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().fd_types[fd] = val;
    }
}

/// 读取 socket 阻塞标志 (D10): `true` = 阻塞 (POSIX 默认), `false` = 非阻塞.
///
/// `fd` 为 Smoltcp 段槽位索引 (调用方经 `sm_slot` 换算), 与 `fd_type` 同索
/// 引空间. 调用方须持有 `NET_STATE` 锁.
pub fn is_blocking(fd: usize) -> bool {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().blocking[fd] }
}

/// 写入 socket 阻塞标志 (D10). 调用方须持有 `NET_STATE` 锁.
pub fn set_blocking(fd: usize, val: bool) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().blocking[fd] = val;
    }
}

/// 读取 socket 超时刻度 (P5d / DECISION-096): `recv=true` 取 SO_RCVTIMEO,
/// `false` 取 SO_SNDTIMEO. 返回单位纳秒, `0` = 无超时. `fd` 为 Smoltcp 段槽位
/// 索引 (与 `blocking` 同空间). 调用方须持有 `NET_STATE` 锁.
pub fn sock_timeout_ns(fd: usize, recv: bool) -> u64 {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        if recv {
            state().recv_timeout_ns[fd]
        } else {
            state().send_timeout_ns[fd]
        }
    }
}

/// 写入 socket 超时刻度 (P5d). 调用方须持有 `NET_STATE` 锁.
pub fn set_sock_timeout_ns(fd: usize, recv: bool, ns: u64) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        if recv {
            state().recv_timeout_ns[fd] = ns;
        } else {
            state().send_timeout_ns[fd] = ns;
        }
    }
}

/// 读取 socket handle
pub fn socket_handle(fd: usize) -> Option<SocketHandle> {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().socket_table[fd] }
}

/// 写入 socket handle
pub fn set_socket_handle(fd: usize, val: Option<SocketHandle>) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().socket_table[fd] = val;
    }
}

/// 读取 socket 本地端点 (D1)
pub fn socket_local_endpoint(fd: usize) -> Option<NetEndpoint> {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().local_endpoints[fd] }
}

/// 写入 socket 本地端点 (D1)
pub fn set_socket_local_endpoint(fd: usize, val: Option<NetEndpoint>) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().local_endpoints[fd] = val;
    }
}

/// 读取 socket 对端端点 (D1, UDP 专属)
pub fn socket_remote_endpoint(fd: usize) -> Option<NetEndpoint> {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().remote_endpoints[fd] }
}

/// 写入 socket 对端端点 (D1, UDP 专属)
pub fn set_socket_remote_endpoint(fd: usize, val: Option<NetEndpoint>) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().remote_endpoints[fd] = val;
    }
}

/// 读取 TCP RX buffer 指针
pub fn tcp_rx_buf(fd: usize) -> *mut u8 {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().tcp_rx_bufs[fd] }
}

/// 写入 TCP RX buffer 指针
pub fn set_tcp_rx_buf(fd: usize, val: *mut u8) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().tcp_rx_bufs[fd] = val;
    }
}

/// 读取 TCP TX buffer 指针
pub fn tcp_tx_buf(fd: usize) -> *mut u8 {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().tcp_tx_bufs[fd] }
}

/// 写入 TCP TX buffer 指针
pub fn set_tcp_tx_buf(fd: usize, val: *mut u8) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().tcp_tx_bufs[fd] = val;
    }
}

/// 读取 UDP RX buffer 指针
pub fn udp_rx_buf(fd: usize) -> *mut u8 {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().udp_rx_bufs[fd] }
}

/// 写入 UDP RX buffer 指针
pub fn set_udp_rx_buf(fd: usize, val: *mut u8) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().udp_rx_bufs[fd] = val;
    }
}

/// 读取 UDP TX buffer 指针
pub fn udp_tx_buf(fd: usize) -> *mut u8 {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe { state().udp_tx_bufs[fd] }
}

/// 写入 UDP TX buffer 指针
pub fn set_udp_tx_buf(fd: usize, val: *mut u8) {
    // SAFETY: 调用方持有 NET_STATE 锁.
    unsafe {
        state().udp_tx_bufs[fd] = val;
    }
}

/// 读取 UDP RX metadata 数组 (可变借用, 用于 `PacketBuffer` 构造)
///
/// # Safety
///
/// 调用方持有 `NET_STATE` 锁; 返回的引用仅在本次 socket 构造期间有效.
pub unsafe fn udp_rx_meta(fd: usize) -> &'static mut [udp::PacketMetadata; UDP_META_COUNT] {
    // SAFETY: 调用方持有 NET_STATE 锁, 数据在 static 中.
    unsafe { &mut state().udp_rx_metas[fd] }
}

/// 读取 UDP TX metadata 数组 (可变借用, 用于 `PacketBuffer` 构造)
///
/// # Safety
///
/// 调用方持有 `NET_STATE` 锁; 返回的引用仅在本次 socket 构造期间有效.
pub unsafe fn udp_tx_meta(fd: usize) -> &'static mut [udp::PacketMetadata; UDP_META_COUNT] {
    // SAFETY: 调用方持有 NET_STATE 锁, 数据在 static 中.
    unsafe { &mut state().udp_tx_metas[fd] }
}

/// 安全获取 `SocketSet` 指针 (保留为 static mut, 自引用结构)
pub fn socket_set() -> *mut SocketSet<'static> {
    // SAFETY: SOCKET_SET 在 init_sockets 后已初始化, 调用方在 NET_STATE 锁下.
    unsafe { SOCKET_SET.as_mut_ptr() }
}

/// 安全初始化 sockets
pub fn init_sockets() {
    // SAFETY: 调用方持有 NET_STATE 锁, 单次初始化.
    unsafe { super::init_sockets() }
}

/// 安全处理 DHCP 事件
pub fn process_dhcp_events(sockets: &mut SocketSet<'_>) {
    // SAFETY: 调用方持有 NET_STATE 锁, sockets 来自本模块的 socket_set().
    unsafe { super::process_dhcp_events(sockets) }
}

// ========================================================================
// REVAL-W W4.2 桥接 raw helpers (2026-06-25)
//
// 为 SmoltcpNetStack (W3.2) 提供 smoltcp 实际操作入口. 桥接方案:
//   - SmoltcpNetStack 是 functions 层 trait 翻译骨架, 不持 smoltcp 状态
//   - 实际 smoltcp 操作由 init.rs (privileged 层, 允许 unsafe) 提供
//   - SmoltcpNetStack::socket_open 等方法内部委托给本 raw 模块
//
// 阶段 1 (W4.2.1): 声明函数签名 + 0 逻辑, 验证编译
// 阶段 2 (W4.2.2): 实装 socket_close + dhcp_state 翻译
// 阶段 3+ (W4.2.3+): 实装 socket_open (buffer 整合) + SmoltcpNetStack 改造
// ========================================================================

/// W4.2.2 DHCP 状态翻译的 prev state 缓存.
///
/// 使用 `core::sync::atomic::AtomicU8` 持有 `DhcpState` 的 discriminant
/// (枚举 tag). `DhcpState::Bound` 的 ipv4 + `lease_expires_at` 字段
/// 用 `AtomicU32` (ipv4) + `AtomicU64` (`lease_expires_at`) 单独存储.
///
/// ## 设计选择
///
/// 不使用 `static mut` + 裸指针: Rust 2024 edition 启用了
/// `invalid_reference_casting` lint, 编译失败. 不使用 `UnsafeCell<T>`
/// 包装: `static` 要求 `Sync`, 而 `UnsafeCell<T>: Sync` 需要 `T: Send`,
/// 但 `unsafe impl Send` 在 `no_std` 环境下行为不可靠.
///
/// ## 同步策略
///
/// 调用方需持有 `NET_LOCK` 互斥访问. 原子操作保证多线程可见性.
/// 4 个原子 (tag, ipv4`[4]`, `lease_expires_at`) 的"组合"通过 read 顺序保证
/// 一致性 (看 acquire/release).
///
/// ## 简化 (W4.2.2 阶段 1)
///
/// 仅 `AtomicU8` 持有 tag. Bound 的额外数据 (ipv4, `lease_expires_at`) 通过
/// `G_IPV4` (`AtomicU32`) + 单独的 `AtomicU64` 持有. W4.2.2 阶段不实装完整
/// 数据, 仅追踪 tag 转换.
static PREV_DHCP_TAG: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// W4.2.2 DHCP 状态 Bound 的 ipv4.
static PREV_DHCP_IPV4: [core::sync::atomic::AtomicU8; 4] = [
    core::sync::atomic::AtomicU8::new(0),
    core::sync::atomic::AtomicU8::new(0),
    core::sync::atomic::AtomicU8::new(0),
    core::sync::atomic::AtomicU8::new(0),
];

/// DHCP 租约的地址前缀长度 (Bound 时有效, 供 init flow 重建 iface CIDR).
static PREV_DHCP_PREFIX: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// DHCP 租约的默认网关 (全 0 = 无网关, 作 None 哨兵).
static PREV_DHCP_ROUTER: [core::sync::atomic::AtomicU8; 4] = [
    core::sync::atomic::AtomicU8::new(0),
    core::sync::atomic::AtomicU8::new(0),
    core::sync::atomic::AtomicU8::new(0),
    core::sync::atomic::AtomicU8::new(0),
];

/// DHCP 租约的 DNS 服务器 (最多 3 个, 0 = 无, 存网络字节序).
static PREV_DHCP_DNS: [core::sync::atomic::AtomicU32; 3] = [
    core::sync::atomic::AtomicU32::new(0),
    core::sync::atomic::AtomicU32::new(0),
    core::sync::atomic::AtomicU32::new(0),
];

/// 实际打开一个 socket (W4.2.3.2 实装).
///
/// 根据 `kind` 构造 smoltcp socket (Tcp/Udp), 加入 `sockets`, 记录 buffer
/// 指针到 `SOCKET_TABLE` / `TCP_RX_BUFS` / `TCP_TX_BUFS` / `UDP_RX_BUFS` /
/// `UDP_TX_BUFS` / `FD_TYPES`, 返回 `smol_handle`.
///
/// ## 索引空间分配 (W4.2.3.1)
///
/// `slot_idx` ∈ [0, `TOTAL_SLOTS)`:
/// - `0..MAX_SM_FD`:           `sm_socket` 路径 (现有 `sm_socket` 调用)
/// - `MAX_SM_FD..TOTAL_SLOTS`: `SmoltcpNetStack` 路径 (W4.2.4 整合后)
///
/// 两个范围严格隔离, 不冲突.
///
/// ## buffer 来源 (W4.2.3.2 实装)
///
/// Tcp/Udp RX/TX buffer 走 `k_malloc` (slab), 与现有 `sm_socket` 路径一致.
/// buffer 指针记入 `TCP_RX_BUFS` / `TCP_TX_BUFS` / `UDP_RX_BUFS` / `UDP_TX_BUFS`
/// (索引 = `slot_idx`). close 时通过 `socket_close_stub` + `sm_close` 归还.
///
/// ## 安全性
///
/// buffer 'static 借用: smoltcp `SocketSet`<'static> 要求 socket 借用
/// 'static. 我们用 `unsafe { core::slice::from_raw_parts_mut(ptr, size) }`
/// 强制 'static (与现有 `sm_socket` 模式一致). 安全性依赖于:
///   - `k_malloc` 不会在进程生命周期内释放 (slab 进程级)
///   - `socket_close` 时通过 `k_free` 归还 (W4.2.3.3 迁移时实装)
///
/// ## 简化 (W4.2.3.2 阶段)
///
/// - 暂不实装 Icmp/Raw/Dhcpv4/Dns (返回 None)
/// - `sm_socket` 路径暂不调用本函数 (W4.2.3.3 迁移)
/// - `SmoltcpNetStack` 路径暂不调用本函数 (W4.2.3.4 整合)
pub fn socket_open_stub(
    sockets: &mut SocketSet<'_>,
    kind: crate::privileged::net::iface_trait::SocketKind,
    slot_idx: usize,
) -> Option<smoltcp::iface::SocketHandle> {
    use crate::privileged::net::iface_trait::SocketKind;

    // SAFETY: 调用方持有 NET_STATE 锁, 整个函数体通过 raw accessor 访问 NetState.
    unsafe {
        // 1. 校验 slot_idx 范围
        if slot_idx >= TOTAL_SLOTS {
            return None;
        }
        // 2. 校验槽位空闲
        if socket_handle(slot_idx).is_some() {
            return None;
        }

        match kind {
            SocketKind::Tcp => {
                // TD-07: TCP RX/TX 缓冲走 slab, 与 sm_socket 路径一致.
                // SAFETY: k_malloc 在初始化后可用, 返回非空或 null. null 时立即返回 None.
                let rx_ptr = crate::privileged::mm::k_malloc(TCP_BUF_SIZE);
                if rx_ptr.is_null() {
                    return None;
                }
                let tx_ptr = crate::privileged::mm::k_malloc(TCP_BUF_SIZE);
                if tx_ptr.is_null() {
                    crate::privileged::mm::k_free(rx_ptr);
                    return None;
                }
                // SAFETY: rx_ptr/tx_ptr 来自 k_malloc(TCP_BUF_SIZE), 长度合法, 唯一别名.
                //         'static 借用基于: slab 进程级 + 索引化生命周期管理.
                let rx_slice = core::slice::from_raw_parts_mut(rx_ptr, TCP_BUF_SIZE);
                let tx_slice = core::slice::from_raw_parts_mut(tx_ptr, TCP_BUF_SIZE);
                let tcp_sock = smoltcp::socket::tcp::Socket::new(
                    smoltcp::socket::tcp::SocketBuffer::new(rx_slice),
                    smoltcp::socket::tcp::SocketBuffer::new(tx_slice),
                );
                let handle = sockets.add(tcp_sock);
                set_socket_handle(slot_idx, Some(handle));
                set_fd_type(slot_idx, 1);
                set_tcp_rx_buf(slot_idx, rx_ptr);
                set_tcp_tx_buf(slot_idx, tx_ptr);
                Some(handle)
            }
            SocketKind::Udp => {
                // TD-07: UDP RX/TX 缓冲走 slab. metas 仍静态 (16 KB).
                let rx_ptr = crate::privileged::mm::k_malloc(UDP_BUF_SIZE);
                if rx_ptr.is_null() {
                    return None;
                }
                let tx_ptr = crate::privileged::mm::k_malloc(UDP_BUF_SIZE);
                if tx_ptr.is_null() {
                    crate::privileged::mm::k_free(rx_ptr);
                    return None;
                }
                // SAFETY: 同 TCP 注释, 'static 借用基于 slab 进程级 + 索引化管理.
                let rx_slice = core::slice::from_raw_parts_mut(rx_ptr, UDP_BUF_SIZE);
                let tx_slice = core::slice::from_raw_parts_mut(tx_ptr, UDP_BUF_SIZE);
                let rx_meta = udp_rx_meta(slot_idx);
                let tx_meta = udp_tx_meta(slot_idx);
                let udp_sock = smoltcp::socket::udp::Socket::new(
                    smoltcp::socket::udp::PacketBuffer::new(&mut rx_meta[..], rx_slice),
                    smoltcp::socket::udp::PacketBuffer::new(&mut tx_meta[..], tx_slice),
                );
                let handle = sockets.add(udp_sock);
                set_socket_handle(slot_idx, Some(handle));
                set_fd_type(slot_idx, 2);
                set_udp_rx_buf(slot_idx, rx_ptr);
                set_udp_tx_buf(slot_idx, tx_ptr);
                Some(handle)
            }
            SocketKind::Icmp => {
                // ICMP socket: 使用 UDP socket buffer (ICMP 无连接, 类似 UDP)
                let rx_ptr = crate::privileged::mm::k_malloc(UDP_BUF_SIZE);
                if rx_ptr.is_null() {
                    return None;
                }
                let tx_ptr = crate::privileged::mm::k_malloc(UDP_BUF_SIZE);
                if tx_ptr.is_null() {
                    crate::privileged::mm::k_free(rx_ptr);
                    return None;
                }
                // SAFETY: rx_ptr/tx_ptr 来自 k_malloc, 长度合法, 唯一别名
                let rx_slice = core::slice::from_raw_parts_mut(rx_ptr, UDP_BUF_SIZE);
                let tx_slice = core::slice::from_raw_parts_mut(tx_ptr, UDP_BUF_SIZE);
                let rx_meta = udp_rx_meta(slot_idx);
                let tx_meta = udp_tx_meta(slot_idx);
                let udp_sock = smoltcp::socket::udp::Socket::new(
                    smoltcp::socket::udp::PacketBuffer::new(&mut rx_meta[..], rx_slice),
                    smoltcp::socket::udp::PacketBuffer::new(&mut tx_meta[..], tx_slice),
                );
                let handle = sockets.add(udp_sock);
                set_socket_handle(slot_idx, Some(handle));
                set_fd_type(slot_idx, 2); // ICMP 走 UDP socket 类型
                set_udp_rx_buf(slot_idx, rx_ptr);
                set_udp_tx_buf(slot_idx, tx_ptr);
                Some(handle)
            }
            SocketKind::Raw | SocketKind::Dhcpv4 | SocketKind::Dns => {
                // Raw/Dhcpv4/Dns: 用户态不可见, 暂不支持
                None
            }
        }
    }
}

/// 实际获取 DHCP 状态 (W4.2.2 实装).
///
/// 翻译 `dhcpv4::Socket::poll()` → `DhcpState`:
/// - `None` → 保持 prev tag 翻译结果 (内部 static, 0 初始化 = Idle)
/// - `Some(Event::Deconfigured)` → 区分初始启动与租约丢失 (见下)
/// - `Some(Event::Configured(config))` → Bound { ipv4, `lease_expires_at`: `u64::MAX` }
///
/// ## 内部状态
///
/// 使用 `PREV_DHCP_TAG` (tag) + `PREV_DHCP_IPV4` / `PREV_DHCP_PREFIX` /
/// `PREV_DHCP_ROUTER` / `PREV_DHCP_DNS` 维护翻译结果.
/// 调用方需在 `NET_STATE` 锁保护下调用 (确保互斥访问).
///
/// ## dhcpv4 poll 语义
///
/// smoltcp `dhcpv4::Socket::poll()` 返回 Option<Event>:
/// - None: 无新事件, DHCP 状态机内部推进中
/// - `Some(Event::Configured)`: 收到 DHCP ACK, 已配置
/// - `Some(Event::Deconfigured)`: 内部 `config_changed` 置位且 state 非 Renewing
///
/// 注意: `dhcpv4::Socket::new()` 初始 `config_changed = true`, 故**首次**
/// `poll()` 必返回 `Some(Deconfigured)` (初始态), 而非租约丢失. 因此
/// Deconfigured 需按 prev tag 区分:
/// - prev tag 为 3/4 (曾 Bound/Renewing): 租约丢失/NAK → 回 Idle (tag 0)
/// - 其他 (初始 Discovering 启动): 记为 Discovering (tag 1), 否则会把
///   prev tag 覆盖为 0, 导致后续每轮 `None` 都退化为 Idle 并重复拆配置.
///
/// 我们翻译为 trait `DhcpState`, 简化 `lease_expires_at` = `u64::MAX`
/// (实际租约管理在 init flow 中通过 `G_IPV4` / `G_GATEWAY` 跟踪).
pub fn dhcp_state_stub(
    sockets: &mut SocketSet<'_>,
    dhcp_handle: Option<smoltcp::iface::SocketHandle>,
) -> crate::privileged::net::iface_trait::DhcpState {
    use crate::privileged::net::iface_trait::DhcpState;
    use core::sync::atomic::Ordering;

    // 读取 prev tag (Acquire 同步)
    let prev_tag = PREV_DHCP_TAG.load(Ordering::Acquire);

    // 无 DHCP handle 时, DHCP 未启动, 状态为 Idle
    let Some(handle) = dhcp_handle else {
        return DhcpState::Idle;
    };

    let dhcp = sockets.get_mut::<dhcpv4::Socket>(handle);
    match dhcp.poll() {
        None => {
            // 无新事件, 翻译 prev tag → DhcpState
            tag_to_dhcp_state(prev_tag)
        }
        Some(dhcpv4::Event::Deconfigured) => {
            if prev_tag == 3 || prev_tag == 4 {
                // 曾 Bound/Renewing: 租约丢失或 NAK, 回到 Idle (tag = 0)
                PREV_DHCP_TAG.store(0, Ordering::Release);
                DhcpState::Idle
            } else {
                // 初始启动: 首次 poll 必返回 Deconfigured, 记为 Discovering
                // (tag = 1), 避免退化为 Idle 并重复拆配置.
                PREV_DHCP_TAG.store(1, Ordering::Release);
                DhcpState::Discovering
            }
        }
        Some(dhcpv4::Event::Configured(config)) => {
            // DHCP 配置完成, 提取 IP/前缀/网关/DNS + 写 tag
            let ipv4 = config.address.address().octets();
            for (i, &byte) in ipv4.iter().enumerate() {
                PREV_DHCP_IPV4[i].store(byte, Ordering::Release);
            }
            PREV_DHCP_PREFIX.store(config.address.prefix_len(), Ordering::Release);
            // 无网关时写全 0 (None 哨兵)
            let router = config.router.map_or([0u8; 4], |r| r.octets());
            for (i, &byte) in router.iter().enumerate() {
                PREV_DHCP_ROUTER[i].store(byte, Ordering::Release);
            }
            // DNS 服务器最多取前 3 个, 缺省写 0
            for (i, slot) in PREV_DHCP_DNS.iter().enumerate() {
                let v = config
                    .dns_servers
                    .get(i)
                    .map_or(0u32, |d| u32::from_be_bytes(d.octets()));
                slot.store(v, Ordering::Release);
            }
            PREV_DHCP_TAG.store(3, Ordering::Release); // tag 3 = Bound
            DhcpState::Bound {
                ipv4,
                lease_expires_at: u64::MAX, // 简化: 实际租约管理在 init flow
            }
        }
    }
}

/// 读取最近一次 DHCP 租约的地址前缀长度.
///
/// 调用方需持有 `NET_STATE` 锁. 仅在 `dhcp_state_stub` 返回 `Bound` 后有义.
pub fn dhcp_prefix_len() -> u8 {
    PREV_DHCP_PREFIX.load(core::sync::atomic::Ordering::Acquire)
}

/// 读取最近一次 DHCP 租约的默认网关; 无网关时返回 `None`.
///
/// 调用方需持有 `NET_STATE` 锁.
pub fn dhcp_router() -> Option<[u8; 4]> {
    use core::sync::atomic::Ordering;
    let octets = [
        PREV_DHCP_ROUTER[0].load(Ordering::Acquire),
        PREV_DHCP_ROUTER[1].load(Ordering::Acquire),
        PREV_DHCP_ROUTER[2].load(Ordering::Acquire),
        PREV_DHCP_ROUTER[3].load(Ordering::Acquire),
    ];
    if octets == [0, 0, 0, 0] {
        None
    } else {
        Some(octets)
    }
}

/// 读取最近一次 DHCP 租约的 DNS 服务器 (最多 3 个, 每项网络字节序, 0 = 无).
///
/// 调用方需持有 `NET_STATE` 锁.
pub fn dhcp_dns_servers() -> [[u8; 4]; 3] {
    use core::sync::atomic::Ordering;
    let mut out = [[0u8; 4]; 3];
    for (i, slot) in PREV_DHCP_DNS.iter().enumerate() {
        out[i] = slot.load(Ordering::Acquire).to_be_bytes();
    }
    out
}

/// `SmoltcpNetStack::close` 的 safe wrapper (W4.2.3.4).
///
/// 关闭 `[MAX_SM_FD, TOTAL_SLOTS)` 范围内的 smoltcp socket,
/// 释放 buffer 并清空槽位状态. 与 `sm_close` 逻辑对称, 但索引
/// 校验针对 `SmoltcpNetStack` 专属范围.
///
/// ## 返回
///
/// - `true`: 关闭成功
/// - `false`: `slot_idx` 越界或槽位空闲
pub fn smoltcp_net_stack_socket_close(slot_idx: usize) -> bool {
    // SAFETY: 调用方持有 NET_STATE 锁, 整个函数体通过 raw accessor 访问 NetState.
    unsafe {
        // 1. 校验 slot_idx 在 SmoltcpNetStack 范围内
        if !(MAX_SM_FD..TOTAL_SLOTS).contains(&slot_idx) {
            return false;
        }
        // 2. 校验槽位已占用
        if socket_handle(slot_idx).is_none() || fd_type(slot_idx) == 0 {
            return false;
        }

        let handle = socket_handle(slot_idx).unwrap();
        let stype = fd_type(slot_idx);
        let sockets = &mut *socket_set();

        // 3. 根据类型关闭 TCP/UDP socket
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

        // 4. 从 SocketSet 移除
        sockets.remove(handle);

        // 5. 释放 slab buffer
        if !tcp_rx_buf(slot_idx).is_null() {
            crate::privileged::mm::k_free(tcp_rx_buf(slot_idx));
            set_tcp_rx_buf(slot_idx, core::ptr::null_mut());
        }
        if !tcp_tx_buf(slot_idx).is_null() {
            crate::privileged::mm::k_free(tcp_tx_buf(slot_idx));
            set_tcp_tx_buf(slot_idx, core::ptr::null_mut());
        }
        if !udp_rx_buf(slot_idx).is_null() {
            crate::privileged::mm::k_free(udp_rx_buf(slot_idx));
            set_udp_rx_buf(slot_idx, core::ptr::null_mut());
        }
        if !udp_tx_buf(slot_idx).is_null() {
            crate::privileged::mm::k_free(udp_tx_buf(slot_idx));
            set_udp_tx_buf(slot_idx, core::ptr::null_mut());
        }

        // 6. 清空槽位状态
        set_socket_handle(slot_idx, None);
        set_fd_type(slot_idx, 0);
        true
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `SmoltcpNetStack::poll` 的 safe wrapper (W4.2.3.4).
///
/// 驱动 smoltcp 协议栈轮询 (TX/RX + 定时器 + DHCP), 返回 `PollOutcome`.
/// 与 `poll_network` 逻辑对称, 但由 `SmoltcpNetStack` 调用方主动触发
/// (而非 timer ISR 自动轮询).
pub fn smoltcp_net_stack_poll() -> crate::privileged::net::iface_trait::PollOutcome {
    use crate::privileged::net::iface_trait::PollOutcome;

    let nic = match device_mut() {
        Some(d) => d,
        None => return PollOutcome::idle(),
    };
    let stack = match stack_mut() {
        Some(s) => s,
        None => return PollOutcome::idle(),
    };
    // SAFETY: socket_set() 返回已初始化的 SocketSet 指针, 调用方持有 NET_LOCK.
    let sockets = unsafe { &mut *socket_set() };

    // SAFETY: stack.poll 驱动 smoltcp 协议栈 (RX/TX + 定时器), 返回 PollResult.
    let poll_result = stack.poll(nic, sockets);
    // SAFETY: process_dhcp_events 在 NET_LOCK 保护下处理 DHCP 事件.
    unsafe { super::process_dhcp_events(sockets) };

    // 将 smoltcp PollResult 翻译为 Edgine PollOutcome
    match poll_result {
        smoltcp::iface::PollResult::SocketStateChanged => PollOutcome {
            packet_received: true,
            socket_woken: true,
            dhcp_progressed: false,
            tx_pending: 0,
        },
        smoltcp::iface::PollResult::None => PollOutcome::idle(),
    }
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
/// `DhcpState` tag → `DhcpState` 翻译.
///
/// tag 值 (来自 `PREV_DHCP_TAG)`:
/// - 0: Idle
/// - 1: Discovering
/// - 2: Requesting
/// - 3: Bound (含 ipv4)
/// - 4: Renewing
/// - 5: Failed
fn tag_to_dhcp_state(tag: u8) -> crate::privileged::net::iface_trait::DhcpState {
    use crate::privileged::net::iface_trait::DhcpState;
    use core::sync::atomic::Ordering;
    match tag {
        0 => DhcpState::Idle,
        1 => DhcpState::Discovering,
        2 => DhcpState::Requesting,
        3 => {
            let ipv4 = [
                PREV_DHCP_IPV4[0].load(Ordering::Acquire),
                PREV_DHCP_IPV4[1].load(Ordering::Acquire),
                PREV_DHCP_IPV4[2].load(Ordering::Acquire),
                PREV_DHCP_IPV4[3].load(Ordering::Acquire),
            ];
            DhcpState::Bound {
                ipv4,
                lease_expires_at: u64::MAX,
            }
        }
        4 => {
            let ipv4 = [
                PREV_DHCP_IPV4[0].load(Ordering::Acquire),
                PREV_DHCP_IPV4[1].load(Ordering::Acquire),
                PREV_DHCP_IPV4[2].load(Ordering::Acquire),
                PREV_DHCP_IPV4[3].load(Ordering::Acquire),
            ];
            DhcpState::Renewing { ipv4 }
        }
        5 => DhcpState::Failed,
        _ => DhcpState::Idle, // 默认
    }
}

/// klog 网络消息 (C 字符串) - 安全包装
pub fn klog_msg(s: &str) {
    let mut buf = [0u8; 256];
    let bytes = s.as_bytes();
    let len = bytes.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&bytes[..len]);
    // SAFETY: 临时 C 字符串, 在 klog_net 调用期间有效。
    unsafe { klog_net(buf.as_ptr().cast()) };
}

/// klog 初始化消息 (走 `klog_init_msg`)
pub fn klog_init(s: &str) {
    let mut buf = [0u8; 256];
    let bytes = s.as_bytes();
    let len = bytes.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&bytes[..len]);
    // SAFETY: 临时 C 字符串, 在 klog_init_msg 调用期间有效。
    unsafe { klog_init_msg(buf.as_ptr().cast()) };
}

/// klog 错误消息
pub fn klog_err(s: &str) {
    let mut buf = [0u8; 256];
    let bytes = s.as_bytes();
    let len = bytes.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&bytes[..len]);
    // SAFETY: 临时 C 字符串, 在 klog_net_err 调用期间有效。
    unsafe { klog_net_err(buf.as_ptr().cast()) };
}
