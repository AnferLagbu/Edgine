#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! `VirtIO` 设备驱动 — services 层 (Phase 2.1.2 + 2.1.3)
//!
//! 提供 virtio-blk (Phase 2.1.3) 和 virtio-net (Phase 2.1.2) 的 100% safe 设备驱动。
//! MMIO transport 机制由 framework [`crate::framework::driver::virtio::VirtioMmioDevice`]
//! 提供 (单一实现, 批次 Z ③ transport 去重), services 层仅承载设备业务。
//!
//! ## 模块结构
//!
//! - [blk] — `VirtIO` 块设备安全驱动, 0 unsafe
//! - [net] — `VirtIO` 网络设备安全驱动, 0 unsafe
//!
//! ## 迁移状态
//!
//! - [`blk::VirtioBlkDriver`] — 块设备初始化 + 特性协商 + 配置读取全 100% safe
//! - [`net::VirtioNetDriver`] — 网卡初始化 + 特性协商 + MAC/链路读取全 100% safe
//!
//! 评估日期: 2026-06-04
//! Phase 2.1.2/2.1.3 任务

pub mod blk;
pub mod net;

/// services virtio-net 探测回调 (DECISION-K 注册契约: framework 单向拉取)
///
/// 扫描 virtio-mmio 区域, 发现网络设备 (`VIRTIO_ID_NET`) 即创建 services
/// `VirtioNetDriver`, 完成初始化 (`finalize`: vq0/vq1 MMIO 配置 +
/// DRIVER_OK + RX 预填) 后经 framework `register_net_device` 桥接为
/// `NetDeviceRegistration`。由 services 复合探测 `net_services_probe`
/// (位于 `services::driver::net`) 在 e1000 探测失败后回落调用
/// (启动临界区单线程)。
///
/// ## 条件编译
///
/// 唯一消费者 `net_services_probe` 受 `#[cfg(not(feature = "kernel_test"))]`
/// 门控, 故本函数同样门控以在 kernel_test 构建下不编译, 避免死代码
/// (AGENTS.md §5 F9)。
#[cfg(not(feature = "kernel_test"))]
pub(crate) fn virtio_net_registration() -> Option<crate::framework::net::NetDeviceRegistration> {
    use crate::framework::driver::virtio::{
        VIRTIO_ID_NET, VIRTIO_MMIO_BASE, VIRTIO_MMIO_MAX_DEVICES, VIRTIO_MMIO_STRIDE,
        VirtioMmioDevice,
    };
    use crate::framework::net::register_net_device;

    for i in 0..VIRTIO_MMIO_MAX_DEVICES {
        let base = VIRTIO_MMIO_BASE + u64::from(i) * VIRTIO_MMIO_STRIDE;
        let Some(dev) = VirtioMmioDevice::probe(base) else {
            continue;
        };
        if dev.device_id() != VIRTIO_ID_NET {
            continue;
        }
        let Some(mut driver) = net::VirtioNetDriver::new(dev) else {
            crate::slog_warn!(Driver, "virtio-net: 发现设备但初始化失败");
            continue;
        };
        // 完成初始化: vq0/vq1 MMIO 配置 + DRIVER_OK + RX 预填 (设备进入 live)
        driver.finalize();
        let reg = register_net_device(alloc::boxed::Box::new(driver));
        crate::slog_info!(
            Driver,
            "virtio-net: registered via services bridge (MAC={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X})",
            reg.mac[0],
            reg.mac[1],
            reg.mac[2],
            reg.mac[3],
            reg.mac[4],
            reg.mac[5]
        );
        return Some(reg);
    }
    None
}

/// 初始化 VirtIO 块设备并注册到 Chitin (§6.4 直接方案 B: services 权威)
///
/// 探测 virtio-mmio 区域, 为块设备创建 services `VirtioBlkDriver`,
/// 完成初始化 (`finalize`: vq0 MMIO 配置 + DRIVER_OK) 后经
/// `proto_block::register_block_device` 注册为块设备。
///
/// framework 保留: `VirtioMmioDevice` (MMIO 传输机制) + `queue` (DMA 环机制)。
/// aarch64 (QEMU -M virt) 是 virtio-blk 的主战场; x86_64 走 PCI AHCI/NVMe。
pub fn blk_init() {
    use crate::framework::chitin::proto_block::register_block_device;
    use crate::framework::driver::virtio::{
        VIRTIO_ID_BLOCK, VIRTIO_MMIO_BASE, VIRTIO_MMIO_MAX_DEVICES, VIRTIO_MMIO_STRIDE,
        VirtioMmioDevice,
    };
    use blk::VirtioBlkDriver;

    let mut blk_count = 0u32;
    for i in 0..VIRTIO_MMIO_MAX_DEVICES {
        let base = VIRTIO_MMIO_BASE + u64::from(i) * VIRTIO_MMIO_STRIDE;
        let Some(dev) = VirtioMmioDevice::probe(base) else {
            continue;
        };
        if dev.device_id() != VIRTIO_ID_BLOCK {
            continue;
        }
        let Some(mut blk) = VirtioBlkDriver::new(dev) else {
            continue;
        };
        // 完成初始化: vq0 MMIO 配置 + DRIVER_OK (设备进入 live)
        blk.finalize();
        let name = alloc::format!("virtio-blk{blk_count}");
        let name: &'static str = name.leak();
        let mmio_base = blk.device().mmio_base();
        register_block_device(name, blk, Some(mmio_base));
        blk_count += 1;
        crate::slog_info!(
            Driver,
            "virtio-blk: registered device #{} (services 权威)",
            blk_count
        );
    }
    if blk_count > 0 {
        crate::slog_info!(
            Driver,
            "virtio-blk: {} device(s) registered (services 权威)",
            blk_count
        );
    }
}
