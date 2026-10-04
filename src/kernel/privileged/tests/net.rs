// UT-07 (2026-09-26): net::e1000 注册副本已删 — 其纯逻辑断言以
// privileged/driver/net/e1000.rs 的 #[cfg(test)] 为唯一归属.
//
// UT-07 (2026-09-26): 门控修正 —— 原文件整体门控 `#[cfg(not(feature = "kernel_test"))]`,
// 而本模块声明与 register_tests() 调用均在 tests/mod.rs 的 kernel_test 块内, 两分支互斥
// ⇒ 注册从未生效. e1000 组删除后本文件仅剩纯逻辑用例, 按 E-03 约定归入
// `any(kernel_test, host-test)` 双端模块 (无裸机依赖, 不需 gate).
// 另: 零断言的空壳用例 hton_ntoh / mac_formatting (其工具函数全库已无定义) 一并删除.
use crate::privileged::tests::{TestResult, assert_eq_test, runner};
use crate::register_tests_inner;

fn net_byteorder() -> TestResult {
    let val: u16 = 0x1234;
    assert_eq_test!(val.to_be(), val.to_le().swap_bytes(), "swap");
    TestResult::Pass
}

pub fn register_tests() {
    let r = runner();
    register_tests_inner! { r:
        "net::utils": {
            "byteorder": net_byteorder,
        },
    }
}
