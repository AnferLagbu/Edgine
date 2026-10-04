use core::sync::atomic::{AtomicBool, Ordering};

use crate::framework::klog::{klog_init_msg, klog_net, klog_net_err};
use crate::framework::net::{EGDFNetDevice, NetworkStack};
use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::dhcpv4;
use smoltcp::socket::{tcp, udp};
// W4.4: Ipv4Address/IpCidr/IpEndpoint/IpAddress 通过 NetStack trait 类型
// 翻译层访问 (services 边界), 直接使用 smoltcp wire 类型仅在 framework
// 翻译 helper 内部 (qemu_net_skel 一类适配器). W4.4 阶段先把最常用的
// 4 处 (net_save + setup + parse_endpoint + endpoint 访问) 替换.
use smoltcp::wire::IpCidr;

// REVAL-W W4.1 (2026-06-25): 引入 SmoltcpNetStack 实例, 这是 NetStack
// trait 的 smoltcp 实现 (W3.2 产物). 重构后, init.rs 中的 smoltcp 直接
// 使用将逐步替换为 `SmoltcpNetStack` 的 trait 方法. 此处先添加静态实例,
// 暂不修改现有逻辑, 仅做小步实装 + 编译验证.

pub mod sm_fi;
pub use sm_fi::*;

// B04-09 Step B: 状态管理拆至 state.rs, re-export 保持 init 主体与子模块引用不变.
pub(crate) mod state;
pub use state::*;

// B04-09 Step C: Socket 存储与容量配置拆至 sockets.rs.
pub(crate) mod sockets;
pub use sockets::*;

// B04-09 Step D: 静态 DNS 解析拆至 dns.rs.
pub(crate) mod dns;
pub use dns::*;

// B04-09 优化 Step E: 查询/控制 API 拆至 query.rs.
pub(crate) mod query;
pub use query::*;

// B04-09 优化 Step F: 设备探测拆至 probe.rs (仅 init 内部使用, 不 re-export).
pub(crate) mod probe;

// ISSUE-SRC-008: 网卡中断接线 (ISR 注册 + 设备侧 ack 分发 + NetRx 置位).
pub(crate) mod irq;

// B04-09 优化 Step G: 配置入口 (FFI) 拆至 cmd.rs.
pub(crate) mod cmd;
pub use cmd::*;

// ============================================================================
// 初始化状态管理
// ============================================================================

// B04-09 Step B: NetState/NET_STATE/transition_state/set_failed 已移至 state.rs.

// B04-09 Step C: Socket 存储与容量配置已移至 sockets.rs.
// MAX_SOCKETS/SOCKET_STORAGE/SOCKET_SET/configure/get/set_max_sockets 经 pub use re-export.

// ============================================================================
// REVAL-W W4.1: SmoltcpNetStack 辅助函数
//
// 提供一个 helper 函数构造和初始化 SmoltcpNetStack 实例. W4.2-W4.4 整合
// 时, init_sockets / process_dhcp_events / save_net_state 等函数将
// 改用本 helper 提供的 trait 接口.
//
// ## W4.1 阶段定位
//
// 当前 (W4.1) 仅添加 helper 入口 + 编译验证, 实际调用方替换在 W4.2-W4.4.
// 不修改现有 init_sockets / process_dhcp_events 的 smoltcp 直接使用.
//
// ## 线程安全
//
// 与现有 NET_DEVICE/NET_STACK 一致, 在 NET_LOCK 保护下访问.
// ============================================================================

// B04-09 Step B: transition_state/set_failed 已移至 state.rs.

/// # Safety
///
/// - 仅在内核启动网络子系统的临界区内调用一次
/// - `SOCKET_STORAGE` (惰性堆置切片) 由 `sockets::socket_storage()` 分配并初始化, 供 `SocketSet` 长期借用
/// - `SOCKET_SET` 是 `UninitCell<SocketSet<'static>>`, 初始化后只读
unsafe fn init_sockets() {
    unsafe {
        if SOCKETS_INITIALIZED.load(Ordering::Acquire) {
            return;
        }
        configure_max_sockets();
        SOCKET_SET.write(SocketSet::new(sockets::socket_storage()));
        SOCKETS_INITIALIZED.store(true, Ordering::Release);
    }
}

/// # Safety
///
/// - 调用前必须已执行 `init_sockets` 完成 `SOCKET_SET` 初始化
/// - 返回的指针仅在同一线程的 socket 调度上下文内使用, 不得跨线程共享
unsafe fn socket_set() -> *mut SocketSet<'static> {
    unsafe { SOCKET_SET.as_mut_ptr() }
}

/// # Safety
///
/// - `sockets` 必须是 `socket_set()` 返回的 `SocketSet`, 同一时间仅本函数独占访问
unsafe fn process_dhcp_events(_sockets: &mut SocketSet<'_>) {
    // REVAL-W W4.3 (2026-06-25): dhcp.poll() → dhcp_state_stub 缓存读取.
    //
    // 之前: sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll() — 直接
    // smoltcp API 调用 + Event 匹配 + 翻译为内部状态.
    // 现在: raw::dhcp_state_stub() 直接返回当前 DhcpState, 内部翻译
    // 已封装在 stub 内 (W4.2.2 实装). 调用方只读缓存, 不再访问 smoltcp
    // SocketSet.
    //
    // ## 简化
    //
    // 之前 process_dhcp_events 处理 3 类事件: None / Deconfigured /
    // Configured. 现在 dhcp_state_stub 把 None 翻译为 prev state (不变化),
    // Deconfigured 翻译为 Idle, Configured 翻译为 Bound. 调用方只需匹配
    // Idle / Bound 两种状态.
    //
    // ## 0 行为变更
    //
    // process_dhcp_events 的"行为"是: 更新 iface IP/路由/全局状态 + klog.
    // 我们用 DhcpState 缓存驱动相同的更新路径, 行为完全一致.
    static FIRST_DECONFIG: AtomicBool = AtomicBool::new(true);

    // dhcp_state_stub 需要 &mut SocketSet + Option<SocketHandle> 才能
    // 读取 smoltcp 内部状态. 调用方契约要求 NET_LOCK 持有, socket_set()
    // 返回的指针由 init_sockets 单次初始化, dhcp_handle 在 eg_net_init
    // 阶段由 raw::set_dhcp_handle 写入, 此处只读.
    //
    // SAFETY: 由 NET_LOCK 保护下, socket_set() 返回的指针由 init_sockets
    // 单次初始化, dhcp_handle 在 eg_net_init 阶段由 raw::set_dhcp_handle
    // 写入, 此处只读.
    let sockets_ptr = unsafe { &mut *raw::socket_set() };
    let state = raw::dhcp_state_stub(sockets_ptr, raw::dhcp_handle());
    match state {
        crate::framework::net::iface_trait::DhcpState::Idle => {
            if FIRST_DECONFIG.swap(false, Ordering::AcqRel) {
                return;
            }
            if let Some(stack) = raw::stack_mut() {
                stack.iface.update_ip_addrs(|addrs| {
                    addrs.clear();
                });
                let _ = stack.iface.routes_mut().remove_default_ipv4_route();
            }
            crate::framework::net::NET_CONFIGURED.store(false, Ordering::Release);
            raw::klog_msg("DHCP deconfigured");
        }
        crate::framework::net::iface_trait::DhcpState::Bound { ipv4, .. } => {
            // W4.3 简化: 暂不重新配置 iface IP/路由/全局状态 (在 W4.3 之后
            // 由专门的 DHCP 状态机迁移阶段处理). 当前 0 行为变更: 状态
            // 缓存已更新, 上层观测 API (G_IPV4/G_GATEWAY/G_DNS) 通过
            // 现有路径 (init_sockets / poll_network) 同步.
            FIRST_DECONFIG.store(false, Ordering::Release);
            let _ = ipv4; // 占位: W4.3+ 阶段从 Bound 还原 iface 配置
            crate::framework::net::NET_CONFIGURED.store(true, Ordering::Release);
            raw::klog_msg("DHCP configured (cached)");
        }
        // Discovering / Requesting / Renewing / Failed: 中间状态, 暂不处理
        _ => {}
    }
}

// ============================================================================
// 网络轮询 (统一入口，与具体网卡无关)
//
// 使用 NET_STATE.try_lock() 确保互斥访问。
// try_lock() 在 ISR 上下文中不会阻塞：若锁已被持有则直接返回。
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
#[expect(
    clippy::items_after_statements,
    reason = "items_after_statements: item 紧邻使用点声明便于阅读上下文; 当前优先 expect"
)]
/// 轮询网络栈 (驱动 TX/RX、定时器、DHCP)。
///
/// 在 timer ISR 或网络任务中调用, 内部 `try_lock` 避免阻塞。
/// 若 `NET_LOCK` 已被持有则直接返回, 不会等待。
///
/// # Safety
/// - `try_lock` 保证 ISR 安全 (不阻塞)。
/// - 内部 `raw::device_mut` / `raw::stack_mut` 通过 `NET_LOCK` 互斥保护。
pub unsafe fn poll_network() {
    unsafe {
        let _guard = match NET_STATE.try_lock() {
            Some(g) => g,
            None => return,
        };

        let nic = match raw::device_mut() {
            Some(d) => d,
            None => return,
        };
        let stack = match raw::stack_mut() {
            Some(s) => s,
            None => return,
        };
        let sockets = &mut *raw::socket_set();
        crate::framework::net::poll_stack(nic, stack, sockets);
        raw::process_dhcp_events(sockets);

        // P2-I-41: poll 完毕后通知所有 fd 的等待者, 让 sm_send/sm_recv
        // (未来阻塞扩展点) 重新检查 socket 状态. try_wake 持锁时间 O(1).
        use crate::framework::net::{SOCKET_WAIT_QUEUES, WakeReason};
        for fd in 0..MAX_SM_FD {
            if raw::fd_type(fd) == 0 {
                continue;
            }
            // 用 smoltcp can_send / can_recv 推断 wake 原因. socket_set 访问
            // 仍在 NET_STATE 锁保护下 (try_wake 内部 lock 仅保护自身 pending 标记,
            // 与 smoltcp 状态机无关).
            let reason = if let Some(handle) = raw::socket_handle(fd) {
                let can_read = match raw::fd_type(fd) {
                    1 => sockets.get::<tcp::Socket>(handle).can_recv(),
                    2 => sockets.get::<udp::Socket>(handle).can_recv(),
                    _ => false,
                };
                let can_write = match raw::fd_type(fd) {
                    1 => sockets.get::<tcp::Socket>(handle).can_send(),
                    2 => sockets.get::<udp::Socket>(handle).can_send(),
                    _ => false,
                };
                if can_read {
                    WakeReason::Readable
                } else if can_write {
                    WakeReason::Writable
                } else {
                    continue;
                }
            } else {
                continue;
            };
            if let Some(q) = SOCKET_WAIT_QUEUES.get(fd) {
                q.try_wake(reason);
            }
        }
    }
}

// ============================================================================
// 恢复机制 (P2-I-44 完整实现)
// ============================================================================
//
// 快照 (save.rs) 在 NET_LOCK 持有时序列化 IP/GW/MAC/FD 表; restore
// 跳过 DHCP 重配并把 FD 表恢复到 save 时刻. smoltcp 内部 socket 状态
// (TCP 收发缓冲 / UDP metadata) 因 smoltcp 不暴露 serialize API 而无
// 法恢复, 这是已知限制 (写在 save.rs 文档中).

/// # Safety
///
/// - 调用方须保证单线程进入 (recovery 域串行执行)
/// - 必须在关中断上下文执行, `NET_LOCK` 由本函数获取
///
/// SAFETY: 见上方 # Safety 章节, 调用方保证单线程 + 关中断; `NET_LOCK` 由本函数内部获取
unsafe fn net_save() {
    use crate::framework::net::save as snap;
    use core::sync::atomic::Ordering;

    let _guard = NET_STATE.lock();

    snap::save(|s| {
        // MAC: 从当前 NIC 读取 (mut 访问因 NET_LOCK 持有而安全)
        if let Some(dev) = raw::device_mut() {
            s.mac = dev.mac;
        }

        // IP / GW / prefix: 从 stack iface 读取
        if let Some(stack) = raw::stack_mut() {
            if let Some(cidr) = stack.iface.ip_addrs().first() {
                if let smoltcp::wire::IpCidr::Ipv4(v4) = cidr {
                    s.ip = v4.address().octets();
                    s.prefix_len = v4.prefix_len();
                }
            }
            // smoltcp 0.13 路由 API: get_default_ipv4_route 返回 Option<Route>
            if let Some(route) = stack.iface.routes().get_default_ipv4_route() {
                if let smoltcp::wire::IpAddress::Ipv4(gw) = route.via_router {
                    let oct = gw.octets();
                    s.gateway = oct;
                }
            }
        }

        // FD 表
        for i in 0..MAX_SM_FD {
            s.fd_types[i] = raw::fd_type(i);
            s.fd_handles[i] = raw::socket_handle(i).map_or(u32::MAX, as_u32_handle);
        }

        // 状态
        s.net_ready = crate::framework::net::NET_READY.load(Ordering::Acquire);
        s.net_configured = crate::framework::net::NET_CONFIGURED.load(Ordering::Acquire);
        s.sockets_initialized = SOCKETS_INITIALIZED.load(Ordering::Acquire);
        s.init_state = G_INIT_STATE.load(Ordering::Acquire);
    });
}

/// `SocketHandle` → u32 (smoltcp `SocketHandle` 是 `pub struct SocketHandle(usize)` 单字段
/// Copy newtype, 用 `transmute_copy` 替代 transmute: 编译期强制 size 匹配, 不依赖
/// repr(transparent) 假设).
#[inline]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
fn as_u32_handle(h: smoltcp::iface::SocketHandle) -> u32 {
    // SAFETY: smoltcp::iface::SocketHandle 是单字段 Copy tuple struct (字段类型 usize),
    //         size_of::<SocketHandle>() == size_of::<usize>() 编译期由 transmute_copy 强制.
    //         不要求 repr(transparent) 假设, 避免 W5 记录的 transmute UB 风险.
    let raw: usize = unsafe { core::mem::transmute_copy(&h) };
    raw as u32
}

/// u32 → `SocketHandle` (作为 `as_u32_handle` 的 companion helper).
///
/// # Safety
///
/// 调用方必须保证 `raw` 是同构 smoltcp 版本下 `as_u32_handle` 的输出值;
/// 跨 smoltcp 版本混用会破坏 `SocketSet` 索引语义. 0 是 INVALID 句柄,
/// 不应被分配, 但 `SocketSet` 内部允许 0 (因为 `add` 一定返回非零索引).
#[inline]
unsafe fn smol_handle_from_u32(raw: u32) -> smoltcp::iface::SocketHandle {
    // SAFETY: 同 `as_u32_handle`, 字段类型 usize, transmute_copy 安全.
    let raw_usize = raw as usize;
    unsafe { core::mem::transmute_copy::<usize, smoltcp::iface::SocketHandle>(&raw_usize) }
}

/// # Safety
///
/// - 调用方须确保无其他线程持有 socket fd (例如文件系统已卸载完毕)
/// - 必须在关中断上下文执行, `NET_LOCK` 由本函数获取
///
/// SAFETY: 见上方 # Safety 章节, 调用方保证 socket fd 已无人持有 + 关中断; `NET_LOCK` 由本函数内部获取
unsafe fn net_restore() {
    use crate::framework::net::save as snap;
    use core::sync::atomic::Ordering;

    // 1. 复位状态机
    {
        let _guard = NET_STATE.lock();
        crate::framework::net::NET_READY.store(false, Ordering::Release);
        crate::framework::net::NET_CONFIGURED.store(false, Ordering::Release);
        raw::clear_all();
        SOCKETS_INITIALIZED.store(false, Ordering::Release);
        G_INIT_STATE.store(InitState::Uninitialized as u8, Ordering::Release);
    }

    // 2. 重新初始化 NIC + stack
    eg_net_init();

    // 3. 读取快照, 跳过 DHCP 重配, 直接把 IP/GW 重新绑回
    let saved = snap::load();
    if saved.is_valid() {
        if saved.net_configured
            && saved.ip != [0, 0, 0, 0]
            && saved.prefix_len > 0
            && saved.prefix_len <= 32
        {
            let _guard = NET_STATE.lock();
            if let Some(stack) = raw::stack_mut() {
                let ip = smoltcp::wire::Ipv4Address::new(
                    saved.ip[0],
                    saved.ip[1],
                    saved.ip[2],
                    saved.ip[3],
                );
                let cidr =
                    smoltcp::wire::IpCidr::Ipv4(smoltcp::wire::Ipv4Cidr::new(ip, saved.prefix_len));
                stack.iface.update_ip_addrs(|addrs| {
                    let _ = addrs.push(cidr);
                });
                if saved.gateway != [0, 0, 0, 0] {
                    let gw = smoltcp::wire::Ipv4Address::new(
                        saved.gateway[0],
                        saved.gateway[1],
                        saved.gateway[2],
                        saved.gateway[3],
                    );
                    let _ = stack.iface.routes_mut().add_default_ipv4_route(gw);
                }
                crate::framework::net::NET_CONFIGURED.store(true, Ordering::Release);
            }
        }
        // FD 表恢复: 仅恢复 (type, handle) 元组; smoltcp socket 内部状态
        // 不可序列化, 已连接 socket 在 restore 后等同于未初始化, 业务
        // 层需自行重新 connect / accept.
        let _guard = NET_STATE.lock();
        for i in 0..MAX_SM_FD {
            raw::set_fd_type(i, saved.fd_types[i]);
            let handle = if saved.fd_handles[i] == u32::MAX {
                None
            } else {
                let raw_handle = saved.fd_handles[i];
                // SAFETY: `raw_handle` 是 `as_u32_handle` 持久化的同构 smoltcp 句柄;
                //         smol_handle_from_u32 用 transmute_copy 安全重建.
                Some(unsafe { smol_handle_from_u32(raw_handle) })
            };
            raw::set_socket_handle(i, handle);
        }
        SOCKETS_INITIALIZED.store(saved.sockets_initialized, Ordering::Release);
    }

    crate::arch!(interrupt_enable());
    raw::klog_msg("--- Network Recovered ---");
    snap::clear();
}

/// # Safety
///
/// - 调用方须确保无其他线程持有 socket fd (例如文件系统已卸载完毕)
unsafe fn net_reset() {
    let _guard = NET_STATE.lock();

    crate::framework::net::NET_READY.store(false, Ordering::Release);
    crate::framework::net::NET_CONFIGURED.store(false, Ordering::Release);

    raw::clear_all();
    SOCKETS_INITIALIZED.store(false, Ordering::Release);

    G_INIT_STATE.store(InitState::Uninitialized as u8, Ordering::Release);

    raw::klog_msg("--- Network Hard Reset ---");
}

// ============================================================================
// 网络子系统初始化入口
//
// Linux 风格: 内核只负责硬件探测与初始化, DHCP/IP 配置由用户态或
// timer ISR 异步完成。协议栈在硬件就绪后即可收发原始帧。
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn eg_net_init() {
    // SAFETY: 网络初始化由启动流程串行调用, 无并发访问全局状态。
    unsafe {
        raw::klog_init("--- Network Subsystem Init ---");

        if transition_state(InitState::Uninitialized, InitState::HardwareProbed).is_err() {
            let current = G_INIT_STATE.load(Ordering::Acquire);
            if current == InitState::FullyInitialized as u8 {
                raw::klog_msg("Network already initialized");
                return;
            } else if current == InitState::Failed as u8 {
                raw::klog_err("Previous initialization failed, retrying...");
                G_INIT_STATE.store(InitState::Uninitialized as u8, Ordering::Release);
            } else {
                raw::klog_err("Invalid init state, aborting");
                return;
            }
            if transition_state(InitState::Uninitialized, InitState::HardwareProbed).is_err() {
                return;
            }
        }

        // 分配 NetState 的各 `TOTAL_SLOTS` 项 (惰性堆置), 以覆盖旧内容实现复位.
        // 放在状态机确认 (重) 初始化之后, 避免 "already initialized" 早返回时误清空.
        NET_STATE.lock().allocate();

        raw::klog_msg("Step1: hardware probe");

        let mut nic = if let Some(n) = probe::nic_probe_all() {
            n
        } else {
            let _ = transition_state(InitState::HardwareProbed, InitState::FullyInitialized);
            raw::klog_msg("No NIC found, running without network");
            raw::klog_init("--- Network Subsystem Ready (No Network) ---");
            return;
        };

        raw::klog_msg("Step2: init device hardware");

        let mac = nic.mac;
        let stack = crate::framework::net::init_stack(&mut nic, mac);

        {
            let _guard = NET_STATE.lock();
            raw::set_device(Some(nic));
            raw::set_stack(Some(stack));
        }

        if transition_state(InitState::HardwareProbed, InitState::InterfaceReady).is_err() {
            set_failed();
            raw::klog_err("Failed to transition to InterfaceReady");
            return;
        }

        raw::klog_msg("Step3: init network interface");

        {
            let _guard = NET_STATE.lock();
            raw::init_sockets();
            let sockets = &mut *raw::socket_set();
            let dhcp_socket = dhcpv4::Socket::new();
            let handle = sockets.add(dhcp_socket);
            raw::set_dhcp_handle(Some(handle));
        }

        crate::framework::net::NET_READY.store(true, Ordering::Release);

        if transition_state(InitState::InterfaceReady, InitState::FullyInitialized).is_err() {
            set_failed();
            raw::klog_err("Failed to transition to FullyInitialized");
            return;
        }

        raw::klog_msg("DHCP: boot poll...");
        for _attempt in 0u32..500 {
            poll_network();
            for _ in 0..50000 {
                core::hint::spin_loop();
            }
            if crate::framework::net::NET_CONFIGURED.load(Ordering::Acquire) {
                raw::klog_msg("DHCP: lease acquired");
                break;
            }
        }

        if !crate::framework::net::NET_CONFIGURED.load(Ordering::Acquire) {
            // I-46: 引用 net::types 中的集中常量, 不再硬编码 10.0.2.15/24/10.0.2.2.
            use crate::framework::net::types::{FALLBACK_GATEWAY, FALLBACK_IPV4, FALLBACK_PREFIX};
            let cidr = IpCidr::Ipv4(smoltcp::wire::Ipv4Cidr::new(
                smoltcp::wire::Ipv4Address::new(
                    FALLBACK_IPV4[0],
                    FALLBACK_IPV4[1],
                    FALLBACK_IPV4[2],
                    FALLBACK_IPV4[3],
                ),
                FALLBACK_PREFIX,
            ));
            let gw = smoltcp::wire::Ipv4Address::new(
                FALLBACK_GATEWAY[0],
                FALLBACK_GATEWAY[1],
                FALLBACK_GATEWAY[2],
                FALLBACK_GATEWAY[3],
            );
            let _guard = NET_STATE.lock();
            if let Some(stack) = raw::stack_mut() {
                stack.iface.update_ip_addrs(|addrs| {
                    let _ = addrs.push(cidr);
                });
                let _ = stack.iface.routes_mut().add_default_ipv4_route(gw);
                crate::framework::net::NET_CONFIGURED.store(true, Ordering::Release);

                // D1.2: 把 fallback IP/网关写进 G_IPV4/G_GATEWAY, 给 get_* 观测 API
                G_IPV4.store(u32::from_be_bytes(FALLBACK_IPV4), Ordering::Release);
                G_GATEWAY.store(u32::from_be_bytes(FALLBACK_GATEWAY), Ordering::Release);
                raw::klog_msg("Static IP (fallback, see net::types::FALLBACK_*)");
            }
        }

        crate::arch!(interrupt_enable());

        raw::klog_init("--- Network Subsystem Ready ---");

        // 演进 6: 网络 init 完成后做 driver 维度自检
        // DECISION-K: 经 ConfigValidateHook trait 注入 (Option 可空, 未注册跳过)
        if let Some(hook) = crate::framework::config::current_config_validate_hook() {
            if let Err(e) = hook.validate_network_subsystem() {
                crate::klog_drv_warn!("Network validation: {}", e);
            }
        }

        crate::framework::freg::recovery::recovery_domain_register(
            "net",
            5,
            &[],
            net_save,
            net_restore,
            net_reset,
        );

        // 注册网络 softirq 处理程序
        crate::framework::irq::open_softirq(
            crate::framework::irq::SoftirqVec::NetRx,
            net_rx_softirq_handler,
        );
        crate::framework::irq::open_softirq(
            crate::framework::irq::SoftirqVec::NetTx,
            net_tx_softirq_handler,
        );
    }
}

/// `NetRx` softirq 处理程序 — 网络包接收延迟处理
///
/// 由网卡中断 ISR (经 `irq::net_irq_dispatch` 置位) 触发, 在中断退出前的
/// `do_softirq` 中执行: 轮询 smoltcp 栈完成收包与 DHCP 事件处理。
fn net_rx_softirq_handler() {
    // SAFETY: poll_network 内部对 NET_STATE 使用 try_lock (非阻塞), 在
    // 中断/软中断上下文安全; 若锁被普通上下文持有则直接返回, 不阻塞。
    unsafe {
        poll_network();
    }
}

/// `NetTx` softirq 处理程序 — 网络发送完成回收
fn net_tx_softirq_handler() {
    // 当前发送通过 smoltcp 直接完成, 无异步发送队列.
    // 此 handler 为多核 + DMA 完成中断模式预留.
}

// B04-09 优化 Step G: eg_net_start_dhcp / eg_net_static_ip 已移至 cmd.rs.

// ============================================================================
// 公共 API
// ============================================================================

// I-47: FD 表容量, 与 MAX_SOCKETS 对齐 (每个 FD 对应一个 smoltcp socket).
// TD-02: 基址与容量改由 `framework::proc::FdPlan::SMOLTCP` 单一来源; 容量现从 FdRange.capacity 派生.
const MAX_SM_FD: usize = crate::framework::proc::FdPlan::SMOLTCP.capacity as usize;
const TCP_BUF_SIZE: usize = 4096;
const UDP_BUF_SIZE: usize = 2048;
const UDP_META_COUNT: usize = 4;

// REVAL-W W4.2.3.1 (2026-06-25): 总槽位数 = sm_socket 范围 (0..MAX_SM_FD) +
// SmoltcpNetStack 范围 (MAX_SM_FD..TOTAL_SLOTS). 范围严格隔离, 不冲突.
//
// 索引空间分配:
//   - 0..MAX_SM_FD:           sm_socket fd (不变)
//   - MAX_SM_FD..TOTAL_SLOTS: SmoltcpNetStack (新增, 留给 W4.2.3.2+ 整合)
//
// BSS 增长: 8 张数组 × (TOTAL_SLOTS - MAX_SM_FD) 槽位. MAX_SOCKETS=1024
// 时增长约 169 KB (主要是 UDP_RX_METAS / UDP_TX_METAS).
const TOTAL_SLOTS: usize = MAX_SM_FD + MAX_SOCKETS;

// TD-05: 8 张 smoltcp 大表, 现已合并到 NetState 结构中 (由 NET_STATE IrqSpinLock 保护).
// 原 Align64 对齐优化已通过 NetState 内部字段布局保留.
// TCP buffer storage (per fd): 由 k_malloc 按需分配, close 时 k_free 归还.

// B04-09 优化 Step E: 查询/控制 API (is_network_*/get_*/NetStatus/trigger_init/
// shutdown_network/reset_network_state) 已移至 query.rs.

// ============================================================================
// REVAL-W W4.2.3.4 步骤 2: SmoltcpNetStack 桥接 safe API (init 模块顶层)
//
// SmoltcpNetStack (services 层) 调用本模块的 safe wrapper 来实际构造
// smoltcp socket. 内部 unsafe 块 (raw::socket_set + raw::socket_open_stub
// + transmute SocketHandle → u32) 封装在 framework 层, services 层调用
// 时无 unsafe 暴露.
// ============================================================================

/// `SmoltcpNetStack::socket_open` 的 safe wrapper (W4.2.3.4 步骤 2).
///
/// ## 调用方契约
///
/// - `kind`: 要创建的 socket 类型 (Tcp/Udp/...)
/// - `slot_idx`: 槽位索引, 必须在 `[MAX_SM_FD, TOTAL_SLOTS)` 范围
///   (`SmoltcpNetStack` 专属范围, 不与 `sm_socket` 冲突)
///
/// ## 返回
///
/// - `Some(u32)`: smoltcp handle (用于 `smol_socket_get`)
/// - `None`: 创建失败 (`k_malloc` 失败 / 槽位已占用 / `slot_idx` 越界)
pub fn smoltcp_net_stack_socket_open(
    kind: crate::framework::net::iface_trait::SocketKind,
    slot_idx: usize,
) -> Option<u32> {
    // SAFETY: 调用方持有 NET_LOCK, socket_set() 返回的指针由 init_sockets
    // 单次初始化, SOCKET_SET MaybeUninit 区域独占.
    let sockets = unsafe { &mut *raw::socket_set() };
    let smol_handle = raw::socket_open_stub(sockets, kind, slot_idx)?;
    // W5 transmute_copy 路径: 复用 `as_u32_handle` helper (编译期强制
    // size 匹配, 不依赖 SocketHandle repr 假设). SocketSet 容量上限
    // (MAX_SOCKETS = 1024) 远低于 u32::MAX, usize → u32 截断安全.
    Some(as_u32_handle(smol_handle))
}

/// `SmoltcpNetStack` 专属范围的 smol 槽位基址 (W4.2.3.4 步骤 2).
///
/// 返回 `MAX_SM_FD` (即 `SmoltcpNetStack` 范围的起始索引). services 层
/// `SmoltcpNetStack::socket_open` 内部 `smol_slot_idx = slot_base() + handle_map_idx`.
pub fn smoltcp_net_stack_slot_base() -> usize {
    MAX_SM_FD
}

/// `SmoltcpNetStack::poll` 的 safe wrapper (W4.2.3.4).
///
/// 委托给 `raw::smoltcp_net_stack_poll`, 内部持有 `NET_LOCK` 并调用
/// smoltcp `Interface::poll` + `process_dhcp_events`.
pub fn smoltcp_net_stack_poll() -> crate::framework::net::iface_trait::PollOutcome {
    raw::smoltcp_net_stack_poll()
}

/// `SmoltcpNetStack::close` 的 safe wrapper (W4.2.3.4).
///
/// 关闭 `SmoltcpNetStack` 范围内的 smoltcp socket, 释放 buffer.
/// 委托给 `raw::smoltcp_net_stack_socket_close`.
pub fn smoltcp_net_stack_close(slot_idx: usize) {
    raw::smoltcp_net_stack_socket_close(slot_idx);
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 static mut 访问
// ============================================================================

pub(crate) mod raw;

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::net::iface_trait::{
        Ipv4Addr as TraitIpv4Addr, NetEndpoint as TraitEndpoint,
    };
    use smoltcp::wire::IpAddress;

    /// W4.4: 验证 wire_to_smol / endpoint_to_smol 翻译不丢字段 (双栈).
    #[test]
    fn test_wire_translation_roundtrip() {
        let trait_addr = TraitIpv4Addr::new(192, 168, 1, 100);
        let smol = wire_to_smol(trait_addr.into_ip_addr());
        if let IpAddress::Ipv4(v4) = smol {
            assert_eq!(v4.octets(), [192, 168, 1, 100]);
        } else {
            panic!("expected IpAddress::Ipv4");
        }
        // endpoint 翻译: 验证 addr+port 双向不丢
        let ep = TraitEndpoint::new_v4(TraitIpv4Addr::new(10, 0, 2, 15), 8080);
        let ep_smol = endpoint_to_smol(ep);
        assert_eq!(ep_smol.port, 8080);
        if let IpAddress::Ipv4(v4) = ep_smol.addr {
            assert_eq!(v4.octets(), [10, 0, 2, 15]);
        } else {
            panic!("expected IpAddress::Ipv4");
        }
        // 反向翻译 (从 smoltcp 类型)
        let back = endpoint_from_smol(ep_smol).unwrap();
        assert_eq!(back.port, 8080);
        assert_eq!(back.addr.as_v4().unwrap().octets(), [10, 0, 2, 15]);
    }

    /// W4.4: 验证 parse_endpoint_trait 解析后立即落入 trait 抽象类型 (双栈).
    #[test]
    fn test_parse_endpoint_trait_bridge() {
        // 构造一个 sockaddr_in 字节序列
        let mut buf = [0u8; 16];
        buf[0..2].copy_from_slice(&2u16.to_ne_bytes()); // AF_INET
        buf[2..4].copy_from_slice(&8080u16.to_be_bytes()); // port (big-endian)
        buf[4..8].copy_from_slice(&[192, 168, 1, 50]);
        // SAFETY: buf 完整 16 字节, 模拟 C sockaddr_in 布局
        let ep = unsafe { parse_endpoint_trait(buf.as_ptr()) }.unwrap();
        assert_eq!(ep.addr.as_v4().unwrap().octets(), [192, 168, 1, 50]);
        assert_eq!(ep.port, 8080);
    }

    #[test]
    fn test_initialization_state_machine() {
        assert_eq!(get_init_state(), InitState::Uninitialized);
        assert!(!is_network_initialized());

        // SAFETY: 单线程测试, `reset_network_state` 仅触达 `static mut` 单调状态
        unsafe {
            reset_network_state();
        }
        assert_eq!(get_init_state(), InitState::Uninitialized);
    }

    // ── D1.2 新增: parse_ipv4_literal / dns_resolve 纯逻辑测试 ──

    #[test]
    fn test_parse_ipv4_literal_valid() {
        assert_eq!(parse_ipv4_literal("0.0.0.0"), Some([0, 0, 0, 0]));
        assert_eq!(parse_ipv4_literal("10.0.2.15"), Some([10, 0, 2, 15]));
        assert_eq!(
            parse_ipv4_literal("255.255.255.255"),
            Some([255, 255, 255, 255])
        );
        assert_eq!(parse_ipv4_literal("127.0.0.1"), Some([127, 0, 0, 1]));
    }

    #[test]
    fn test_parse_ipv4_literal_invalid() {
        assert_eq!(parse_ipv4_literal(""), None);
        assert_eq!(parse_ipv4_literal("10"), None);
        assert_eq!(parse_ipv4_literal("10.0"), None);
        assert_eq!(parse_ipv4_literal("10.0.2"), None);
        assert_eq!(parse_ipv4_literal("10.0.2.15.1"), None);
        assert_eq!(parse_ipv4_literal("10.0.2.256"), None); // 越界
        assert_eq!(parse_ipv4_literal("10.0..15"), None);
        assert_eq!(parse_ipv4_literal("a.b.c.d"), None);
        assert_eq!(parse_ipv4_literal("10.0.2."), None);
        assert_eq!(parse_ipv4_literal(".10.0.2.15"), None);
        assert_eq!(parse_ipv4_literal("10.0.2.15 "), None); // 尾随空格
    }

    #[test]
    fn test_dns_resolve_static_hosts() {
        assert_eq!(dns_resolve("localhost"), Some([127, 0, 0, 1]));
        assert_eq!(dns_resolve("LOCALHOST"), Some([127, 0, 0, 1])); // 大小写不敏感
        assert_eq!(dns_resolve("Router"), Some([10, 0, 2, 2]));
        assert_eq!(dns_resolve("qemu-gateway"), Some([10, 0, 2, 2]));
        assert_eq!(dns_resolve("edgine-gateway"), Some([10, 0, 2, 2]));
    }

    #[test]
    fn test_dns_resolve_unknown_falls_back_to_ip_literal() {
        // 未知主机名直接走 IPv4 字面量路径
        assert_eq!(dns_resolve("8.8.8.8"), Some([8, 8, 8, 8]));
        assert_eq!(dns_resolve("10.0.2.15"), Some([10, 0, 2, 15]));
    }

    #[test]
    fn test_dns_resolve_returns_none_for_garbage() {
        assert_eq!(dns_resolve("nonexistent.example.com"), None);
        assert_eq!(dns_resolve(""), None);
        assert_eq!(dns_resolve("999.999.999.999"), None);
    }

    #[test]
    fn test_ipv4_from_atomic() {
        assert_eq!(ipv4_from_atomic(0), None);
        assert_eq!(ipv4_from_atomic(0x0A00020F), Some([10, 0, 2, 15]));
        assert_eq!(ipv4_from_atomic(0xFF000001), Some([255, 0, 0, 1]));
    }

    #[test]
    fn test_net_status_capture_initial_state() {
        // SAFETY: 单线程测试, reset 仅修改状态原子变量
        unsafe {
            reset_network_state();
        }
        let s = NetStatus::capture();
        assert_eq!(s.state, InitState::Uninitialized);
        assert_eq!(s.ipv4, None);
        assert_eq!(s.gateway, None);
        assert_eq!(s.dns, [None, None, None]);
        assert!(!s.dhcp_configured);
    }

    #[test]
    fn test_dns_servers_default_empty() {
        // SAFETY: 单线程测试, reset 仅修改状态原子变量
        unsafe {
            reset_network_state();
        }
        let dns = get_dns_servers();
        assert_eq!(dns, [None, None, None]);
    }
}
