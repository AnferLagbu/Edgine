// SPDX-License-Identifier: MPL-2.0
// TD-08: services 错误统一契约测试.
//
// 验收:
//   1. SocketError 字段数 ≤ 2 (现 = 0 字段, type alias to KernelError)
//   2. UnixSocketError 字段数 ≤ 2 (现 = 2 字段: PathNotFound + Kernel)
//   3. KernelError 单一来源, 共享错误 (BadFd / WouldBlock 等) 跨 2 个枚举映射一致
//   4. From<fw::UdsError> 单一映射, 9 个变体全数下沉
//
// B09-12/DECISION-H13 P0-2 更新: KernelError 定义已迁回 framework/error.rs,
// services/error.rs 改为 re-export. 静态断言指向 framework/error.rs.

use std::fs;

const SERVICES_ERROR: &str = "../src/kernel/services/error.rs";
const FRAMEWORK_ERROR: &str = "../src/kernel/framework/error.rs";
const NET_SOCKET: &str = "../src/kernel/services/net/socket.rs";
const NET_UNIX: &str = "../src/kernel/services/net/unix.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p))
}

fn variant_count(enum_body: &str) -> usize {
    // 严格匹配 enum variant 行: 缩进 + PascalCase 标识符 + ,/ {/ (三种结束符)
    enum_body
        .lines()
        .filter(|l| {
            let t = l.trim();
            // 必须是变体行: 标识符 + (',' | '{' | '(') 结束, 标识符首字母大写
            if t.is_empty() || t.starts_with("//") {
                return false;
            }
            // 取第一个空白或符号前的部分
            let head: String = t
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if head.is_empty() || !head.chars().next().unwrap().is_ascii_uppercase() {
                return false;
            }
            // 剩余部分必须以 , { ( 之一结束 (允许空白)
            let rest = t[head.len()..].trim_start();
            rest.starts_with(',') || rest.starts_with('{') || rest.starts_with('(')
        })
        .count()
}

#[test]
fn test_kernel_error_module_exists() {
    // B09-12 P0-2: KernelError 定义在 framework/error.rs, services/error.rs re-export
    let fw = read(FRAMEWORK_ERROR);
    assert!(
        fw.contains("pub enum KernelError"),
        "framework/error.rs 必须定义 KernelError"
    );
    assert!(
        fw.contains("pub const fn from_i32"),
        "必须有 POSIX errno 映射"
    );
    assert!(
        fw.contains("pub const fn as_errno"),
        "必须有反向 errno 映射"
    );
    // services/error.rs 必须是 re-export 壳 (单向依赖)
    let svc = read(SERVICES_ERROR);
    assert!(
        svc.contains("pub use crate::framework::error::KernelError"),
        "services/error.rs 必须 re-export framework KernelError"
    );
    assert!(
        !svc.contains("pub enum KernelError"),
        "services/error.rs 不应再定义 KernelError"
    );
}

#[test]
fn test_socket_error_is_kernel_error_alias() {
    let src = read(NET_SOCKET);
    // SocketError 现为 type alias to KernelError, 字段数应为 0
    assert!(
        src.contains("pub use crate::services::error::KernelError as SocketError")
            || src.contains("pub use crate::services::error::KernelError as SocketError;"),
        "SocketError 必须是 KernelError 的 type alias"
    );
    // 不再含独立 enum 定义
    assert!(
        !src.contains("pub enum SocketError {"),
        "SocketError 不应是独立 enum, 应是 type alias"
    );
}

#[test]
fn test_unix_socket_error_has_at_most_2_variants() {
    let src = read(NET_UNIX);
    // 提取 UnixSocketError enum 体
    let start = src.find("pub enum UnixSocketError").expect("enum 必须存在");
    let body_start = src[start..].find('{').unwrap() + start + 1;
    let body_end = src[body_start..].find("}").unwrap() + body_start;
    let body = &src[body_start..body_end];
    let count = variant_count(body);
    assert!(count <= 2, "UnixSocketError 字段数={count} > 2, 验收失败");
    // 至少应有 PathNotFound + Kernel
    assert!(
        body.contains("PathNotFound"),
        "必须保留 UDS 特有字段 PathNotFound"
    );
    assert!(body.contains("Kernel("), "必须有 Kernel(KernelError) 包装");
}

#[test]
fn test_kernel_error_posix_round_trip() {
    let src = read(FRAMEWORK_ERROR);
    // 验证关键共享 errno 都已映射: 1, 9, 11, 12, 14, 22, 95, 97, 98, 99, 104, 107, 111
    for raw in [1, 9, 11, 12, 14, 22, 95, 97, 98, 99, 104, 107, 111] {
        let needle = format!("{} => Self::", raw);
        assert!(src.contains(&needle), "errno={raw} 必须在 from_i32 映射");
    }
}

#[test]
fn test_from_uds_error_covers_all_variants() {
    let src = read(NET_UNIX);
    // 验证 UdsError 9 个变体都有对应分支
    // UdsError 已迁移到 services 本地, 可用 fw:: 或直接 UdsError:: 前缀
    for variant in [
        "BadFd",
        "Again",
        "NoMem",
        "AddrFamily",
        "AddrInUse",
        "ConnRefused",
        "Invalid",
        "NotFound",
        "NoSys",
    ] {
        assert!(
            src.contains(&format!("fw::UdsError::{} =>", variant))
                || src.contains(&format!("UdsError::{} =>", variant)),
            "必须覆盖 UdsError::{}",
            variant
        );
    }
}

#[test]
fn test_kernel_error_exported_from_services_mod() {
    let src = fs::read_to_string("../src/kernel/services/mod.rs").expect("read services/mod.rs");
    assert!(
        src.contains("pub mod error"),
        "services/mod.rs 必须导出 error 子模块"
    );
}

#[test]
fn test_socket_error_uses_kernel_error_in_path() {
    // 静态验证 socket.rs 路径: type alias -> KernelError
    let src = read(NET_SOCKET);
    let path_present = src.contains("services::error::KernelError");
    assert!(
        path_present,
        "socket.rs 必须引用 services::error::KernelError"
    );
}
