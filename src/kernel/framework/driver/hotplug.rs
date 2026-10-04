//! 设备热插拔管理器 (Device Hotplug Manager)
//!
//! 统一管理 PCIe/USB 等总线的设备插入/移除事件，
//! 将底层硬件事件分发给已注册的监听器（文件系统、设备管理器等）。
//!
//! ## 设计理念
//!
//! ```text
//! 硬件中断源 (PCIe MSI / USB Port Change)
//!   → HotplugManager.poll()
//!     → 扫描所有已知热插拔槽位
//!     → 生成 HotplugEvent
//!     → 分发给 HotplugListener 链表
//!       → UNKFS hotplug listener (磁盘插入/移除)
//!       → Storage listener (重新注册 BlockDevice)
//!       → 未来: 用户态通知 (/dev/hotplug)
//! ```
//!
//! 不使用中断线程, 采用轮询模式: 调度器 tick 周期唤醒, 由 softirq 上下文执行 poll。

use crate::framework::irq::{self, SoftirqVec};
use crate::framework::pci::PcieHotplugSlot;
use crate::framework::sync::IrqSpinLock as Mutex;
use crate::framework::sync::OnceLock;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
// ── 事件类型 ──

/// 总线类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusType {
    Pcie,
    Usb,
    Virtio,
    /// SATA (AHCI 端口级热插拔)
    Sata,
}

/// 设备位置标识
#[derive(Debug, Clone, Copy)]
pub struct DeviceLocation {
    pub bus_type: BusType,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub slot: u8,
}

/// 热插拔事件
#[derive(Debug, Clone)]
pub enum HotplugEvent {
    /// 设备已插入，需重新扫描并注册
    DeviceAdded { location: DeviceLocation },
    /// 设备已移除 (正常流程)
    DeviceRemoved { location: DeviceLocation },
    /// 意外拔出 (未事先通知)
    SurpriseRemoval { location: DeviceLocation },
}

// ── 监听器 ──

/// 热插拔事件监听器 trait。
///
/// 各子系统（如 UNKFS、存储管理器）实现此 trait 并注册到 `HotplugManager`。
pub trait HotplugListener: Send + Sync {
    /// 设备插入通知。
    /// 在事件分发给所有监听器后, 由第一个返回 true 的监听器"认领"该设备。
    fn on_device_added(&self, event: &HotplugEvent) -> bool;

    /// 设备移除通知。
    /// 监听器应在此清理与该设备相关的内部状态。
    fn on_device_removed(&self, event: &HotplugEvent);
}

// ── 管理器 ──

/// 全局热插拔事件管理器。
pub struct HotplugManager {
    slots: Mutex<Vec<PcieHotplugSlot>>,
    listeners: Mutex<Vec<Box<dyn HotplugListener>>>,
    initialized: Mutex<bool>,
}

impl HotplugManager {
    pub const fn new() -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
            initialized: Mutex::new(false),
        }
    }

    /// 初始化: 扫描 `PCIe` 热插拔槽位。
    pub fn init(&self) {
        let mut init = self.initialized.lock();
        if *init {
            return;
        }

        #[cfg(target_arch = "x86_64")]
        {
            let found = crate::framework::pci::scan_hotplug_slots();
            if !found.is_empty() {
                crate::klog_info!(Driver, "hotplug: {} PCIe slot(s) found", found.len());
            }
            *self.slots.lock() = found;
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            crate::klog_info!(
                Driver,
                "hotplug: PCIe hotplug not supported on this architecture"
            );
        }
        *init = true;
    }

    /// 注册热插拔事件监听器。
    pub fn register_listener(&self, listener: Box<dyn HotplugListener>) {
        self.listeners.lock().push(listener);
    }

    /// 轮询所有热插拔槽位, 检测事件变化并分发给监听器。
    ///
    /// 应在每个调度周期或 idle loop 中调用 (开销很低: 非热插拔场景下无任何 PCI 配置空间访问)。
    ///
    /// 每个事件在分发给 `HotplugListener` **之前**, 先调用 services 注册的
    /// 重枚举回调 (见 `register_reenum_hook`), 使 services 有机会同步总线扫描
    /// 与块设备注册/注销, 随后监听器才能基于最新的设备表处理事件。
    ///
    /// 注: 事件分发在 `slots` 锁之下进行 (热插拔事件罕见, 且回调内不反向
    /// 获取本管理器的 `slots` 锁, 无死锁风险), 以简化锁边界。
    pub fn poll(&self) {
        let mut slots = self.slots.lock();
        if slots.is_empty() {
            return;
        }

        for slot in slots.iter_mut() {
            let events = slot.read_and_clear_events();
            if events == 0 {
                continue;
            }

            let location = DeviceLocation {
                bus_type: BusType::Pcie,
                bus: slot.bus,
                device: slot.device,
                function: slot.function,
                slot: slot.slot_number,
            };

            if slot.has_surprise_removal(events) {
                self.dispatch(&HotplugEvent::SurpriseRemoval { location });
            } else if slot.has_insertion_event(events) {
                self.dispatch(&HotplugEvent::DeviceAdded { location });
            } else if slot.has_removal_event(events) {
                self.dispatch(&HotplugEvent::DeviceRemoved { location });
            }
        }
    }

    /// 分发单个热插拔事件: 先调用 services 重枚举回调, 再通知所有监听器。
    ///
    /// 供自行检测到事件的总线驱动 (如 xHCI 端口变化) 直接调用, 复用与
    /// `poll` 完全一致的分发语义, 保证"重枚举先行、监听器后处理"的时序。
    pub fn dispatch(&self, event: &HotplugEvent) {
        dispatch_reenum(event);
        let listeners = self.listeners.lock();
        match event {
            HotplugEvent::DeviceAdded { .. } => {
                for l in listeners.iter() {
                    l.on_device_added(event);
                }
            }
            HotplugEvent::DeviceRemoved { .. } | HotplugEvent::SurpriseRemoval { .. } => {
                for l in listeners.iter() {
                    l.on_device_removed(event);
                }
            }
        }
    }

    /// 返回热插拔状态摘要 (供 syscall / 调试使用)。
    ///
    /// 返回 (`slot_count`, `slot_summary`, `blk_device_count`, `blk_device_states`) 的扁平化视图。
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn status(&self) -> HotplugStatus {
        let init = self.initialized.lock();
        let enabled = *init;

        let slots = self.slots.lock();
        let slot_infos: Vec<HotplugSlotInfo> = slots
            .iter()
            .map(|s| HotplugSlotInfo {
                bus: s.bus,
                device: s.device,
                function: s.function,
                slot_number: s.slot_number,
                presence: s.presence_state,
                surprise_capable: s.surprise_removal,
                hotplug_capable: s.hotplug_capable,
            })
            .collect();
        drop(slots);

        // 按 EGDF 全局下标枚举块设备 (含已墓碑化设备), 保证上报的 `drive`
        // 与 `hdd_*` / `egdf_blk_*` 使用的索引一致。
        let drives = crate::framework::egdf::egdf_blk_drives();
        let mut blk_states: Vec<BlockDeviceState> = Vec::new();
        for d in drives {
            let (present, removing, io_count) = crate::framework::driver::block_device_state(d);
            blk_states.push(BlockDeviceState {
                drive: d,
                present,
                removing,
                io_count,
            });
        }
        let blk_count = blk_states.len();

        HotplugStatus {
            enabled,
            slot_count: slot_infos.len() as u32,
            slots: slot_infos,
            blk_device_count: blk_count as u32,
            blk_devices: blk_states,
        }
    }
}

// ── 状态数据结构 ──

/// 单个热插拔槽位状态
#[derive(Debug, Clone, Copy)]
pub struct HotplugSlotInfo {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub slot_number: u8,
    pub presence: bool,
    pub surprise_capable: bool,
    pub hotplug_capable: bool,
}

/// 单个块设备状态
#[derive(Debug, Clone, Copy)]
pub struct BlockDeviceState {
    pub drive: u8,
    pub present: bool,
    pub removing: bool,
    pub io_count: u32,
}

/// 热插拔系统状态汇总
#[derive(Debug, Clone)]
pub struct HotplugStatus {
    pub enabled: bool,
    pub slot_count: u32,
    pub slots: Vec<HotplugSlotInfo>,
    pub blk_device_count: u32,
    pub blk_devices: Vec<BlockDeviceState>,
}

// ── 全局单例 ──

pub static HOTPLUG_MANAGER: HotplugManager = HotplugManager::new();

/// DECISION-K: services 侧总线重枚举回调 (无捕获函数指针)。
///
/// services 层在启动期通过 `register_reenum_hook` 注册回调; framework 在
/// 分发每个 `HotplugEvent` 给监听器**之前**调用它, 使 services 有机会重新
/// 扫描总线并完成块设备的注册/注销。未注册时 fail-quiet (与 NVMe MSIX
/// dispatch 同模式), 保证 framework 不依赖任何 services 符号。
static HOTPLUG_REENUM_HOOK: OnceLock<fn(&HotplugEvent)> = OnceLock::new();

/// 注册总线重枚举回调 (由 services 侧调用, 注册一次)。
///
/// # Errors
/// 若回调已被注册, 返回 `Err(hook)` 将本次传入的函数指针原样退回。
pub fn register_reenum_hook(hook: fn(&HotplugEvent)) -> Result<(), fn(&HotplugEvent)> {
    HOTPLUG_REENUM_HOOK.set(hook)
}

/// 在分发监听器之前调用 services 重枚举回调 (若已注册)。
fn dispatch_reenum(event: &HotplugEvent) {
    if let Some(hook) = HOTPLUG_REENUM_HOOK.get() {
        hook(event);
    }
}

/// DECISION-K: services 侧辅助轮询回调表 (无捕获函数指针)。
///
/// 供 framework 自身无法探测、必须由 services 读取设备 MMIO 才能发现变化的
/// 总线注册 (如 xHCI 端口状态变化、AHCI 端口插入/移除)。framework 在每次
/// `poll()` 之后依次调用全部已注册回调; 表为空时 fail-quiet, 保证 framework
/// 不依赖任何 services 符号。
///
/// 采用多槽 (`Vec`) 而非单槽 (`OnceLock`), 允许多条总线各自注册自己的
/// 端口轮询器; 回调仅在启动期注册, 分发时以只读遍历执行。
static HOTPLUG_AUX_POLL: Mutex<Vec<fn()>> = Mutex::new(Vec::new());

/// 注册辅助轮询回调 (由 services 侧调用)。
///
/// 支持多次注册 (每条总线一个); 同一函数指针重复注册会被忽略, 避免
/// 重复轮询。启动期单线程调用, 无竞争。
pub fn register_aux_poll(hook: fn()) {
    let mut hooks = HOTPLUG_AUX_POLL.lock();
    if !hooks.contains(&hook) {
        hooks.push(hook);
    }
}

/// 在 framework 轮询之后调用全部 services 辅助轮询回调。
///
/// 遍历时持锁执行 (回调不反向获取本表, 无死锁风险); 因回调仅在启动期
/// 注册, 遍历期间表内容不会变化, 无需额外分配。
fn dispatch_aux_poll() {
    let hooks = HOTPLUG_AUX_POLL.lock();
    for hook in hooks.iter() {
        hook();
    }
}

/// 热插拔 softirq 唤醒去重标志 (参照 kswapd 模式)。
///
/// 同一周期内多次 `hotplug_wakeup` 只触发一次 softirq, 避免重复入队。
static HOTPLUG_PENDING: AtomicBool = AtomicBool::new(false);

/// 热插拔 softirq 处理程序: 在软中断上下文轮询所有热插拔槽位。
///
/// 软中断上下文可开中断但不可睡眠、不可长时间持锁, 因此仅做
/// "读寄存器 + 分发监听器" 的短操作, 重枚举等重活由监听器/回调承担。
fn hotplug_softirq_handler() {
    HOTPLUG_PENDING.store(false, Ordering::Release);
    HOTPLUG_MANAGER.poll();
    dispatch_aux_poll();
}

/// 外部调用入口: 初始化热插拔管理器并注册核心监听器
pub fn hotplug_init() {
    HOTPLUG_MANAGER.init();
    // 注册 softirq 处理程序 (启动期单线程, 且本函数在 interrupt_late_init 之后调用)
    irq::open_softirq(SoftirqVec::Hotplug, hotplug_softirq_handler);
}

/// 外部调用入口: 周期唤醒热插拔轮询 (由调度器 tick 调用)。
///
/// 采用 pending 标志去重 + softirq 延迟执行 (参照 kswapd 先例):
/// 调度器 tick 只负责"唤醒", 真正的寄存器读取与事件分发在软中断
/// 上下文完成, 避免在 tick 路径上长时间持锁。
/// 非热插拔场景下 `poll` 不做任何 PCI 配置空间访问, 开销可忽略。
pub fn hotplug_wakeup() {
    if HOTPLUG_PENDING.load(Ordering::Acquire) {
        return;
    }
    HOTPLUG_PENDING.store(true, Ordering::Release);
    irq::raise_softirq(SoftirqVec::Hotplug);
}
