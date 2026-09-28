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
/// 发现系统 xHCI 控制器并注册到 Chitin 设备表 (proto=Bus);
/// `chitin_register_driver` 内部触发 `Driver::init` (`init_hardware` 完成 reset + start).
pub fn usb_init() {
    use crate::framework::chitin::{ChitinProto, chitin_register_driver};

    let controllers = discover_xhci_controllers();
    crate::slog_info!(
        Driver,
        "[USB] discovered {} xHCI controller(s)",
        controllers.len()
    );

    for ctrl in controllers {
        chitin_register_driver("xhci", ChitinProto::Bus, None, None, Box::new(ctrl));
    }

    enumerate_connected_devices();
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
