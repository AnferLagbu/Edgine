//! 网络初始化状态管理 (B04-09 拆分 Step B, 2026-08-25)
//!
//! 原 init.rs 内联定义: `InitState` / `G_INIT_STATE` / `G_*` 配置快照 /
//! `NetState` / `NET_STATE` / `transition_state` / `set_failed`.
//! 抽出为独立子模块后, init.rs 通过 `pub use state::*` re-export,
//! 保持 init 主体与子模块 (raw/sm_fi) 的 `super::NET_STATE` 等引用不变.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::privileged::net::{EGDFNetDevice, NetworkStack};
use crate::privileged::sync::IrqSpinLock as Mutex;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::udp;

use super::{TOTAL_SLOTS, UDP_META_COUNT};

// ============================================================================
// 初始化状态管理
// ============================================================================

/// 网络初始化状态机 — 记录从硬件探测到就绪或失败的推进阶段
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitState {
    Uninitialized = 0,
    HardwareProbed = 1,
    InterfaceReady = 2,
    FullyInitialized = 3,
    Failed = 255,
}

pub static G_INIT_STATE: AtomicU8 = AtomicU8::new(InitState::Uninitialized as u8);

// 当前网络配置快照 (D1.1/D1.2 高层 API 支撑)
// 全部为 Atomic, 单字段读写无需 NET_LOCK; 多字段一致性由 NetStatus::capture 原子复制.
// 未配置时全部 = 0; 0.0.0.0 表示"无".
// pub: 供 init.rs 主体 (NetStatus::capture 等) 与 `pub use state::*` re-export 访问.
pub static G_MAC: AtomicU64 = AtomicU64::new(0); // 6 字节大端打包为 u64
pub static G_IPV4: AtomicU32 = AtomicU32::new(0); // 网络字节序
pub static G_GATEWAY: AtomicU32 = AtomicU32::new(0); // 网络字节序
pub static G_DNS: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

// ============================================================================
// 全局网络状态 (NetState 统一结构)
//
// 原 12 个 static mut 合并为 NetState, 由 NET_STATE (IrqSpinLock) 保护。
// poll_network() 使用 try_lock() 避免在 ISR 上下文中阻塞；
// 其他函数使用 lock() 获取互斥访问。
// 所有字段访问通过 raw 模块的 accessor 函数, 保证集中 unsafe 边界。
// ============================================================================

/// 网络子系统全局状态, 集中原 12 个 static mut.
///
/// 由 `NET_STATE` (`IrqSpinLock`) 保护, 所有字段访问通过 `raw` 模块 accessor.
pub struct NetState {
    pub(crate) device: Option<EGDFNetDevice>,
    pub(crate) stack: Option<NetworkStack>,
    pub(crate) dhcp_handle: Option<SocketHandle>,
    pub(crate) socket_table: Vec<Option<SocketHandle>>,
    pub(crate) fd_types: Vec<u8>,
    pub(crate) tcp_rx_bufs: Vec<*mut u8>,
    pub(crate) tcp_tx_bufs: Vec<*mut u8>,
    pub(crate) udp_rx_bufs: Vec<*mut u8>,
    pub(crate) udp_tx_bufs: Vec<*mut u8>,
    pub(crate) udp_rx_metas: Vec<[udp::PacketMetadata; UDP_META_COUNT]>,
    pub(crate) udp_tx_metas: Vec<[udp::PacketMetadata; UDP_META_COUNT]>,
}

// SAFETY: NetState 包含 *mut u8 裸指针, 但所有指针由 k_malloc 分配、
// 在 NET_STATE (IrqSpinLock) 保护下串行访问, 无跨线程共享裸指针.
// 各字段为 Vec<T>, 满足 Send/Sync 所需的内层 T: Send 约束.
unsafe impl Send for NetState {}
unsafe impl Sync for NetState {}

impl NetState {
    /// 构造空状态: 所有 `Vec` 字段为空 (零容量, 不分配).
    ///
    /// 必须在访问字段前调用 [`Self::allocate`] 填充 `TOTAL_SLOTS` 项.
    /// 保持 `const fn` 以便 `static NET_STATE` 静态构造.
    pub const fn empty() -> Self {
        Self {
            device: None,
            stack: None,
            dhcp_handle: None,
            socket_table: Vec::new(),
            fd_types: Vec::new(),
            tcp_rx_bufs: Vec::new(),
            tcp_tx_bufs: Vec::new(),
            udp_rx_bufs: Vec::new(),
            udp_tx_bufs: Vec::new(),
            udp_rx_metas: Vec::new(),
            udp_tx_metas: Vec::new(),
        }
    }

    /// 逐元素填充 `TOTAL_SLOTS` 项 (避免构造大栈临时数组).
    ///
    /// 每次网络 (重) 初始化时调用, 以 `Vec` 覆盖旧内容实现复位.
    /// 调用方须持有 `NET_STATE` 锁.
    pub fn allocate(&mut self) {
        self.socket_table = (0..TOTAL_SLOTS).map(|_| None).collect();
        self.fd_types = (0..TOTAL_SLOTS).map(|_| 0u8).collect();
        self.tcp_rx_bufs = (0..TOTAL_SLOTS).map(|_| core::ptr::null_mut()).collect();
        self.tcp_tx_bufs = (0..TOTAL_SLOTS).map(|_| core::ptr::null_mut()).collect();
        self.udp_rx_bufs = (0..TOTAL_SLOTS).map(|_| core::ptr::null_mut()).collect();
        self.udp_tx_bufs = (0..TOTAL_SLOTS).map(|_| core::ptr::null_mut()).collect();
        self.udp_rx_metas = (0..TOTAL_SLOTS)
            .map(|_| [udp::PacketMetadata::EMPTY; UDP_META_COUNT])
            .collect();
        self.udp_tx_metas = (0..TOTAL_SLOTS)
            .map(|_| [udp::PacketMetadata::EMPTY; UDP_META_COUNT])
            .collect();
    }
}

/// 全局网络状态, `IrqSpinLock` 保护 (替代原 `NET_LOCK` + 12 static mut).
/// `poll_network` 使用 `try_lock()` 避免 ISR 上下文阻塞.
pub static NET_STATE: Mutex<NetState> = Mutex::new(NetState::empty());

// ============================================================================
// 辅助函数
// ============================================================================

/// 状态机推进 — 以 CAS 将当前状态从 `from` 迁移到 `to`
///
/// 返回 `Err(())` 表示迁移被拒绝 (CAS 竞争失败、非法回退或已处于 `Failed` 终态)。
#[expect(
    clippy::missing_errors_doc,
    reason = "missing_errors_doc: transition_state 返回 Result<(), ()>; Err 仅表示状态转换被拒绝 (compare_exchange 竞争或非法迁移), 非结构化错误, 语义由调用方按 InitState 自行判定, 当前优先 expect 兑底"
)]
pub fn transition_state(from: InitState, to: InitState) -> Result<(), ()> {
    match G_INIT_STATE.compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Relaxed) {
        Ok(_) => Ok(()),
        Err(current) => {
            if current == InitState::Failed as u8 {
                Err(())
            } else if current >= to as u8 {
                Ok(())
            } else {
                Err(())
            }
        }
    }
}

/// 将状态机置为 `Failed` 终态 (后续不再重试)
pub fn set_failed() {
    G_INIT_STATE.store(InitState::Failed as u8, Ordering::Release);
}
