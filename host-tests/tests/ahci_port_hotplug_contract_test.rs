//! AHCI 端口级热插拔契约 host 专项测试
//!
//! ## 背景
//!
//! SATA 端口插拔不产生 PCIe 热插拔事件, 故新增 services 侧 `ahci_port_poll`
//! 辅助轮询 (读 `PxSSTS` 检测链路变化) 并经 `register_aux_poll` 接入 framework
//! softirq, 复用统一热插拔分发语义 (`BusType::Sata` 事件)。本文件补齐该新链路
//! 的可在主机端验证的部分。
//!
//! ## 覆盖范围
//!
//! - `BusType::Sata` 事件经 `HotplugManager::dispatch` 正确路由到监听器
//!   (`DeviceAdded` → `on_device_added`; `DeviceRemoved`/`SurpriseRemoval`
//!   → `on_device_removed`)
//! - `drives_for_location` 对 `Sata`/`Usb`/`Virtio` 总线在无控制器台账时的
//!   空边界 (Pcie 分支会触真实 PCI 端口 I/O, 主机端不可调用, 故不覆盖)
//! - 端口级热插拔接线契约 (静态): 轮询注册 + 端口表固定槽位 + 端口号还原入口
//!
//! ## 隔离说明
//!
//! `HOTPLUG_MANAGER` / `PROBED` 均为进程内全局单例; 本文件为独立测试二进制,
//! 不与其它测试共享全局状态。本文件内含 IO/探针的用例以 `MGR_LOCK` 串行执行。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use edgine::kernel::framework::driver::hotplug::{
    BusType, DeviceLocation, HOTPLUG_MANAGER, HotplugEvent, HotplugListener,
};
use edgine::kernel::services::driver::storage::drives_for_location;

/// 分发类用例的进程内串行锁 (全局管理器单例)。
static MGR_LOCK: Mutex<()> = Mutex::new(());

/// `on_device_added` 调用次数
static ADDED_CALLS: AtomicU32 = AtomicU32::new(0);
/// `on_device_removed` 调用次数
static REMOVED_CALLS: AtomicU32 = AtomicU32::new(0);

/// 计数探针监听器。
struct CountListener;

impl HotplugListener for CountListener {
    fn on_device_added(&self, _event: &HotplugEvent) -> bool {
        ADDED_CALLS.fetch_add(1, Ordering::SeqCst);
        false
    }

    fn on_device_removed(&self, _event: &HotplugEvent) {
        REMOVED_CALLS.fetch_add(1, Ordering::SeqCst);
    }
}

/// 构造一个 SATA 端口位置 (`slot` 为硬件端口号, 其余为所属 AHCI 控制器 BDF)。
fn sata_location(port_num: u8) -> DeviceLocation {
    DeviceLocation {
        bus_type: BusType::Sata,
        bus: 0,
        device: 0x1F,
        function: 2,
        slot: port_num,
    }
}

/// `BusType::Sata` 事件经统一管理器正确路由。
#[test]
fn sata_hotplug_event_routes_through_manager() {
    let _guard = MGR_LOCK.lock().expect("获取管理器串行锁失败");
    HOTPLUG_MANAGER.register_listener(Box::new(CountListener));

    let location = sata_location(3);

    HOTPLUG_MANAGER.dispatch(&HotplugEvent::DeviceAdded { location });
    assert_eq!(
        ADDED_CALLS.load(Ordering::SeqCst),
        1,
        "SATA DeviceAdded 应触发 on_device_added"
    );
    assert_eq!(REMOVED_CALLS.load(Ordering::SeqCst), 0);

    HOTPLUG_MANAGER.dispatch(&HotplugEvent::SurpriseRemoval { location });
    HOTPLUG_MANAGER.dispatch(&HotplugEvent::DeviceRemoved { location });
    assert_eq!(
        REMOVED_CALLS.load(Ordering::SeqCst),
        2,
        "SATA SurpriseRemoval/DeviceRemoved 均应路由到 on_device_removed"
    );
    assert_eq!(
        ADDED_CALLS.load(Ordering::SeqCst),
        1,
        "不应再触发 on_device_added"
    );
}

/// `drives_for_location` 在无控制器台账时的空边界 (不触真实硬件 I/O 的分支)。
#[test]
fn drives_for_location_empty_without_controller() {
    // 未运行 storage_init ⇒ PROBED 为空 ⇒ SATA 查询落空
    assert!(
        drives_for_location(&sata_location(0)).is_empty(),
        "无已探测控制器时 SATA 位置应解析为空"
    );
    assert!(
        drives_for_location(&sata_location(31)).is_empty(),
        "越界端口号在无台账时同样应解析为空"
    );

    let usb = DeviceLocation {
        bus_type: BusType::Usb,
        bus: 1,
        device: 0,
        function: 0,
        slot: 0,
    };
    let virtio = DeviceLocation {
        bus_type: BusType::Virtio,
        bus: 0,
        device: 0,
        function: 0,
        slot: 0,
    };
    assert!(
        drives_for_location(&usb).is_empty(),
        "USB 总线不解析存储块设备"
    );
    assert!(
        drives_for_location(&virtio).is_empty(),
        "Virtio 总线不解析存储块设备"
    );
}

/// 端口级热插拔接线契约 (静态): 防未来回归。
#[test]
fn ahci_port_hotplug_wiring_present() {
    let storage = std::fs::read_to_string("../src/kernel/services/driver/storage/mod.rs")
        .expect("读取 services storage mod.rs 失败");
    assert!(
        storage.contains("register_aux_poll(ahci_port_poll)"),
        "storage_init 未注册 AHCI 端口级轮询回调"
    );
    assert!(
        storage.contains("fn ahci_port_poll"),
        "缺 ahci_port_poll 端口轮询实现"
    );

    let ahci = std::fs::read_to_string("../src/kernel/services/driver/storage/ahci.rs")
        .expect("读取 services ahci.rs 失败");
    assert!(
        ahci.contains("pub fn scan_ports"),
        "缺 scan_ports 端口扫描入口"
    );
    assert!(
        ahci.contains("pub fn port_index_of"),
        "缺 port_index_of 硬件端口号→索引还原入口"
    );
    // 端口表下标必须与硬件端口号一一对应 (固定槽位), 否则热插拔前后
    // `AhciBlockDevice.port_index` 会漂移 ⇒ init_controller 须无条件 push
    assert!(
        ahci.contains("self.ports.push(port)"),
        "init_controller 应无条件 push 全部已实现端口 (固定槽位)"
    );
}
