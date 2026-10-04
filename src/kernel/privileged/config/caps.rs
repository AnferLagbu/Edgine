//! 调试摘要与运行时能力查询 — privileged 机制类型与函数
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原类型定义 (`ConfigSummary`, `KernelCapabilities`) 于 T6-9 (2026-06-16) 迁至
//! `functions::config::caps`, 本文件仅 re-export + 运行时查询函数。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：`KernelCapabilities`
//! 被 privileged mm/vmm_x86_64 的 KPTI 决策直接消费、`ConfigSummary` 被 privileged
//! config 机制 (init/print_config_table) 消费 — 属机制类型, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::config::caps::*` 保持 API 兼容。
//! 本文件 0 unsafe (纯类型 + 编译期 cfg 检测 + 运行时查询函数).

// ============================================================================
// 类型定义
// ============================================================================

/// 配置摘要结构.
#[derive(Debug, Clone, Copy)]
pub struct ConfigSummary {
    pub max_cpus: usize,
    pub actual_cpus: u32,
    pub max_irqs: usize,
    pub max_processes: usize,
    pub max_threads: usize,
    pub apic_enabled: bool,
    pub ioapic_enabled: bool,
    pub page_size: u64,
    /// 演进 9: 运行时 KASLR 偏移 (由 bootloader/entry 设置).
    pub kaslr_offset: u64,
    pub capabilities: KernelCapabilities,
}

/// 编译期 + 运行时能力标志.
#[derive(Debug, Clone, Copy)]
pub struct KernelCapabilities {
    /// 编译期启用了 SMP.
    pub smp: bool,
    /// Preempt-RT 内核.
    pub preempt: bool,
    /// 内核地址空间布局随机化.
    pub kaslr: bool,
    /// `x86_64` KPTI 缓解措施.
    pub kpti: bool,
    /// `Edgine` FREG 子系统已编译入.
    pub freg: bool,
}

impl KernelCapabilities {
    /// 从编译期 `cfg` 标志检测能力.
    pub const fn detect() -> Self {
        Self {
            smp: cfg!(feature = "smp"),
            preempt: cfg!(feature = "preempt"),
            kaslr: cfg!(feature = "kaslr"),
            // 测试模式下禁用 KPTI: 避免 KPTI 初始化修改共享页表导致 bitmap 映射被破坏
            kpti: cfg!(all(target_arch = "x86_64", not(feature = "kernel_test"))),
            freg: cfg!(feature = "freg"),
        }
    }
}

// ============================================================================
// 运行时查询函数
// ============================================================================

use super::capacity::{MAX_CPUS, MAX_IRQS, MAX_PROCESSES, MAX_THREADS};
use super::kaslr::get_kaslr_offset;
use super::memory::PAGE_SIZE;

/// 获取配置摘要用于调试.
pub fn get_config_summary() -> ConfigSummary {
    ConfigSummary {
        max_cpus: MAX_CPUS,
        actual_cpus: crate::privileged::smp::get_cpu_count(),
        max_irqs: MAX_IRQS,
        max_processes: MAX_PROCESSES,
        max_threads: MAX_THREADS,
        apic_enabled: apic_initialized(),
        ioapic_enabled: ioapic_initialized(),
        page_size: PAGE_SIZE,
        kaslr_offset: get_kaslr_offset(),
        capabilities: KernelCapabilities::detect(),
    }
}

/// 跨架构安全: 在 `x86_64` 上查 APIC 状态, 其他架构默认 false。
#[cfg(target_arch = "x86_64")]
fn apic_initialized() -> bool {
    crate::privileged::arch::apic::is_initialized()
}

#[cfg(not(target_arch = "x86_64"))]
fn apic_initialized() -> bool {
    false
}

#[cfg(target_arch = "x86_64")]
fn ioapic_initialized() -> bool {
    crate::privileged::arch::ioapic::is_initialized()
}

#[cfg(not(target_arch = "x86_64"))]
fn ioapic_initialized() -> bool {
    false
}
