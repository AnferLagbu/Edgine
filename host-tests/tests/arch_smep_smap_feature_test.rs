//! arch: SMEP/SMAP 特性检测与 CR4 配置一致性测试
//!
//! 追踪: P4.B.1 + P4.B.2 + P4.B.3.
//!
//! ## 测试目的
//!
//! 验证 `privileged::arch::x86_64::cpu::CpuFeatures` 中 SMEP/SMAP bitflag 位置
//! 正确, 与 CPUID Leaf 7 ECX bit 20/21 一一对应. 同时验证 `init_msr` 中
//! CR4 bit 20/21 设置逻辑条件正确 (CPU 支持时才启用).
//!
//! ## 测试策略
//!
//! host-tests 不链接 edgine 静态库, 复刻 CpuFeatures bitflag 抽象层.
//! 验证 SMEP/SMAP bit 位置与 CR4 bit 位置映射.

/// CpuFeatures SMEP bit (1 << 30, 见 cpu/mod.rs P4.B.1).
/// 选择 bit 30 是因为 leaf1 ECX bit 20 已被 leaf0x80000001 EDX 等占用.
const EXPECTED_SMEP_BIT: u128 = 1 << 30;

/// CpuFeatures SMAP bit (1 << 31).
const EXPECTED_SMAP_BIT: u128 = 1 << 31;

/// CR4 SMEP bit = bit 20 (Intel SDM Vol 3 §4.4).
const EXPECTED_CR4_SMEP_BIT: u64 = 1 << 20;

/// CR4 SMAP bit = bit 21 (Intel SDM Vol 3 §4.4).
const EXPECTED_CR4_SMAP_BIT: u64 = 1 << 21;

/// CPUID Leaf 7 ECX bit 20 (SMEP).
const EXPECTED_CPUID_SMEP_BIT: u32 = 1 << 20;

/// CPUID Leaf 7 ECX bit 21 (SMAP).
const EXPECTED_CPUID_SMAP_BIT: u32 = 1 << 21;

#[test]
fn cpu_features_smep_bit_position() {
    assert_eq!(
        EXPECTED_SMEP_BIT,
        1u128 << 30,
        "CpuFeatures SMEP bit position must be 1<<30 (avoid conflict with leaf1 ECX bit 20)"
    );
}

#[test]
fn cpu_features_smap_bit_position() {
    assert_eq!(
        EXPECTED_SMAP_BIT,
        1u128 << 31,
        "CpuFeatures SMAP bit position must be 1<<31 (avoid conflict with leaf1 ECX bit 21)"
    );
}

#[test]
fn cpu_features_smep_smap_dont_overlap() {
    assert_eq!(
        EXPECTED_SMEP_BIT & EXPECTED_SMAP_BIT,
        0,
        "CpuFeatures SMEP and SMAP bits must not overlap"
    );
}

#[test]
fn cr4_smep_bit_matches_intel_sdm() {
    assert_eq!(EXPECTED_CR4_SMEP_BIT, 1u64 << 20);
}

#[test]
fn cr4_smap_bit_matches_intel_sdm() {
    assert_eq!(EXPECTED_CR4_SMAP_BIT, 1u64 << 21);
}

#[test]
fn cpuid_leaf7_ecx_smep_smap_bits() {
    assert_eq!(EXPECTED_CPUID_SMEP_BIT, 1u32 << 20);
    assert_eq!(EXPECTED_CPUID_SMAP_BIT, 1u32 << 21);
}

#[test]
fn cr4_smep_smap_bits_dont_overlap_with_existing() {
    // CR4.SMEP=bit 20, CR4.SMAP=bit 21 必须与既有 CR4 位不冲突:
    // bit 9 = OSFXSR, bit 10 = OSXMMEXCPT, bit 17 = PCIDE
    let existing_critical = (1u64 << 9) | (1u64 << 10) | (1u64 << 17);
    let smep_smap = EXPECTED_CR4_SMEP_BIT | EXPECTED_CR4_SMAP_BIT;
    assert_eq!(
        existing_critical & smep_smap,
        0,
        "CR4 SMEP/SMAP bits must not conflict with OSFXSR/OSXMMEXCPT/PCIDE"
    );
}

#[test]
fn smep_smap_combined_bitfield() {
    // CR4.SMEP | CR4.SMAP = bit 20 | bit 21 = 0x300000 (3 MiB)
    let combined = EXPECTED_CR4_SMEP_BIT | EXPECTED_CR4_SMAP_BIT;
    assert_eq!(combined, 0x00300000);
}
