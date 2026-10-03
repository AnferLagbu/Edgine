// SPDX-License-Identifier: MPL-2.0
// TD-14: services::ipc shm/msgq/sem 三子系统公开 API 完整性契约测试.
//
// 验收:
//   - shm 子系统至少有 shm_create / shm_attach / shm_detach / shm_destroy 四个公开 fn
//   - msgq 子系统至少有 msgq_create / msgq_send / msgq_recv / msgq_destroy 四个公开 fn
//   - sem  子系统至少有 sem_create  / sem_wait  / sem_post  / sem_destroy 四个公开 fn
//   - 顶层 free functions 同步存在
//   - IpcError 走 TD-08 薄包装: InvalidOp 域语义变体 + Kernel(KernelError) 共享包装
//     (POSIX 兼容错误统一走 KernelError, 避免在 IpcError 内重复)
//   - ShmHandle::from_id_and_addr / MsgqHandle::from / SemHandle::from 三个 ctor 都存在
//   - 模块顶部 #![deny(unsafe_code)]
//
// 该测试为 I-54 验收的强化版 (原 services_ipc_complete_test 只检查 close/destroy).

use std::fs;
use std::path::Path;

const SERVICES_IPC: &str = "src/kernel/services/ipc/mod.rs";

fn read_services_ipc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(SERVICES_IPC);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

#[test]
fn test_shm_subsystem_full_lifecycle() {
    let src = read_services_ipc();
    for fn_name in &["shm_create", "shm_attach", "shm_detach", "shm_destroy"] {
        let top_needle = format!("pub fn {}(", fn_name);
        assert!(
            src.contains(&top_needle),
            "services::ipc 必须有顶层 {} (TD-14 / I-54)",
            top_needle
        );
    }
}

#[test]
fn test_msgq_subsystem_full_lifecycle() {
    let src = read_services_ipc();
    for fn_name in &["msgq_create", "msgq_send", "msgq_recv", "msgq_destroy"] {
        let needle = format!("pub fn {}(", fn_name);
        assert!(
            src.contains(&needle),
            "services::ipc 必须有顶层 {} (TD-14 / I-54)",
            needle
        );
    }
}

#[test]
fn test_sem_subsystem_full_lifecycle() {
    let src = read_services_ipc();
    for fn_name in &["sem_create", "sem_wait", "sem_post", "sem_destroy"] {
        let needle = format!("pub fn {}(", fn_name);
        assert!(
            src.contains(&needle),
            "services::ipc 必须有顶层 {} (TD-14 / I-54)",
            needle
        );
    }
}

#[test]
fn test_ipc_error_variants_complete() {
    let src = read_services_ipc();
    // TD-08 薄包装: 共享 POSIX 错误统一走 Kernel(KernelError),
    // IpcError 只保留 IPC 域内独有且无法用 KernelError 表达的语义.
    //
    // 必须保留的域语义变体:
    //   - InvalidOp: 操作对当前 IPC 资源类型不合法
    //                (例如在 msgq 上调用 shm 专属 API, 跨子系统语义)
    //
    // 必须保留的包装变体:
    //   - Kernel(crate::services::error::KernelError)
    //     单一来源, 与 TD-20 错误统一测试保持一致.
    let invalid_op_needles = ["    InvalidOp,\n", "        InvalidOp,\n", "    InvalidOp "];
    assert!(
        invalid_op_needles.iter().any(|n| src.contains(n)),
        "IpcError 必须保留 InvalidOp 域语义变体 (TD-14 + TD-08 薄包装)"
    );
    assert!(
        src.contains("Kernel(crate::services::error::KernelError)"),
        "IpcError 必须以 Kernel(KernelError) 共享包装 (TD-08 薄包装)"
    );
    // 旧版 7 变体已废弃, 不得再出现在 IpcError 定义中.
    for legacy in &[
        "NoResources",
        "BadFd",
        "NotFound",
        "WouldBlock",
        "PermissionDenied",
        "InvalidArgument",
    ] {
        let defined_needles = [
            format!("    {},\n", legacy),
            format!("        {},\n", legacy),
            format!("    {} ", legacy),
        ];
        let defined = defined_needles.iter().any(|n| src.contains(n));
        assert!(
            !defined,
            "IpcError::{} 已被 TD-08 薄包装废弃, 改走 IpcError::Kernel(KernelError::...)",
            legacy
        );
    }
}

#[test]
fn test_handle_constructors_present() {
    let src = read_services_ipc();
    assert!(
        src.contains("pub fn from_id_and_addr"),
        "ShmHandle::from_id_and_addr 必须存在 (TD-14)"
    );
    assert!(
        src.contains("impl MsgqHandle") && src.contains("MsgqHandle::from"),
        "MsgqHandle::from 必须存在 (TD-14)"
    );
    assert!(
        src.contains("impl SemHandle") && src.contains("SemHandle::from"),
        "SemHandle::from 必须存在 (TD-14)"
    );
}

#[test]
fn test_deny_unsafe_code_at_module_top() {
    let src = read_services_ipc();
    let first_line = src.lines().next().unwrap_or("");
    assert!(
        first_line.contains("deny(unsafe_code)"),
        "services::ipc 顶部必须 #![deny(unsafe_code)] (TD-14)"
    );
}

#[test]
fn test_doc_status_reflects_actual_migration() {
    // 防回归: 顶部 doc 注释不应再宣称 "1/4" 或 "待迁移"
    let src = read_services_ipc();
    let head: String = src.lines().take(20).collect::<Vec<_>>().join("\n");
    assert!(
        !head.contains("已完成 1/4") && !head.contains("待迁移:"),
        "services::ipc 顶部 doc 注释必须反映 4/4 完成状态, 不应再写 1/4 或待迁移"
    );
    assert!(
        head.contains("v2.7") || head.contains("4 子系统") || head.contains("4 子系统已完成"),
        "services::ipc 顶部 doc 应明确写出 4 子系统全完成 (TD-14)"
    );
}
