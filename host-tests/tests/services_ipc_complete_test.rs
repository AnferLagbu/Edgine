//! I-54: services IPC 4 子系统 (pipe/shm/msgq/sem) 全部完成迁移 + 0 unsafe 静态契约测试
//!
//! 验证 maintenance-2026-06-11.md 中 I-54 验收:
//!   "services/ipc 4 个子系统 (pipe/shm/msgq/sem) 全部完成"
//!   "0 unsafe 验证"

use std::fs;
use std::path::Path;

const SERVICES_IPC: &str = "src/kernel/services/ipc/mod.rs";

#[test]
fn test_services_ipc_4_subsystems_migrated() {
    // IpcLock + free functions 必须全部覆盖 pipe/shm/msgq/sem 4 子系统
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(SERVICES_IPC);
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    // pipe/shm/msgq/sem 各自的 close/destroy 标识
    let close_fns: &[(&str, &str)] = &[
        ("pipe", "pipe_close"),
        ("shm", "shm_destroy"),
        ("msgq", "msgq_destroy"),
        ("sem", "sem_destroy"),
    ];
    for (_sub, fn_name) in close_fns {
        let needle = format!("fn {}(", fn_name);
        assert!(
            src.contains(&needle),
            "services/ipc 应实现 {} (I-54)",
            needle
        );
    }
}

#[test]
fn test_services_ipc_0_unsafe_blocks() {
    // services/ipc 必须 0 unsafe 代码块 (仅允许 #![deny(unsafe_code)] 属性)
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(SERVICES_IPC);
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    assert!(
        src.lines()
            .next()
            .unwrap_or("")
            .contains("deny(unsafe_code)"),
        "services/ipc 应启用 #![deny(unsafe_code)] (I-54)"
    );

    // 查找 unsafe 代码块, 排除属性行
    let mut unsafe_blocks = 0usize;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with("unsafe") && !t.starts_with("unsafe_code") {
            // 形如 `unsafe {` 或 `unsafe fn` — 但 services 不应有
            unsafe_blocks += 1;
        }
    }
    assert_eq!(
        unsafe_blocks, 0,
        "services/ipc 不应出现 unsafe 代码块, 实际 {} 处 (I-54)",
        unsafe_blocks
    );
}

#[test]
fn test_services_ipc_uses_framework_safe_api() {
    // T6-1: pipe/shm/msgq 策略函数已迁移到 services 本地,
    // services/ipc/mod.rs 不再导入 framework::ipc::pipe/shm/msgq,
    // 而是通过本地子模块 (pub mod pipe/shm/msgq) 实现.
    // 验证:
    // 1. services/ipc 有本地 pipe/shm/msgq 子模块声明
    // 2. 本地子模块通过 framework 机制 API (IPC_NAMESPACE, pmm_alloc_pages 等) 访问硬件
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(SERVICES_IPC);
    let src =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e));

    for sub in &["pipe", "shm", "msgq"] {
        let mod_decl = format!("pub mod {}", sub);
        assert!(
            src.contains(&mod_decl),
            "services/ipc 应声明本地 {} 子模块 (T6-1)",
            sub
        );
    }
    // 仍通过 framework::ipc 访问全局状态
    let ipc_ns = format!("framework::ipc::IPC{}", "_NAMESPACE");
    assert!(
        src.contains(&ipc_ns),
        "services/ipc 应通过 framework ipc 访问全局状态 (I-54)"
    );
}
