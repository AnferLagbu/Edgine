#![deny(unsafe_code)]
//! `UNKFS` (Hypervisor File System) — 模块入口
//!
//! 热插拔监听 + 公共类型重导出.

use crate::framework::driver::hotplug::{HotplugEvent, HotplugListener};
use alloc::boxed::Box;

// 公共类型重导出 (必须在 HotplugListener 之前, 因为热插拔代码使用 get_unkfs)
pub use super::unkfs_data::*;
pub use super::unkfs_inode::*;

/// `UNKFS` 热插拔监听器 — 将块设备热插拔事件转发到 `UNKFS`
struct UnkfsHotplugListener;

impl HotplugListener for UnkfsHotplugListener {
    fn on_device_added(&self, event: &HotplugEvent) -> bool {
        let HotplugEvent::DeviceAdded { location } = event else {
            return false;
        };
        // 事件定位到的是 PCIe 端口 (端口 BDF), 块设备挂在该端口 secondary bus
        // 上的存储控制器之后; 需先解析出对应的 EGDF 块设备编号再挂盘。
        let drives = crate::services::driver::storage::drives_for_location(location);
        let mut claimed = false;
        for drive in drives {
            crate::slog_info!(
                FS,
                "[UNKFS] HOTPLUG: device added at bus={}/{} -> drive={}",
                location.bus,
                location.device,
                drive
            );
            claimed |= get_unkfs().hotplug_add_disk(drive);
        }
        claimed
    }

    fn on_device_removed(&self, event: &HotplugEvent) {
        let location = match event {
            HotplugEvent::DeviceRemoved { location }
            | HotplugEvent::SurpriseRemoval { location } => location,
            HotplugEvent::DeviceAdded { .. } => return,
        };
        let drives = crate::services::driver::storage::drives_for_location(location);
        for drive in drives {
            crate::slog_info!(FS, "[UNKFS] HOTPLUG: device removed -> drive={}", drive);
            get_unkfs().hotplug_remove_disk(drive);
        }
    }
}

/// 注册 `UNKFS` 热插拔监听器到全局热插拔管理器
pub fn unkfs_hotplug_register() {
    use crate::framework::driver::hotplug::HOTPLUG_MANAGER;
    HOTPLUG_MANAGER.register_listener(Box::new(UnkfsHotplugListener));
}
