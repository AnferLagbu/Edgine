//! E-04 (2026-09-06): 测试运行器双端适配 — host 端共享测试集执行载体
//!
//! 调用内核 `privileged::tests::host_test_runner_main()`, 在 host (std) 下执行与
//! kernel_test (QEMU) 共享的同一套纯逻辑测试代码 (privileged/tests 门控外 15 mod
//! + any(kernel_test, host-test) 5 mod), 断言 0 failed.
//!
//! 输出 (每个测试的 Pass/Fail/Skip + 汇总) 经 serial_print host 分支走 stdout,
//! 观察完整输出: `cargo test --test e04_shared_runner_test -- --nocapture`
//!
//! 本文件保留为 E-05 共享测试集的 host 端执行载体.

use edgine::kernel::privileged::tests::host_test_runner_main;

#[test]
fn e04_shared_runner_zero_failed() {
    let summary = host_test_runner_main();
    assert_eq!(
        summary.failed, 0,
        "host 侧共享测试集存在 FAILED: passed={}, failed={}, skipped={}",
        summary.passed, summary.failed, summary.skipped
    );
}
