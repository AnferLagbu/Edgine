//! 跨层测试载体 (P-71 上移出 TCB).
//!
//! 本模块收纳同时依赖 `privileged` 与 `functions` 的用例 (`sys` / `config` / `pwm` /
//! `ipc` / `uds`). `privileged` 独立成 crate 后不能再反向依赖 `functions`, 故这些
//! 用例改由 glue crate 注册到 `privileged::tests::runner()` 的同一注册表.
//!
//! 注册入口与原 `privileged::tests::register_all_tests()` 合并执行, 双端
//! (kernel_test / host-test) 行为不变.

pub mod sys;
pub mod test_config;
pub mod test_ipc;
#[cfg(test)]
pub mod test_ipc_inline;
pub mod test_pwm;
pub mod test_uds;

/// FS 全局单例初始化 — 测试模式下注册前必须调用.
///
/// 历史 bug (2026-06-25): 缺少 init 会让 `DevFS::mount` 测试触发
/// "devfs::global() called before init_global()" panic 并卡死.
/// `init_global` 是幂等的 (OnceCell::get_or_init), 多次调用安全.
pub fn prepare_test_env() {
    crate::functions::fs::devfs::init_global();
    crate::functions::fs::procfs::init_global();
}

/// 注册跨层用例 — 与 `privileged::tests::register_all_tests()` 共用同一 runner.
pub fn register_upper_tests() {
    test_config::register_config_tests();
    test_pwm::register_pwm_tests();
    test_ipc::register_ipc_tests();
    test_uds::register_uds_tests();
    #[cfg(target_arch = "x86_64")]
    sys::register_tests();
}

/// 注册入口 — 双端共用: 先做 FS 全局初始化, 再按「TCB 用例 + 跨层用例」顺序注册.
pub fn register_tests() {
    prepare_test_env();
    crate::privileged::tests::register_all_tests();
    register_upper_tests();
}

/// kernel_test 启动路径的测试入口 (原 `privileged::tests::test_runner_init`).
///
/// 页表诊断读取与 `run_all()` 编排依赖 glue 侧可见的 `privileged` 与 `functions`,
/// 故随跨层注册一并从 TCB 上移.
pub fn test_runner_init() {
    crate::klog_boot_info!("[TEST] === Edgine Test Privileged ===");

    register_tests();

    let r = crate::privileged::tests::runner();
    let count = r.count();
    crate::klog_boot_info!("[TEST] Registered {} test cases", count);

    // 诊断: test 运行前检查页表
    {
        let read_u64 = |phys: u64, idx: usize| -> u64 {
            let va = phys + crate::privileged::mm::KERNEL_BASE + idx as u64 * 8;
            // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
            unsafe { core::ptr::read_volatile(va as *const u64) }
        };
        let pd24 = read_u64(0x10_9000, 24);
        let pd63 = read_u64(0x10_9000, 63);
        crate::klog_boot_info!(
            "[PAGETABLE] before run_all: pd[24]=0x{:016X} pd[63]=0x{:016X}",
            pd24,
            pd63
        );
    }

    r.run_all();

    let p = r.passed.load(core::sync::atomic::Ordering::Relaxed);
    let f = r.failed.load(core::sync::atomic::Ordering::Relaxed);
    if f == 0 {
        crate::klog_boot_info!(
            "[TEST] ALL TESTS PASSED ({}/{})",
            p,
            p + r.skipped.load(core::sync::atomic::Ordering::Relaxed)
        );
    } else {
        crate::klog_boot_info!("[TEST] COMPLETE: {} passed, {} FAILED", p, f);
    }
}

/// host-test 侧测试入口 (原 `privileged::tests::host_test_runner_main`).
///
/// 与 kernel_test 路径共用同一注册编排, 返回汇总供 host-tests 断言 "0 failed".
#[cfg(feature = "host-test")]
pub fn host_test_runner_main() -> crate::privileged::tests::TestSummary {
    register_tests();
    let r = crate::privileged::tests::runner();
    r.run_all();
    crate::privileged::tests::TestSummary {
        passed: r.passed.load(core::sync::atomic::Ordering::Relaxed),
        failed: r.failed.load(core::sync::atomic::Ordering::Relaxed),
        skipped: r.skipped.load(core::sync::atomic::Ordering::Relaxed),
    }
}
