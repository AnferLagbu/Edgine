//! FdTable 归属契约测试 (P1-I-01 → DECISION-J 反转)
//!
//! 历史: P1-I-01 (2026-06-16) 将 FdTable 从 framework/proc/process.rs 提取
//! 到 services/proc/fd_table.rs。DECISION-J (2026-09-13) 按"机制持有的
//! 数据结构归 framework"统一判据反转迁回 — FdTable 是 Process 结构体字段
//! (framework 进程机制状态)。
//!
//! 静态契约 (反转后口径):
//! 1. FdTable 类型定义必须位于 framework/proc/fd_table.rs
//! 2. framework/proc/fd_table.rs 必须 `#![deny(unsafe_code)]`
//! 3. services 侧为纯 re-export 代理壳, 不重复定义
//! 4. framework/proc/process.rs 引 framework 本地路径
//! 5. 核心 API 一致: alloc_fd / get_global_fd / close_fd
//!
//! 主机端测试: 模拟 FdTable 行为 (从源码扫描确认, 不直接执行内核代码).

use std::fs;

fn framework_fd_table_rs() -> String {
    let path = format!(
        "{}/../src/kernel/framework/proc/fd_table.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read framework/proc/fd_table.rs")
}

fn framework_process_rs() -> String {
    let path = format!(
        "{}/../src/kernel/framework/proc/process.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read framework/proc/process.rs")
}

fn services_fd_table_rs() -> String {
    let path = format!(
        "{}/../src/kernel/services/proc/fd_table.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read services/proc/fd_table.rs")
}

#[test]
fn fd_table_defined_in_framework() {
    // DECISION-J 验收: FdTable 类型定义必须位于 framework/proc/fd_table.rs
    let src = framework_fd_table_rs();
    assert!(
        src.contains("pub struct FdTable"),
        "DECISION-J: FdTable 必须定义在 framework/proc/fd_table.rs"
    );
    assert!(
        src.contains("pub const MAX_FDS_PER_PROCESS"),
        "DECISION-J: MAX_FDS_PER_PROCESS 必须定义在 framework/proc/fd_table.rs"
    );
}

#[test]
fn fd_table_framework_module_denies_unsafe() {
    // framework/proc/fd_table.rs 必须 deny unsafe_code (0 unsafe 机制文件)
    let src = framework_fd_table_rs();
    assert!(
        src.contains("#![deny(unsafe_code)]"),
        "DECISION-J: framework/proc/fd_table.rs 必须 #![deny(unsafe_code)]"
    );
}

#[test]
fn fd_table_uses_framework_irq_spinlock() {
    // FdTable 使用 framework 提供的 safe API (IrqSpinLock)
    let src = framework_fd_table_rs();
    assert!(
        src.contains("use crate::framework::sync::IrqSpinLock"),
        "DECISION-J: FdTable 应使用 framework::sync::IrqSpinLock"
    );
}

#[test]
fn framework_process_re_exports_fd_table_locally() {
    // DECISION-J 验收: process.rs 引 framework 本地路径 (不再是 services)
    let src = framework_process_rs();
    assert!(
        src.contains("pub use crate::framework::proc::fd_table::{FdTable, MAX_FDS_PER_PROCESS}"),
        "DECISION-J: framework/proc/process.rs 必须 re-export framework::fd_table"
    );
    assert!(
        !src.contains("crate::services::proc::fd_table"),
        "DECISION-J: framework/proc/process.rs 不得再引用 services::fd_table"
    );
    // 不能有 struct FdTable 重复定义
    let struct_count = src.matches("pub struct FdTable").count();
    assert_eq!(
        struct_count, 0,
        "DECISION-J: framework/proc/process.rs 不应定义 struct FdTable, 重复 {} 次",
        struct_count
    );
}

#[test]
fn services_fd_table_is_pure_reexport_shell() {
    // DECISION-J 验收: services 侧为纯 re-export 代理壳, 不重复定义
    let src = services_fd_table_rs();
    assert!(
        src.contains("pub use crate::framework::proc::fd_table::*;"),
        "DECISION-J: services/proc/fd_table.rs 必须为 glob re-export 壳"
    );
    let struct_count = src.matches("pub struct FdTable").count();
    assert_eq!(
        struct_count, 0,
        "DECISION-J: services/proc/fd_table.rs 不应定义 struct FdTable"
    );
}

#[test]
fn fd_table_alloc_uses_first_fit_strategy() {
    // P1-I-01 验收: 分配策略是 first-fit 线性扫描
    let src = framework_fd_table_rs();
    assert!(
        src.contains("for i in 0..MAX_FDS_PER_PROCESS"),
        "P1-I-01: alloc_fd 必须 first-fit 线性扫描 (O(MAX_FDS_PER_PROCESS))"
    );
    // 新实现使用 u32::MAX 表示空闲 (OpenFile handle_id)
    assert!(
        src.contains("u32::MAX") || src.contains("== -1"),
        "P1-I-01: alloc_fd 必检查 slot 空闲"
    );
}

#[test]
fn fd_table_close_zeros_slot() {
    // P1-I-01 验收: close_fd 必清空 slot
    let src = framework_fd_table_rs();
    let close_fn = src.find("pub fn close_fd").expect("close_fd not found");
    let body_start = src[close_fn..].find('{').unwrap() + close_fn;
    let body = &src[body_start..];
    // 新实现使用 u32::MAX 表示空闲
    assert!(
        body.contains("u32::MAX") || body.contains("= -1"),
        "P1-I-01: close_fd 必清空 slot"
    );
    assert!(
        body.contains("local_fd >= MAX_FDS_PER_PROCESS"),
        "P1-I-01: close_fd 必检查 local_fd 越界"
    );
}
