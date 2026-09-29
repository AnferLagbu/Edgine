//! 防回归: 测试注册入口必须 init FS 全局单例
//!
//! 历史 bug (2026-06-25): test_runner_init 缺少 init_global 调用,
//! 导致 DevFS::mount 测试 (测试 129/256) 触发
//! "devfs::global() called before init_global()" panic, 测试卡死.
//!
//! 修复: test_runner_init 在 register_tests 之前调用:
//! - crate::services::fs::devfs::init_global()
//! - crate::services::fs::procfs::init_global()
//!
//! E-04 (2026-09-06): 测试运行器双端适配 — 注册逻辑已从 test_runner_init 抽取至
//! `register_all_tests()` (kernel_test 的 test_runner_init 与 host 入口
//! host_test_runner_main 共用), 本文件扫描目标同步迁移至 register_all_tests.
//!
//! 本测试验证修复通过静态扫描 source 存在, 防止后续误删.

use std::fs;

const TESTS_MOD_PATH: &str = "../src/kernel/framework/tests/mod.rs";

fn read_source() -> String {
    fs::read_to_string(TESTS_MOD_PATH).unwrap_or_else(|e| {
        panic!("无法读取 {}: {}", TESTS_MOD_PATH, e)
    })
}

#[test]
fn test_runner_init_calls_devfs_init_global() {
    // E-04 (2026-09-06): 扫描目标为 register_all_tests (注册逻辑抽取后的唯一入口).
    // 验收: 注册入口必须 init devfs 全局单例
    let content = read_source();
    let fn_start = content
        .find("pub fn register_all_tests()")
        .expect("应有 pub fn register_all_tests");
    // 找下一个 fn 起始 (粗略)
    let remaining = &content[fn_start..];
    let next_fn = remaining
        .find("\npub fn ")
        .or_else(|| remaining.find("\nfn "))
        .unwrap_or(remaining.len());
    let body = &remaining[..next_fn];
    assert!(
        body.contains("devfs::init_global"),
        "register_all_tests 应调用 devfs::init_global(), 否则 DevFS::mount 测试会 panic"
    );
}

#[test]
fn test_runner_init_calls_procfs_init_global() {
    // E-04 (2026-09-06): 扫描目标为 register_all_tests (注册逻辑抽取后的唯一入口).
    // 验收: 注册入口应 init procfs 全局单例
    let content = read_source();
    let fn_start = content
        .find("pub fn register_all_tests()")
        .expect("应有 pub fn register_all_tests");
    let remaining = &content[fn_start..];
    let next_fn = remaining
        .find("\npub fn ")
        .or_else(|| remaining.find("\nfn "))
        .unwrap_or(remaining.len());
    let body = &remaining[..next_fn];
    assert!(
        body.contains("procfs::init_global"),
        "register_all_tests 应调用 procfs::init_global(), 否则 procfs 测试会 panic"
    );
}

#[test]
fn test_init_global_is_idempotent() {
    // 验收: devfs::init_global() 是幂等的 (OnceCell::get_or_init).
    // 多次调用不会 panic, 不会重置状态.
    // 主机端无 devfs 全局, 这里验证 init_global 签名可见且文档承诺幂等.
    // 阶段 4b: devfs 实现已下沉 services/fs/devfs.rs
    let devfs_rs = fs::read_to_string(
        "../src/kernel/services/fs/devfs.rs",
    ).expect("无法读取 devfs.rs");
    assert!(
        devfs_rs.contains("OnceCell::get_or_init") || devfs_rs.contains("get_or_init"),
        "devfs::init_global 应使用 OnceCell::get_or_init 保证幂等"
    );
}

#[test]
fn test_smoltcp_impl_kernel_test_fw_init_stub() {
    // 验收: services/net/init (kernel_test 桩) 提供 smoltcp_net_stack_* stub
    let services_mod = fs::read_to_string(
        "../src/kernel/services/net/mod.rs",
    ).expect("无法读取 services/net/mod.rs");
    assert!(
        services_mod.contains("smoltcp_net_stack_socket_open")
            && services_mod.contains("smoltcp_net_stack_slot_base"),
        "services/net/init kernel_test 桩必须包含 smoltcp_net_stack_* stub"
    );

    // smoltcp_impl.rs 必须 cfg-gate fw_init import
    let smoltcp_impl = fs::read_to_string(
        "../src/kernel/services/net/smoltcp_impl.rs",
    ).expect("无法读取 smoltcp_impl.rs");
    assert!(
        smoltcp_impl.contains("#[cfg(not(feature = \"kernel_test\"))]")
            && smoltcp_impl.contains("use crate::framework::net::init as fw_init"),
        "smoltcp_impl.rs 应 cfg-gate framework::net::init import (kernel_test 模式走桩)"
    );
}