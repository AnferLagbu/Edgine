#![deny(unsafe_code)]
#![cfg(target_arch = "x86_64")]
//! ACPI 电源管理 — functions 层安全代理
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `privileged::arch::acpi`。
//!
//! ## 职责
//!
//! - 查询 ACPI 解析状态 (RSDP / MADT / FADT / HPET / DMAR)
//! - 提供类型安全的 `HpetInfo` / `ApInfo` 包装
//! - 暴露安全 shutdown / reboot API (不直接操作 `PM1a_CNT`)
//!
//! ## 注意
//!
//! aarch64 走 FDT, 不使用 ACPI. 本模块在 aarch64 下编译为 0 内容.

// ============================================================================
// ACPI 解析状态查询
// ============================================================================

/// 是否找到 RSDP (Root System Description Pointer)
#[inline]
pub fn has_rsdp() -> bool {
    crate::privileged::arch::acpi::find_rsdp(0).is_some()
}

/// 是否解析过 MADT (Multiple APIC Description Table)
#[inline]
pub fn has_madt() -> bool {
    crate::privileged::arch::acpi::has_madt()
}

/// 是否解析过 FADT (电源管理)
#[inline]
pub fn has_fadt() -> bool {
    // B04-21: 委托 privileged::arch::acpi::has_fadt(), 替代原硬编码 `true`.
    // 旧实现未查询框架层 → 即使未解析 FADT 也返回 true, 电源管理走 ACPI 路径
    // → 可能空指针 deref PM1a_CNT 寄存器.
    crate::privileged::arch::acpi::has_fadt()
}

/// 是否解析过 HPET (高精度事件定时器)
#[inline]
pub fn has_hpet() -> bool {
    crate::privileged::arch::acpi::get_hpet_info().is_some()
}

/// 是否解析过 DMAR (IOMMU DRHD)
#[inline]
pub fn has_dmar() -> bool {
    // 简化: DMAR 解析与启动日志挂钩, 当前仅返回 false 占位
    false
}

// ============================================================================
// MADT 多核信息
// ============================================================================

/// 获取 AP (Application Processor) 数量
#[inline]
pub fn ap_count() -> u32 {
    crate::privileged::arch::acpi::get_ap_count()
}

/// LAPIC 基址
#[inline]
pub fn lapic_base() -> u64 {
    crate::privileged::arch::acpi::get_lapic_base()
}

/// IOAPIC 基址
#[inline]
pub fn ioapic_addr() -> u64 {
    crate::privileged::arch::acpi::get_ioapic_addr()
}

/// IOAPIC 全局系统中断基址
#[inline]
pub fn ioapic_gsib() -> u32 {
    crate::privileged::arch::acpi::get_ioapic_gsib()
}

// ============================================================================
// IOAPIC 信息查询 (多 IOAPIC 支持)
// ============================================================================

/// IOAPIC 信息 (safe 拷贝)
#[derive(Debug, Clone, Copy)]
pub struct IoApicInfoSafe {
    pub id: u8,
    pub base_addr: u64,
    pub gsi_base: u32,
    pub max_irq: u8,
}

/// 获取所有 IOAPIC 信息
pub fn ioapic_list() -> [Option<IoApicInfoSafe>; 8] {
    let fw = crate::privileged::arch::acpi::get_ioapics();
    let mut result = [None; 8];
    for (i, item) in fw.iter().enumerate() {
        if i < 8 {
            result[i] = item.map(|info| IoApicInfoSafe {
                id: info.id,
                base_addr: info.base_addr,
                gsi_base: info.gsi_base,
                max_irq: info.max_irq,
            });
        }
    }
    result
}

/// IOAPIC 数量
pub fn ioapic_count() -> u32 {
    crate::privileged::arch::acpi::get_ioapic_count()
}

// ============================================================================
// HPET 高精度定时器信息
// ============================================================================

/// HPET 信息 (拷贝自框架层, 避免 functions 持有裸指针)
#[derive(Debug, Clone, Copy)]
pub struct HpetInfoSafe {
    pub base_addr: u64,
    pub hpet_number: u8,
    pub comparator_count: u8,
    pub counter_size: u8,
}

/// 获取 HPET 信息 (safe 拷贝)
#[inline]
pub fn hpet_info() -> Option<HpetInfoSafe> {
    crate::privileged::arch::acpi::get_hpet_info().map(|info| HpetInfoSafe {
        base_addr: info.base_addr,
        hpet_number: info.hpet_number,
        comparator_count: info.comparator_count,
        counter_size: info.counter_size,
    })
}

// ============================================================================
// PCI MSI 分配
// ============================================================================

/// MSI 分配结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsiResult {
    /// 成功, vector 0..=255
    Allocated(u8),
    /// 失败, 池已满
    PoolExhausted,
    /// 不支持
    Unsupported,
}

/// 分配一个 MSI 中断向量 (0..=255)
pub fn msi_alloc_vector() -> MsiResult {
    crate::privileged::pci::msi::msi_alloc_vector()
        .map_or(MsiResult::PoolExhausted, MsiResult::Allocated)
}

/// 释放 MSI 中断向量
pub fn msi_free_vector(vector: u8) {
    crate::privileged::pci::msi::msi_free_vector(vector);
}

// ============================================================================
// 电源管理 (shutdown / reboot)
// ============================================================================

/// ACPI 关机 (S5 soft-off)
///
/// # Safety
///
/// 写 `PM1a_CNT` S5 位后将不可恢复地停止系统.
pub fn acpi_shutdown() -> ! {
    crate::privileged::arch::acpi::acpi_shutdown()
}

/// ACPI 重启
pub fn acpi_reboot() -> ! {
    crate::privileged::arch::acpi::acpi_reboot()
}

// ============================================================================
// 综合状态报告
// ============================================================================

/// ACPI 整体状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiStatus {
    pub has_rsdp: bool,
    pub has_madt: bool,
    pub has_fadt: bool,
    pub has_hpet: bool,
    pub has_dmar: bool,
    pub ap_count: u32,
}

/// 获取 ACPI 整体状态 (供 sysinfo / procfs 使用)
pub fn acpi_status() -> AcpiStatus {
    AcpiStatus {
        has_rsdp: has_rsdp(),
        has_madt: has_madt(),
        has_fadt: has_fadt(),
        has_hpet: has_hpet(),
        has_dmar: has_dmar(),
        ap_count: ap_count(),
    }
}
