//! FdTable 归属契约测试 (P1-I-01 → DECISION-J 反转)
//!
//! 历史: P1-I-01 (2026-06-16) 将 FdTable 从 privileged/proc/process.rs 提取
//! 到 functions/proc/fd_table.rs。DECISION-J (2026-09-13) 按"机制持有的
//! 数据结构归 privileged"统一判据反转迁回 — FdTable 是 Process 结构体字段
//! (privileged 进程机制状态)。
//!
//! 静态契约 (反转后口径):
//! 1. FdTable 类型定义必须位于 privileged/proc/fd_table.rs
//! 2. privileged/proc/fd_table.rs 必须 `#![deny(unsafe_code)]`
//! 3. functions 侧为纯 re-export 代理壳, 不重复定义
//! 4. privileged/proc/process.rs 引 privileged 本地路径
//! 5. 核心 API 一致: alloc_fd / get_global_fd / close_fd
//!
//! 主机端测试: 模拟 FdTable 行为 (从源码扫描确认, 不直接执行内核代码).

use std::fs;

fn privileged_fd_table_rs() -> String {
    let path = format!(
        "{}/../src/kernel/privileged/proc/fd_table.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read privileged/proc/fd_table.rs")
}

fn privileged_process_rs() -> String {
    let path = format!(
        "{}/../src/kernel/privileged/proc/process.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read privileged/proc/process.rs")
}

fn functions_fd_table_rs() -> String {
    let path = format!(
        "{}/../src/kernel/functions/proc/fd_table.rs",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("read functions/proc/fd_table.rs")
}

#[test]
fn fd_table_defined_in_privileged() {
    // DECISION-J 验收: FdTable 类型定义必须位于 privileged/proc/fd_table.rs
    let src = privileged_fd_table_rs();
    assert!(
        src.contains("pub struct FdTable"),
        "DECISION-J: FdTable 必须定义在 privileged/proc/fd_table.rs"
    );
    assert!(
        src.contains("pub const MAX_FDS_PER_PROCESS"),
        "DECISION-J: MAX_FDS_PER_PROCESS 必须定义在 privileged/proc/fd_table.rs"
    );
}

#[test]
fn fd_table_privileged_module_denies_unsafe() {
    // privileged/proc/fd_table.rs 必须 deny unsafe_code (0 unsafe 机制文件)
    let src = privileged_fd_table_rs();
    assert!(
        src.contains("#![deny(unsafe_code)]"),
        "DECISION-J: privileged/proc/fd_table.rs 必须 #![deny(unsafe_code)]"
    );
}

#[test]
fn fd_table_uses_privileged_irq_spinlock() {
    // FdTable 使用 privileged 提供的 safe API (IrqSpinLock)
    let src = privileged_fd_table_rs();
    assert!(
        src.contains("use crate::privileged::sync::IrqSpinLock"),
        "DECISION-J: FdTable 应使用 privileged::sync::IrqSpinLock"
    );
}

#[test]
fn privileged_process_re_exports_fd_table_locally() {
    // DECISION-J 验收: process.rs 引 privileged 本地路径 (不再是 functions)
    let src = privileged_process_rs();
    assert!(
        src.contains("pub use crate::privileged::proc::fd_table::{FdTable, MAX_FDS_PER_PROCESS}"),
        "DECISION-J: privileged/proc/process.rs 必须 re-export privileged::fd_table"
    );
    assert!(
        !src.contains("crate::functions::proc::fd_table"),
        "DECISION-J: privileged/proc/process.rs 不得再引用 functions::fd_table"
    );
    // 不能有 struct FdTable 重复定义
    let struct_count = src.matches("pub struct FdTable").count();
    assert_eq!(
        struct_count, 0,
        "DECISION-J: privileged/proc/process.rs 不应定义 struct FdTable, 重复 {} 次",
        struct_count
    );
}

#[test]
fn functions_fd_table_is_pure_reexport_shell() {
    // DECISION-J 验收: functions 侧为纯 re-export 代理壳, 不重复定义
    let src = functions_fd_table_rs();
    assert!(
        src.contains("pub use crate::privileged::proc::fd_table::*;"),
        "DECISION-J: functions/proc/fd_table.rs 必须为 glob re-export 壳"
    );
    let struct_count = src.matches("pub struct FdTable").count();
    assert_eq!(
        struct_count, 0,
        "DECISION-J: functions/proc/fd_table.rs 不应定义 struct FdTable"
    );
}

#[test]
fn fd_table_alloc_uses_first_fit_strategy() {
    // P1-I-01 验收: 分配策略是 first-fit 线性扫描
    let src = privileged_fd_table_rs();
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
    let src = privileged_fd_table_rs();
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
