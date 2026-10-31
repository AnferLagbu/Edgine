//! IOAPIC (I/O 高级可编程中断控制器) 驱动
//!
//! 将外部硬件中断路由到指定 CPU 核心.
//! 支持多 IOAPIC 控制器, 通过 GSI (Global System Interrupt) 路由.

#![deny(missing_docs)]

use crate::privileged::sync::IrqSpinLock;
use core::sync::atomic::{AtomicU32, Ordering};

const IOAPIC_BASE_DEFAULT: u64 = 0xFEC0_0000;

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
/// 极性位: 1 = 低电平有效 (`INTx` 总线语义).
const REDTBL_ACTIVE_LOW: u64 = 1 << 13;

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
pub fn init(base_addr: u64) {
    let _ = init_one(base_addr);
}

/// 注册单个 IOAPIC 控制器, 返回其重定向表条目数 (`IOAPICVER` bit`[23:16]` + 1).
fn init_one(base_addr: u64) -> u8 {
    let base = if base_addr == 0 {
        IOAPIC_BASE_DEFAULT
    } else {
        base_addr
    };

    let ver = ioapic_read_on(base, IOAPIC_VER);
    // 条目数 = 最大重定向表索引 + 1; 字段为 0xFF 时 saturating 到 255 避免 u8 溢出
    let max_irq = crate::privileged::glue::low_u8_from_u32((ver >> 16) & 0xFF).saturating_add(1);

    // 屏蔽所有重定向表条目 (向量字段预置 irq+32, 兼容按线号 unmask 的旧入口)
    for irq in 0..max_irq {
        ioapic_write_on(
            base,
            IOREDTBL_BASE + u32::from(irq) * 2,
            crate::privileged::glue::low_u32_from_u64(
                REDTBL_MASK | DELIVERY_FIXED | (u64::from(irq) + 32),
            ),
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
        IOAPIC_COUNT.store(
            crate::privileged::glue::low_u32_from_usize(idx + 1),
            Ordering::Release,
        );
    }
    max_irq
}

/// 按 ACPI MADT 枚举初始化全部 IOAPIC, 并回填每条控制器的重定向表条目数.
///
/// 幂等: 控制器已注册时直接返回现有数量. 启动期由 `smp_init::init` 在
/// MADT 解析后、任何设备 `INTx` 接线之前调用一次.
///
/// 返回本次 (或此前) 已初始化的控制器数量; 0 表示无 MADT IOAPIC, 中断
/// 投递仍停留在 8259A/轮询兜底路径.
pub fn init_from_acpi() -> u32 {
    if is_initialized() {
        return IOAPIC_COUNT.load(Ordering::Acquire);
    }

    let mut count = 0u32;
    for (idx, info) in crate::privileged::arch::acpi::get_ioapics()
        .iter()
        .enumerate()
    {
        let Some(info) = info else { continue };
        let max_irq = init_one(info.base_addr);
        crate::privileged::arch::acpi::set_ioapic_max_irq(idx, max_irq);
        count += 1;
        crate::klog_info!(
            Boot,
            "[IOAPIC] controller {} base={:#X} id={} gsi_base={} lines={}",
            idx,
            info.base_addr,
            info.id,
            info.gsi_base,
            max_irq
        );
    }

    if count == 0 {
        crate::klog_info!(Boot, "[IOAPIC] no controller in MADT, keep legacy PIC path");
    }
    count
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
            crate::privileged::glue::low_u32_from_u64(entry),
        );
        ioapic_write_on(
            state.base_addr,
            IOREDTBL_BASE + u32::from(local_irq) * 2 + 1,
            (entry >> 32) as u32,
        );
    }
}

/// 按 GSI 屏蔽 IRQ
pub fn mask_irq_gsi(gsi: u32) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        let ioapics = IOAPICS.lock();
        if let Some(ref state) = ioapics[idx] {
            let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
            let val = ioapic_read_on(state.base_addr, reg);
            ioapic_write_on(
                state.base_addr,
                reg,
                val | crate::privileged::glue::low_u32_from_u64(REDTBL_MASK),
            );
        }
    }
}

/// 按 GSI 取消屏蔽 IRQ
pub fn unmask_irq_gsi(gsi: u32) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        let ioapics = IOAPICS.lock();
        if let Some(ref state) = ioapics[idx] {
            let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
            let val = ioapic_read_on(state.base_addr, reg);
            ioapic_write_on(
                state.base_addr,
                reg,
                val & !crate::privileged::glue::low_u32_from_u64(REDTBL_MASK),
            );
        }
    }
}

/// 按 GSI 设置触发模式 (电平/边沿)
pub fn set_irq_level_gsi(gsi: u32, level_triggered: bool) {
    if let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) {
        let ioapics = IOAPICS.lock();
        if let Some(ref state) = ioapics[idx] {
            let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
            let val = ioapic_read_on(state.base_addr, reg);
            if level_triggered {
                ioapic_write_on(
                    state.base_addr,
                    reg,
                    val | crate::privileged::glue::low_u32_from_u64(REDTBL_LEVEL),
                );
            } else {
                ioapic_write_on(
                    state.base_addr,
                    reg,
                    val & !crate::privileged::glue::low_u32_from_u64(REDTBL_LEVEL),
                );
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
// INTx 接线 (PCI 传统中断线 → IOAPIC 重定向表)
// ============================================================================

/// 一条 `INTx` 线的 IOAPIC 重定向参数.
#[derive(Debug, Clone, Copy)]
pub struct IntxRoute {
    /// 投递向量号 (IDT 向量, 即 `IRQ_BASE + irq`)
    pub vector: u8,
    /// 目标 LAPIC ID
    pub apic_id: u8,
    /// 触发模式: true = 电平触发
    pub level_triggered: bool,
    /// 极性: true = 低电平有效
    pub active_low: bool,
}

/// 组装 `INTx` 重定向表项 (低地址域: 向量/电平/极性; 高地址域: 目标 LAPIC).
fn encode_intx_rte(route: IntxRoute) -> u64 {
    let mut entry = u64::from(route.vector) | DELIVERY_FIXED | (u64::from(route.apic_id) << 56);
    if route.level_triggered {
        entry |= REDTBL_LEVEL;
    }
    if route.active_low {
        entry |= REDTBL_ACTIVE_LOW;
    }
    entry
}

/// 将一条 GSI 按完整 `INTx` 参数写入 IOAPIC 重定向表并解除屏蔽.
///
/// 与 [`set_irq_gsi`] 的差异: 该入口一次性写入向量/目标/触发/极性并解除
/// 屏蔽, 供启动期单线程接线使用; 写入次序为"高字 → 屏蔽低字 → 未屏蔽低字",
/// 避免重配已使能条目时出现半新半旧的投递窗口.
///
/// # Errors
/// - GSI 不在任何已注册 IOAPIC 的区间内;
/// - 目标控制器未初始化, 或本地 IRQ 超出其重定向表条目数.
pub fn program_intx_gsi(gsi: u32, route: IntxRoute) -> Result<(), &'static str> {
    let Some((idx, local_irq)) = crate::privileged::arch::acpi::gsi_to_ioapic(gsi) else {
        return Err("IOAPIC: GSI not mapped to any controller");
    };
    let ioapics = IOAPICS.lock();
    let Some(state) = ioapics[idx].as_ref() else {
        return Err("IOAPIC: controller not registered");
    };
    if !state.initialized {
        return Err("IOAPIC: controller not initialized");
    }
    if local_irq >= state.max_irq {
        return Err("IOAPIC: local irq out of redirection table");
    }

    let entry = encode_intx_rte(route);
    let reg = IOREDTBL_BASE + u32::from(local_irq) * 2;
    ioapic_write_on(state.base_addr, reg + 1, (entry >> 32) as u32);
    ioapic_write_on(
        state.base_addr,
        reg,
        crate::privileged::glue::low_u32_from_u64(entry | REDTBL_MASK),
    );
    ioapic_write_on(
        state.base_addr,
        reg,
        crate::privileged::glue::low_u32_from_u64(entry),
    );
    Ok(())
}

/// 解析并接通一条 PCI `INTx` 线的 IOAPIC 路由, 返回所用 GSI.
///
/// `irq` 为固件写在 PCI 配置空间 0x3C 的线号, `vector` 为 IDT 向量.
/// 极性与触发模式取值: MADT ISO 显式指定优先; 未指定时按 PCI `INTx` 总线
/// 语义取"电平触发 + 低电平有效".
// SIMPLIFIED: 不解析 AML `_PRT`/链路设备 `_CRS`, 只消费固件已写入 PCI 配置空间
// 的 `interrupt_line` 与 MADT ISO; 影响面 = 固件经 `_PRT` 改路而线号为陈旧值时
// 路由落空 (网卡仍由轮询兜底收包); 扩展条件 = 出现必须 `_PRT` 的机型或共享引脚
// 多设备分发需求时, 引入 AML 解释器与共享线 handler 链.
///
/// # Errors
/// 同 [`program_intx_gsi`] (GSI 无法映射或控制器未初始化).
pub fn route_intx_irq(irq: u8, vector: u8, apic_id: u8) -> Result<u32, &'static str> {
    let override_flags = crate::privileged::arch::acpi::irq_override(irq).map(|ov| ov.flags);
    // 未指定 (`Conforms`, 含保留编码) 时按 PCI `INTx` 总线语义补齐.
    let level_triggered = !matches!(
        override_flags.map(|f| f.trigger),
        Some(crate::privileged::arch::acpi::IsoTrigger::Edge)
    );
    let active_low = !matches!(
        override_flags.map(|f| f.polarity),
        Some(crate::privileged::arch::acpi::IsoPolarity::ActiveHigh)
    );
    let gsi = crate::privileged::arch::acpi::irq_to_gsi(irq);

    program_intx_gsi(
        gsi,
        IntxRoute {
            vector,
            apic_id,
            level_triggered,
            active_low,
        },
    )?;
    crate::klog_info!(
        Boot,
        "[IOAPIC] INTx irq={} -> gsi={} vector={} apic={} level={} active_low={}",
        irq,
        gsi,
        vector,
        apic_id,
        level_triggered,
        active_low
    );
    Ok(gsi)
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
#[must_use]
pub fn get_id() -> u8 {
    get_id_on(0)
}

/// 向后兼容: 设置第一个 IOAPIC 的 ID
pub fn set_id(id: u8) {
    set_id_on(0, id);
}

/// 向后兼容: 读取第一个 IOAPIC 的仲裁 ID
#[must_use]
pub fn get_arbitration_id() -> u8 {
    get_arbitration_id_on(0)
}

// ============================================================================
// 投递模式常量
// ============================================================================

/// 返回 Fixed 投递模式常量
#[must_use]
pub fn delivery_fixed() -> u64 {
    DELIVERY_FIXED
}
/// 返回 Lowest Priority 投递模式常量
#[must_use]
pub fn delivery_lowest() -> u64 {
    DELIVERY_LOWEST
}
/// 返回 SMI 投递模式常量
#[must_use]
pub fn delivery_smi() -> u64 {
    DELIVERY_SMI
}
/// 返回 NMI 投递模式常量
#[must_use]
pub fn delivery_nmi() -> u64 {
    DELIVERY_NMI
}
/// 返回 INIT 投递模式常量
#[must_use]
pub fn delivery_init() -> u64 {
    DELIVERY_INIT
}
/// 返回 `ExtINT` 投递模式常量 (8259A 兼容)
#[must_use]
pub fn delivery_extint() -> u64 {
    DELIVERY_EXTINT
}

// ============================================================================
// extern "C" FFI 包装 (保持不变)
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 以给定 MMIO 基址初始化 IOAPIC
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_init(base_addr: u64) {
    init(base_addr);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 屏蔽指定 IRQ 线
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_mask_irq(irq: u8) {
    mask_irq(irq);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 解除指定 IRQ 线屏蔽
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_unmask_irq(irq: u8) {
    unmask_irq(irq);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 配置指定 IRQ 线的向量、目标 CPU 与屏蔽状态
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_set_irq(irq: u8, vector: u8, apic_id: u8, masked: bool) {
    set_irq(irq, vector, apic_id, masked);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 配置指定 IRQ 线, 并附带投递模式 (如电平/边沿)
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
/// C ABI 导出: 将指定 IRQ 线路由到目标 CPU
#[unsafe(no_mangle)]
pub extern "C" fn ioapic_route_irq_to_cpu(irq: u8, apic_id: u8) {
    route_irq_to_cpu(irq, apic_id);
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intx_rte_encodes_vector_and_destination() {
        let entry = encode_intx_rte(IntxRoute {
            vector: 43,
            apic_id: 0x12,
            level_triggered: false,
            active_low: false,
        });
        assert_eq!(entry & 0xFF, 43, "向量落在 bit[7:0]");
        assert_eq!((entry >> 56) & 0xFF, 0x12, "目标 LAPIC 落在 bit[63:56]");
        assert_eq!((entry >> 8) & 0x7, 0, "固定投递模式的 bit[10:8] 恒为 0");
    }

    #[test]
    fn intx_rte_encodes_level_and_polarity() {
        let level_low = encode_intx_rte(IntxRoute {
            vector: 43,
            apic_id: 0,
            level_triggered: true,
            active_low: true,
        });
        assert_eq!(level_low & REDTBL_LEVEL, REDTBL_LEVEL, "电平触发置 bit15");
        assert_eq!(
            level_low & REDTBL_ACTIVE_LOW,
            REDTBL_ACTIVE_LOW,
            "低电平有效置 bit13"
        );
        assert_eq!(level_low & REDTBL_MASK, 0, "编码本身不带屏蔽位");

        let edge_high = encode_intx_rte(IntxRoute {
            vector: 33,
            apic_id: 0,
            level_triggered: false,
            active_low: false,
        });
        assert_eq!(edge_high & (REDTBL_LEVEL | REDTBL_ACTIVE_LOW), 0);
    }
}
