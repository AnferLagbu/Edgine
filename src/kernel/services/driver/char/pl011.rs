#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! ARM PL011 UART 驱动 — services 层安全代理 (MIG-003)
//!
//! QEMU virt 机器默认使用 PL011 @ 0x0900_0000, 寄存器定义基于 ARM DDI 0183G。
//! 通过 [`IoMem`] 安全代理 MMIO 访问, 100% safe Rust。
//!
//! ## 架构 (§6.4 直接方案 B: services 权威)
//!
//! ```text
//! EGDF 设备表 (egdf_register_driver)
//!   └── services::driver::char::pl011::Pl011Driver
//!         └── framework::iomem::IoMem (MMIO 安全代理)
//!               └── 高半区别名 (phys_to_virt)
//! ```
//!
//! ## 与早期控制台的关系
//!
//! framework `arch::uart` 在 boot 阶段已初始化 PL011 供早期输出 (直接 MMIO, 属机制)。
//! 本驱动负责 EGDF 枚举注册, 基址经
//! [`pl011_phys_base`](crate::framework::driver::pl011_phys_base) 安全面获取
//! (启动期可被设备树覆盖); `init()` 幂等检测 UARTCR.UARTEN, 已启用则跳过重复初始化。

use crate::framework::driver::{DeviceType, Driver, DriverError};
use crate::framework::iomem::IoMem;
use crate::framework::mm::PhysAddr;

// ── PL011 寄存器偏移 (相对 MMIO 基址, ARM DDI 0183G) ──

const UARTDR: usize = 0x000; // 数据寄存器 (Data Register)
const UARTFR: usize = 0x018; // 标志寄存器 (Flag Register)
const UARTIBRD: usize = 0x024; // 整数波特率分频值 (Integer Baud Rate Divisor)
const UARTFBRD: usize = 0x028; // 小数波特率分频值 (Fractional Baud Rate Divisor)
const UARTLCR_H: usize = 0x02C; // 线路控制寄存器 (Line Control Register)
const UARTCR: usize = 0x030; // 控制寄存器 (Control Register)
const UARTIMSC: usize = 0x038; // 中断掩码设置/清除 (Interrupt Mask Set/Clear)

// ── UARTFR 标志位 ──

const UARTFR_TXFF: u32 = 1 << 5; // 发送 FIFO 满 (Transmit FIFO Full)
const UARTFR_RXFE: u32 = 1 << 4; // 接收 FIFO 空 (Receive FIFO Empty)

// ── UARTCR 控制位 ──

const UARTCR_UARTEN: u32 = 1 << 0; // UART 使能
const UARTCR_TXE: u32 = 1 << 8; // 发送使能
const UARTCR_RXE: u32 = 1 << 9; // 接收使能

// ── UARTLCR_H 配置 ──

const UARTLCR_8N1: u32 = 0b11 << 5; // 8 数据位, 无校验, 1 停止位

/// PL011 MMIO 区域大小 (寄存器空间, 4 KiB 页)
pub const PL011_MMIO_LEN: usize = 0x1000;

/// PL011 UART 字符设备驱动 (services 权威)。
///
/// 内部封装 [`IoMem`], 提供类型安全的 MMIO 寄存器读写。
pub struct Pl011Driver {
    regs: IoMem,
    initialized: bool,
}

impl Pl011Driver {
    /// 创建 PL011 驱动实例。
    ///
    /// 基址取自 [`pl011_phys_base`](crate::framework::driver::pl011_phys_base)
    /// (启动期可被设备树覆盖), 经 [`IoMem::from_platform_device`] 建立安全 MMIO 句柄。
    ///
    /// # 返回
    /// - `Some`: 构造成功
    /// - `None`: 基址为 0, 或 MMIO 别名冲突 / 校验失败
    pub fn new() -> Option<Self> {
        let base = crate::framework::driver::pl011_phys_base();
        let regs = IoMem::from_platform_device(PhysAddr(base), PL011_MMIO_LEN, "pl011").ok()?;
        Some(Self {
            regs,
            initialized: false,
        })
    }

    /// 读取单个字节 (阻塞, 等待 RX FIFO 非空)。
    ///
    /// SIMPLIFIED: EGDF char 读写路径当前无生产消费者 (休眠), 本方法暂作为
    /// 公共 API 供后续 CharOps 安全桥接入, 未被内部调用。
    pub fn read_byte(&self) -> u8 {
        while self.regs.read_u32(UARTFR) & UARTFR_RXFE != 0 {
            core::hint::spin_loop();
        }
        self.regs.read_u32(UARTDR) as u8
    }

    /// 写入单个字节 (阻塞, 等待 TX FIFO 非满)。
    ///
    /// SIMPLIFIED: 同 [`Self::read_byte`], 暂作为公共 API 供后续 CharOps 安全桥接入。
    pub fn write_byte(&self, c: u8) {
        while self.regs.read_u32(UARTFR) & UARTFR_TXFF != 0 {
            core::hint::spin_loop();
        }
        self.regs.write_u32(UARTDR, u32::from(c));
    }

    /// 检查 UART 硬件是否已启用 (UARTCR.UARTEN)。
    fn is_hw_enabled(&self) -> bool {
        self.regs.read_u32(UARTCR) & UARTCR_UARTEN != 0
    }

    /// 初始化 PL011 (115200-8N1), 序列同 framework `arch::uart::init`。
    fn hw_init(&self) {
        self.regs.write_u32(UARTCR, 0);
        // 115200 @ 24MHz: 24000000 / (16 * 115200) ≈ 13.02 → IBRD=13, FBRD=0
        self.regs.write_u32(UARTIBRD, 13);
        self.regs.write_u32(UARTFBRD, 0);
        self.regs.write_u32(UARTLCR_H, UARTLCR_8N1 | (1 << 4)); // 8N1 + FIFO enable
        self.regs.write_u32(UARTIMSC, 0);
        self.regs
            .write_u32(UARTCR, UARTCR_UARTEN | UARTCR_TXE | UARTCR_RXE);
    }
}

impl Driver for Pl011Driver {
    fn name(&self) -> &'static str {
        "pl011"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Char
    }

    fn init(&mut self) -> Result<(), DriverError> {
        if !self.is_hw_enabled() {
            self.hw_init();
        }
        self.initialized = true;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), DriverError> {
        self.regs.write_u32(UARTCR, 0);
        self.initialized = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.initialized
    }

    fn status(&self) -> &'static str {
        if self.initialized {
            "PL011 ready (115200-8N1)"
        } else {
            "PL011 not initialized"
        }
    }
}
