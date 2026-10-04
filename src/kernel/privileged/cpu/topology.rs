//! CPU 多核拓扑信息检测模块
//!
//! B03-16 拆分 (自 `cpu/mod.rs` 迁出): 承载 `TopologyInfo` 类型定义
//! + 多核拓扑探测 (`detect_topology`, 仅 x86_64, 依赖 cpuid)。

#[cfg(target_arch = "x86_64")]
use super::cpuid;
#[cfg(target_arch = "x86_64")]
use super::{CpuFeatures, CpuSignature, CpuVendor};

/// 多核拓扑信息
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct TopologyInfo {
    /// 物理核心数 (每个 CPU 插槽)
    pub physical_cores: u8,
    /// 逻辑线程总数 (含超线程)
    pub logical_threads: u8,
    /// 本地 APIC ID
    pub apic_id: u8,
    /// 是否启用超线程
    pub hyperthreading_enabled: bool,
    /// 是否为 BSP (Bootstrap Processor, 启动处理器)
    pub is_bsp: bool,
}

impl TopologyInfo {
    /// 获取每物理核心的逻辑线程数
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn threads_per_core(&self) -> u8 {
        if self.physical_cores > 0 && self.logical_threads >= self.physical_cores {
            self.logical_threads / self.physical_cores
        } else {
            1
        }
    }

    /// 检查是否为单核 CPU
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn is_single_core(&self) -> bool {
        self.physical_cores <= 1 && self.logical_threads <= 1
    }
}

/// 探测多核拓扑 (Intel: Leaf 0xB, AMD: Leaf 80000008)
///
/// B03-16 语义: 未知厂商 (`CpuVendor::Unknown`) 时跳过所有厂商特定 CPUID
/// 分支, 回退到"按逻辑线程数假定无超线程" (或超线程时 2 threads/core 假设).
#[cfg(target_arch = "x86_64")]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
)]
pub(super) fn detect_topology(
    topo_out: &mut TopologyInfo,
    _sig: &CpuSignature,
    feat: &CpuFeatures,
    max_std: u32,
    max_ext: u32,
    vendor: CpuVendor,
) {
    topo_out.is_bsp = true; // 我们总是运行在 BSP 上
    topo_out.hyperthreading_enabled =
        feat.contains(CpuFeatures::HTT) && topo_out.logical_threads > 1;

    // Intel: 扩展拓扑 Leaf (0xB)
    if vendor == CpuVendor::Intel && max_std >= 0xB {
        let (_, ebx, ecx, _) = cpuid::cpuid(0xB, 0);

        if ebx != 0 {
            let logical_per_pkg = (ebx & 0xFFFF) as u16;
            let cores_per_pkg = (ecx & 0xFF) as u8;

            if cores_per_pkg > 0 {
                topo_out.physical_cores = cores_per_pkg;

                if logical_per_pkg as u8 > cores_per_pkg {
                    topo_out.hyperthreading_enabled = true;
                }
            }
        }
    }
    // AMD: 核心计数 (Leaf 80000008)
    else if vendor == CpuVendor::Amd && max_ext >= 0x8000_0008 {
        let (_, _, ecx, _) = cpuid::cpuid(0x8000_0008, 0);
        let nc = (ecx & 0xFF) as u8; // NC = CoreCount - 1

        if nc > 0 {
            topo_out.physical_cores = nc + 1;
        }
    }
    // 回退: 假设无超线程
    else if topo_out.hyperthreading_enabled {
        // 有超线程但无法确定物理核心数, 假设 2 threads/core
        topo_out.physical_cores = topo_out.logical_threads / 2;
        if topo_out.physical_cores == 0 {
            topo_out.physical_cores = 1;
        }
    } else {
        topo_out.physical_cores = topo_out.logical_threads;
    }

    // 安全边界检查
    if topo_out.physical_cores == 0 {
        topo_out.physical_cores = 1;
    }
    if topo_out.logical_threads < topo_out.physical_cores {
        topo_out.logical_threads = topo_out.physical_cores;
    }
}
