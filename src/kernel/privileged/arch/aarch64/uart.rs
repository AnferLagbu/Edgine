//! PL011 UART 驱动 (ARM PrimeCell UART)
//!
//! QEMU virt 机器默认使用 PL011 @ 0x09000000。
//! 寄存器定义基于 ARM DDI 0183G。
//! UARTDR 寄存器低 8 位是数据字节, as u8 截断是已知硬件行为.

use core::ptr::{read_volatile, write_volatile};

/// PL011 寄存器基地址
///
/// 初值为 QEMU virt 的物理地址 0x0900_0000 (identity mapping);
/// 引导期若从设备树探测到其他基址, 由 [`set_base`] 覆盖.
/// 用户态初始化前经 [`switch_to_high_half`] 加 TTBR1 高半区别名
/// (0xFFFF_0000_0900_0000), 确保在 TTBR0_EL1 切换到用户页表后仍可访问。
pub static PL011_BASE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x0900_0000);

/// 以物理地址设置 PL011 基址 (引导期由设备树探测结果调用)
pub fn set_base(pa: u64) {
    PL011_BASE.store(pa, core::sync::atomic::Ordering::Release);
}

/// 切换到 TTBR1 高半区地址 (在用户态初始化前调用)
///
/// 对当前基址取高半区别名 (VA = KERNEL_BASE + PA). 因按位或幂等,
/// 在每次进入 EL0 前重复调用均安全。
pub fn switch_to_high_half() {
    let current = base();
    PL011_BASE.store(
        current | crate::privileged::mm::KERNEL_BASE,
        core::sync::atomic::Ordering::Release,
    );
}

/// 获取 PL011 UART 的当前 MMIO 基址。
#[inline(always)]
pub fn base() -> u64 {
    PL011_BASE.load(core::sync::atomic::Ordering::Acquire)
}

/// PL011 寄存器偏移
const UARTDR: u64 = 0x000; // Data Register
const UARTFR: u64 = 0x018; // Flag Register
const UARTIBRD: u64 = 0x024; // 整数波特率分频值 (Integer Baud Rate Divisor)
const UARTFBRD: u64 = 0x028; // 小数波特率分频值 (Fractional Baud Rate Divisor)
const UARTLCR_H: u64 = 0x02C; // 线路控制寄存器 (Line Control Register)
pub const UARTCR: u64 = 0x030; // Control Register
const UARTIMSC: u64 = 0x038; // 中断掩码设置/清除 (Interrupt Mask Set/Clear)

/// 标志位
const UARTFR_TXFF: u32 = 1 << 5; // 发送 FIFO 满 (Transmit FIFO Full)
const UARTFR_RXFE: u32 = 1 << 4; // 接收 FIFO 空 (Receive FIFO Empty)

/// 控制位
pub const UARTCR_UARTEN: u32 = 1 << 0; // UART Enable
const UARTCR_TXE: u32 = 1 << 8; // Transmit Enable
const UARTCR_RXE: u32 = 1 << 9; // Receive Enable

/// LCR 配置: 8N1 (8 data bits, no parity, 1 stop bit)
const UARTLCR_8N1: u32 = 0b11 << 5;

// ============================================================================
// 寄存器 I/O
// ============================================================================

#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn read(offset: u64) -> u32 {
    unsafe { read_volatile((base() + offset) as *const u32) }
}

#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn write(offset: u64, val: u32) {
    unsafe {
        write_volatile((base() + offset) as *mut u32, val);
    }
}

// ============================================================================
// UART 初始化
// ============================================================================

/// 初始化 PL011 UART (115200-8N1)
///
/// # Safety
///
/// 调用者必须确保在初始化 MMU 之后调用，且 PL011_BASE (0x09000000) 已映射。
pub unsafe fn init() {
    unsafe {
        // 1. 禁用 UART
        write(UARTCR, 0);

        // 2. 设置波特率 (115200 @ 24MHz 或 62.5MHz 时钟)
        // QEMU virt 的 PL011 时钟频率取决于具体配置, 默认 ~24MHz
        // 波特率除数 = UARTCLK / (16 * BaudRate)
        // 以 24MHz 为例: 24000000 / (16 * 115200) ≈ 13.02 → IBRD=13, FBRD≈0
        write(UARTIBRD, 13); // 整数部分
        write(UARTFBRD, 0); // 小数部分 (0.02 * 64 ≈ 1, 但 QEMU 不严格要求)

        // 3. 设置数据格式: 8N1, FIFO enable
        write(UARTLCR_H, UARTLCR_8N1 | (1 << 4)); // 8N1 + FIFO enable

        // 4. 禁用中断 (Polling 模式)
        write(UARTIMSC, 0);

        // 5. 启用 UART: TX + RX + UART
        write(UARTCR, UARTCR_UARTEN | UARTCR_TXE | UARTCR_RXE);
    }
}

// ============================================================================
// 数据收发
// ============================================================================

/// 发送单字节 (阻塞)
///
/// # Safety
///
/// 调用者必须确保 UART 已初始化且 PL011_BASE MMIO 区域已映射。
#[inline(always)]
#[expect(
    clippy::cast_lossless,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
pub unsafe fn putc(c: u8) {
    unsafe {
        // 等待 TX FIFO 非满
        while read(UARTFR) & UARTFR_TXFF != 0 {
            core::hint::spin_loop();
        }
        write(UARTDR, c as u32);
    }
}

/// 接收单字节 (阻塞)
///
/// # Safety
///
/// 调用者必须确保 UART 已初始化且 PL011_BASE MMIO 区域已映射。
#[inline(always)]
pub unsafe fn getc() -> u8 {
    unsafe {
        // 等待 RX FIFO 非空
        while read(UARTFR) & UARTFR_RXFE != 0 {
            core::hint::spin_loop();
        }
        read(UARTDR) as u8
    }
}

/// 发送字符串
pub fn puts(s: &str) {
    for c in s.bytes() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            putc(c);
        }
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        putc(b'\r');
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        putc(b'\n');
    }
}
