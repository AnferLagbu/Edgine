// UT-07 (2026-09-25): idt::statistics 与 idt::types 注册副本已删 —
// 其纯逻辑断言分别以 privileged/idt/statistics.rs 与 privileged/idt/types.rs
// 的 #[cfg(test)] 为唯一归属.
// UT-07 (2026-09-26): idt::handlers 注册副本 (8 组) 已删 — 源侧
// privileged/idt/handlers.rs #[cfg(test)] 为唯一归属 (含互补的 0x02 内核态写缺页
// 场景与 #99 未知分类断言); idt::safety 仅保留 cpu_features_no_panic —
// 其 address_validation 七条地址谓词已迁 privileged/idt/safety.rs #[cfg(test)].
use crate::privileged::idt::CpuFeatures;
use crate::privileged::tests::{TestResult, check, runner};
use crate::register_tests_inner;

fn cpu_features_no_panic() -> TestResult {
    let features = CpuFeatures::detect();
    // x86_64: detect() 经 CPUID leaf 0/1 真实解析 — 须满足最低硬件不变量
    #[cfg(target_arch = "x86_64")]
    {
        check!(features.max_cpuid_leaf >= 1, "max_cpuid_leaf must be >= 1");
        check!(features.has_apic, "leaf1 EDX bit9 (APIC) must be set");
        // x2APIC 为可选特性 (QEMU 默认 CPU 不暴露); 架构上 x2APIC 蕴含 APIC
        check!(
            !features.has_x2apic || features.has_apic,
            "x2APIC must imply APIC"
        );
    }
    // aarch64: 中断控制器为 GIC, 无 APIC/x2APIC (detect 返回架构中性缺省值)
    #[cfg(target_arch = "aarch64")]
    {
        check!(!features.has_apic, "aarch64 must not report APIC");
        check!(!features.has_x2apic, "aarch64 must not report x2APIC");
        check!(
            features.max_cpuid_leaf == 0,
            "aarch64 max_cpuid_leaf default"
        );
    }
    TestResult::Pass
}

pub fn register_idt_safety_tests() {
    let r = runner();
    register_tests_inner! { r:
        "idt::safety": {
            "cpu_features_no_panic": cpu_features_no_panic,
        },
    }
}

pub fn register_tests() {
    register_idt_safety_tests();
}
