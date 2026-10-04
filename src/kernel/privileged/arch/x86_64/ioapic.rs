//! IOAPIC (I/O 高级可编程中断控制器) 驱动
//!
//! 将外部硬件中断路由到指定 CPU 核心.
//! 支持多 IOAPIC 控制器, 通过 GSI (Global System Interrupt) 路由.

use crate::privileged::sync::IrqSpinLock;
use core::sync::atomic::{AtomicU32, Ordering};

const IOAPIC_BASE_DEFAULT: u64 = 0xFEC00000;

const IOREGSEL: u32 = 0x00;
const IOWIN: u32 = 0x10;

const IOAPIC_VER: u32 = 0x01;
/// IOAPIC ID 寄存器 (读写, bit`[24:27]` 为 APIC ID)
const IOAPIC_ID: u32 = 0x00;
/// IOAPIC 仲裁 ID 寄存器 (只读, bit`[24:27]` 为仲裁 ID)
const IOAPIC_ARB: u32 = 0x02;
const IOREDTBL_BASE: u32 = 0x10;

const REDTBL_MASK: u64 = 1 << 16;
const REDTBL_LEVEL: u64 = 1 << 15;

// IOAPIC 投递模式 (重定向表 bit[8:10], 与 Local APIC LVT 编码一致)
const DELIVERY_FIXED: u64 = 0x000;
/// 最低优先级投递 — 同一 APIC ID 组中选择最低优先级 CPU
const DELIVERY_LOWEST: u64 = 0x100;
/// 系统管理中断 — 固件与 OS 通信机制
const DELIVERY_SMI: u64 = 0x200;
/// 不可屏蔽中断 — 硬件错误报告 (ECC 内存错误、看门狗超时)
const DELIVERY_NMI: u64 = 0x400;
/// INIT 投递 — 用于处理器初始化序列
const DELIVERY_INIT: u64 = 0x500;
/// 8259A 兼容中断 — 遗留 ISA 设备 (`ExtINT` 需配合 Local APIC LINT0)
const DELIVERY_EXTINT: u64 = 0x700;

/// 最大 IOAPIC 控制器数量
const MAX_IOAPICS: usize = 8;

/// 单个 IOAPIC 控制器的运行时状态
struct IoApicState {
    base_addr: u64,
    max_irq: u8,
    initialized: bool,
}

/// 所有 IOAPIC 控制器状态
static IOAPICS: IrqSpinLock<[Option<IoApicState>; MAX_IOAPICS]> =
    IrqSpinLock::new([const { None }; MAX_IOAPICS]);
/// 已注册的 IOAPIC 控制器数量
static IOAPIC_COUNT: AtomicU32 = AtomicU32::new(0);

// ============================================================================
// 底层 MMIO 读写 (参数化, 支持多控制器)
// ============================================================================

/// 在指定基地址上读取 IOAPIC 寄存器
fn ioapic_read_on(base: u64, reg: u32) -> u32 {
    // SAFETY: base 来自 ACPI MADT 枚举的 MMIO 地址, 调用方保证有效
    unsafe {
        core::ptr::write_volatile((base + u64::from(IOREGSEL)) as *mut u32, reg);
        core::ptr::read_volatile((base + u64::from(IOWIN)) as *const u32)
    }
}

/// 在指定基地址上写入 IOAPIC 寄存器
fn ioapic_write_on(base: u64, reg: u32, value: u32) {
    // SAFETY: 同上
    unsafe {
        core::ptr::write_volatile((base + u64::from(IOREGSEL)) as *mut u32, reg);
        core::ptr::write_volatile((base + u64::from(IOWIN)) as *mut u32, value);
    }
}

// ============================================================================
// 初始化
// ============================================================================

/// 注册一个新的 IOAPIC 控制器
///
/// 每次从 ACPI MADT 枚举到 IOAPIC 时调用此函数.
/// 多次调用会依次注册多个控制器.
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn init(base_addr: u64) {
    let base = if base_addr == 0 {
        IOAPIC_BASE_DEFAULT
    } else {
        base_addr
    };

    let ver = ioapic_read_on(base, IOAPIC_VER);
    let max_irq = ((ver >> 16) & 0xFF) as u8 + 1;

    // 屏蔽所有重定向表条目
    for irq in 0..24u8 {
        ioapic_write_on(
            base,
            IOREDTBL_BASE + u32::from(irq) * 2,
            (REDTBL_MASK | DELIVERY_FIXED | (u64::from(irq) + 32)) as u32,
        );
    }

    let idx = IOAPIC_COUNT.load(Ordering::Acquire) as usize;
    if idx < MAX_IOAPICS {
        let mut ioapics = IOAPICS.lock();
        ioapics[idx] = Some(IoApicState {
            base_addr: base,
            max_irq,
            initialized: true,
        });
        drop(ioapics);
        IOAPIC_COUNT.store((idx + 1) as u32, Ordering::Release);
    }
}

/// 是否至少有一个 IOAPIC 已初始化
pub fn is_initialized() -> bool {
    IOAPIC_COUNT.load(Ordering::Acquire) > 0
}

/// 返回所有已注册 IOAPIC 中最大的 `max_irq` 值
pub fn get_max_irq() -> u8 {
    let ioapics = IOAPICS.lock();
    ioapics
        .iter()
        .flatten()
        .map(|s| s.max_irq)
        .max()
        .unwrap_or(0)
}

// ============================================================================
// GSI-based API (多 IOAPIC 路由)
// ============================================================================

/// 按 GSI 设置 IRQ (自动路由到正确的 IOAPIC)
pub fn set_irq_gsi(gsi: u32, vector: u8, apic_id: u8, masked: bool) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        set_irq_on(idx, local_irq, vector, apic_id, masked, DELIVERY_FIXED);
    }
}

/// 按 IOAPIC 索引 + 本地 IRQ 设置
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn set_irq_on(
    ioapic_idx: usize,
    local_irq: u8,
    vector: u8,
    apic_id: u8,
    masked: bool,
    mode: u64,
) {
    let ioapics = IOAPICS.lock();
    if let Some(ref state) = ioapics[ioapic_idx] {
        if !state.initialized {
            return;
        }
        let mut entry = u64::from(vector) | mode | (u64::from(apic_id) << 56);
        if masked {
            entry |= REDTBL_MASK;
        }
        ioapic_write_on(
            state.base_addr,
            IOREDTBL_BASE + u32::from(local_irq) * 2,
            entry as u32,
        );
        ioapic_write_on(
            state.base_addr,
            IOREDTBL_BASE + u32::from(local_irq) * 2 + 1,
            (entry >> 32) as u32,
        );
    }
}

/// 按 GSI 屏蔽 IRQ
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn mask_irq_gsi(gsi: u32) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        let ioapics = IOAPICS.lock();
        if let Some(ref state) = ioapics[idx] {
            let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
            let val = ioapic_read_on(state.base_addr, reg);
            ioapic_write_on(state.base_addr, reg, val | REDTBL_MASK as u32);
        }
    }
}

/// 按 GSI 取消屏蔽 IRQ
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn unmask_irq_gsi(gsi: u32) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        let ioapics = IOAPICS.lock();
        if let Some(ref state) = ioapics[idx] {
            let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
            let val = ioapic_read_on(state.base_addr, reg);
            ioapic_write_on(state.base_addr, reg, val & !(REDTBL_MASK as u32));
        }
    }
}

/// 按 GSI 设置触发模式 (电平/边沿)
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub fn set_irq_level_gsi(gsi: u32, level_triggered: bool) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        let ioapics = IOAPICS.lock();
        if let Some(ref state) = ioapics[idx] {
            let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
            let val = ioapic_read_on(state.base_addr, reg);
            if level_triggered {
                ioapic_write_on(state.base_addr, reg, val | REDTBL_LEVEL as u32);
            } else {
                ioapic_write_on(state.base_addr, reg, val & !(REDTBL_LEVEL as u32));
            }
        }
    }
}

/// 按 GSI 路由 IRQ 到指定 CPU
pub fn route_irq_to_cpu_gsi(gsi: u32, apic_id: u8) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        let ioapics = IOAPICS.lock();
        if let Some(ref state) = ioapics[idx] {
            let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
            let val = ioapic_read_on(state.base_addr, reg);
            let new_val = (val & !(0xFFu32 << 24)) | ((u32::from(apic_id) & 0x0F) << 24);
            ioapic_write_on(state.base_addr, reg, new_val);
        }
    }
}

// ============================================================================
// 向后兼容 API (假设单 IOAPIC, GSI = IRQ)
// ============================================================================

/// 向后兼容: 按 IRQ 设置 (假设单 IOAPIC, GSI = IRQ)
pub fn set_irq(irq: u8, vector: u8, apic_id: u8, masked: bool) {
    set_irq_gsi(u32::from(irq), vector, apic_id, masked);
}

/// 向后兼容: 按 IRQ 设置投递模式
pub fn set_irq_with_mode(irq: u8, vector: u8, apic_id: u8, masked: bool, mode: u64) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(u32::from(irq)) {
        set_irq_on(idx, local_irq, vector, apic_id, masked, mode);
    }
}

/// 向后兼容: 屏蔽 IRQ
pub fn mask_irq(irq: u8) {
    mask_irq_gsi(u32::from(irq));
}

/// 向后兼容: 取消屏蔽 IRQ
pub fn unmask_irq(irq: u8) {
    unmask_irq_gsi(u32::from(irq));
}

/// 向后兼容: 设置触发模式
pub fn set_irq_level(irq: u8, level_triggered: bool) {
    set_irq_level_gsi(u32::from(irq), level_triggered);
}

/// 向后兼容: 路由 IRQ 到 CPU
pub fn route_irq_to_cpu(irq: u8, apic_id: u8) {
    route_irq_to_cpu_gsi(u32::from(irq), apic_id);
}

// ============================================================================
// IOAPIC ID / 仲裁 API
// ============================================================================

/// 读取指定 IOAPIC 的 ID (bit`[24:27]`)
///
/// 多 IOAPIC 系统中, 每个 IOAPIC 有唯一 ID, 用于中断路由决策.
pub fn get_id_on(ioapic_idx: usize) -> u8 {
    let ioapics = IOAPICS.lock();
    ioapics[ioapic_idx].as_ref().map_or(0, |state| {
        let val = ioapic_read_on(state.base_addr, IOAPIC_ID);
        ((val >> 24) & 0x0F) as u8
    })
}

/// 设置指定 IOAPIC 的 ID (bit`[24:27]`)
///
/// # Safety
///
/// 多 IOAPIC 系统中, ID 冲突会导致中断路由错误.
/// 仅在 MADT 枚举阶段由初始化代码调用.
pub fn set_id_on(ioapic_idx: usize, id: u8) {
    let ioapics = IOAPICS.lock();
    if let Some(ref state) = ioapics[ioapic_idx] {
        let val = ioapic_read_on(state.base_addr, IOAPIC_ID);
        let new_val = (val & !(0x0F << 24)) | ((u32::from(id) & 0x0F) << 24);
        ioapic_write_on(state.base_addr, IOAPIC_ID, new_val);
    }
}

/// 读取指定 IOAPIC 的仲裁 ID (只读, bit`[24:27]`)
pub fn get_arbitration_id_on(ioapic_idx: usize) -> u8 {
    let ioapics = IOAPICS.lock();
    ioapics[ioapic_idx].as_ref().map_or(0, |state| {
        let val = ioapic_read_on(state.base_addr, IOAPIC_ARB);
        ((val >> 24) & 0x0F) as u8
    })
}

/// 向后兼容: 读取第一个 IOAPIC 的 ID
pub fn get_id() -> u8 {
    get_id_on(0)
}

/// 向后兼容: 设置第一个 IOAPIC 的 ID
pub fn set_id(id: u8) {
    set_id_on(0, id);
}

/// 向后兼容: 读取第一个 IOAPIC 的仲裁 ID
pub fn get_arbitration_id() -> u8 {
    get_arbitration_id_on(0)
}

// ============================================================================
// 投递模式常量
// ============================================================================

/// 返回 Fixed 投递模式常量
pub fn delivery_fixed() -> u64 {
    DELIVERY_FIXED
}
/// 返回 Lowest Priority 投递模式常量
pub fn delivery_lowest() -> u64 {
    DELIVERY_LOWEST
}
/// 返回 SMI 投递模式常量
pub fn delivery_smi() -> u64 {
    DELIVERY_SMI
}
/// 返回 NMI 投递模式常量
pub fn delivery_nmi() -> u64 {
    DELIVERY_NMI
}
/// 返回 INIT 投递模式常量
pub fn delivery_init() -> u64 {
    DELIVERY_INIT
}
/// 返回 `ExtINT` 投递模式常量 (8259A 兼容)
pub fn delivery_extint() -> u64 {
    DELIVERY_EXTINT
}

// ============================================================================
// extern "C" FFI 包装 (保持不变)
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_init(base_addr: u64) {
    init(base_addr);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_mask_irq(irq: u8) {
    mask_irq(irq);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_unmask_irq(irq: u8) {
    unmask_irq(irq);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_set_irq(irq: u8, vector: u8, apic_id: u8, masked: bool) {
    set_irq(irq, vector, apic_id, masked);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_set_irq_with_mode(
    irq: u8,
    vector: u8,
    apic_id: u8,
    masked: bool,
    mode: u64,
) {
    set_irq_with_mode(irq, vector, apic_id, masked, mode);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_route_irq_to_cpu(irq: u8, apic_id: u8) {
    route_irq_to_cpu(irq, apic_id);
}
