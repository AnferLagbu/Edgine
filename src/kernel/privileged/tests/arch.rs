use crate::privileged::cpu::{CacheInfo, CpuSignature, CpuVendor, TopologyInfo};
// UT-07 (2026-09-25): arch::gdt 注册副本已删 — 其纯逻辑断言以
// privileged/arch/x86_64/gdt.rs 的 #[cfg(test)] 为唯一归属.
// UT-07 (2026-09-26): arch::tss 注册副本已删 — 其断言以
// privileged/arch/x86_64/tss.rs 的 #[cfg(test)] 为唯一归属 (源侧 zeroed 已改用
// 公有取值器 get_ist, API 级断言不降级; 注册侧 `TSS_SIZE >= 92` / `% 2 == 0`
// 两条弱于源侧 `>= TSS_MINIMUM_SIZE` / `== TSS_MINIMUM_SIZE`, 被蕴含故丢弃).
use crate::privileged::tests::{TestResult, assert_eq_test, check, runner};
use crate::register_tests_inner;

fn cpu_vendor_recognition() -> TestResult {
    assert_eq_test!(
        CpuVendor::from_vendor_string(b"GenuineIntel"),
        CpuVendor::Intel,
        "Intel vendor"
    );
    assert_eq_test!(
        CpuVendor::from_vendor_string(b"AuthenticAMD"),
        CpuVendor::Amd,
        "AMD vendor"
    );
    assert_eq_test!(
        CpuVendor::from_vendor_string(b"CentaurHauls"),
        CpuVendor::Via,
        "VIA vendor"
    );
    assert_eq_test!(
        CpuVendor::from_vendor_string(b"TCGTCGTCG??\0"),
        CpuVendor::Qemu,
        "QEMU vendor"
    );
    assert_eq_test!(
        CpuVendor::from_vendor_string(b"UnknownVend\0"),
        CpuVendor::Unknown,
        "Unknown vendor"
    );
    TestResult::Pass
}

fn cpu_signature_effective_values() -> TestResult {
    let sig = CpuSignature {
        family: 6,
        model: 0x9E,
        ext_family: 0,
        ext_model: 0,
        ..Default::default()
    };
    assert_eq_test!(sig.effective_family(), 6, "effective family");
    assert_eq_test!(sig.effective_model(), 0x9E, "effective model");

    let sig_ext = CpuSignature {
        family: 0xF,
        model: 0x07,
        ext_family: 0x06,
        ext_model: 0x09,
        ..Default::default()
    };
    assert_eq_test!(sig_ext.effective_family(), 0x15, "ext effective family");
    assert_eq_test!(sig_ext.effective_model(), 0x97, "ext effective model");
    TestResult::Pass
}

fn cpu_cache_info_total() -> TestResult {
    let cache = CacheInfo {
        l1d_size: 32 * 1024,
        l1i_size: 32 * 1024,
        l2_size: 256 * 1024,
        l3_size: 8 * 1024 * 1024,
        ..Default::default()
    };
    assert_eq_test!(
        cache.total_size(),
        (32 + 32 + 256 + 8192) * 1024,
        "cache total size"
    );
    check!(cache.has_l3(), "should have L3");

    let no_l3 = CacheInfo {
        l3_size: 0,
        ..Default::default()
    };
    check!(!no_l3.has_l3(), "should not have L3");
    TestResult::Pass
}

fn cpu_topology_threads_per_core() -> TestResult {
    let single = TopologyInfo {
        physical_cores: 4,
        logical_threads: 4,
        hyperthreading_enabled: false,
        ..Default::default()
    };
    assert_eq_test!(single.threads_per_core(), 1, "single thread per core");
    check!(!single.is_single_core(), "4 cores is not single");

    let ht = TopologyInfo {
        physical_cores: 4,
        logical_threads: 8,
        hyperthreading_enabled: true,
        ..Default::default()
    };
    assert_eq_test!(ht.threads_per_core(), 2, "HT threads per core");

    let mono = TopologyInfo {
        physical_cores: 1,
        logical_threads: 1,
        ..Default::default()
    };
    check!(mono.is_single_core(), "1 core is single");
    TestResult::Pass
}

pub fn register_cpu_tests() {
    let r = runner();
    register_tests_inner! { r:
        "cpu": {
            "vendor_recognition": cpu_vendor_recognition,
            "signature_effective_values": cpu_signature_effective_values,
            "cache_info_total": cpu_cache_info_total,
            "topology_threads_per_core": cpu_topology_threads_per_core,
        },
    }
}

pub fn register_cpuid_tests() {}

pub fn register_tests() {
    register_cpu_tests();
    register_cpuid_tests();
}
