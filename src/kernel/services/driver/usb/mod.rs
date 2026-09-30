#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! USB 驱动 — services 层 (Phase 2.1.6)
//!
//! 100% safe USB 子系统, 0 unsafe.
//! 所有 MMIO 操作通过 `framework::IoMem` 安全代理, DMA 通过 framework safe wrapper.
//!
//! ## 模块结构
//!
//! ```text
//! services::driver::usb
//! ├── xhci.rs         — xHCI 主机控制器 (USB 3.0) 安全代理
//! ├── usb_core.rs     — USB 核心类型 (Descriptors, URB, HostController Trait)
//! ├── enumerate.rs    — USB 设备枚举 (描述符解析 + 枚举流程)
//! ├── ring.rs         — xHCI 环形缓冲区 (Command Ring + Event Ring)
//! ├── hid.rs          — HID 类驱动 (键盘/鼠标 Boot Protocol)
//! └── mass_storage.rs — 大容量存储类驱动 (BBB + SCSI)
//! ```
//!
//! ## 设计原则
//!
//! - **零 unsafe**: USB 核心业务 (PCI 探测 / 枚举 / 类驱动) 全部实装于 services
//! - **MMIO 代理**: `xhci.rs` 通过 `IoMem` 安全访问寄存器
//! - **DMA 代理**: 缓冲区经 framework DMA 安全包装分配 / 拷贝
//!
//! ## 后续添加
//!
//! - `ehci.rs`  — EHCI (USB 2.0) (Phase 2.1.6 后续)
//! - `uhci.rs`  — UHCI (USB 1.1) (Phase 2.1.6 后续)
//! - `ohci.rs`  — OHCI (USB 1.1) (Phase 2.1.6 后续)
//!
//! 评估日期: 2026-07-04
//! Phase 2.1.6 任务: USB/XHCI 驱动迁移

use alloc::boxed::Box;
use alloc::vec::Vec;

pub mod enumerate;
pub mod hid;
pub mod mass_storage;
pub mod ring;
pub mod usb_core;
pub mod xhci;

use xhci::XhciController;

use crate::services::sync::irq_lock::IrqSpinLock as Mutex;

/// services 自持的 xHCI 控制器列表 (供端口变化轮询).
///
/// 控制器同时以裸指针登记进 Chitin 设备表 (proto=Bus) 供统一展示, 但 Chitin
/// 只持有指针不拥有对象; `Box::leak` 得到的 `&'static mut` 由本表持有, 保证
/// 轮询时可安全访问 PORTSC。
static USB_CONTROLLERS: Mutex<Vec<&'static mut XhciController>> = Mutex::new(Vec::new());

// ============================================================================
// xHCI PCI 发现 (USB-1.2)
// ============================================================================
//
// PCI class code 0x0C (Serial Bus 串行总线), subclass 0x03 (USB), prog_if 0x30 (xHCI)
// 来源: PCI Code and ID Assignment Specification §6
//
// 注: 不强制依赖 ACPI MCFG / 物理 MMIO base, 直接从 PciDevice.bars[0] 读取
//      xHCI 控制器的 BAR0 (MMIO 32-bit 或 64-bit).

/// PCI class code 串行总线控制器
const PCI_CLASS_SERIAL_BUS: u8 = 0x0C;
/// PCI subclass code USB 控制器
const PCI_SUBCLASS_USB: u8 = 0x03;
/// PCI 编程接口 xHCI (USB 3.0)
const PCI_PROGIF_XHCI: u8 = 0x30;

/// 默认 xHCI MMIO 映射大小 (xHCI 规范要求至少 256 字节; 现代控制器通常 64 KiB).
///
/// 真实 BAR size 由 PciBar.size 提供, 但 `find_by_class` 返回的 PciDevice.bar
/// 可能 size=0 (某些固件未配置), 此 fallback 用于该情况.
const XHCI_DEFAULT_MMIO_SIZE: usize = 0x10000; // 64 KiB

/// 发现系统中的所有 xHCI 控制器 (USB-1.2: TRACK-558BA7 消除).
///
/// 扫描 PCI 总线, 过滤 `class=0x0C/subclass=0x03/prog_if=0x30` 的设备,
/// 为每个设备创建 `XhciController` (未初始化, 由注册方触发 `init`).
///
/// 找不到控制器 (返回空 Vec) 或 MMIO 映射失败均跳过, 不视为错误.
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
pub fn discover_xhci_controllers() -> Vec<XhciController> {
    // 1. 扫描所有 Serial Bus 设备
    let serial_bus_devs = crate::framework::pci::find_by_class(PCI_CLASS_SERIAL_BUS);
    let mut controllers = Vec::new();

    for dev in &serial_bus_devs {
        // 2. 过滤: subclass=0x03 (USB), prog_if=0x30 (xHCI)
        if !is_xhci_device(dev) {
            continue;
        }

        // 3. 读取 BAR0 (MMIO 基地址 + size)
        let (bar_base, bar_size) = match dev.bars.first() {
            Some(bar) if bar.bar_type != crate::framework::pci::BarType::None => (
                bar.base_addr,
                if bar.size > 0 {
                    bar.size
                } else {
                    XHCI_DEFAULT_MMIO_SIZE as u64
                },
            ),
            _ => {
                // 无 BAR0 跳过 (设备未配置, 通常是 BIOS 尚未枚举的设备)
                continue;
            }
        };

        // 4. MMIO 映射大小钳制到默认上限; `XhciController::new` 经 framework IoMem 安全代理
        let mmio_size = bar_size.min(XHCI_DEFAULT_MMIO_SIZE as u64) as usize;
        if let Some(ctrl) = XhciController::new(bar_base, mmio_size) {
            controllers.push(ctrl);
        }
    }

    controllers
}

/// 判断 `PciDevice` 是否为 xHCI 控制器.
fn is_xhci_device(dev: &crate::framework::pci::PciDevice) -> bool {
    dev.class_code == PCI_CLASS_SERIAL_BUS
        && dev.subclass_code == PCI_SUBCLASS_USB
        && dev.prog_if == PCI_PROGIF_XHCI
}

// ============================================================================
// 初始化函数
// ============================================================================

/// 初始化 USB 子系统.
///
/// 发现系统 xHCI 控制器, 逐个 `init_hardware` (reset + start) 后登记到 Chitin
/// 设备表 (proto=Bus), 并由 services 自持控制器所有权以支持端口变化轮询。
/// 最后注册 xHCI 端口轮询回调 (framework 无法自行探测 USB 端口变化)。
pub fn usb_init() {
    use crate::framework::chitin::{ChitinProto, chitin_register};
    use crate::framework::driver::hotplug::register_aux_poll;

    let controllers = discover_xhci_controllers();
    crate::slog_info!(
        Driver,
        "[USB] discovered {} xHCI controller(s)",
        controllers.len()
    );

    for mut ctrl in controllers {
        let _ = ctrl.init_hardware();
        // 所有权移交 services (Box::leak); Chitin 仅登记裸指针 (非所有权)
        let leaked: &'static mut XhciController = Box::leak(Box::new(ctrl));
        chitin_register(
            "xhci",
            ChitinProto::Bus,
            None,
            None,
            core::ptr::from_mut(leaked).cast::<u8>(),
        );
        if leaked.is_initialized() {
            USB_CONTROLLERS.lock().push(leaked);
        }
    }

    // 注册端口轮询回调 (DECISION-K: framework 经 softirq 周期调用)
    register_aux_poll(usb_port_poll);

    enumerate_connected_devices();
}

/// xHCI 端口变化轮询 (DECISION-K 辅助轮询回调).
///
/// USB 端口插拔不产生 PCIe 热插拔事件, framework 无法自行探测; 本函数读取各
/// 控制器 PORTSC 的变化位 (CSC/PEC/OCC/RC), 应答后以统一的热插拔事件分发
/// (复用重枚举先行的时序)。仅在检测到变化时才触碰事件通路。
fn usb_port_poll() {
    use crate::framework::driver::hotplug::{
        BusType, DeviceLocation, HOTPLUG_MANAGER, HotplugEvent,
    };
    use xhci::{PORTSC_CCS, PORTSC_CSC, PORTSC_OCC, PORTSC_PEC, PORTSC_RC};

    // 变化位集合 (RW1CS: 写 1 应答)
    const CHANGE_BITS: u32 = PORTSC_CSC | PORTSC_PEC | PORTSC_OCC | PORTSC_RC;

    // 先收集事件并在锁内应答, 释放锁后再分发 (分发会进入 storage/NestFS 等
    // 子系统并获取其锁, 不在持 USB 锁时跨界)。
    let mut events = Vec::new();
    {
        let controllers = USB_CONTROLLERS.lock();
        for ctrl in controllers.iter() {
            if !ctrl.has_port_change() {
                continue;
            }
            for port in 1..=ctrl.num_ports() {
                let sc = ctrl.portsc(port);
                if sc & CHANGE_BITS == 0 {
                    continue;
                }
                ctrl.ack_port_change(port, sc & CHANGE_BITS);
                let location = DeviceLocation {
                    bus_type: BusType::Usb,
                    bus: 0,
                    device: 0,
                    function: 0,
                    slot: port,
                };
                let attached = sc & PORTSC_CCS != 0;
                crate::slog_info!(
                    Driver,
                    "[USB] port {} change: {}",
                    port,
                    if attached { "attached" } else { "detached" }
                );
                events.push(if attached {
                    HotplugEvent::DeviceAdded { location }
                } else {
                    HotplugEvent::DeviceRemoved { location }
                });
            }
        }
    }

    for event in &events {
        HOTPLUG_MANAGER.dispatch(event);
    }
}

/// 枚举已连接 USB 设备 (USB-1.6).
///
/// SIMPLIFIED: 当前为软件骨架 — 固定示例 1 个端口 (Full Speed) 走枚举流程;
///   影响: 未读取真实 PORTSC 端口映射, 也未与真实 xHCI 传输绑定;
///   扩展时机: Phase E 接入 Event Ring / Transfer Ring 后, 改为遍历
///   `HostController::num_ports` + `port_has_device` 真实枚举.
fn enumerate_connected_devices() {
    use self::enumerate::enumerate_new_device;
    use self::usb_core::UsbSpeed;

    let _ = enumerate_new_device(1, UsbSpeed::Full, || Ok(1));
}
