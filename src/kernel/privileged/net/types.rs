//! 网络子系统公共类型 — privileged 机制常量与全局状态
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量与全局状态于 T6-9 (2026-06-16) 迁至 `functions::net::types`, 本文件仅
//! re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：
//! `FALLBACK_*` 常量被 privileged net init/dns 机制直接消费、`NET_READY`/
//! `NET_CONFIGURED` 为机制持有的全局状态 — 属机制项, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::net::types::*` 保持 API 兼容。
//! 本文件 0 unsafe (纯常量 + AtomicBool 全局状态).

use core::sync::atomic::AtomicBool;

/// 网络子系统公共状态 (smoltcp 状态机共享)
///
/// - `NET_READY`     : 协议栈已就绪 (`eg_net_init` 完成, 可收发原始帧)
/// - `NET_CONFIGURED`: 已配置 IP (DHCP 完成或静态 IP 已设置)
pub static NET_READY: AtomicBool = AtomicBool::new(false);

/// 网络已配置 IP (DHCP 完成或静态 IP 设置)
pub static NET_CONFIGURED: AtomicBool = AtomicBool::new(false);

// ============================================================================
// I-46: DHCP 失败时的 fallback 静态 IP 配置 — 集中常量, 避免散落硬编码
// ============================================================================
//
// 语义: QEMU user-mode networking 默认子网 10.0.2.0/24 (QEMU 文档 §Using the
// user mode network). 当 DHCP discover/offer/ack 全部失败 (无 DHCP server,
// 链路断开等), 协议栈仍需一个可用的 link-local 地址, 否则路由表为空导致
// 任何 IPv4 通信都不可达. 此 fallback 适配 QEMU 默认, 真实硬件部署应
// 通过 eg_net_static_ip() 或配置覆盖.
//
// 注意: 不要在多处重复这 4 个数字, 一改全改; 引用本常量保持单一来源.
pub const FALLBACK_IPV4: [u8; 4] = [10, 0, 2, 15];
pub const FALLBACK_PREFIX: u8 = 24;
pub const FALLBACK_GATEWAY: [u8; 4] = [10, 0, 2, 2];
