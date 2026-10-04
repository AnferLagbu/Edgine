//! 网络配置入口 (B04-09 优化拆分 Step G, 2026-08-25)
//!
//! 原 init.rs 内联定义: `eg_net_start_dhcp` / `eg_net_static_ip`.
//! 抽出为独立子模块后, init.rs 主体与外部调用方 (FFI) 经
//! `pub use cmd::*` re-export 保持 `init::eg_net_*` 路径不变.
//!
//! ## CIDR 解析复用 (2026-08-25)
//!
//! `eg_net_static_ip` 原内联手写 "a.b.c.d/prefix" 与网关解析, 与
//! `dns.rs::parse_ipv4_literal` 逻辑重复. 本次重构复用 `dns::` 解析:
//! - CIDR 部分: `parse_cidr` (新增, 支持可选 /prefix, 默认 24)
//! - 网关部分: `parse_ipv4_literal` (dns.rs 既有)
//! 消除两套并行的 IPv4 文本解析实现.

use core::sync::atomic::Ordering;

use smoltcp::wire::IpCidr;

use crate::privileged::net::{NET_CONFIGURED, NET_READY};

use super::dns::{parse_cidr, parse_ipv4_literal};
use super::poll_network;
use super::raw;
use super::state::NET_STATE;

/// 启动 DHCP (异步, 由 timer ISR 驱动 poll 完成)
///
/// 调用后 DHCP Discover 会在下一个 timer tick 发出。
/// 用户态通过 poll/select 或轮询 `NET_CONFIGURED` 等待完成。
///
/// # Safety
/// 调用方保证 NET 已初始化 (通过 `eg_net_init` 注册)，
/// `NET_READY` 由网络栈在链路就绪后置位。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eg_net_start_dhcp() -> i32 {
    unsafe {
        if !NET_READY.load(Ordering::Acquire) {
            return -1;
        }
        poll_network();
        0
    }
}

/// 设置静态 IP (x.x.x.x/prefix, gateway)
///
/// 格式: "10.0.2.15/24,10.0.2.2"
/// 返回 0 成功, -1 失败
///
/// # Safety
/// - `cidr_str` 与 `gw_str` 必须是有效的 C 字符串指针 (NUL 终止),
///   指向的内存必须在调用期间保持有效。
/// - 调用方保证 NET 已初始化。
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: eg_net_static_ip 内 5 处 `match Option { Some(v)=>v, None=>return -1 }` 用于 FFI 参数 (cidr/gw) 解析的提前返回; 保持 match-return 结构以最小化 diff, 当前优先 expect 兑底"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eg_net_static_ip(cidr_str: *const u8, gw_str: *const u8) -> i32 {
    unsafe {
        if !NET_READY.load(Ordering::Acquire) {
            return -1;
        }

        let _guard = NET_STATE.lock();

        let stack = match raw::stack_mut() {
            Some(s) => s,
            None => return -1,
        };

        // 解析 CIDR 字符串 "a.b.c.d/prefix" (复用 dns::parse_cidr)
        let cidr_str = match cstr_to_str(cidr_str) {
            Some(s) => s,
            None => return -1,
        };
        let (ip_octets, prefix) = match parse_cidr(cidr_str) {
            Some(v) => v,
            None => return -1,
        };
        let ip =
            smoltcp::wire::Ipv4Address::new(ip_octets[0], ip_octets[1], ip_octets[2], ip_octets[3]);

        // 解析网关 (复用 dns::parse_ipv4_literal)
        let gw_str = match cstr_to_str(gw_str) {
            Some(s) => s,
            None => return -1,
        };
        let gw_octets = match parse_ipv4_literal(gw_str) {
            Some(v) => v,
            None => return -1,
        };
        let gw =
            smoltcp::wire::Ipv4Address::new(gw_octets[0], gw_octets[1], gw_octets[2], gw_octets[3]);

        let cidr = IpCidr::Ipv4(smoltcp::wire::Ipv4Cidr::new(ip, prefix));
        stack.iface.update_ip_addrs(|addrs| {
            addrs.clear();
            let _ = addrs.push(cidr);
        });
        let _ = stack.iface.routes_mut().add_default_ipv4_route(gw);

        NET_CONFIGURED.store(true, Ordering::Release);

        raw::klog_msg("Static IP configured");
        0
    }
}

/// 将 NUL 终止的 C 字符串转为 `&str` (非 UTF-8 返回 None)。
///
/// # Safety
/// `ptr` 必须是有效的 NUL 终止字符串指针, 调用期间内存有效。
///
/// 复用 `core::str::from_utf8` 避免手写循环; 非法字节序列返回 None
/// (与旧实现逐字节仅接受 ASCII 数字/点/斜杠的行为一致 — 非 ASCII 输入
/// 都会被拒绝)。
unsafe fn cstr_to_str(ptr: *const u8) -> Option<&'static str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: 调用方保证 NUL 终止; 扫描长度无上界约束 (FFI 契约)
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: ptr..ptr+len 是有效内存区间 (调用方保证), 且已被扫描确认
    let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
    core::str::from_utf8(slice).ok()
}
