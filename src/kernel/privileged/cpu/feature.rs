//! CPU 特性标志集与特性收集模块
//!
//! B03-16 拆分 (自 `cpu/mod.rs` 迁出): 承载 `CpuFeatures` 位标志集定义
//! + 特性收集逻辑 (`collect_features`, 多 CPUID leaf 汇总)。
//! `CpuFeatures` 类型双架构编译; `collect_features` 仅 x86_64 (依赖 cpuid)。

#[cfg(target_arch = "x86_64")]
use super::cpuid;

bitflags::bitflags! {
    /// CPU 特性标志集
    ///
    /// 包含 x86/x86-64 所有重要特性位。
    /// 通过 CPUID 不同 leaf 收集。
    ///
    /// # Example
    /// ```ignore
    /// let features = CpuFeatures::empty();
    /// features.set(CpuFeatures::SSE, true);
    /// assert!(features.contains(CpuFeatures::SSE));
    /// ```
    #[derive(Debug, Clone, Copy, Default)]
    pub struct CpuFeatures: u128 {  // u128 支持最多128个特性标志

        // ====== 基础特性 (Leaf 1 EDX, bits 0-31) ======

        /// FPU (浮点单元) 可用
        const FPU         = 1 << 0;
        /// DE (调试扩展) 支持
        const DE          = 1 << 2;
        /// PSE (页面大小扩展) 支持
        const PSE         = 1 << 3;
        /// TSC (时间戳计数器) 可用
        const TSC         = 1 << 4;
        /// MSR (模型特定寄存器) 支持
        const MSR         = 1 << 5;
        /// PAE (物理地址扩展) 支持
        const PAE         = 1 << 6;
        /// MCE (机器检查异常) 支持
        const MCE         = 1 << 7;
        /// CMPXCHG8B 指令支持
        const CX8         = 1 << 8;
        /// APIC (本地APIC) 支持
        const APIC        = 1 << 9;
        /// SEP (快速系统调用) 支持
        const SEP         = 1 << 11;
        /// MTRR (内存类型范围寄存器) 支持
        const MTRR        = 1 << 12;
        /// PGE (全局页面启用) 支持
        const PGE         = 1 << 13;
        /// MCA (机器检查架构) 支持
        const MCA         = 1 << 14;
        /// CMOV (条件移动指令) 支持
        const CMOV        = 1 << 15;
        /// PAT (页属性表) 支持
        const PAT         = 1 << 16;
        /// PSE-36 (36位页面支持) 支持
        const PSE36       = 1 << 17;
        /// CLFLUSH 指令支持
        const CLFLUSH     = 1 << 19;
        /// MMX 指令支持
        const MMX         = 1 << 23;
        /// FXSAVE/FXRSTOR 指令支持
        const FXSR        = 1 << 24;
        /// SSE (流SIMD扩展) 支持
        const SSE         = 1 << 25;
        /// SSE2 支持
        const SSE2        = 1 << 26;
        /// HTT (超线程技术) 支持
        const HTT         = 1 << 28;

        // ====== 基础扩展特性 (Leaf 1 ECX, bits 32-63, 映射到 +32) ======

        const SSE3        = 1 << 32;
        const MONITOR     = 1 << 35;
        /// VMX (Intel VT-x 虚拟化) 支持
        const VMX         = 1 << 37;
        /// SMX (Intel 安全模式扩展)
        const SMX         = 1 << 38;
        const EST         = 1 << 39;
        const TM2         = 1 << 40;
        const SSSE3       = 1 << 41;
        const CID         = 1 << 42;
        const CX16        = 1 << 45;
        const XTPR        = 1 << 46;
        const PDCM        = 1 << 47;
        const PCID        = 1 << 49;
        const SSE41       = 1 << 51;
        const SSE42       = 1 << 52;
        const X2APIC      = 1 << 53;
        const MOVBE       = 1 << 54;
        const POPCNT      = 1 << 55;
        const AES         = 1 << 57;
        const XSAVE       = 1 << 58;
        const OSXSAVE     = 1 << 59;
        /// AVX (高级向量扩展) 支持
        const AVX         = 1 << 60;
        /// RDRAND (硬件随机数生成器)
        const RDRAND      = 1 << 62;

        // ====== 扩展特性 (Leaf 80000001 EDX, 映射到 +64) ======

        /// SYSCALL/SYSRET 指令支持 (AMD64必需)
        const SYSCALL     = 1 << 75;  // 64+11
        /// NX bit (No-Execute) 支持
        const NX          = 1 << 84;  // 64+20
        const MMXEXT      = 1 << 86;  // 64+22
        const FFXSR       = 1 << 88;  // 64+24
        /// 1GB 大页面支持
        const PAGE1GB     = 1 << 90;  // 64+26
        const RDTSCP      = 1 << 91;  // 64+27
        /// LM (Long Mode) - x86-64 支持!
        const LM          = 1 << 93;  // 64+29
        // P4.B.1: SMEP/SMAP (CPUID Leaf7 ECX bit20/21) - 启用后拒绝 Ring 0 执行/访问用户页.
        // bit 20/21 在 CpuFeatures 中已被 leaf1 ECX (1<<20= SSE4.1 检测等) + leaf0x80000001 EDX 等占用,
        // 故使用 bit 30/31 (leaf7 ECX bit 30/31 未被任何现有 feature 检测, 安全).
        // CpuFeatures bitflags 是 CPU feature 抽象层, 与 CPUID 寄存器 bit 不强制 1:1.
        const SMEP        = 1 << 30;
        const SMAP        = 1 << 31;
        /// SVM (AMD-V 虚拟化) 支持
        const SVM         = 1 << 94;  // 64+30
        const _3DNOWEXT   = 1 << 95;  // 64+31
        const _3DNOW      = 1 << 96;  // 64+32

        // ====== 高级特性 (Leaf 7 EBX, 映射到 +96) ======

        const FSGSBASE    = 1 << 96;
        const BMI1        = 1 << 97;
        const HLE         = 1 << 98;
        /// AVX2 支持
        const AVX2        = 1 << 99;
        const BMI2        = 1 << 100;
        const ERMS        = 1 << 101;
        const INVPCID     = 1 << 102;
        const RTM         = 1 << 103;
        const MPX         = 1 << 104;
        /// AVX-512 Foundation
        const AVX512F     = 1 << 105;
        const AVX512DQ    = 1 << 106;
        const RDSEED      = 1 << 107;
        const ADX         = 1 << 108;
        const AVX512IFMA  = 1 << 109;
        const CLWB        = 1 << 110;
        /// CLFLUSHOPT (优化缓存行刷写) 支持 — CPUID Leaf 7 EBX bit 23
        const CLFLUSHOPT  = 1 << 115;
        const AVX512CD    = 1 << 111;
        /// SHA (SHA-1/SHA-256) 指令
        const SHA         = 1 << 112;
        const AVX512BW    = 1 << 113;
        const AVX512VL    = 1 << 114;
    }
}

impl CpuFeatures {
    /// 检查是否为 Intel 处理器 (基于特性组合判断)
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn is_intel_style(&self) -> bool {
        self.contains(Self::VMX) && !self.contains(Self::SVM)
    }

    /// 检查是否为 AMD 处理器
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn is_amd_style(&self) -> bool {
        self.contains(Self::SVM) && !self.contains(Self::VMX)
    }

    /// 检查是否支持 x86-64 长模式
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub const fn supports_64bit(&self) -> bool {
        self.contains(Self::LM)
    }

    /// 检查是否支持 SIMD 向量指令
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn supports_simd(&self) -> bool {
        self.contains(Self::SSE | Self::SSE2)
    }

    /// 检查是否支持 AVX/AVX2
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn supports_avx(&self) -> bool {
        self.contains(Self::AVX | Self::AVX2)
    }

    /// 检查是否支持虚拟化扩展
    #[inline]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn supports_virtualization(&self) -> bool {
        self.contains(Self::VMX | Self::SVM)
    }
}

/// 收集 CPU 特性标志 (多个 CPUID leaf)
#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
pub(super) fn collect_features(
    features_out: &mut CpuFeatures,
    brand_out: &mut [u8; super::BRAND_STRING_LEN],
    max_std_out: &mut u32,
    max_ext_out: &mut u32,
) {
    // 清空特性位图
    *features_out = CpuFeatures::empty();

    // ====== 标准 Leaf 0: 获取支持的 leaf 范围 ======
    let (max_leaf, _, _, _) = cpuid::cpuid(0, 0);
    *max_std_out = max_leaf;

    // ====== 标准 Leaf 1: 基础特性 (EDX + ECX) ======
    if max_leaf >= 1 {
        let (_, _, ecx, edx) = cpuid::cpuid(1, 0);

        // 解析 EDX (bits 0-31)
        let mut feat = CpuFeatures::empty();
        if edx & (1 << 0) != 0 {
            feat.insert(CpuFeatures::FPU);
        }
        if edx & (1 << 2) != 0 {
            feat.insert(CpuFeatures::PSE);
        }
        if edx & (1 << 3) != 0 {
            feat.insert(CpuFeatures::TSC);
        }
        if edx & (1 << 5) != 0 {
            feat.insert(CpuFeatures::MSR);
        }
        if edx & (1 << 6) != 0 {
            feat.insert(CpuFeatures::PAE);
        }
        if edx & (1 << 9) != 0 {
            feat.insert(CpuFeatures::APIC);
        }
        if edx & (1 << 11) != 0 {
            feat.insert(CpuFeatures::SEP);
        }
        if edx & (1 << 15) != 0 {
            feat.insert(CpuFeatures::CMOV);
        }
        if edx & (1 << 19) != 0 {
            feat.insert(CpuFeatures::CLFLUSH);
        }
        if edx & (1 << 23) != 0 {
            feat.insert(CpuFeatures::MMX);
        }
        if edx & (1 << 24) != 0 {
            feat.insert(CpuFeatures::FXSR);
        }
        if edx & (1 << 25) != 0 {
            feat.insert(CpuFeatures::SSE);
        }
        if edx & (1 << 26) != 0 {
            feat.insert(CpuFeatures::SSE2);
        }
        if edx & (1 << 28) != 0 {
            feat.insert(CpuFeatures::HTT);
        }

        // 解析 ECX (bits 32-63)
        if ecx & (1 << 0) != 0 {
            feat.insert(CpuFeatures::SSE3);
        }
        if ecx & (1 << 5) != 0 {
            feat.insert(CpuFeatures::VMX);
        }
        if ecx & (1 << 6) != 0 {
            feat.insert(CpuFeatures::SMX);
        }
        if ecx & (1 << 9) != 0 {
            feat.insert(CpuFeatures::SSSE3);
        }
        if ecx & (1 << 19) != 0 {
            feat.insert(CpuFeatures::SSE41);
        }
        if ecx & (1 << 20) != 0 {
            feat.insert(CpuFeatures::SSE42);
        }
        if ecx & (1 << 21) != 0 {
            feat.insert(CpuFeatures::X2APIC);
        }
        if ecx & (1 << 23) != 0 {
            feat.insert(CpuFeatures::POPCNT);
        }
        if ecx & (1 << 25) != 0 {
            feat.insert(CpuFeatures::AES);
        }
        if ecx & (1 << 26) != 0 {
            feat.insert(CpuFeatures::XSAVE);
        }
        if ecx & (1 << 27) != 0 {
            feat.insert(CpuFeatures::OSXSAVE);
        }
        if ecx & (1 << 28) != 0 {
            feat.insert(CpuFeatures::AVX);
        }
        if ecx & (1 << 30) != 0 {
            feat.insert(CpuFeatures::RDRAND);
        }

        features_out.insert(feat);
    }

    // ====== 扩展 Leaf 80000000: 获取扩展范围 ======
    let (max_ext, _, _, _) = cpuid::cpuid(super::CPUID_LEAF_EXT_BASE, 0);
    *max_ext_out = max_ext;

    // ====== 扩展 Leaf 80000001: 扩展特性 (EDX + ECX) ======
    if max_ext >= 0x8000_0001 {
        let (_, _, ecx, edx) = cpuid::cpuid(0x8000_0001, 0);

        let mut feat = CpuFeatures::empty();
        if edx & (1 << 11) != 0 {
            feat.insert(CpuFeatures::SYSCALL);
        }
        if edx & (1 << 20) != 0 {
            feat.insert(CpuFeatures::NX);
        }
        if edx & (1 << 26) != 0 {
            feat.insert(CpuFeatures::PAGE1GB);
        }
        if edx & (1 << 27) != 0 {
            feat.insert(CpuFeatures::RDTSCP);
        }
        if edx & (1 << 29) != 0 {
            feat.insert(CpuFeatures::LM);
        }

        if ecx & (1 << 0) != 0 { /* LAHF_LM */ }
        if ecx & (1 << 5) != 0 { /* ABM */ }
        if ecx & (1 << 6) != 0 { /* SSE4A */ }

        features_out.insert(feat);

        // 品牌字符串 (Leaf 80000002~4)
        if max_ext >= 0x8000_0004 {
            let (a, b, c, d) = cpuid::cpuid(0x8000_0002, 0);
            brand_out[0..16].copy_from_slice(&[a, b, c, d].map(u32::to_le_bytes).concat());

            let (a, b, c, d) = cpuid::cpuid(0x8000_0003, 0);
            brand_out[16..32].copy_from_slice(&[a, b, c, d].map(u32::to_le_bytes).concat());

            let (a, b, c, d) = cpuid::cpuid(0x8000_0004, 0);
            brand_out[32..48].copy_from_slice(&[a, b, c, d].map(u32::to_le_bytes).concat());

            brand_out[47] = 0; // null 终止
        } else {
            brand_out[..7].copy_from_slice(b"Generic");
            brand_out[7] = 0;
        }
    }

    // ====== 高级 Leaf 7 Sub-leaf 0: 高级特性 (EBX) ======
    if max_leaf >= 7 {
        let (_, ebx, ecx, _) = cpuid::cpuid(7, 0);

        let mut feat = CpuFeatures::empty();
        if ebx & (1 << 0) != 0 {
            feat.insert(CpuFeatures::FSGSBASE);
        }
        if ebx & (1 << 3) != 0 {
            feat.insert(CpuFeatures::BMI1);
        }
        if ebx & (1 << 5) != 0 {
            feat.insert(CpuFeatures::AVX2);
        }
        if ebx & (1 << 8) != 0 {
            feat.insert(CpuFeatures::BMI2);
        }
        if ebx & (1 << 9) != 0 {
            feat.insert(CpuFeatures::ERMS);
        }
        if ebx & (1 << 16) != 0 {
            feat.insert(CpuFeatures::AVX512F);
        }
        if ebx & (1 << 21) != 0 {
            feat.insert(CpuFeatures::AVX512IFMA);
        }
        if ebx & (1 << 24) != 0 {
            feat.insert(CpuFeatures::CLWB);
        }
        if ebx & (1 << 23) != 0 {
            feat.insert(CpuFeatures::CLFLUSHOPT);
        }
        if ebx & (1 << 29) != 0 {
            feat.insert(CpuFeatures::SHA);
        }
        // P4.B.2: SMEP/SMAP (CPUID Leaf7 ECX bit20/21).
        // SMEP = Supervisor Mode Execution Prevention: Ring 0 不能执行 USER 页.
        // SMAP = Supervisor Mode Access Prevention: Ring 0 不能访问 USER 页 (除非 stac).
        if ecx & (1 << 20) != 0 {
            feat.insert(CpuFeatures::SMEP);
        }
        if ecx & (1 << 21) != 0 {
            feat.insert(CpuFeatures::SMAP);
        }

        features_out.insert(feat);
    }
}
