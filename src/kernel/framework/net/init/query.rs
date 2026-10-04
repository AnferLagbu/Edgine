//! 网络查询/控制 API (B04-09 优化拆分 Step E, 2026-08-25)
//!
//! 原 init.rs 内联定义: `is_network_initialized` / `is_network_configured` /
//! `get_init_state` / `NetStatus` / `ipv4_from_atomic` / `trigger_init` /
//! `get_mac_address` / `get_ipv4_address` / `get_default_gateway` /
//! `get_dns_servers` / `shutdown_network` / `reset_network_state`.
//! 抽出为独立子模块后, init.rs 通过 `pub use query::*` re-export,
//! 保持 init 主体与外部调用方 (net/api.rs) 的 `init::xxx` 路径不变.

use core::sync::atomic::Ordering;

use crate::framework::net::{NET_CONFIGURED, NET_READY};

use super::raw;
use super::sockets::SOCKETS_INITIALIZED;
use super::state::{G_DNS, G_GATEWAY, G_INIT_STATE, G_IPV4, G_MAC, InitState, NET_STATE};

// ============================================================================
// 网络状态查询
// ============================================================================

pub fn is_network_initialized() -> bool {
    NET_READY.load(Ordering::Acquire)
}

pub fn is_network_configured() -> bool {
    NET_CONFIGURED.load(Ordering::Acquire)
}

pub fn get_init_state() -> InitState {
    match G_INIT_STATE.load(Ordering::Acquire) {
        0 => InitState::Uninitialized,
        1 => InitState::HardwareProbed,
        2 => InitState::InterfaceReady,
        3 => InitState::FullyInitialized,
        _ => InitState::Failed,
    }
}

// ============================================================================
// D1.1/D1.2 高层 API 底层实现
// ============================================================================

/// 网络状态快照 (单次原子读, 多字段可能轻微不一致 — 用于观测/debug)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetStatus {
    pub state: InitState,
    pub mac: [u8; 6],
    pub ipv4: Option<[u8; 4]>,
    pub gateway: Option<[u8; 4]>,
    pub dns: [Option<[u8; 4]>; 3],
    pub dhcp_configured: bool,
}

impl NetStatus {
    pub fn capture() -> Self {
        let mac_raw = G_MAC.load(Ordering::Acquire);
        let mac = mac_raw.to_be_bytes()[2..8].try_into().unwrap_or([0; 6]);
        let ipv4 = ipv4_from_atomic(G_IPV4.load(Ordering::Acquire));
        let gateway = ipv4_from_atomic(G_GATEWAY.load(Ordering::Acquire));
        let dns = [
            ipv4_from_atomic(G_DNS[0].load(Ordering::Acquire)),
            ipv4_from_atomic(G_DNS[1].load(Ordering::Acquire)),
            ipv4_from_atomic(G_DNS[2].load(Ordering::Acquire)),
        ];
        Self {
            state: get_init_state(),
            mac,
            ipv4,
            gateway,
            dns,
            dhcp_configured: NET_CONFIGURED.load(Ordering::Acquire),
        }
    }
}

/// u32 原子量 -> IPv4 点分字节 (0 视为未配置)
///
/// B04-09 拆分后本函数归入 query 子模块; 单元测试位于父模块 `init` (经
/// `pub use query::*` 引入), 故可见性取 `pub(crate)` 而非私有.
pub(crate) fn ipv4_from_atomic(v: u32) -> Option<[u8; 4]> {
    if v == 0 { None } else { Some(v.to_be_bytes()) }
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
/// 主动触发网络初始化 (非阻塞; 失败返回 false)
///
/// # 行为
/// - 状态机 = Uninitialized 时, 直接返回 false (需要先有 egdf 设备注册)
/// - 状态机 = HardwareProbed/InterfaceReady 时, 启动 DHCP 握手
/// - 状态机 = `FullyInitialized` 时, 直接返回 true
/// - 状态机 = Failed 时, 不重试, 返回 false
pub fn trigger_init() -> bool {
    match get_init_state() {
        InitState::FullyInitialized => true,
        InitState::HardwareProbed | InitState::InterfaceReady => {
            // DHCP 已经在轮询路径里跑了, 此处仅给上层一个"我已确认"信号
            true
        }
        _ => false,
    }
}

/// 查询设备 MAC 地址
pub fn get_mac_address() -> Option<[u8; 6]> {
    let raw = G_MAC.load(Ordering::Acquire);
    if raw == 0 {
        None
    } else {
        let bytes = raw.to_be_bytes();
        Some([bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]])
    }
}

/// 查询当前 IPv4
pub fn get_ipv4_address() -> Option<[u8; 4]> {
    ipv4_from_atomic(G_IPV4.load(Ordering::Acquire))
}

/// 查询默认网关
pub fn get_default_gateway() -> Option<[u8; 4]> {
    ipv4_from_atomic(G_GATEWAY.load(Ordering::Acquire))
}

/// 查询 DNS 服务器列表
pub fn get_dns_servers() -> [Option<[u8; 4]>; 3] {
    [
        ipv4_from_atomic(G_DNS[0].load(Ordering::Acquire)),
        ipv4_from_atomic(G_DNS[1].load(Ordering::Acquire)),
        ipv4_from_atomic(G_DNS[2].load(Ordering::Acquire)),
    ]
}

/// 显式关闭网络栈 (重置配置 + 状态)
pub fn shutdown_network() {
    let _guard = NET_STATE.lock();
    G_IPV4.store(0, Ordering::Release);
    G_GATEWAY.store(0, Ordering::Release);
    G_DNS[0].store(0, Ordering::Release);
    G_DNS[1].store(0, Ordering::Release);
    G_DNS[2].store(0, Ordering::Release);
    NET_CONFIGURED.store(false, Ordering::Release);
    G_INIT_STATE.store(InitState::Uninitialized as u8, Ordering::Release);
    raw::klog_msg("Network shutdown");
}

/// 重置网络栈状态 (供FREG BHR / 异常恢复使用)。
///
/// # Safety
/// - 必须持有 `NET_LOCK` (内部获取)。
/// - 必须在所有 socket 关闭后调用, 否则可能泄漏资源。
pub unsafe fn reset_network_state() {
    let _guard = NET_STATE.lock();

    G_INIT_STATE.store(InitState::Uninitialized as u8, Ordering::Release);

    raw::clear_all();
    SOCKETS_INITIALIZED.store(false, Ordering::Release);
}
