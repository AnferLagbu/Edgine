#![deny(unsafe_code)]
//! functions 层网络驱动 (阶段 3 驱动双份合并)
//!
//! - [e1000] — Intel 8254x (e1000/e1000e) PCI 网卡安全驱动
//!
//! 复合探测: e1000 (PCI) 优先, 失败回落 virtio-net (MMIO), 见
//! `net_functions_probe`。

pub mod e1000;

/// functions 复合网卡探测回调 (DECISION-K 注册契约: privileged 单向拉取)
///
/// 探测顺序: e1000 (PCI) → virtio-net (MMIO); 首个成功者返回其
/// `NetDeviceRegistration`, 全部失败返回 `None`。
///
/// 由 privileged `nic_probe_all` 经注册槽位单向拉取, 故本函数无直接调用者。
#[cfg(not(feature = "kernel_test"))]
fn net_functions_probe() -> Option<crate::privileged::net::NetDeviceRegistration> {
    e1000::e1000_net_registration()
        .or_else(crate::functions::driver::virtio::virtio_net_registration)
}

/// 初始化 functions 网络驱动 (注册复合探测回调槽)
///
/// DECISION-K 注册契约模式 (同 virtio-blk / NVMe): 仅注册探测回调槽
/// (functions→privileged 单向, privileged 不引用 functions), 实际设备探测由
/// privileged `nic_probe_all` 经槽位拉取。crate root lib.rs 在 `eg_net_init`
/// 之前编排调用。
pub fn net_init() {
    #[cfg(not(feature = "kernel_test"))]
    {
        let _ = crate::privileged::net::net_register_functions_driver(net_functions_probe);
    }
}
