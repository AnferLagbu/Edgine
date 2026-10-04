//! PCI 总线 API 层
//!
//! PCI/PCIe 配置空间访问、总线扫描、BAR 解析、设备枚举的统一入口。
//!
//! ## 调用方契约
//! - `driver::bus::pci` —— 设备初始化阶段的配置空间读写
//! - `egdf::devtree` —— 设备树生成时枚举 PCI 设备
//! - `driver::net::e1000` —— E1000 网卡 probe
//! - `driver::storage::nvme` —— `NVMe` SSD 初始化
//! - `driver::storage::ahci` —— AHCI SATA 控制器初始化
//! - `services::driver::usb` —— XHCI USB 控制器探测 (2-D 下沉)
//!
//! ## 内部接口
//! - `mod.rs` —— `read/write_config_byte/word/dword`, `pci_scan_all_buses`, `probe_device` 等接口
//! - `hotplug.rs` —— `PCIe` 热插拔支持
//!
//! ## 安全约束
//! - config 访问包含 unsafe (Port I/O / volatile MMIO)
//! - `pci_scan_all_buses()` 必须在启动早期单线程调用
//! - `DEVICE_LIST` 由 `spin::Mutex` 保护, 线程安全
//! - BAR 地址不可跨设备共享 (由 egdf `IoMem` 别名检测保证)
//!
//! ## 性能特征
//! - 配置空间访问: `x86_64` Port I/O O(1), aarch64 ECAM MMIO O(1)
//! - 总线扫描: O(bus × dev × func), 实际 < 1000 次
//! - 设备查找: Mutex + Vec O(N) (N ≤ 256)

use super::PciDevice;

use crate::framework::sync::IrqSpinLock;
// ============================================================================
// 契约常量
// ============================================================================

pub const CLASS_STORAGE: u8 = 0x01;
pub const CLASS_NETWORK: u8 = 0x02;
pub const CLASS_DISPLAY: u8 = 0x03;
pub const CLASS_BRIDGE: u8 = 0x06;

pub const PCI_CMD_IO_SPACE: u16 = 1 << 0;
pub const PCI_CMD_MEMORY_SPACE: u16 = 1 << 1;
pub const PCI_CMD_BUS_MASTER: u16 = 1 << 2;

// ============================================================================
// 契约: 配置空间读写
// ============================================================================

pub fn read_config_byte(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    super::read_config_byte(bus, dev, func, offset)
}

pub fn read_config_word(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    super::read_config_word(bus, dev, func, offset)
}

pub fn read_config_dword(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    super::read_config_dword(bus, dev, func, offset)
}

pub fn write_config_byte(bus: u8, dev: u8, func: u8, offset: u8, val: u8) {
    super::write_config_byte(bus, dev, func, offset, val);
}

pub fn write_config_word(bus: u8, dev: u8, func: u8, offset: u8, val: u16) {
    super::write_config_word(bus, dev, func, offset, val);
}

pub fn write_config_dword(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
    super::write_config_dword(bus, dev, func, offset, val);
}

// ============================================================================
// 契约 trait: PciScanner — 设备枚举抽象
// ============================================================================

/// PCI 总线扫描器。
///
/// 整个系统只有一个实例 (PCI 是单根总线)。trait 化是为了
/// 在 `kernel_test` 模式下注入 mock 设备。
pub trait PciScanner: Send + Sync {
    /// 枚举所有已发现的 PCI 设备
    fn devices(&self) -> &[PciDevice];

    /// 获取设备数量
    fn device_count(&self) -> usize;
}

// ============================================================================
// 契约: 注册机制
// ============================================================================

static REGISTERED_SCANNER: IrqSpinLock<Option<&'static dyn PciScanner>> = IrqSpinLock::new(None);

/// 注册 PCI 扫描器 (启动时由平台代码调用)
///
/// 必须先注册再调用 `scanner()`。若已有注册则覆盖 (启动期单线程,无竞争)。
pub fn register_scanner(scanner: &'static dyn PciScanner) {
    *REGISTERED_SCANNER.lock() = Some(scanner);
}

/// 获取已注册的 PCI 扫描器
pub fn scanner() -> Option<&'static dyn PciScanner> {
    *REGISTERED_SCANNER.lock()
}
