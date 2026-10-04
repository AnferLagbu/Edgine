//! 字符设备驱动子系统 (Character Device Driver Subsystem)
//!
//! ## 架构 (2026 §6.4 直接方案 B 后)
//!
//! 字符设备业务已整体下沉 services (services/driver/char: serial/vga/pl011 权威)。
//! framework 仅保留 PL011 UART 早期控制台机制 ([arch::uart](crate::framework::arch::uart),
//! boot 阶段直接 MMIO 输出) 与平台 MMIO 基址安全面 ([`pl011_phys_base`]),
//! 后者供 services pl011 驱动构造 `IoMem` 使用。
//!
//! ## 历史
//!
//! - x86_64 serial.rs / vga.rs 已删除 (2026-09-12): 业务迁 services,
//!   framework 保留 IoPort/IoMem 机制; EGDF 注册由 services::driver::char::char_init 完成。
//! - MIG-003: aarch64 PL011 业务迁 services (services/driver/char/pl011.rs),
//!   framework 删除 driver/char/pl011.rs, 仅暴露 [`pl011_phys_base`] 安全基址面。

/// 返回 PL011 UART 物理基址 (aarch64 安全基址面)。
///
/// 归一为**物理地址**: 剥离 [`switch_to_high_half`](crate::framework::arch::uart::switch_to_high_half)
/// 可能已附加的 TTBR1 高半区别名。services 层据此构造
/// [`IoMem`](crate::framework::iomem::IoMem) 时, `IoMem` 内部会再做 `phys_to_virt`
/// 得到高别名, 故此处必须给出物理地址 (两次换算互逆, 不会重复附加)。
///
/// 基址来源为 [`arch::uart::PL011_BASE`](crate::framework::arch::uart::PL011_BASE),
/// 启动期可被设备树探测结果覆盖 (见 boot/aarch64 entry)。
#[cfg(target_arch = "aarch64")]
pub fn pl011_phys_base() -> u64 {
    crate::framework::arch::uart::base() & !crate::framework::mm::KERNEL_BASE
}
