//! smoltcp 网络协议栈集成模块
//!
//! 实现 smoltcp 的 `Device` trait, 通过 EGDF `NetOps` 驱动任意网卡。
//! 不依赖具体驱动类型 (E1000 / Virtio-Net)。
//!
//! ## 架构
//!
//! ```text
//! smoltcp Interface
//! └── phy::Device ── EGDFNetDevice
//!     └── NetOps (send / try_receive / get_mac / handle_irq)
//!         └── EGDF device registry
//!             ├── e1000
//!             └── virtio-net
//! ```

use core::sync::atomic::{AtomicU64, Ordering};

use crate::slog_warn;
use smoltcp::iface::{Config, Interface, PollResult, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv6Cidr};

use crate::privileged::egdf::NetOps;
use crate::privileged::timer::get_uptime_ms;
use crate::privileged::timer::hrtimer_clock_read;

const RX_BUF_SIZE: usize = 2048;
const TX_BUF_SIZE: usize = 2048;

/// S-2 (docs/plan/net-e2e-tcp-echo-flakiness.md §3): 驱动发送失败的累计掉帧数。
///
/// `TxToken::consume` 原先直接丢弃 `NetOps::send` 的返回值 —— 驱动返 -1 时帧
/// 静默消失, 事后无从判断是否发生过 (smoltcp 侧永远认为已发出). 现在失败即计数
/// 并限频上报到 Net 日志, 使"是否掉过帧"成为 e2e 排查可直接判读的事实。
static TX_DROPPED: AtomicU64 = AtomicU64::new(0);

/// 掉帧日志的限频步长: 首次必报, 其后每 N 次一条, 避免故障态下的日志风暴。
const TX_DROP_LOG_STRIDE: u64 = 32;

// P1-I-50: 网络时钟优先用 hrtimer (纳秒), 未校准时回退到 ms 上报给 smoltcp.
// 这样 TCP RTT/retransmit/dhcp 计时精度从 ms 级提升到 μs 级, 真实网络超时
// (RST/ARP 老化) 与 TCP keepalive 立即受益. smoltcp::time::Instant 内部
// 表示为 i64 毫秒, 因此 ns/1_000_000 后截断精度与原 tick 路径相同,
// 但在校准时窗口内 (校准完成后) 纳秒级抖动被吸收, 不再受 tick 节流.
fn smoltcp_now() -> Instant {
    // 校准后: ns → ms, 抖动 < 1ms; 校准前: 直接 ms, 行为不变.
    let ns = hrtimer_clock_read();
    let ms = ns / 1_000_000;
    if ms > i64::MAX as u64 {
        return Instant::from_millis(get_uptime_ms() as i64);
    }
    Instant::from_millis(ms as i64)
}

// ============================================================================
// EGDFNetDevice — 通过 EGDF NetOps 驱动任意网卡
// ============================================================================

/// smoltcp `Device` 实现 — 通过 EGDF `NetOps` 驱动任意网卡 (E1000/Virtio-Net)
pub struct EGDFNetDevice {
    ops: &'static NetOps,
    driver_data: *mut core::ffi::c_void,
    pub mac: [u8; 6],
    rx_buf: [u8; RX_BUF_SIZE],
    rx_len: usize,
    tx_buf: [u8; TX_BUF_SIZE],
}

/// smoltcp 接收令牌 — 持有从网卡收到的报文字节切片
pub struct EGDFRxToken<'a> {
    buf: &'a [u8],
}

/// smoltcp 发送令牌 — 持有发送缓冲区并回写至网卡
pub struct EGDFTxToken<'a> {
    tx_buf: &'a mut [u8],
    ops: &'static NetOps,
    driver_data: *mut core::ffi::c_void,
}

impl EGDFNetDevice {
    pub fn new(ops: &'static NetOps, driver_data: *mut core::ffi::c_void, mac: [u8; 6]) -> Self {
        Self {
            ops,
            driver_data,
            mac,
            rx_buf: [0u8; RX_BUF_SIZE],
            rx_len: 0,
            tx_buf: [0u8; TX_BUF_SIZE],
        }
    }
}

// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe impl Send for EGDFNetDevice {}
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe impl Sync for EGDFNetDevice {}

impl Device for EGDFNetDevice {
    type RxToken<'a>
        = EGDFRxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = EGDFTxToken<'a>
    where
        Self: 'a;

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let n = self
            .ops
            .try_receive(self.driver_data as *mut u8, &mut self.rx_buf);
        if n <= 0 {
            return None;
        }
        self.rx_len = n as usize;
        let rx = EGDFRxToken {
            buf: &self.rx_buf[..self.rx_len],
        };
        let tx = EGDFTxToken {
            tx_buf: &mut self.tx_buf[..],
            ops: self.ops,
            driver_data: self.driver_data,
        };
        Some((rx, tx))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(EGDFTxToken {
            tx_buf: &mut self.tx_buf[..],
            ops: self.ops,
            driver_data: self.driver_data,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.max_burst_size = Some(64);
        caps.medium = Medium::Ethernet;
        caps
    }
}

impl RxToken for EGDFRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.buf)
    }
}

impl TxToken for EGDFTxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let result = f(&mut self.tx_buf[..len]);
        // 驱动侧失败不改变对 smoltcp 的"已消费"语义 (Device trait 无失败回报通道),
        // 但必须留下可观测痕迹: 计数 + 限频告警, 见 `TX_DROPPED` 文档。
        if self
            .ops
            .send(self.driver_data as *mut u8, &self.tx_buf[..len])
            < 0
        {
            let total = TX_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
            if total == 1 || total.is_multiple_of(TX_DROP_LOG_STRIDE) {
                slog_warn!(
                    Net,
                    "[net] TX 掉帧: 驱动发送失败, 累计 {} 帧 (本帧 {} 字节)",
                    total,
                    len
                );
            }
        }
        result
    }
}

// ============================================================================
// smoltcp 网络栈管理
// ============================================================================

/// smoltcp 网络栈句柄 — 封装 Interface 与 MAC/初始化标志
pub struct NetworkStack {
    pub iface: Interface,
    pub mac: [u8; 6],
    pub initialized: bool,
}

impl NetworkStack {
    pub fn poll<D: Device>(&mut self, device: &mut D, sockets: &mut SocketSet<'_>) -> PollResult {
        self.iface.poll(smoltcp_now(), device, sockets)
    }
}

// ============================================================================
// 公共 API：统一的初始化与轮询
// ============================================================================

/// 初始化 smoltcp 网络栈 — 以给定 MAC 构造 Interface 并返回 [`NetworkStack`]
pub fn init_stack(device: &mut EGDFNetDevice, mac: [u8; 6]) -> NetworkStack {
    let hw = HardwareAddress::Ethernet(EthernetAddress::from_bytes(&mac));
    let mut config = Config::new(hw);
    // P6a: 启用 SLAAC (无状态地址自动配置), 由 smoltcp 内置实现驱动
    // (feature `proto-ipv6-slaac` 已在 Cargo.toml 启用). 开启后 `Interface::poll`
    // 会自动发 Router Solicitation / 处理 Router Advertisement, 从 RA 的前缀
    // 派生全局 IPv6 地址与默认路由, 无需 Edgine 侧额外代码.
    config.slaac = true;
    let mut iface = Interface::new(config, device, smoltcp_now());
    // P6a: 注入 IPv6 link-local 地址 (fe80::/64, 接口标识由 MAC 经 EUI-64 派生).
    // SLAAC 发 RS 时以 link-local 作源地址 (smoltcp `ndisc_rs_egress` 内
    // `link_local_ipv6_address().unwrap()` 依赖其存在), 同时 link-local 是 IPv6 邻居发现 (NDP) 的前提.
    //
    // SIMPLIFIED: link-local 仅在 init_stack 注入一次, 未在 update_ip_addrs(clear())
    //   路径 (init/cmd.rs 静态 IP 配置 / init.rs DHCP deconfigured) 之后重建;
    //   影响面: 经这些路径会移除 link-local, 致 SLAAC 停摆, 若此时仍处 RS 发现期
    //   还可能触发 smoltcp ndisc_rs_egress 的 unwrap panic; 何时需扩展: P6a 硬化
    //   时将上述 clear 路径改为保留 IPv6 地址, 或统一经 ensure 助手重建.
    let ll_prefix = Ipv6Cidr::new(Ipv6Cidr::LINK_LOCAL_PREFIX.address(), 64);
    if let Some(ll) = Ipv6Cidr::from_link_prefix(&ll_prefix, hw) {
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv6(ll));
        });
    }
    NetworkStack {
        iface,
        mac,
        initialized: true,
    }
}

/// 轮询 smoltcp 网络栈 — 推进协议状态机并处理收发
pub fn poll_stack(nic: &mut EGDFNetDevice, stack: &mut NetworkStack, sockets: &mut SocketSet<'_>) {
    stack.poll(nic, sockets);
}
