//! CPU 缓存信息检测模块
//!
//! B03-16 拆分 (自 `cpu/mod.rs` 迁出): 承载 `CacheInfo` 类型定义
//! + 缓存配置检测 (`detect_cache`, 仅 x86_64, 依赖 cpuid)。

#[cfg(target_arch = "x86_64")]
use super::CpuVendor;
#[cfg(target_arch = "x86_64")]
use super::cpuid;

/// 缓存配置信息
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CacheInfo {
    /// L1 数据缓存大小 (bytes)
    pub l1d_size: u32,
    /// L1 指令缓存大小 (bytes)
    pub l1i_size: u32,
    /// L2 统一缓存大小 (bytes)
    pub l2_size: u32,
    /// L3 缓存大小 (bytes, 0表示不存在)
    pub l3_size: u32,
    /// L1 数据关联度 (路数, 如 4-way)
    pub l1d_associativity: u8,
    /// L2 关联度
    pub l2_associativity: u8,
    /// L3 关联度 (0表示不存在或全相联)
    pub l3_associativity: u8,
    /// 缓存行大小 (bytes, 通常 64)
    pub cache_line_size: u16,
}

impl CacheInfo {
    /// 获取总缓存容量 (L1+L2+L3, bytes)
    #[inline]
    pub const fn total_size(&self) -> u64 {
        self.l1d_size as u64 + self.l1i_size as u64 + self.l2_size as u64 + self.l3_size as u64
    }

    /// 检查是否有 L3 缓存
    #[inline]
    pub const fn has_l3(&self) -> bool {
        self.l3_size > 0
    }
}

/// 检测缓存配置 (Intel: Leaf 4, AMD: Leaf 80000005/6)
///
/// B03-16 语义: 未知厂商 (`CpuVendor::Unknown`) 时跳过所有厂商特定 CPUID
/// 分支, 保留函数开头的保守默认值 (常见 L1 32KB / L2 256KB / 4-way 等),
/// 仅补充缓存行大小探测 (标准 leaf 1, 所有 x86-64 通用).
#[cfg(target_arch = "x86_64")]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
pub(super) fn detect_cache(
    cache_out: &mut CacheInfo,
    max_std: u32,
    max_ext: u32,
    vendor: CpuVendor,
) {
    // 设置默认保守值
    *cache_out = CacheInfo {
        l1d_size: 32 * 1024,  // 32KB
        l1i_size: 32 * 1024,  // 32KB
        l2_size: 256 * 1024,  // 256KB
        l3_size: 0,           // 不确定
        l1d_associativity: 4, // 4-way
        l2_associativity: 8,  // 8-way
        l3_associativity: 0,
        cache_line_size: 64, // 标准 x86-64
    };

    // Intel: 使用 Deterministic Cache Parameter (Leaf 4)
    if vendor == CpuVendor::Intel && max_std >= 4 {
        for subleaf in 0..=3u32 {
            // 通常前几个subleaf包含L1/L2/L3
            let (eax, ebx, ecx, _) = cpuid::cpuid(4, subleaf);

            let cache_type = eax & 0x1F;
            if cache_type == 0 {
                break;
            } // 无更多缓存

            let cache_level = (eax >> 5) & 0x7;
            let line_part = (ebx & 0xFFF) + 1;
            let assoc = ((ebx >> 12) & 0x3FF) + 1;
            let sets = ecx + 1;
            let size = sets * assoc * line_part * ((ebx >> 22) + 1);

            match (cache_type, cache_level) {
                (1, 1) => cache_out.l1d_size = size, // L1 Data
                (2, 1) => cache_out.l1i_size = size, // L1 Instruction
                (3, 2) => {
                    // L2 Unified
                    cache_out.l2_size = size;
                    cache_out.l2_associativity = assoc as u8;
                }
                (3, 3) => {
                    // L3 Unified
                    cache_out.l3_size = size;
                    cache_out.l3_associativity = assoc as u8;
                }
                _ => {}
            }
        }
    }
    // AMD: 使用扩展缓存信息 (Leaf 80000005/6)
    else if vendor == CpuVendor::Amd && max_ext >= 0x8000_0006 {
        // L1 数据/指令缓存 (Leaf 80000005)
        let (_, _, ecx_l1, edx_l1) = cpuid::cpuid(0x8000_0005, 0);
        cache_out.l1d_size = (ecx_l1 >> 24) * 1024; // KB → Bytes
        cache_out.l1i_size = (edx_l1 >> 24) * 1024;

        // L2 Unified (Leaf 80000006)
        let (_, _, ecx_l2, _) = cpuid::cpuid(0x8000_0006, 0);
        cache_out.l2_size = (ecx_l2 >> 16) * 1024;

        // L3 (Leaf 80000008, 可选)
        if max_ext >= 0x8000_0008 {
            let (_, _, ecx_l3, _) = cpuid::cpuid(0x8000_0008, 0);
            let l3_size_kb = (ecx_l3 >> 18) * 512; // 单位: 512KB
            if l3_size_kb > 0 {
                cache_out.l3_size = l3_size_kb * 1024; // KB → Bytes
            }
        }
    }

    // 获取缓存行大小 (几乎所有 x86-64 都是 64 字节)
    if max_std >= 1 {
        let (_, ebx, _, _) = cpuid::cpuid(1, 0);
        cache_out.cache_line_size = (8 * ((ebx >> 8) & 0xFF)) as u16;
    }

    // 最终安全检查
    if cache_out.cache_line_size == 0 {
        cache_out.cache_line_size = 64;
    }
}
