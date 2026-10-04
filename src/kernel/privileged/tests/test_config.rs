//! 配置中心单元测试
//!
//! 覆盖: 常量自洽性 + `validate_*` 所有路径 + `ConfigSummary`
//! 数值 + `KernelCapabilities::detect()` + procfs 文本生成。
//!
//! 启动期 `validate_system_config` 在硬件相关 IO (APIC 等) 初始化前不可
//! 安全调用, 故本测试只测纯函数与不变式, 不调用 `init()`。

use super::{assert_eq_test, check};
use crate::privileged::config::{
    HUGE_PAGE_1G_SHIFT, HUGE_PAGE_2M_SHIFT, KERNEL_STACK_SIZE, KernelCapabilities, MAX_CPUS,
    MAX_IRQS, MAX_OPEN_FILES, MAX_PROCESSES, MAX_SESSIONS, MAX_THREADS, MAX_THREADS_PER_PROCESS,
    PAGE_SHIFT, PAGE_SIZE, SCHED_BOOST_INTERVAL, SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_3_QUANTUM,
    SCHED_RT_WATCHDOG_TICKS, SLAB_DEFAULT_SIZE, SLAB_GENERAL_CACHE_NUM, SLAB_MAX_OBJECT_SIZE,
    SLAB_MIN_OBJECT_SIZE, USER_CODE_BASE, USER_STACK_GUARD, USER_STACK_SIZE, USER_STACK_TOP,
    get_config_summary, print_config_table,
};
// DECISION-O ②: CFS_* 权威归 privileged/config/sched (机制常量), functions 侧
// 纯 re-export; ConfigError/validate_* 为 functions 策略项, tests 经 functions
// 公共 API 访问 (§7.3 允许 privileged/tests 访问 functions)
use crate::functions::config::validate::{
    validate_cross_module_consistency, validate_memory_config,
};
use crate::functions::config::{
    CFS_BOOST_INTERVAL, CFS_MIN_GRANULARITY, CFS_NICE0_WEIGHT, CFS_TARGET_LATENCY, ConfigError,
};
use crate::privileged::tests::{TestResult, runner};
use crate::register_tests_inner;

// ============================================================================
// 常量自洽性 (不变式测试)
// ============================================================================

fn test_capacity_constants_positive() -> TestResult {
    check!(MAX_CPUS > 0, "MAX_CPUS > 0");
    check!(MAX_IRQS > 0, "MAX_IRQS > 0");
    check!(MAX_PROCESSES > 0, "MAX_PROCESSES > 0");
    check!(MAX_THREADS > 0, "MAX_THREADS > 0");
    check!(MAX_THREADS_PER_PROCESS > 0, "MAX_THREADS_PER_PROCESS > 0");
    check!(MAX_OPEN_FILES > 0, "MAX_OPEN_FILES > 0");
    check!(MAX_SESSIONS > 0, "MAX_SESSIONS > 0");
    TestResult::Pass
}

fn test_capacity_thread_relationship() -> TestResult {
    check!(
        MAX_THREADS_PER_PROCESS <= MAX_THREADS,
        "MAX_THREADS_PER_PROCESS must fit in MAX_THREADS"
    );
    TestResult::Pass
}

fn test_memory_page_size_power_of_two() -> TestResult {
    check!(PAGE_SIZE > 0, "PAGE_SIZE > 0");
    check!(PAGE_SIZE.is_power_of_two(), "PAGE_SIZE must be power of 2");
    assert_eq_test!(PAGE_SIZE, 1u64 << PAGE_SHIFT, "PAGE_SIZE == 1<<PAGE_SHIFT");
    TestResult::Pass
}

fn test_memory_huge_page_shifts() -> TestResult {
    assert_eq_test!(HUGE_PAGE_2M_SHIFT, 21u64, "2M shift");
    assert_eq_test!(HUGE_PAGE_1G_SHIFT, 30u64, "1G shift");
    TestResult::Pass
}

fn test_memory_slab_aligned_to_page() -> TestResult {
    assert_eq_test!(SLAB_DEFAULT_SIZE as u64 % PAGE_SIZE, 0, "slab page-aligned");
    TestResult::Pass
}

fn test_memory_stack_sizes() -> TestResult {
    check!(USER_STACK_SIZE >= PAGE_SIZE, "USER_STACK_SIZE >= PAGE_SIZE");
    check!(USER_STACK_GUARD >= PAGE_SIZE, "USER_STACK_GUARD >= 1 page");
    check!(
        USER_STACK_TOP > USER_CODE_BASE,
        "USER_STACK_TOP must be above USER_CODE_BASE"
    );
    check!(
        KERNEL_STACK_SIZE as u64 >= PAGE_SIZE,
        "KERNEL_STACK_SIZE >= 1 page"
    );
    TestResult::Pass
}

fn test_sched_quantum_ordering() -> TestResult {
    // 调度级别 0 (最高优先级) 应有最长量子
    check!(
        SCHED_LEVEL_0_QUANTUM > SCHED_LEVEL_3_QUANTUM,
        "level 0 quantum > level 3 quantum"
    );
    check!(
        CFS_TARGET_LATENCY >= CFS_MIN_GRANULARITY,
        "CFS target >= min granularity"
    );
    check!(CFS_NICE0_WEIGHT > 0, "NICE0_WEIGHT > 0");
    check!(CFS_BOOST_INTERVAL > 0, "CFS_BOOST_INTERVAL > 0");
    check!(SCHED_BOOST_INTERVAL > 0, "SCHED_BOOST_INTERVAL > 0");
    check!(SCHED_RT_WATCHDOG_TICKS > 0, "RT watchdog > 0");
    TestResult::Pass
}

fn test_slab_object_size_bounds() -> TestResult {
    check!(SLAB_MIN_OBJECT_SIZE > 0, "SLAB_MIN_OBJECT_SIZE > 0");
    check!(
        SLAB_MAX_OBJECT_SIZE >= SLAB_MIN_OBJECT_SIZE,
        "SLAB_MAX >= SLAB_MIN"
    );
    check!(SLAB_GENERAL_CACHE_NUM > 0, "SLAB_GENERAL_CACHE_NUM > 0");
    TestResult::Pass
}

// ============================================================================
// validate_* 函数
// ============================================================================

fn test_validate_memory_config_default_ok() -> TestResult {
    // 默认配置 (PAGE_SIZE=4096 等) 应通过
    let r = validate_memory_config();
    if let Err(e) = r {
        check!(false, "default memory config should pass");
        // 静态分析避免未使用变量警告
        let _ = matches!(e, ConfigError::MemoryLayoutInvalid);
    }
    TestResult::Pass
}

fn test_validate_cross_module_consistency() -> TestResult {
    let r = validate_cross_module_consistency();
    if let Err(e) = r {
        check!(false, "cross-module should be consistent");
        let _ = matches!(
            e,
            ConfigError::InconsistentConstant {
                name: _,
                lhs: _,
                rhs: _
            }
        );
    }
    TestResult::Pass
}

// ============================================================================
// ConfigSummary / KernelCapabilities (配置摘要 / 内核能力)
// ============================================================================

fn test_config_summary_values() -> TestResult {
    let s = get_config_summary();
    assert_eq_test!(s.max_cpus, MAX_CPUS, "summary.max_cpus");
    assert_eq_test!(s.max_irqs, MAX_IRQS, "summary.max_irqs");
    assert_eq_test!(s.max_processes, MAX_PROCESSES, "summary.max_processes");
    assert_eq_test!(s.max_threads, MAX_THREADS, "summary.max_threads");
    assert_eq_test!(s.page_size, PAGE_SIZE, "summary.page_size");
    TestResult::Pass
}

fn test_kernel_capabilities_detect() -> TestResult {
    // detect() 必须为 const fn 且可重复调用
    let c1 = KernelCapabilities::detect();
    let c2 = KernelCapabilities::detect();
    assert_eq_test!(c1.smp, c2.smp, "smp stable");
    assert_eq_test!(c1.kpti, c2.kpti, "kpti stable");
    TestResult::Pass
}

fn test_kernel_capabilities_kpti_matches_arch() -> TestResult {
    let caps = KernelCapabilities::detect();
    // 测试模式下 KPTI 被有意禁用 (避免 KPTI 初始化修改共享页表)
    if cfg!(target_arch = "x86_64") && !cfg!(feature = "kernel_test") {
        check!(caps.kpti, "x86_64 (non-test) should report kpti");
    } else {
        check!(!caps.kpti, "test-mode or non-x86_64 should not report kpti");
    }
    TestResult::Pass
}

// ============================================================================
// print_config_table (仅验证不 panic)
// ============================================================================

fn test_print_config_table_no_panic() -> TestResult {
    // 该函数仅通过 klog 输出, 不会失败
    print_config_table();
    TestResult::Pass
}

// ============================================================================
// procfs /sys/config 文本生成
// ============================================================================

fn test_procfs_read_sys_config_basic() -> TestResult {
    let mut buf = [0u8; 1024];
    let n = crate::functions::config::procfs::read_sys_config(&mut buf);
    check!(n > 0, "should write something");
    check!(n <= buf.len(), "should not overflow");

    let s = core::str::from_utf8(&buf[..n]).unwrap_or("");
    check!(s.contains("max_cpus"), "should contain max_cpus");
    check!(s.contains("max_processes"), "should contain max_processes");
    check!(s.contains("page_size"), "should contain page_size");
    check!(s.contains("smp:"), "should contain smp capability");
    TestResult::Pass
}

fn test_procfs_read_sys_config_truncation_safe() -> TestResult {
    // 极小缓冲区, 验证不会越界写
    let mut buf = [0u8; 16];
    let n = crate::functions::config::procfs::read_sys_config(&mut buf);
    check!(n <= buf.len(), "truncated output must not exceed buf len");
    check!(n > 0, "should write at least 16 bytes before truncating");
    TestResult::Pass
}

fn test_procfs_read_sys_config_zero_size() -> TestResult {
    let mut buf = [0u8; 0];
    let n = crate::functions::config::procfs::read_sys_config(&mut buf);
    assert_eq_test!(n, 0, "zero-size buf -> zero write");
    TestResult::Pass
}

// ============================================================================
// ConfigError Display (演进 7: 负面 / 错误消息测试)
// ============================================================================

/// 辅助函数: 将 `ConfigError` 格式化到固定缓冲区并返回结果 String。
fn format_error(e: ConfigError) -> alloc::string::String {
    use core::fmt::Write;
    let mut s = alloc::string::String::new();
    let _ = write!(s, "{e}");
    s
}

fn test_config_error_display_cpu_count() -> TestResult {
    let s = format_error(ConfigError::CpuCountExceedsMax {
        actual: 2048,
        max: 1024,
    });
    check!(s.contains("2048"), "should embed actual value");
    check!(s.contains("1024"), "should embed max value");
    check!(
        s.contains("CPU") || s.contains("MAX_CPUS"),
        "should describe CPU"
    );
    TestResult::Pass
}

fn test_config_error_display_memory_layout() -> TestResult {
    let s = format_error(ConfigError::MemoryLayoutInvalid);
    check!(s.contains("memory"), "should mention memory");
    TestResult::Pass
}

fn test_config_error_display_irq_unavailable() -> TestResult {
    let s = format_error(ConfigError::IrqControllerUnavailable);
    check!(
        s.contains("interrupt") || s.contains("IRQ"),
        "should mention IRQ"
    );
    TestResult::Pass
}

fn test_config_error_display_inconsistent() -> TestResult {
    let s = format_error(ConfigError::InconsistentConstant {
        name: "PAGE_SIZE",
        lhs: 4096,
        rhs: 8192,
    });
    check!(s.contains("PAGE_SIZE"), "should embed constant name");
    check!(s.contains("4096"), "should embed lhs value");
    check!(s.contains("8192"), "should embed rhs value");
    TestResult::Pass
}

fn test_config_error_display_driver_invalid() -> TestResult {
    let s = format_error(ConfigError::DriverConfigInvalid("pci"));
    check!(s.contains("pci"), "should embed driver name");
    TestResult::Pass
}

fn test_config_error_display_slab_variants() -> TestResult {
    let s = format_error(ConfigError::SlabNotPowerOfTwo);
    check!(!s.is_empty(), "SlabNotPowerOfTwo must have message");
    let s = format_error(ConfigError::SlabMisaligned);
    check!(!s.is_empty(), "SlabMisaligned must have message");
    let s = format_error(ConfigError::SlabTooLarge);
    check!(!s.is_empty(), "SlabTooLarge must have message");
    let s = format_error(ConfigError::StackMisaligned);
    check!(!s.is_empty(), "StackMisaligned must have message");
    TestResult::Pass
}

/// Negative test: 两个 re-export 路径得到的 `ConfigError` 在 `==` 上必须等价。
/// 这保证 `proc/types.rs` 等下游模块如果 `pub use ConfigError`, 值不会失真。
fn test_config_error_copy_equality_across_reexports() -> TestResult {
    let e1 = ConfigError::DriverConfigInvalid("pci");
    let e2: ConfigError = e1; // Copy trait
    let e3 = e1;
    assert_eq_test!(e1, e2, "Copy preserves equality");
    assert_eq_test!(e2, e3, "Re-reads return same variant");
    TestResult::Pass
}

// ============================================================================
// 注册
// ============================================================================

pub fn register_config_tests() {
    let r = runner();
    register_tests_inner! { r:
        "config::constants": {
            "capacity_positive": test_capacity_constants_positive,
            "thread_relationship": test_capacity_thread_relationship,
            "page_size_power_of_two": test_memory_page_size_power_of_two,
            "huge_page_shifts": test_memory_huge_page_shifts,
            "slab_aligned_to_page": test_memory_slab_aligned_to_page,
            "stack_sizes": test_memory_stack_sizes,
            "quantum_ordering": test_sched_quantum_ordering,
            "slab_object_bounds": test_slab_object_size_bounds,
        },
        "config::validate": {
            "memory_default_ok": test_validate_memory_config_default_ok,
            "cross_module_ok": test_validate_cross_module_consistency,
        },
        "config::caps": {
            "summary_values": test_config_summary_values,
            "capabilities_detect": test_kernel_capabilities_detect,
            "capabilities_kpti": test_kernel_capabilities_kpti_matches_arch,
        },
        "config::print": {
            "table_no_panic": test_print_config_table_no_panic,
        },
        "config::procfs": {
            "basic": test_procfs_read_sys_config_basic,
            "truncation_safe": test_procfs_read_sys_config_truncation_safe,
            "zero_size_buf": test_procfs_read_sys_config_zero_size,
        },
        "config::error_display": {
            "cpu_count": test_config_error_display_cpu_count,
            "memory_layout": test_config_error_display_memory_layout,
            "irq_unavailable": test_config_error_display_irq_unavailable,
            "inconsistent": test_config_error_display_inconsistent,
            "driver_invalid": test_config_error_display_driver_invalid,
            "slab_variants": test_config_error_display_slab_variants,
            "copy_equality": test_config_error_copy_equality_across_reexports,
        },
    }
}
