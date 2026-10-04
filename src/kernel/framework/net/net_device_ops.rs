//! NetOps 安全桥 (批次 Z ④)
//!
//! services 网络设备驱动 (0 unsafe) 无法直接构造 `NetOps` extern "C"
//! 指针表 (需裸指针转换与指针契约)。本模块提供 **安全桥 trait**
//! [`NetDeviceOps`]: 设备驱动 impl trait 后, 经 [`register_net_device`]
//! 由 framework 泛型桥 (monomorphization) 生成 extern "C" 回调并封装全部
//! unsafe 转换, 产出 [`NetDeviceRegistration`] 供 `EGDFNetDevice`
//! 接入 smoltcp。
//!
//! ## 架构
//!
//! ```text
//! services VirtioNetDriver (0 unsafe)
//! └── impl NetDeviceOps (trait, 安全方法)
//!     └── register_net_device::<T> (framework, 泛型桥)
//!         ├── net_ops_for::<T> → Box::leak(NetOps { extern "C" 回调 })
//!         │   └── unsafe 转换边界 (driver_data 裸指针 → &mut T)
//!         └── NetDeviceRegistration { ops, driver_data, mac }
//!             └── nic_probe_all → EGDFNetDevice → smoltcp
//! ```
//!
//! ## 初始化接线 (DECISION-K 单向注册契约)
//!
//! 依赖方向严格 services→framework 单向 (F2): framework 持有槽位机制
//! (`NET_SERVICES_DRIVER`, OnceLock set-once, 同 storage
//! `NVME_SERVICES_DISPATCH` 模式), services `net_init` 填充探测回调,
//! framework `nic_probe_all` 经 `net_services_driver` 单向拉取。

use alloc::boxed::Box;

use crate::framework::egdf::NetOps;
use crate::framework::sync::OnceLock;

// ============================================================================
// 安全桥 trait
// ============================================================================

/// 网络设备操作契约 (NetOps 安全桥) — services 0 unsafe impl
///
/// 每个方法对应 `NetOps` 指针表的一个槽位; framework 负责将本 trait 的
/// 方法转发为 extern "C" 回调 (unsafe 转换全部留在 framework, 见
/// [`net_ops_for`])。
pub trait NetDeviceOps: Send {
    /// 发送网络包 (data 完整以太网帧, 不含 virtio 头)。
    ///
    /// 返回 0 成功 / <0 失败。
    fn send(&mut self, data: &[u8]) -> i32;

    /// 尝试接收网络包到 buf。
    ///
    /// 返回字节数, 0 = 无数据, <0 = 错误。
    fn try_receive(&mut self, buf: &mut [u8]) -> i32;

    /// 读取 MAC 地址。
    fn get_mac(&self) -> [u8; 6];

    /// 中断处理 (默认空实现 = 轮询模式)。
    fn handle_irq(&mut self) {}
}

// ============================================================================
// 泛型桥 — monomorphization 生成具体类型的 extern "C" 回调
// ============================================================================

// SIMPLIFIED: Box::leak 一次; 设备数量稀少 (每类型 1 个), 与既有 name.leak()
// 注册模式一致; 若未来支持动态多实例需改 OnceLock<Box<NetOps>> 表
/// 为设备类型 `T` 生成 `NetOps` 指针表 (extern "C" 回调 → trait 方法)。
///
/// 返回 `&'static` (Box::leak): 设备注册数量稀少, 表随内核存续;
/// `handle_irq` 恒 `Some` (trait 默认空实现兜底, IRQ 接线统一)。
pub fn net_ops_for<T: NetDeviceOps + 'static>() -> &'static NetOps {
    Box::leak(Box::new(NetOps {
        send: net_send_impl::<T>,
        try_receive: net_recv_impl::<T>,
        get_mac: net_get_mac_impl::<T>,
        handle_irq: Some(net_irq_impl::<T>),
    }))
}

/// extern "C" send 回调: 转发至 `T: NetDeviceOps::send`。
extern "C" fn net_send_impl<T: NetDeviceOps>(
    driver_data: *mut u8,
    data: *const u8,
    len: u32,
) -> i32 {
    // null/len 守卫 (审核 P2-3): 与旧 virtio_net_send 行为等价, 防御
    // from_raw_parts 边界 (空指针/零长构造 slice 是 UB)
    if driver_data.is_null() || data.is_null() || len == 0 {
        return -1;
    }
    // SAFETY: driver_data 由 register_net_device 的 Box::into_raw 提供且存续于
    // 设备生命周期 (内核不回收); 类型由泛型桥的单调化保证 (每类型一张表)。
    let dev = unsafe { &mut *(driver_data.cast::<T>()) };
    // SAFETY: data 非空且 len 字节在调用期有效 (NetOps 契约: smoltcp 内核
    // 缓冲区 tx_buf, 由 EGDFNetDevice 持有)。
    let frame = unsafe { core::slice::from_raw_parts(data, len as usize) };
    dev.send(frame)
}

/// extern "C" try_receive 回调: 转发至 `T: NetDeviceOps::try_receive`。
extern "C" fn net_recv_impl<T: NetDeviceOps>(
    driver_data: *mut u8,
    buf: *mut u8,
    buf_len: u32,
) -> i32 {
    // null 守卫: 与旧 virtio_net_recv 行为等价 (buf_len == 0 时
    // from_raw_parts_mut 对非空指针合法, 交由设备返回 0 = 无数据)
    if driver_data.is_null() || buf.is_null() {
        return -1;
    }
    // SAFETY: driver_data 契约同 net_send_impl。
    let dev = unsafe { &mut *(driver_data.cast::<T>()) };
    // SAFETY: buf 非空且 buf_len 字节在调用期有效 (NetOps 契约: smoltcp
    // 内核缓冲区 rx_buf, 调用方 EGDFNetDevice 持有完整空间)。
    let rx = unsafe { core::slice::from_raw_parts_mut(buf, buf_len as usize) };
    dev.try_receive(rx)
}

/// extern "C" get_mac 回调: 转发至 `T: NetDeviceOps::get_mac`。
extern "C" fn net_get_mac_impl<T: NetDeviceOps>(driver_data: *mut u8, mac: *mut [u8; 6]) {
    if driver_data.is_null() {
        return;
    }
    // SAFETY: driver_data 契约同 net_send_impl; 仅取只读引用 (&self)。
    // 对齐保证 (审核 P2-4): 指针源自 Box::into_raw(Box<T>), 分配器保证
    // 按 T 对齐; 类型正确性由泛型桥单调化保证。
    let dev = unsafe { &*(driver_data.cast::<T>()) };
    // SAFETY: mac 由 NetOps 契约保证 (调用方持有 [u8; 6] 有效存储)。
    unsafe {
        *mac = dev.get_mac();
    }
}

/// extern "C" handle_irq 回调: 转发至 `T: NetDeviceOps::handle_irq`。
extern "C" fn net_irq_impl<T: NetDeviceOps>(driver_data: *mut u8) {
    if driver_data.is_null() {
        return;
    }
    // SAFETY: driver_data 契约同 net_send_impl。
    let dev = unsafe { &mut *(driver_data.cast::<T>()) };
    dev.handle_irq();
}

// ============================================================================
// 注册入口 — driver_data 契约 (唯一持有裸指针处)
// ============================================================================

/// services 网络设备注册数据 (NetOps 安全桥产物)。
pub struct NetDeviceRegistration {
    /// NetOps 指针表 (泛型桥产物, 泄漏存续内核生命周期)。
    pub ops: &'static NetOps,
    /// 设备裸指针 (Box::into_raw 转移所有权, 内核生命周期存续)。
    pub driver_data: *mut core::ffi::c_void,
    /// MAC 地址 (注册时读取, 供 EGDFNetDevice 构造, 免二次回调)。
    pub mac: [u8; 6],
}

/// 注册 services 网络设备 (services 可调用的 0 unsafe 入口)。
///
/// 所有权转移: `Box::into_raw` 泄漏设备存续内核生命周期 (与既有设备全局
/// 实例语义等价); MAC 在转移前经 `Box<T>` 安全读取; 返回的注册数据由
/// `nic_probe_all` 交 `EGDFNetDevice` 接入 smoltcp。
pub fn register_net_device<T: NetDeviceOps + 'static>(dev: Box<T>) -> NetDeviceRegistration {
    let mac = dev.get_mac();
    let raw = Box::into_raw(dev).cast::<core::ffi::c_void>();
    NetDeviceRegistration {
        ops: net_ops_for::<T>(),
        driver_data: raw,
        mac,
    }
}

// ============================================================================
// DECISION-K 注册契约槽 — services 探测回调 (framework 单向拉取)
// ============================================================================

/// services 层网络设备探测回调槽 (DECISION-K 模式, 同 storage
/// `NVME_SERVICES_DISPATCH`): framework 持有槽位机制, services `net_init`
/// 注册探测回调 (无捕获函数指针), `nic_probe_all` 经 `net_services_driver`
/// 单向拉取。未注册时拉取返回 `None` (fail-quiet)。
static NET_SERVICES_DRIVER: OnceLock<fn() -> Option<NetDeviceRegistration>> = OnceLock::new();

/// 注册 services 网络设备探测回调 (services 可调用的 0 unsafe 入口)。
///
/// # Errors
///
/// 回调槽已被占用 (重复注册) 时返回 `Err(已注册回调)`。
pub fn net_register_services_driver(
    probe: fn() -> Option<NetDeviceRegistration>,
) -> Result<(), fn() -> Option<NetDeviceRegistration>> {
    NET_SERVICES_DRIVER.set(probe)
}

/// 经 services 探测回调拉取网络设备注册数据 (framework 内部, 启动临界区)。
///
/// 仅被 `net/init/probe.rs::nic_probe_all` 调用, 而 probe 模块在
/// `kernel_test` 特性下 cfg-out (见 net/mod.rs init 模块声明), 故本函数
/// 同步 gate, 避免 kernel_test 下 dead code (F9).
#[cfg(not(feature = "kernel_test"))]
pub(crate) fn net_services_driver() -> Option<NetDeviceRegistration> {
    NET_SERVICES_DRIVER.get().and_then(|probe| probe())
}
