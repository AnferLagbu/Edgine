//! MSI/MSI-X — PCI 消息信号中断
//!
//! 实现 MSI 和 MSI-X 中断机制, 替代传统 `INTx` 引脚中断.
//!
//! ## MSI vs MSI-X
//!
//! | 特性 | MSI | MSI-X |
//! |------|-----|-------|
//! | 最大向量数 | 1/2/4/8/16/32 | 2048 |
//! | 地址/数据 | 配置空间内 | MMIO Table |
//! | 向量掩码 | 全局 | 每向量 |
//! | Capability ID | 0x05 | 0x11 |
//!
//! ## MSI Capability 结构 (PCI 配置空间)
//!
//! ```text
//! Offset  Field
//! 0x00    Capability ID (0x05) + Next Ptr
//! 0x02    Message Control (16-bit)
//! 0x04    Message Address (32-bit, 低32位)
//! 0x08    Message Upper Address (32-bit, 高32位, 64-bit capable)
//! 0x0C    Message Data (16-bit)
//! 0x0E    Mask Bits (32-bit, 可选, per-vector masking)
//! 0x12    Pending Bits (32-bit, 可选)
//! ```
//!
//! ## MSI-X Capability 结构
//!
//! ```text
//! Offset  Field
//! 0x00    Capability ID (0x11) + Next Ptr
//! 0x02    Message Control (16-bit)
//! 0x04    Table Offset + BAR Indicator (32-bit)
//! 0x08    PBA Offset + BAR Indicator (32-bit)
//! ```
//!
//! ## 与 `IrqLine` 统一
//!
//! MSI 向量注册到 IDT, 复用 IRQ 分发框架.
//! 驱动通过 `msi_alloc_vector()` 获取向量, 再注册 ISR.
//!
//! # Safety
//!
//! - PCI 配置空间访问使用 MMIO/Port I/O (unsafe)
//! - MSI-X Table/PBA 通过 MMIO 访问 (需要映射)

// MSI-X 实现占位, 待中断路由重构后启用。
// MSI/MSI-X 中断基础设施
// msi_alloc_vector/msi_free_vector 已被 functions/driver/acpi.rs 使用
// MsixTable/MsixEntry 等类型待 NVMe/VirtIO 驱动接入后使用

use core::sync::atomic::{AtomicU32, Ordering};

use crate::privileged::pci;

// ============================================================================
// Capability ID 常量
// ============================================================================

/// MSI Capability ID
const PCI_CAP_ID_MSI: u8 = 0x05;
/// MSI-X Capability ID
const PCI_CAP_ID_MSIX: u8 = 0x11;

// ============================================================================
// MSI Message Control 位
// ============================================================================

/// MSI Enable
const MSI_CTRL_ENABLE: u16 = 1 << 0;
/// 64-bit 地址能力
const MSI_CTRL_64BIT: u16 = 0x0080;

// ============================================================================
// MSI-X Message Control 位
// ============================================================================

/// MSI-X Enable
const MSIX_CTRL_ENABLE: u16 = 1 << 15;
/// MSI-X Function Mask (bit 14). 当为 1 时, 所有 vector 视为 masked.
const MSIX_CTRL_MASKALL: u16 = 1 << 14;
/// MSI-X Table Size (bits 0-10)
const MSIX_CTRL_TSIZE: u16 = 0x07FF;

// ============================================================================
// MSI 向量分配
// ============================================================================

/// MSI 向量分配起始 (避免与 IRQ 0-31 异常冲突)
const MSI_VECTOR_BASE: u8 = 0x40;
/// MSI 向量池大小 (vector 0x40-0x9F = 96 vectors, irq32-irq127)
///
/// 约束链: IDT MSI stub 仅编程 vector 0x40-0x9F (isr.asm + idt/mod.rs),
/// `register_msi_irq` 校验 irq ∈ [32,128) (idt.rs)。vector = 0x40 + bit,
/// 若 bit ≥ 96 则 vector ≥ 0xA0 → irq ≥ 128 → register_msi_irq 拒绝,
/// 分配出的 vector 不可用。故 COUNT 必须 ≤ 96 (与 IDT stub 精确一致)。
/// 委托人所列 128 与 IDT 不一致 (2026-08-25 接手修复), 扩展 IDT stub 时同步上调。
const MSI_VECTOR_COUNT: u8 = 96;

/// 全局 MSI 向量分配位图
static MSI_VECTORS: AtomicU32 = AtomicU32::new(0);

/// 分配一个 MSI 向量
///
/// 返回分配的向量号, 或 None (向量耗尽).
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn msi_alloc_vector() -> Option<u8> {
    let mut bitmap = MSI_VECTORS.load(Ordering::Acquire);
    loop {
        // 找到第一个空闲位
        let bit = (0..u32::from(MSI_VECTOR_COUNT)).find(|&i| bitmap & (1 << i) == 0)?;
        let new_bitmap = bitmap | (1 << bit);
        match MSI_VECTORS.compare_exchange(bitmap, new_bitmap, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                let vector = MSI_VECTOR_BASE + bit as u8;
                crate::klog_debug!(Driver, "[MSI] Allocated vector {}", vector);
                return Some(vector);
            }
            Err(current) => {
                bitmap = current;
            }
        }
    }
}

/// 释放 MSI 向量
pub fn msi_free_vector(vector: u8) {
    let bit = u32::from(vector - MSI_VECTOR_BASE);
    if bit < u32::from(MSI_VECTOR_COUNT) {
        MSI_VECTORS.fetch_and(!(1 << bit), Ordering::AcqRel);
        crate::klog_debug!(Driver, "[MSI] Freed vector {}", vector);
    }
}

// ============================================================================
// PCI Capability 链遍历
// ============================================================================

/// 查找 PCI 设备的指定 Capability
///
/// 遍历 Capability 链表, 返回 Capability 的配置空间偏移.
/// 返回 None 表示未找到.
pub fn pci_find_capability(dev: &pci::PciDevice, cap_id: u8) -> Option<u8> {
    if dev.capabilities_ptr == 0 {
        return None;
    }

    let mut offset = dev.capabilities_ptr;
    let mut iterations = 0;

    while offset != 0 && iterations < 48 {
        let cap = pci::read_config_word(dev.bus, dev.device, dev.function, offset);
        let id = (cap & 0xFF) as u8;
        let next = ((cap >> 8) & 0xFF) as u8;

        if id == cap_id {
            return Some(offset);
        }

        offset = next;
        iterations += 1;
    }

    None
}

// ============================================================================
// MSI 配置
// ============================================================================

/// MSI 配置结果
#[derive(Debug, Clone, Copy)]
pub struct MsiConfig {
    /// 分配的中断向量
    pub vector: u8,
    /// Capability 偏移
    pub cap_offset: u8,
    /// 是否 64-bit 地址能力
    pub is_64bit: bool,
}

/// 为 PCI 设备启用 MSI
///
/// 1. 查找 MSI Capability
/// 2. 分配中断向量
/// 3. 配置 Message Address/Data
/// 4. 启用 MSI
///
/// 返回 MSI 配置, 或 None (无 MSI 能力/向量耗尽).
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn msi_enable(dev: &pci::PciDevice) -> Option<MsiConfig> {
    let cap_offset = pci_find_capability(dev, PCI_CAP_ID_MSI)?;

    // 读取 Message Control
    let ctrl = pci::read_config_word(dev.bus, dev.device, dev.function, cap_offset + 0x02);
    let is_64bit = (ctrl & MSI_CTRL_64BIT) != 0;

    // 分配向量
    let vector = msi_alloc_vector()?;

    // 配置 Message Address
    // x86_64: LAPIC 地址 = 0xFEE00000, 目标 CPU = 0
    // 格式: [31:20] = 0xFEE, [19:12] = Destination ID, [11:0] = 0
    #[cfg(target_arch = "x86_64")]
    let msg_addr: u32 = 0xFEE00000; // CPU 0

    #[cfg(target_arch = "aarch64")]
    let msg_addr: u32 = 0; // ARM GIC ITS: 后续集成

    // 配置 Message Data
    // x86_64: [7:0] = Vector, [10:8] = Delivery Mode (000=fixed), [14:13] = 触发模式
    let msg_data = u32::from(vector);

    // 写入 Message Address (低32位)
    pci::write_config_dword(
        dev.bus,
        dev.device,
        dev.function,
        cap_offset + 0x04,
        msg_addr,
    );

    // 写入 Message Upper Address (64-bit capable)
    if is_64bit {
        pci::write_config_dword(dev.bus, dev.device, dev.function, cap_offset + 0x08, 0);
    }

    // 写入 Message Data
    let data_offset = if is_64bit { 0x0C } else { 0x08 };
    pci::write_config_word(
        dev.bus,
        dev.device,
        dev.function,
        cap_offset + data_offset,
        msg_data as u16,
    );

    // 启用 MSI (设置 Enable 位)
    let new_ctrl = ctrl | MSI_CTRL_ENABLE;
    pci::write_config_word(
        dev.bus,
        dev.device,
        dev.function,
        cap_offset + 0x02,
        new_ctrl,
    );

    // 禁用 INTx (PCI 命令寄存器)
    let cmd = pci::read_config_word(dev.bus, dev.device, dev.function, 0x04);
    pci::write_config_word(dev.bus, dev.device, dev.function, 0x04, cmd | 0x0400);

    crate::klog_info!(
        Driver,
        "[MSI] Enabled for {:02x}:{:02x}.{:01x} vector={}",
        dev.bus,
        dev.device,
        dev.function,
        vector
    );

    Some(MsiConfig {
        vector,
        cap_offset,
        is_64bit,
    })
}

/// 禁用 MSI
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是既有 API 约定 (改传值会波及调用点); 当前优先 expect"
)]
pub fn msi_disable(dev: &pci::PciDevice, config: &MsiConfig) {
    let ctrl = pci::read_config_word(dev.bus, dev.device, dev.function, config.cap_offset + 0x02);
    let new_ctrl = ctrl & !MSI_CTRL_ENABLE;
    pci::write_config_word(
        dev.bus,
        dev.device,
        dev.function,
        config.cap_offset + 0x02,
        new_ctrl,
    );

    msi_free_vector(config.vector);

    crate::klog_info!(
        Driver,
        "[MSI] Disabled for {:02x}:{:02x}.{:01x}",
        dev.bus,
        dev.device,
        dev.function
    );
}

// ============================================================================
// MSI-X 配置
// ============================================================================

/// MSI-X Table Entry (MMIO)
///
/// 每个条目 16 字节:
/// - Offset 0: Message Address (低32位)
/// - Offset 4: Message Upper Address (高32位)
/// - Offset 8: Message Data (32位)
/// - Offset 12: Vector Control (32 位, bit 0 = mask)
#[repr(C)]
struct MsixTableEntry {
    msg_addr_lo: u32,
    msg_addr_hi: u32,
    msg_data: u32,
    vector_ctrl: u32,
}

/// MSI-X 配置结果
#[derive(Debug, Clone, Copy)]
pub struct MsixConfig {
    /// 分配的起始向量
    pub base_vector: u8,
    /// MSI-X Table BAR 索引
    pub table_bar: u8,
    /// MSI-X Table 偏移
    pub table_offset: u32,
    /// MSI-X PBA BAR 索引
    pub pba_bar: u8,
    /// MSI-X PBA 偏移
    pub pba_offset: u32,
    /// 向量数
    pub num_vectors: u16,
    /// Capability 偏移
    pub cap_offset: u8,
}

/// 为 PCI 设备启用 MSI-X
///
/// 1. 查找 MSI-X Capability
/// 2. 解析 Table/PBA 位置
/// 3. 分配向量
/// 4. 配置 Table Entry
/// 5. 启用 MSI-X
pub fn msix_enable(dev: &pci::PciDevice, num_vectors: u16) -> Option<MsixConfig> {
    let cap_offset = pci_find_capability(dev, PCI_CAP_ID_MSIX)?;

    // 读取 Message Control
    let ctrl = pci::read_config_word(dev.bus, dev.device, dev.function, cap_offset + 0x02);
    let table_size = ((ctrl & MSIX_CTRL_TSIZE) + 1) as u16;
    let actual_vectors = num_vectors.min(table_size);

    // 解析 Table Offset/BAR
    let table_info = pci::read_config_dword(dev.bus, dev.device, dev.function, cap_offset + 0x04);
    let table_bar = (table_info & 0x07) as u8;
    let table_offset = table_info & !0x07;

    // 解析 PBA Offset/BAR
    let pba_info = pci::read_config_dword(dev.bus, dev.device, dev.function, cap_offset + 0x08);
    let pba_bar = (pba_info & 0x07) as u8;
    let pba_offset = pba_info & !0x07;

    // 分配向量
    let base_vector = msi_alloc_vector()?;

    // 配置 MSI-X Table Entry (第一个向量)
    // 需要映射 Table BAR 的 MMIO 区域
    if (table_bar as usize) < dev.bar_count {
        let bar = &dev.bars[table_bar as usize];
        if bar.bar_type == pci::BarType::Memory32 || bar.bar_type == pci::BarType::Memory64 {
            let table_virt = (bar.base_addr + u64::from(table_offset)) as *mut MsixTableEntry;

            // SAFETY: table_virt 指向 MSI-X Table MMIO 区域
            unsafe {
                // 写入第一个 entry
                let entry = &mut *table_virt;
                #[cfg(target_arch = "x86_64")]
                {
                    // MSIX-03 修复: MSI-X addr 应含 destination 字段 (bits 19:12 = LAPIC ID).
                    // 此前硬编码 0xFEE00000 (dest=0) 在多 LAPIC 系统上不投递 (QEMU 默认
                    // LAPIC ID=0x01000000 拆 bits 19:12 = 0x10, dest=0 无法匹配任何 CPU).
                    let lapic_id = crate::privileged::arch::apic::get_id() as u32;
                    entry.msg_addr_lo = 0xFEE00000 | ((lapic_id & 0xFF) << 12);
                    entry.msg_addr_hi = 0;
                }
                #[cfg(target_arch = "aarch64")]
                {
                    entry.msg_addr_lo = 0;
                    entry.msg_addr_hi = 0;
                }
                entry.msg_data = u32::from(base_vector);
                entry.vector_ctrl = 0; // Unmask
            }
        }
    }

    // 启用 MSI-X: 清除 MASKALL (bit14) 同时设 ENABLE (bit15).
    // QEMU 默认 msix_mask_all 把 vector_ctrl maskbit 全置 1, function_masked=true.
    // 我们已写 Table entry vector_ctrl=0 (unmask), 若不清 MASKALL, QEMU 仍视为 function masked
    // → msix_notify 跳过投递. 必须 ENABLE=1 且 MASKALL=0.
    let new_ctrl = (ctrl | MSIX_CTRL_ENABLE) & !MSIX_CTRL_MASKALL;
    pci::write_config_word(
        dev.bus,
        dev.device,
        dev.function,
        cap_offset + 0x02,
        new_ctrl,
    );

    // 禁用 INTx
    let cmd = pci::read_config_word(dev.bus, dev.device, dev.function, 0x04);
    pci::write_config_word(dev.bus, dev.device, dev.function, 0x04, cmd | 0x0400);

    crate::klog_info!(
        Driver,
        "[MSI-X] Enabled for {:02x}:{:02x}.{:01x} vectors={}/{} base_vec={}",
        dev.bus,
        dev.device,
        dev.function,
        actual_vectors,
        table_size,
        base_vector
    );

    Some(MsixConfig {
        base_vector,
        table_bar,
        table_offset,
        pba_bar,
        pba_offset,
        num_vectors: actual_vectors,
        cap_offset,
    })
}

/// 禁用 MSI-X
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是既有 API 约定 (改传值会波及调用点); 当前优先 expect"
)]
pub fn msix_disable(dev: &pci::PciDevice, config: &MsixConfig) {
    let ctrl = pci::read_config_word(dev.bus, dev.device, dev.function, config.cap_offset + 0x02);
    let new_ctrl = ctrl & !MSIX_CTRL_ENABLE;
    pci::write_config_word(
        dev.bus,
        dev.device,
        dev.function,
        config.cap_offset + 0x02,
        new_ctrl,
    );

    msi_free_vector(config.base_vector);

    crate::klog_info!(
        Driver,
        "[MSI-X] Disabled for {:02x}:{:02x}.{:01x}",
        dev.bus,
        dev.device,
        dev.function
    );
}

/// 屏蔽 MSI-X 向量
pub fn msix_mask_vector(dev: &pci::PciDevice, config: &MsixConfig, index: u16) {
    if index >= config.num_vectors {
        return;
    }
    if (config.table_bar as usize) < dev.bar_count {
        let bar = &dev.bars[config.table_bar as usize];
        if bar.bar_type == pci::BarType::Memory32 || bar.bar_type == pci::BarType::Memory64 {
            let table_virt =
                (bar.base_addr + u64::from(config.table_offset)) as *mut MsixTableEntry;
            // SAFETY: MMIO 访问
            unsafe {
                let entry = &mut *table_virt.add(index as usize);
                entry.vector_ctrl |= 1; // Mask
            }
        }
    }
}

/// 解除屏蔽 MSI-X 向量
pub fn msix_unmask_vector(dev: &pci::PciDevice, config: &MsixConfig, index: u16) {
    if index >= config.num_vectors {
        return;
    }
    if (config.table_bar as usize) < dev.bar_count {
        let bar = &dev.bars[config.table_bar as usize];
        if bar.bar_type == pci::BarType::Memory32 || bar.bar_type == pci::BarType::Memory64 {
            let table_virt =
                (bar.base_addr + u64::from(config.table_offset)) as *mut MsixTableEntry;
            // SAFETY: MMIO 访问
            unsafe {
                let entry = &mut *table_virt.add(index as usize);
                entry.vector_ctrl &= !1; // Unmask
            }
        }
    }
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_msi_vector_alloc_free() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    // 重置位图
    MSI_VECTORS.store(0, Ordering::SeqCst);

    let v1 = msi_alloc_vector();
    check!(v1.is_some(), "alloc v1");
    check!(v1.unwrap() == MSI_VECTOR_BASE, "v1 == base");

    let v2 = msi_alloc_vector();
    check!(v2.is_some(), "alloc v2");
    check!(v2.unwrap() == MSI_VECTOR_BASE + 1, "v2 == base+1");

    msi_free_vector(v1.unwrap());
    let v3 = msi_alloc_vector();
    check!(v3.is_some(), "alloc v3 after free");
    check!(v3.unwrap() == MSI_VECTOR_BASE, "v3 reuses freed");

    msi_free_vector(v2.unwrap());
    msi_free_vector(v3.unwrap());

    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_msi_ctrl_bits() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};

    assert_eq_test!(MSI_CTRL_ENABLE, 0x0001, "MSI enable bit");
    assert_eq_test!(MSI_CTRL_64BIT, 0x0080, "MSI 64-bit bit");
    assert_eq_test!(MSIX_CTRL_ENABLE, 0x8000, "MSI-X enable bit");
    assert_eq_test!(MSIX_CTRL_TSIZE, 0x07FF, "MSI-X table size mask");

    TestResult::Pass
}

/// 注册 MSI 子系统的内核测试用例 (仅 `kernel_test` 模式).
#[cfg(feature = "kernel_test")]
pub fn register_msi_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("msi", "vector_alloc_free", test_msi_vector_alloc_free);
    r.register("msi", "ctrl_bits", test_msi_ctrl_bits);
}
