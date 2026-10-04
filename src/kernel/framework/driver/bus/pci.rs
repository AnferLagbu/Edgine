//! PCI 总线驱动 (PCI Bus Driver)
//!
//! 对接真实的 PCI 子系统 (`crate::framework::pci`)，
//! 提供设备枚举、配置空间访问和 C FFI 导出。
//!
//! 通过 EGDF 框架注册为 Bus 类型设备。

#![cfg(target_arch = "x86_64")]

use crate::framework::driver::{DeviceType, Driver, DriverError};
use crate::klog_info;

struct PciBusDriver;

impl Driver for PciBusDriver {
    fn name(&self) -> &'static str {
        "pci-bus"
    }
    fn device_type(&self) -> DeviceType {
        DeviceType::Bus
    }
    fn init(&mut self) -> core::result::Result<(), DriverError> {
        Ok(())
    }
    fn shutdown(&mut self) -> core::result::Result<(), DriverError> {
        Ok(())
    }
    fn is_ready(&self) -> bool {
        true
    }
}

/// 初始化 PCI 子系统并注册到 EGDF
///
/// 调用内核 PCI 模块执行总线扫描和设备枚举,
/// 然后将 PCI 总线注册到 EGDF 框架。
/// 返回发现的设备数量。
///
/// # 并发约束 (B04-AUDIT-005 #3)
///
/// 本函数**必须在 BSP 单线程阶段调用** (AP 未启动). 调用方 `driver::init_all()`
/// 由 `src/rust/src/lib.rs` 启动序列串行调用, 此时 AP 尚未启动. 即使 AP 启动后
/// 并发触发 PCI 扫描, `framework::pci::init()` 入口的 `PCI_INITIALIZED` 原子检查
/// + `DEVICE_LIST` 内部 `IrqSpinLock` 保证扫描与查询的并发安全.
///
/// 调用方契约: 不得在 SMP 启动后并发调用本函数. 如需运行时重新扫描, 应通过
/// `pci::scan_all_buses()` 单独加锁调用.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn pci_init() -> i32 {
    let count = crate::framework::pci::init() as i32;

    crate::framework::egdf::egdf_register_driver(
        "pci-bus",
        crate::framework::egdf::EGDFProto::Bus,
        Some(0xCF8),
        None,
        alloc::boxed::Box::new(PciBusDriver),
    );

    // 演进 6: PCI init 完成后做 driver 维度自检
    // DECISION-K: 经 ConfigValidateHook trait 注入 (Option 可空, 未注册跳过)
    if let Some(hook) = crate::framework::config::current_config_validate_hook() {
        if let Err(e) = hook.validate_pci_subsystem() {
            crate::klog_drv_warn!("PCI validation: {}", e);
        }
    }

    count
}

/// 扫描所有 PCI 总线并返回设备列表
pub fn pci_scan() {
    let devices = crate::framework::pci::scan_all_buses();
    for dev in &devices {
        klog_info!(
            Driver,
            "PCI {:02X}:{:02X}.{} {:04X}:{:04X} class {:02X}.{:02X}",
            dev.bus,
            dev.device,
            dev.function,
            dev.vendor_id,
            dev.device_id,
            dev.class_code,
            dev.subclass_code
        );
    }
}

// 注意: pci_read_config_word / pci_write_config_word C FFI 符号
// 由 `kernel::pci::mod` 提供 (`#[no_mangle] pub extern "C"`)。
// 在此重导出会导致符号重复。
