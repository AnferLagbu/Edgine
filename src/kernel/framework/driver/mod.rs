//! 设备驱动子系统 (Driver Subsystem)
//!
//! 提供完整的硬件驱动支持，按功能模块化组织：
//! - **统一框架**: Driver Trait 和设备管理
//! - **总线驱动**: PCI、PCIe等总线支持
//! - **字符设备**: 串口、VGA等字符设备
//! - **输入设备**: 键盘、鼠标等输入设备
//! - **存储设备**: NVMe、AHCI、ATA等存储设备
//! - **显示设备**: HDMI、DisplayPort等显示接口
//!
//! ## 依赖声明
//!
//! framework 内部依赖: sync, mm, io, chitin, pci, net, timer, tests
//! services 依赖: `services::driver` (安全代理)
//!
//! ## 架构设计
//!
//! ```text
//! Driver Subsystem
//! ├── framework.rs   # 统一接口和基础设施
//! ├── bus/           # 总线驱动
//! │   └── pci.rs     # PCI总线驱动
//! ├── char/          # 字符设备驱动
//! │   ├── serial.rs  # 串口驱动
//! │   └── vga.rs     # VGA驱动
//! ├── input/         # 输入设备驱动
//! │   └── keyboard.rs # 键盘驱动
//! ├── storage/       # 存储设备驱动
//! │   ├── nvme.rs    # NVMe驱动
//! │   ├── ahci.rs    # AHCI/SATA驱动
//! │   └── ata.rs     # ATA/IDE驱动
//! ├── display/       # 显示设备驱动
//! │   ├── hdmi.rs    # HDMI驱动
//! │   └── dp.rs      # DisplayPort驱动
//! ```
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! // 初始化所有驱动
//! driver::init_all();
//!
//! // 使用存储驱动读取数据
//! let mut buf = [0u8; 512];
//! storage::ata::ata_read_sector(0, 0, buf.as_mut_ptr());
//!
//! // 从键盘读取字符
//! if input::keyboard::keyboard_has_char() > 0 {
//!     let ch = input::keyboard::keyboard_read_char();
//!     println!("Key: {}", ch);
//! }
//! ```

// ============================================================================
// 子模块声明
// ============================================================================

/// 统一驱动框架 (Trait, IO 操作, 错误码)
pub mod framework;

/// 总线驱动子系统
pub mod bus;

/// 字符设备驱动子系统
pub mod char;

/// 输入设备驱动子系统
pub mod input;

/// 存储设备驱动子系统
pub mod storage;

/// 显示设备驱动子系统
pub mod display;

/// 网络设备驱动
pub mod net;

/// VirtIO 驱动框架 (跨架构，MMIO transport)
pub mod virtio;

/// 块设备抽象层 (BlockDevice trait + 全局注册表)
pub mod block;

/// 热插拔管理器 (设备插入/移除事件分发)
pub mod hotplug;
/// D10: kexec (直接内核引导)
pub mod kexec;
/// D5: 电源管理 (CpuIdle/CpuFreq/Suspend)
pub mod power;
/// D11: UEFI 运行时服务
pub mod uefi;

// ============================================================================
// 公共 API 导出 (便捷访问)
// ============================================================================

// --- 框架导出 ---
pub use framework::{
    DeviceInfo, DeviceType, Driver, DriverError, Result as DriverResult, inb, outb,
};

// --- 块设备导出 ---
pub use block::{
    BlockDevice, block_device_count, block_device_info, block_device_list, block_device_name,
    block_device_state, hdd_is_present, hdd_read_sector, hdd_total_sectors, hdd_write_sector,
};

// --- 显示设备导出 ---
pub use display::font::Font;
pub use display::framebuffer::{Color, Framebuffer, Rect, colors};
pub use display::{FB_PHYS_ADDR, FB_PHYS_SIZE, display_init, get_framebuffer};

// --- 总线驱动导出 ---
#[cfg(target_arch = "x86_64")]
pub use bus::pci;

// --- 字符设备导出 ---
// §6.4 直接方案 B (2026-09-12): x86_64 char 业务已迁 services/driver/char,
// framework 仅保留 aarch64 pl011 (机制).
#[cfg(target_arch = "aarch64")]
pub use char::pl011::Pl011Driver;

// --- 网络设备导出 ---
// e1000 内部函数 (`e1000_probe` 等) 在 e1000.rs 中以
// `#[cfg(not(feature = "kernel_test"))]` 守卫 (kernel_test 无 PCI 总线);
// 此处 re-export 必须同步 gate, 否则 kernel_test build 失败 (P0-1 修复).
#[cfg(not(feature = "kernel_test"))]
pub use net::e1000::{
    e1000_net_get_mac, e1000_net_irq, e1000_net_recv, e1000_net_send, e1000_probe,
    take_device as e1000_take_device,
};
// 批次 Z ④: 旧 framework virtio-net 驱动 (net.rs + virtio_net_* FFI) 已删除,
// virtio-net 权威在 services (impl NetDeviceOps 经 NetOps 安全桥接入)。

// --- 输入设备导出 ---
#[cfg(target_arch = "x86_64")]
pub use input::keyboard;

// --- 存储设备导出 (DECISION-H 3 号子步: 控制器业务已迁 services, 仅 wire 类型) ---
pub use storage::{H2dFis, NvmeCommand, NvmeCompletion};

// 为了向后兼容，保留一些直接导入
#[cfg(target_arch = "x86_64")]
pub use storage::ata::{
    ATA_PRIMARY_CTRL, ATA_PRIMARY_IO, ATA_SECONDARY_CTRL, ATA_SECONDARY_IO, AtaController,
    AtaDevice, MAX_ATA_DEVICES, WORDS_PER_SECTOR, get_ctrl_base, get_io_base,
};

// --- e1000 内部细节 re-export (供测试使用) ---
#[cfg(not(feature = "kernel_test"))]
pub use net::e1000::{
    E1000_RX_BUFFER_SIZE, E1000_RX_RING_SIZE, E1000_TX_RING_SIZE, E1000Device, E1000RxDesc,
    E1000TxDesc,
};

// --- power/kexec/uefi 公共接口 re-export ---
pub use kexec::*;
pub use power::*;
pub use uefi::*;

// ============================================================================
// 初始化函数
// ============================================================================

/// 初始化所有设备驱动
///
/// 按照依赖顺序初始化各个子系统并注册到 Chitin 全局设备表：
/// 1. 字符设备 (VGA、串口)
/// 2. 总线驱动 (PCI)
/// 3. 存储设备 (framework 仅 ATA 回退路径; PCI AHCI/NVMe 由 services 接管)
/// 4. 输入设备 (键盘)
/// 5. 显示设备 (HDMI、DP)
/// 6. 组合虚拟设备 (RAID0/RAID1)
pub fn init_all() {
    #[cfg(target_arch = "x86_64")]
    {
        // §6.4 直接方案 B: x86_64 字符设备 (vga/serial) 由 services::driver::char::char_init
        // 注册 (crate root lib.rs 编排), 此处不再调用 framework char_init.
        let _ = bus::bus_init();
        let _ = storage::storage_init();
        input::input_init();
    }
    #[cfg(target_arch = "aarch64")]
    {
        char::char_init();
        let _ = storage::storage_init();
    }

    let _ = display::display_init();
    // 2-D USB 整体下沉: USB (xHCI/枚举/类驱动) 权威实装已迁 services::driver::usb,
    // 探测/注册由 crate root lib.rs 调用 services usb_init 接管, framework 侧无 usb 模块。

    hotplug::hotplug_init();

    // NestFS 热插拔监听器注册已反转至 services::fs::init (DECISION-K 项 6:
    // 注册点前置, framework driver 不再反向调用 services nestfs)

    let _ = crate::framework::chitin::devtree_probe_composites();

    // 注册 Block softirq 处理程序
    crate::framework::irq::open_softirq(
        crate::framework::irq::SoftirqVec::Block,
        block_softirq_handler,
    );
}

/// Block softirq 处理程序 — 块设备 IO 完成延迟处理
fn block_softirq_handler() {
    // 当前块设备路径走同步 VFS → NestFS → chitin 直接完成.
    // 此 handler 为异步 IO (io_uring) + DMA 完成中断模式预留.
}

/// 关闭所有设备驱动
///
/// 通过 Chitin 框架统一关闭所有注册的设备。
pub fn shutdown_all() {
    crate::framework::chitin::chitin_shutdown_all();
}

/// 获取系统已检测到的设备列表 (从 Chitin + BlockDevice 读取)
///
/// 返回格式化的设备信息字符串。
#[cfg(feature = "alloc")]
pub fn list_devices() -> alloc::string::String {
    use alloc::format;
    let mut info = alloc::string::String::from("=== Chitin Device Registry ===\n\n");

    let chitin_devs = crate::framework::chitin::chitin_list();
    if chitin_devs.is_empty() {
        info.push_str("  (no devices)\n");
    } else {
        let mut block = Vec::new();
        let mut input = Vec::new();
        let mut net = Vec::new();
        let mut char_dev = Vec::new();
        let mut other = Vec::new();

        for (id, name, proto, state) in &chitin_devs {
            let st = format!("{:?}", state);
            let line = format!("  [id={}] {} proto={:?} state={}", id, name, proto, st);
            match proto {
                crate::framework::chitin::ChitinProto::Block => block.push(line),
                crate::framework::chitin::ChitinProto::Input => input.push(line),
                crate::framework::chitin::ChitinProto::Net => net.push(line),
                crate::framework::chitin::ChitinProto::Char => char_dev.push(line),
                _ => other.push(line),
            }
        }

        if !block.is_empty() {
            info.push_str("Block:\n");
            for s in &block {
                info.push_str(s);
                info.push('\n');
            }
        }
        if !char_dev.is_empty() {
            info.push_str("Char:\n");
            for s in &char_dev {
                info.push_str(s);
                info.push('\n');
            }
        }
        if !net.is_empty() {
            info.push_str("Net:\n");
            for s in &net {
                info.push_str(s);
                info.push('\n');
            }
        }
        if !input.is_empty() {
            info.push_str("Input:\n");
            for s in &input {
                info.push_str(s);
                info.push('\n');
            }
        }
        if !other.is_empty() {
            info.push_str("Other:\n");
            for s in &other {
                info.push_str(s);
                info.push('\n');
            }
        }
    }

    let blk_count = block::block_device_count();
    if blk_count > 0 {
        let bds = block::block_device_list();
        info.push_str(&format!(
            "\nBlock Device Registry: {} device(s)\n",
            blk_count
        ));
        for (id, name, sectors) in &bds {
            info.push_str(&format!(
                "  [id={}] {} sectors={} size={}MB\n",
                id,
                name,
                sectors,
                *sectors as u64 * 512 / (1024 * 1024)
            ));
        }
    }

    info.push('\n');
    info
}

// ============================================================================
// FFI 兼容层 (C 接口)
// ============================================================================

/// C 兼容的初始化函数
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn driver_init() {
    let () = init_all();
}

/// C 兼容的关闭函数
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn driver_shutdown() {
    let () = shutdown_all();
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::driver::input::KeyboardDriver;
    use alloc::vec;
    use alloc::vec::Vec;

    /// 测试用字符设备驱动.
    ///
    /// x86_64 的字符设备业务 (serial/vga) 已下沉 services, framework 不再持有
    /// 可供测试的 Char 驱动实例 (见 `framework/driver/char/mod.rs` 顶部说明);
    /// framework 测试不得反向依赖 services, 故此处以本地 mock 覆盖
    /// `DeviceType::Char` 的 trait 分发路径.
    struct MockCharDriver;

    impl Driver for MockCharDriver {
        fn name(&self) -> &'static str {
            "mock-char"
        }
        fn device_type(&self) -> DeviceType {
            DeviceType::Char
        }
        fn init(&mut self) -> DriverResult<()> {
            Ok(())
        }
        fn shutdown(&mut self) -> DriverResult<()> {
            Ok(())
        }
    }

    #[test]
    fn test_module_structure() {
        assert_eq!(DeviceType::Block.to_string(), "Block");
        assert_eq!(DeviceType::Char.to_string(), "Char");

        let _controller = AtaController::new();

        let _driver = KeyboardDriver::new();

        let char_driver = MockCharDriver;
        assert_eq!(char_driver.device_type(), DeviceType::Char);
    }

    #[test]
    fn test_driver_trait_polymorphism() {
        let ata = AtaController::new();
        let kb = KeyboardDriver::new();
        let com = MockCharDriver;

        let drivers: Vec<&dyn Driver> = vec![&ata, &kb, &com];

        for driver in &drivers {
            assert!(driver.name().len() > 0);
            assert!(matches!(
                driver.device_type(),
                DeviceType::Block | DeviceType::Input | DeviceType::Char
            ));
        }
    }

    #[test]
    fn test_error_handling() {
        let err = DriverError::InvalidParameter;
        let result: DriverResult<u32> = Err(err);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Invalid parameter");
    }

    #[test]
    fn test_device_info_creation() {
        let info = DeviceInfo::new("test", DeviceType::Other);

        assert!(info.id > 0);
        assert_eq!(info.name, "test");
        assert_eq!(info.device_type, DeviceType::Other);
        assert!(!info.initialized);
    }
}
