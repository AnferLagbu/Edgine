//! PL011 UART 驱动 (ARM `PrimeCell` UART)
//!
//! QEMU virt 机器默认使用 PL011 @ 0x09000000。
//! 寄存器定义基于 ARM DDI 0183G。
//! UARTDR 寄存器低 8 位是数据字节, as u8 截断是已知硬件行为.

use core::ptr::{read_volatile, write_volatile};

/// PL011 寄存器基地址
///
/// 初值为 QEMU virt 的物理地址 `0x0900_0000` (identity mapping);
/// 引导期若从设备树探测到其他基址, 由 [`set_base`] 覆盖.
/// 用户态初始化前经 [`switch_to_high_half`] 加 TTBR1 高半区别名
/// (`0xFFFF_0000_0900_0000`), 确保在 `TTBR0_EL1` 切换到用户页表后仍可访问。
pub static PL011_BASE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x0900_0000);

/// 以物理地址设置 PL011 基址 (引导期由设备树探测结果调用)
pub fn set_base(pa: u64) {
    PL011_BASE.store(pa, core::sync::atomic::Ordering::Release);
}

/// 切换到 TTBR1 高半区地址 (在用户态初始化前调用)
///
/// 对当前基址取高半区别名 (VA = `KERNEL_BASE` + PA). 因按位或幂等,
/// 在每次进入 EL0 前重复调用均安全。
pub fn switch_to_high_half() {
    let current = base();
    PL011_BASE.store(
        current | crate::privileged::mm::KERNEL_BASE,
        core::sync::atomic::Ordering::Release,
    );
}

/// 获取 PL011 UART 的当前 MMIO 基址。
#[inline]
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

// === unsafe 依据单源 (本文件全部 MMIO 位点共用) ===
//
// 类别 U「PL011 MMIO volatile 读写」: 地址由 `base()` = `PL011_BASE.load(Acquire)` 加
// 本模块常量偏移构成. `PL011_BASE` 初值 0x0900_0000 是 QEMU virt 的物理地址, 由
// `mmu::init` 的 L2_DEVICE (512 个 2MB Device 块, 覆盖 [0, 0x4000_0000)) 恒等映射;
// 引导期若 DTB 探到别的基址, `entry.rs` 以 `< DEVICE_WINDOW_END` 过滤后经 `set_base`
// 覆盖; 进入 EL0 前 `switch_to_high_half` 再按位或上 KERNEL_BASE (幂等), 使 TTBR0 换成
// 用户表后 EL1 仍能经 TTBR1 访问同一设备. 指针只以 `(base() + offset) as *const u32` /
// `*mut u32` 形态交给 `read_volatile` / `write_volatile`, **不构造引用** ⇒ 无别名与借用期
// 约束; 偏移全部是 4 的倍数 (0x000/0x018/0x024/0x028/0x02C/0x030/0x038), 故只要基址
// 4 字节对齐即满足 u32 自然对齐 —— `set_base` 不校验对齐, 由 DTB 给出的 PL011 帧基址
// (页对齐) 保证. volatile 是必需的: 设备会在程序之外改变寄存器 (TXFF/RXFE 标志), 写
// 寄存器也有设备副作用, 普通读写会被编译器优化掉或重排. 注意「读」并非一律无副作用:
// 读 UARTDR 会弹出 RX FIFO 一项.
//
// 类别 C「组合块」: `init` / `putc` / `getc` 的块只是把 `read` / `write` 与轮询组合起来,
// MMIO 依据继承类别 U; 各自另有前提 (MMU 已启用且基址已映射 / UART 已 init), 写在对应
// 函数的 `# Safety` 段与块注释里.

// ============================================================================
// 寄存器 I/O
// ============================================================================

#[inline]
// SAFETY(契约): `offset` 须取自本模块 UART* 常量 (落在 PL011 帧内), 且 UART 已
// `init`、基址在当前翻译体制下可达; 具体访问形态见体内类别 U.
unsafe fn read(offset: u64) -> u32 {
    // SAFETY: 类别 U. offset 由本模块常量给出 (UARTFR / UARTDR), 落在 PL011 帧内;
    // 本函数只被 `init` / `putc` / `getc` 调用, 三者都在基址已映射的前提下执行.
    // 读 UARTFR 无副作用, 读 UARTDR 会弹出 RX FIFO 一项 (见类别 U).
    unsafe { read_volatile((base() + offset) as *const u32) }
}

#[inline]
// SAFETY(契约): 同 `read` 的基址已映射前提; 额外要求 `offset` 取自本模块常量且
// 调用点保证写入次序 (整字写、无需读-改-写, 具体见体内类别 U).
unsafe fn write(offset: u64, val: u32) {
    // SAFETY: 类别 U. offset 由本模块常量给出 (UARTCR / UARTIBRD / UARTFBRD /
    // UARTLCR_H / UARTIMSC / UARTDR), 均为整字写, 无需读-改-写; 写有设备副作用
    // (UARTCR=0 关 UART, 写 UARTDR 触发发送), 故写入次序由调用方 `init` / `putc` 保证.
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
/// 调用者必须确保在初始化 MMU 之后调用，且 `PL011_BASE` (0x09000000) 已映射。
pub unsafe fn init() {
    // SAFETY: 类别 U + C. 唯一调用方是 `boot/aarch64/entry.rs::entry` 第 3 步: 在
    // `mmu::init` 之后 (0x0900_0000 或 DTB 覆盖值已经 Device 块映射)、在
    // `apply_fdt_overrides` 之后 (`set_base` 已生效)、在 `exception::init` 之前.
    // 次序: UARTCR=0 先关 UART → IBRD/FBRD 写波特率除数 → LCR_H 写 8N1+FIFO →
    // IMSC=0 关中断 (本驱动是 polling 模式) → UARTCR 置 UARTEN|TXE|RXE. 除数按
    // ~24MHz UARTCLK 估算 115200 (24e6/(16*115200)≈13.02 ⇒ IBRD=13, FBRD=0);
    // 本驱动不从 DTB 取 clock-frequency, 故该除数是固定估值而非按实测时钟算出.
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
/// 调用者必须确保 UART 已初始化且 `PL011_BASE` MMIO 区域已映射。
#[inline]
pub unsafe fn putc(c: u8) {
    // SAFETY: 类别 U + C. 轮询 UARTFR.TXFF 直到 TX FIFO 非满, 再写 UARTDR;
    // `u32::from(c)` 是零扩展, 只有低 8 位是数据字节. 轮询**无超时上限**: 设备未就绪
    // 或基址不可达时表现为死循环挂死而非报错 (与 `getc` 同形). 调用方须保证 UART 已
    // `init` 且基址在当前翻译体制下可达 (见 `# Safety` 段).
    unsafe {
        // 等待 TX FIFO 非满
        while read(UARTFR) & UARTFR_TXFF != 0 {
            core::hint::spin_loop();
        }
        write(UARTDR, u32::from(c));
    }
}

/// 接收单字节 (阻塞)
///
/// # Safety
///
/// 调用者必须确保 UART 已初始化且 `PL011_BASE` MMIO 区域已映射。
#[inline]
#[must_use]
pub unsafe fn getc() -> u8 {
    // SAFETY: 类别 U + C. 轮询 UARTFR.RXFE 直到 RX FIFO 非空, 再读 UARTDR (该读弹出
    // FIFO 一项), `low_u8_from_u32` 取低 8 位 —— 纯截断 (`to_le_bytes()[0]`), 无 panic 面.
    // 与 `putc` 同样无超时上限. 本函数 `#[must_use]`: 读有副作用, 丢弃返回值等于静默
    // 吞掉一个输入字节.
    unsafe {
        // 等待 RX FIFO 非空
        while read(UARTFR) & UARTFR_RXFE != 0 {
            core::hint::spin_loop();
        }
        crate::privileged::glue::low_u8_from_u32(read(UARTDR))
    }
}

/// 发送字符串
pub fn puts(s: &str) {
    for c in s.bytes() {
        // SAFETY: `putc` 的 `# Safety` 要求 UART 已 init 且基址在当前翻译体制下可达;
        // `puts` 只在控制台已初始化的路径调用, 逐字节发送不改变该前提.
        unsafe {
            putc(c);
        }
    }
    // SAFETY: 同循环体内的 `putc` 前提 (UART 已 init 且基址可达).
    unsafe {
        putc(b'\r');
    }
    // SAFETY: 同循环体内的 `putc` 前提 (UART 已 init 且基址可达).
    unsafe {
        putc(b'\n');
    }
}
