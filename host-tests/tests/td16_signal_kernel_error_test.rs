// SPDX-License-Identifier: MPL-2.0
// TD-16: services::proc::signal 错误类型收敛到 KernelError 单一来源
//
// 验收:
//   - services/proc/signal.rs 暴露 `pub use KernelError as SignalError;` (type alias)
//   - 不再独立定义 `pub enum SignalError { ... }`
//   - 4 个 `SignalError::X` 使用点全部用 KernelError 已覆盖变体
//   - 不再引用旧变体 `ProcessExited` / `InvalidSignal`
//   - 进程域错误覆盖 (NoSuchProcess 必须从 KernelError 暴露)
//
// 运行: cargo test -p host-tests --test td16_signal_kernel_error_test

use std::fs;
use std::path::Path;

const SIGNAL_RS: &str = "src/kernel/services/proc/signal.rs";
// B09-12/DECISION-H13 P0-2: KernelError 定义迁回 framework/error.rs, services 侧 re-export.
// 静态断言指向 framework/error.rs (变体/映射定义所在).
const FRAMEWORK_ERROR_RS: &str = "src/kernel/framework/error.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn signal_error_is_kernel_error_alias() {
    let src = read(SIGNAL_RS);
    assert!(
        src.contains("pub use crate::services::error::KernelError as SignalError;"),
        "SignalError 必须为 KernelError 的 type alias (TD-16)"
    );
}

#[test]
fn no_legacy_signal_error_enum() {
    let src = read(SIGNAL_RS);
    // 不允许再出现独立 `pub enum SignalError {`
    assert!(
        !src.contains("pub enum SignalError {"),
        "SignalError 不应再独立定义 enum, 必须 alias 到 KernelError"
    );
    // 不应残留旧 5 字段变体名
    for legacy in &["ProcessExited", "InvalidSignal"] {
        assert!(
            !src.contains(&format!("SignalError::{}", legacy)),
            "SignalError::{} 已废弃, 应改走 KernelError 对应变体",
            legacy
        );
    }
}

#[test]
fn four_signal_error_usages_under_kernel_error() {
    let src = read(SIGNAL_RS);
    // send() 内部必须有 NoSuchProcess + InvalidArgument
    assert!(
        src.contains("SignalError::NoSuchProcess"),
        "send() 应使用 SignalError::NoSuchProcess 表示进程不存在/已退出"
    );
    assert!(
        src.contains("SignalError::InvalidArgument"),
        "send() 应使用 SignalError::InvalidArgument 表示无效信号编号"
    );
}

#[test]
fn kernel_error_exposes_no_such_process() {
    // B09-12 P0-2: KernelError 定义在 framework/error.rs
    let src = read(FRAMEWORK_ERROR_RS);
    assert!(
        src.contains("NoSuchProcess"),
        "KernelError 必须暴露 NoSuchProcess 变体 (ESRCH=3) 供 SignalError::NoSuchProcess 复用"
    );
    // POSIX ESRCH = 3
    assert!(
        src.contains("3 => Self::NoSuchProcess"),
        "KernelError::from_i32(3) 必须映射到 NoSuchProcess"
    );
    assert!(
        src.contains("Self::NoSuchProcess => Errno::ESRCH"),
        "KernelError::NoSuchProcess 必须反向映射到 Errno::ESRCH"
    );
}

#[test]
fn signal_module_still_safe() {
    let src = read(SIGNAL_RS);
    // 顶层 deny 不可松
    assert!(
        src.starts_with("#![deny(unsafe_code)]"),
        "signal.rs 必须保持 #![deny(unsafe_code)]"
    );
    // 全局 unsafe_code 标记必须为 0
    let unsafe_count = src.matches("unsafe {").count() + src.matches("unsafe fn").count();
    assert_eq!(unsafe_count, 0, "signal.rs 必须 0 unsafe 块");
}

#[test]
fn deny_unsafe_code_intact() {
    // 静态契约: services/proc/signal.rs 第一行必须是 #![deny(unsafe_code)]
    let src = read(SIGNAL_RS);
    let first_line = src.lines().next().expect("non-empty");
    assert_eq!(first_line, "#![deny(unsafe_code)]");
}
