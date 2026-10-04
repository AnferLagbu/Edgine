// SPDX-License-Identifier: MPL-2.0
// TD-20: services 域 NetError / IpcError / SyncError / PiMutexError /
//        DevTreeError / EGDFError / StorageError / PwmError /
//        AuditError / SessionError 收敛到 KernelError (TD-08 V6)
//
// 验收:
//   - 每个错误枚举统一为 [N 字段特有] + Kernel(KernelError) 薄包装结构
//   - 旧变体 (NoResources / BadFd / NotFound / WouldBlock / PermissionDenied /
//     InvalidArgument / Other / NoResources / NotReady / Io / AlreadyExists /
//     PathTooLong / OpenFailed / IoFailed / Truncated / TableFull /
//     TooManySessions 等) 全部废弃, 改走 Kernel(KernelError::...) 包装
//   - 10 个 to_errno() 方法 全部变体 → POSIX Errno 双向映射
//
// 运行: cargo test -p host-tests --test td20_services_kernel_error_test

use std::fs;
use std::path::Path;

const NET_RS: &str = "src/kernel/services/net/mod.rs";
const IPC_RS: &str = "src/kernel/services/ipc/mod.rs";
const SYNC_RS: &str = "src/kernel/services/sync/mod.rs";
const PIMUTEX_RS: &str = "src/kernel/services/sync/pi_mutex.rs";
const DEVTREE_RS: &str = "src/kernel/services/egdf/devtree.rs";
const EGDF_RS: &str = "src/kernel/services/egdf/mod.rs";
const STORAGE_RS: &str = "src/kernel/services/sgeg/crypto.rs";
const PWM_RS: &str = "src/kernel/services/sgeg/identity.rs";
const AUDIT_RS: &str = "src/kernel/services/sgeg/audit.rs";
const SESSION_RS: &str = "src/kernel/services/sgeg/sessions.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn assert_has_kernel_wrapper(src: &str, enum_name: &str) {
    // 接受两种形式:
    //   - `EnumName::Kernel(` (变体引用点, 用于 match/from_i32)
    //   - `Kernel(crate::services::error::KernelError)` (枚举字段定义)
    let has_variant_use = src.contains(&format!("{enum_name}::Kernel("));
    let has_field_def = src.contains("Kernel(crate::services::error::KernelError)");
    assert!(
        has_variant_use || has_field_def,
        "{enum_name} 必须含 `Kernel(KernelError)` 共享包装字段 (变体引用或字段定义)"
    );
}

// ============================================================================
// NetError
// ============================================================================

#[test]
fn net_error_uses_kernel_wrapper() {
    let src = read(NET_RS);
    assert_has_kernel_wrapper(&src, "NetError");
}

#[test]
fn no_legacy_net_error_variants() {
    let src = read(NET_RS);
    for legacy in &[
        "NetError::NotReady",
        "NetError::InvalidArgument",
        "NetError::NotFound",
        "NetError::Io",
        "NetError::Other",
    ] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 NetError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// IpcError
// ============================================================================

#[test]
fn ipc_error_uses_kernel_wrapper() {
    let src = read(IPC_RS);
    assert_has_kernel_wrapper(&src, "IpcError");
}

#[test]
fn no_legacy_ipc_error_variants() {
    let src = read(IPC_RS);
    for legacy in &[
        "IpcError::NoResources",
        "IpcError::BadFd",
        "IpcError::NotFound",
        "IpcError::WouldBlock",
        "IpcError::PermissionDenied",
        "IpcError::InvalidArgument",
        "IpcError::Other",
    ] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 IpcError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// SyncError
// ============================================================================

#[test]
fn sync_error_uses_kernel_wrapper() {
    let src = read(SYNC_RS);
    assert_has_kernel_wrapper(&src, "SyncError");
}

#[test]
fn no_legacy_sync_error_variants() {
    let src = read(SYNC_RS);
    for legacy in &["SyncError::WouldBlock", "SyncError::Other"] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 SyncError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// PiMutexError
// ============================================================================

#[test]
fn pi_mutex_error_uses_kernel_wrapper() {
    let src = read(PIMUTEX_RS);
    assert_has_kernel_wrapper(&src, "PiMutexError");
}

#[test]
fn no_legacy_pi_mutex_error_variants() {
    let src = read(PIMUTEX_RS);
    {
        let legacy = &"PiMutexError::WouldBlock";
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 PiMutexError::Kernel(KernelError::WouldBlock)",
            legacy
        );
    }
}

// ============================================================================
// DevTreeError
// ============================================================================

#[test]
fn devtree_error_uses_kernel_wrapper() {
    let src = read(DEVTREE_RS);
    assert_has_kernel_wrapper(&src, "DevTreeError");
}

#[test]
fn no_legacy_devtree_error_variants() {
    let src = read(DEVTREE_RS);
    for legacy in &[
        "DevTreeError::NotFound",
        "DevTreeError::InvalidArgument",
        "DevTreeError::Other",
    ] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 DevTreeError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// EGDFError
// ============================================================================

#[test]
fn egdf_error_uses_kernel_wrapper() {
    let src = read(EGDF_RS);
    assert_has_kernel_wrapper(&src, "EGDFError");
}

#[test]
fn no_legacy_egdf_error_variants() {
    let src = read(EGDF_RS);
    for legacy in &[
        "EGDFError::NotFound",
        "EGDFError::AlreadyExists",
        "EGDFError::Io",
        "EGDFError::InvalidArgument",
        "EGDFError::NoResources",
        "EGDFError::NotReady",
        "EGDFError::PermissionDenied",
        "EGDFError::Other",
    ] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 EGDFError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// StorageError
// ============================================================================

#[test]
fn storage_error_uses_kernel_wrapper() {
    let src = read(STORAGE_RS);
    assert_has_kernel_wrapper(&src, "StorageError");
}

#[test]
fn no_legacy_storage_error_variants() {
    let src = read(STORAGE_RS);
    for legacy in &[
        "StorageError::PathTooLong",
        "StorageError::OpenFailed",
        "StorageError::IoFailed",
        "StorageError::Truncated",
        "StorageError::Other",
    ] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 StorageError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// PwmError
// ============================================================================

#[test]
fn pwm_error_uses_kernel_wrapper() {
    let src = read(PWM_RS);
    assert_has_kernel_wrapper(&src, "PwmError");
}

#[test]
fn no_legacy_pwm_error_variants() {
    let src = read(PWM_RS);
    for legacy in &["PwmError::NotFound", "PwmError::Other"] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 PwmError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// AuditError
// ============================================================================

#[test]
fn audit_error_uses_kernel_wrapper() {
    let src = read(AUDIT_RS);
    assert_has_kernel_wrapper(&src, "AuditError");
}

// ============================================================================
// SessionError
// ============================================================================

#[test]
fn session_error_uses_kernel_wrapper() {
    let src = read(SESSION_RS);
    assert_has_kernel_wrapper(&src, "SessionError");
}

#[test]
fn no_legacy_session_error_variants() {
    let src = read(SESSION_RS);
    for legacy in &["SessionError::NotFound", "SessionError::TooManySessions"] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 SessionError::Kernel(KernelError::...)",
            legacy
        );
    }
}

// ============================================================================
// to_errno 通用检查
// ============================================================================

#[test]
fn all_to_errno_methods_present() {
    for (file, marker) in &[
        (NET_RS, "NetError"),
        (IPC_RS, "IpcError"),
        (SYNC_RS, "SyncError"),
        (PIMUTEX_RS, "PiMutexError"),
        (DEVTREE_RS, "DevTreeError"),
        (EGDF_RS, "EGDFError"),
        (STORAGE_RS, "StorageError"),
        (PWM_RS, "PwmError"),
        (AUDIT_RS, "AuditError"),
        (SESSION_RS, "SessionError"),
    ] {
        let src = read(file);
        let has_to_errno = src.contains("pub fn to_errno")
            || src.contains("fn to_errno(")
            || (src.contains("to_errno") && src.contains("Self::Kernel(e) => e.as_errno()"));
        assert!(
            has_to_errno,
            "{} 中 {} 必须有 to_errno 方法 (或委托 Kernel::as_errno)",
            file, marker
        );
    }
}
