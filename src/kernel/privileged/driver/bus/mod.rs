//! 总线驱动子系统 (Bus Driver Subsystem)
//!
//! 提供系统总线支持：
//! - **PCI**: 外设组件互连总线
//! - **PCIe**: PCI Express总线 (未来)
//!
//! ## 架构
//!
//! ```text
//! Bus Subsystem
//! ├── pci.rs    # PCI总线驱动
//! └── pcie.rs   # PCIe总线驱动 (未来)
//! ```

#[cfg(target_arch = "x86_64")]
pub mod pci;

#[cfg(target_arch = "x86_64")]
use super::infra;
#[cfg(target_arch = "x86_64")]
use crate::klog_info;

/// 初始化系统总线驱动 (`x86_64` 上执行 PCI 枚举)。
/// # Errors
/// PCI 初始化失败 (未枚举到有效设备) 时返回 Err。
#[cfg(target_arch = "x86_64")]
pub fn bus_init() -> infra::Result<()> {
    let count = pci::pci_init();
    if count >= 0 {
        klog_info!(Driver, "PCI bus initialized: {} device(s)", count);
        Ok(())
    } else {
        Err(infra::DriverError::HardwareError)
    }
}

/// AArch64 总线初始化 stub — ARM 平台通过设备树/FDT 发现设备，无需 PCI 枚举。
#[cfg(not(target_arch = "x86_64"))]
pub fn bus_init() {
    // ARM 平台使用 FDT (Flattened Device Tree) 或 ACPI 发现设备
    // PCIe 在 ARM 上存在但通过 ECAM 而非 legacy PCI 访问
    // 当前阶段无操作 — 设备树解析属于后续工作
}
