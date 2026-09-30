//! 网络设备探测 (B04-09 优化拆分 Step F, 2026-08-25)
//!
//! 原 init.rs 内联定义 `nic_probe_all`; 抽出为独立子模块后, init.rs 主体
//! 通过 `probe::nic_probe_all()` 调用 (仅 init 模块内部可见, 不对外暴露).
//!
//! 批次 Z ④ (NetOps 安全桥): framework 旧 `VirtioNet`/`E1000Net` 驱动与
//! 静态 `NetOps` 表均已删除, 网卡探测改为经 DECISION-K 注册契约槽位单向
//! 拉取 services 驱动注册数据 (services→framework 单向, framework 不引用
//! services)。
//!
//! 阶段 3 (驱动双份合并, E1000 回迁): e1000 驱动权威迁 services, e1000 与
//! virtio-net 的探测统一收敛于 services 复合探测函数 `net_services_probe`
//! (顺序 e1000 → virtio-net); 本模块仅做槽位单向拉取, 不再自行探测设备。

use crate::framework::net::ChitinNetDevice;

use super::raw;

/// 探测网卡设备 (经 services 注册契约槽位单向拉取)
///
/// # Safety
///
/// - 在网络子系统初始化入口被调用, 期间无其他并发探测
/// - 依赖的 chitin/driver 框架自身保证设备独占
#[cfg(not(feature = "kernel_test"))]
// SAFETY: 仅由 qx_net_init 在启动临界区调用一次 (单线程), 无并发探测;
// 返回的 ChitinNetDevice 所有权转移给调用方, 内部裸指针由驱动生命周期保证.
pub(super) unsafe fn nic_probe_all() -> Option<ChitinNetDevice> {
    // I-53 修复: 去除编译时架构互斥, 双架构二进制按运行时探测顺序
    // 尝试 e1000 (PCI 设备) 与 virtio-net (MMIO 设备). 两者驱动代码
    // 均架构无关, 仅依赖 IoMem / PCI 抽象. QEMU 配置决定哪一个会成功.
    //
    // 阶段 3 起: 探测顺序 (e1000 -> virtio-net) 收敛于 services
    // `net_services_probe`, framework 此处仅单向拉取其注册结果。

    // services 权威 (批次 Z ④ NetOps 安全桥 + 阶段 3 复合探测)
    // DECISION-K 单向注册契约: services net_init 已注册探测回调, 此处
    // 经 framework 槽位单向拉取 (framework 不引用 services, F2 合规)。
    // 未注册/探测失败返回 None → nic_probe_all 返回 None (与旧行为一致)。
    if let Some(reg) = crate::framework::net::net_device_ops::net_services_driver() {
        let nic = ChitinNetDevice::new(reg.ops, reg.driver_data, reg.mac);
        raw::klog_msg("nic: probed successfully (services bridge)");
        return Some(nic);
    }

    None
}
