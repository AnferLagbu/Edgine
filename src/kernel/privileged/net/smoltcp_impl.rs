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

use smoltcp::iface::{Config, Interface, PollResult, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress};

use crate::privileged::egdf::NetOps;
use crate::privileged::timer::get_uptime_ms;
use crate::privileged::timer::hrtimer_clock_read;

const RX_BUF_SIZE: usize = 2048;
const TX_BUF_SIZE: usize = 2048;

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
        self.ops
            .send(self.driver_data as *mut u8, &self.tx_buf[..len]);
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
    let config = Config::new(HardwareAddress::Ethernet(EthernetAddress::from_bytes(&mac)));
    let iface = Interface::new(config, device, smoltcp_now());
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
