#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! 字符设备驱动 — services 层 (Phase 2.1.5 / MIG-003)
//!
//! 包含字符设备的 100% safe API, 为内核早期控制台和字符设备提供统一接口。
//!
//! ## 模块结构
//!
//! - [vga] — VGA 文本模式 (0xB8000 MMIO + 0x3D4/0x3D5 PIO), 0 unsafe (x86_64)
//! - [serial] — 16550 UART 串口 (COM1-COM4 PIO), 0 unsafe (x86_64)
//! - [pl011] — ARM PL011 UART 串口 (0x0900_0000 MMIO), 0 unsafe (aarch64)
//!
//! 评估日期: 2026-06-04 (pl011 于 MIG-003 补入)

#[cfg(target_arch = "aarch64")]
pub mod pl011;
pub mod serial;
pub mod vga;

/// 初始化字符设备子系统并注册到 Chitin (§6.4 直接方案 B: services 权威)
///
/// services 层权威实现:
/// - x86_64: VGA 文本控制台 + COM1 串口
/// - aarch64: PL011 UART 串口
///
/// framework 侧已退位 (删除 driver/char 业务, 保留 IoPort/IoMem 机制 +
/// arch::uart 早期控制台 + [`pl011_phys_base`](crate::framework::driver::pl011_phys_base) 安全基址面)。
///
/// SIMPLIFIED: 注册走 `chitin_register_driver` (无 CharOps 读写绑定)——Chitin char
/// 读写路径 (`chitin_char_write/read`) 当前无生产消费者 (休眠); 待 devfs char 读写
/// 接入时按 §6.2 补 framework 提供的 CharOps 安全桥 trait (unsafe 转换留 framework)。
pub fn char_init() {
    #[cfg(target_arch = "x86_64")]
    {
        use crate::framework::chitin::{ChitinProto, chitin_register_driver};
        use alloc::boxed::Box;
        use serial::{ComPort, SerialConfig, SerialPort};
        use vga::VgaConsole;

        if let Some(vga) = VgaConsole::new() {
            chitin_register_driver("vga", ChitinProto::Char, None, None, Box::new(vga));
        }
        if let Some(com1) = SerialPort::new(ComPort::Com1, SerialConfig::default_115200_8n1()) {
            chitin_register_driver(
                "serial0",
                ChitinProto::Char,
                Some(u64::from(serial::COM1_BASE)),
                Some(4),
                Box::new(com1),
            );
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        use crate::framework::chitin::{ChitinProto, chitin_register_driver};
        use alloc::boxed::Box;

        if let Some(pl011) = pl011::Pl011Driver::new() {
            chitin_register_driver(
                "pl011",
                ChitinProto::Char,
                Some(crate::framework::driver::pl011_phys_base()),
                None,
                Box::new(pl011),
            );
        }
    }
}
