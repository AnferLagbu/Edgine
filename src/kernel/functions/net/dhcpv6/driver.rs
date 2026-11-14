#![deny(unsafe_code)]
#![deny(missing_docs)]
//! `DHCPv6` 客户端驱动 (P6b Phase 4): functions 侧 IRQ 安全静态 + 反转钩子.
//!
//! 持有 [`Dhcpv6Client`] 状态机 (Phase 3), 由 privileged `poll_network` 经擦除
//! 钩子 [`poll_hook`] 在 `NET_STATE` 锁内驱动. 客户端锁 `DHCPV6_CLIENT` 为
//! `IrqSpinLock` (ISR 上下文安全), 与 `NET_STATE` 同属锁层级 12 (同层取锁允许).
//! 启动 (`start_stateful` / `start_stateless`) 置 pending 标志, 由下一次 poll
//! 钩子消费 (此时才有 ctx 执行发包). Phase 5: poll 钩子另经 raw `ICMPv6` 捕获 RA,
//! 纯字节解析 (`ra`) 提取 M/O 标志, 仅 `Idle` 时触发有状态 (M=1) / 无状态 (O=1).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::functions::net::dhcpv6::policy::Dhcpv6PolicyConfig;
use crate::functions::net::dhcpv6::ra;
use crate::functions::net::dhcpv6::state::{Dest, Dhcpv6Client, Output};
use crate::privileged::net::iface_trait::Dhcpv6State;
use crate::privileged::net::init::dhcpv6::{ALL_DHCPV6_MULTICAST, Dhcpv6PollCtx};
use crate::privileged::sync::IrqSpinLock;

/// 单接口 IAID (RFC 8415 §12: 每接口唯一; 本内核单网卡, 固定 1).
const IAID: u32 = 1;
/// 收报文栈缓冲字节数 (与 privileged UDP 缓冲同量级, 覆盖典型 `DHCPv6` 报文).
const RECV_BUF_SIZE: usize = 2048;
/// RA 捕获栈缓冲字节数 (与 privileged raw `ICMPv6` 缓冲同量级, 覆盖标准 MTU).
const RA_RECV_BUF_SIZE: usize = 1500;

/// 启动模式.
#[derive(Clone, Copy)]
enum Mode {
    /// 有状态 `IA_NA` 地址分配 (RA M=1).
    Stateful,
    /// 无状态 Information-Request (RA O=1).
    Stateless,
}

/// 驱动状态: 客户端实例 + 待消费启动标志.
struct Dhcpv6Driver {
    client: Option<Dhcpv6Client>,
    pending_start: Option<Mode>,
}

/// 全局 `DHCPv6` 客户端 (IRQ 安全; 由 poll 钩子在 `NET_STATE` 锁内驱动).
static DHCPV6_CLIENT: IrqSpinLock<Dhcpv6Driver> = IrqSpinLock::named_lazy(
    "DHCPV6_CLIENT",
    Dhcpv6Driver {
        client: None,
        pending_start: None,
    },
);

/// 绑定里程碑一次性观测标志 (仿 SLAAC `SLAAC_OBSERVED`: 首次落地址即上报,
/// 续约刷新与后续 tick 不重复打印).
static DHCPV6_BOUND_OBSERVED: AtomicBool = AtomicBool::new(false);

/// 由 MAC 派生 `DUID-LL` (RFC 8415 §11.2: type=3, hwtype=1 以太网, + 6 字节 MAC).
fn duid_ll(mac: [u8; 6]) -> Vec<u8> {
    let mut duid = Vec::with_capacity(10);
    duid.extend_from_slice(&3u16.to_be_bytes());
    duid.extend_from_slice(&1u16.to_be_bytes());
    duid.extend_from_slice(&mac);
    duid
}

/// 由单调时钟低 24 位派生初始事务标识 (privileged 供给熵).
fn txid_from(now_ms: u64) -> [u8; 3] {
    let bytes = now_ms.to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

/// 将 16 字节 IPv6 地址格式化为全展开十六进制组 (`2001:db8:0:0:0:0:0:1`).
///
/// 仅用于绑定里程碑日志, 不压缩零段 (保持与落表地址逐字节对应, 便于 e2e 核对).
fn fmt_ipv6(a: [u8; 16]) -> String {
    let mut s = String::new();
    for i in 0..8 {
        if i > 0 {
            s.push(':');
        }
        let _ = write!(s, "{:x}", u16::from_be_bytes([a[i * 2], a[i * 2 + 1]]));
    }
    s
}

/// 执行状态机输出: 发包 + 地址落表.
fn execute(ctx: &mut Dhcpv6PollCtx<'_>, out: &Output) {
    for pkt in &out.tx_packets {
        let dest = match pkt.dest {
            Dest::Multicast => ALL_DHCPV6_MULTICAST,
            Dest::Unicast(a) => a,
        };
        ctx.send(&pkt.bytes, dest);
    }
    if let Some(addr) = &out.apply_addr {
        ctx.apply_addr(addr.ipv6, addr.remove);
        // 绑定里程碑 (仿 SLAAC 一次性观测): 首次安装地址即上报, 续约刷新不重复.
        if !addr.remove && !DHCPV6_BOUND_OBSERVED.swap(true, Ordering::Relaxed) {
            crate::privileged::klog_info!(
                Net,
                "DHCPv6 bound: {} lease={}ms",
                fmt_ipv6(addr.ipv6),
                u64::from(addr.valid_s) * 1000
            );
        }
    }
    // DNS 双栈 (docs/plan/dns-dual-stack.md Phase 1): 消费 DHCPv6 取得的 IPv6
    // DNS 服务器, 经 ctx 合并喂入 smoltcp dns::Socket (IPv6 优先, D3).
    if let Some(opts) = &out.apply_options {
        ctx.apply_dns6(&opts.dns_servers);
    }
    // SIMPLIFIED: apply_options.refresh_s (无状态信息刷新间隔) 暂不落配置;
    //   影响面: Information-request 的周期刷新间隔不被消费, DNS 服务器注入与
    //   地址落表不受影响; 何时扩展: 引入无状态周期刷新调度时消费 refresh_s.
}

/// privileged poll 钩子: 在 `NET_STATE` 锁内驱动 `DHCPv6` 客户端.
///
/// 惰性构造客户端 (首次 poll 时从 ctx 取 MAC / 时钟), 消费 pending 启动标志,
/// 捕获 RA 按 M/O 触发启动, 收取报文喂状态机, 推进定时器, 执行输出指令.
pub fn poll_hook(ctx: &mut Dhcpv6PollCtx<'_>) {
    let mut guard = DHCPV6_CLIENT.lock();
    let now_ms = ctx.now_ms();
    if guard.client.is_none() {
        guard.client = Some(Dhcpv6Client::new(
            duid_ll(ctx.mac()),
            IAID,
            txid_from(now_ms),
            Dhcpv6PolicyConfig::default(),
        ));
    }
    let pending = guard.pending_start.take();
    let Some(client) = guard.client.as_mut() else {
        return;
    };
    if let Some(mode) = pending {
        let out = match mode {
            Mode::Stateful => client.start_stateful(now_ms),
            Mode::Stateless => client.start_stateless(now_ms),
        };
        execute(ctx, &out);
    }
    // P6b Phase 5: RA M/O 触发 (raw ICMPv6 捕获 + 纯字节解析).
    // 仅 Idle 时触发, 避免周期性 RA 重启已启动客户端; M=1 优先于 O=1.
    let mut ra_buf = [0u8; RA_RECV_BUF_SIZE];
    while let Some(n) = ctx.recv_ra(&mut ra_buf) {
        let Some(trigger) = ra::parse(&ra_buf[..n]) else {
            continue;
        };
        if !matches!(client.state(), Dhcpv6State::Idle) {
            continue;
        }
        let out = if trigger.managed {
            client.start_stateful(now_ms)
        } else if trigger.other {
            client.start_stateless(now_ms)
        } else {
            continue;
        };
        execute(ctx, &out);
    }
    let mut buf = [0u8; RECV_BUF_SIZE];
    while let Some((n, src)) = ctx.recv(&mut buf) {
        let out = client.handle_packet(&buf[..n], src, now_ms);
        execute(ctx, &out);
    }
    let out = client.tick(now_ms);
    execute(ctx, &out);
}

/// 启动有状态 `DHCPv6` (`IA_NA` 地址分配; RA M=1 触发).
///
/// 置 pending 标志, 由下一次 poll 钩子发 Solicit. 进程上下文调用.
pub fn start_stateful() {
    DHCPV6_CLIENT.lock().pending_start = Some(Mode::Stateful);
}

/// 启动无状态 `DHCPv6` (Information-Request; RA O=1 触发).
///
/// 置 pending 标志, 由下一次 poll 钩子发 Information-Request. 进程上下文调用.
pub fn start_stateless() {
    DHCPV6_CLIENT.lock().pending_start = Some(Mode::Stateless);
}

/// 当前 `DHCPv6` 客户端观察态 (未启动返回 `Idle`).
#[must_use]
pub fn state() -> Dhcpv6State {
    DHCPV6_CLIENT
        .lock()
        .client
        .as_ref()
        .map_or(Dhcpv6State::Idle, Dhcpv6Client::state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duid_ll_rfc8415_format() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let duid = duid_ll(mac);
        assert_eq!(duid.len(), 10);
        assert_eq!(&duid[0..2], &3u16.to_be_bytes());
        assert_eq!(&duid[2..4], &1u16.to_be_bytes());
        assert_eq!(&duid[4..10], &mac);
    }

    #[test]
    fn txid_takes_low_24_bits() {
        let txid = txid_from(0x0011_2233_4455_6677);
        assert_eq!(txid, [0x77, 0x66, 0x55]);
    }

    #[test]
    fn txid_zero() {
        assert_eq!(txid_from(0), [0, 0, 0]);
    }

    #[test]
    fn fmt_ipv6_full_form() {
        let a = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(fmt_ipv6(a), "2001:db8:0:0:0:0:0:1");
    }

    #[test]
    fn fmt_ipv6_all_zero() {
        assert_eq!(fmt_ipv6([0u8; 16]), "0:0:0:0:0:0:0:0");
    }

    #[test]
    fn fmt_ipv6_link_local() {
        let a = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(fmt_ipv6(a), "fe80:0:0:0:0:0:0:1");
    }
}
