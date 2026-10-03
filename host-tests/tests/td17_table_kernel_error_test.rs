// SPDX-License-Identifier: MPL-2.0
// TD-17: services::proc::table::TableError 收敛到 KernelError (TD-08 V3 proc域)
//
// 验收:
//   - services/proc/table.rs 不再独立定义 `pub enum TableError { NotFound, Other(i32) }`
//   - TableError 3 表特有字段 (TableFull/RefCountUnderflow/InvalidStateTransition)
//     + 1 `Kernel(KernelError)` 共享包装
//   - `TableError::NotFound` 全部改走 `KernelError::NoSuchProcess` (经 `From` 自动包装)
//   - `TableError::Other` 完全废弃
//   - `to_errno()` 方法存在, 4 个变体全覆盖
//   - `From<KernelError> for TableError` 自动派生式包装
//   - try_inc_ref 错误返回点改用 `.into()` 让 KernelError 自动包装
//
// 运行: cargo test -p host-tests --test td17_table_kernel_error_test

use std::fs;
use std::path::Path;

const TABLE_RS: &str = "src/kernel/services/proc/table.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn table_error_is_thin_wrapper() {
    let src = read(TABLE_RS);
    // 必须有 Kernel(KernelError) 共享包装
    assert!(
        src.contains("Kernel(crate::services::error::KernelError)"),
        "TableError 必须含 `Kernel(KernelError)` 共享包装字段"
    );
}

#[test]
fn no_legacy_table_error_variants() {
    let src = read(TABLE_RS);
    // 旧 NotFound 必须废弃
    assert!(
        !src.contains("TableError::NotFound"),
        "TableError::NotFound 已废弃, 应改走 KernelError::NoSuchProcess + .into()"
    );
    // 旧 Other(i32) 必须废弃
    assert!(
        !src.contains("TableError::Other"),
        "TableError::Other(i32) 已废弃, 共享错误统一走 KernelError::Other"
    );
    // 旧 5 字段 enum 不应再出现
    assert!(
        !src.contains("pub enum TableError {\n    /// 进程不存在\n    NotFound,"),
        "TableError 不应再独立 5 字段 enum 定义"
    );
}

#[test]
fn table_error_preserves_three_table_specific_variants() {
    let src = read(TABLE_RS);
    // 表子系统特有错误保留
    assert!(src.contains("TableFull"), "TableFull 应保留");
    assert!(
        src.contains("RefCountUnderflow"),
        "RefCountUnderflow 应保留"
    );
    assert!(
        src.contains("InvalidStateTransition"),
        "InvalidStateTransition 应保留"
    );
}

#[test]
fn to_errno_method_present() {
    let src = read(TABLE_RS);
    assert!(
        src.contains("pub fn to_errno(self) -> crate::framework::syscall::types::Errno")
            || src.contains("pub fn to_errno(self) -> crate::framework::syscall::Errno"),
        "TableError 必须有 to_errno() 方法 (4 变体全覆盖)"
    );
    // to_errno 必须处理 4 个变体
    let to_errno_body_start = src.find("pub fn to_errno(self)").expect("to_errno 存在");
    let to_errno_block = &src[to_errno_body_start..];
    for variant in &[
        "Self::TableFull",
        "Self::RefCountUnderflow",
        "Self::InvalidStateTransition",
        "Self::Kernel",
    ] {
        assert!(
            to_errno_block.contains(variant),
            "to_errno 必须映射 {} 变体",
            variant
        );
    }
}

#[test]
fn from_kernel_error_impl() {
    let src = read(TABLE_RS);
    assert!(
        src.contains("impl From<crate::services::error::KernelError> for TableError"),
        "TableError 必须有 From<KernelError> 包装实现, 让 `?` 操作符自动转换"
    );
}

#[test]
fn try_inc_ref_uses_into() {
    let src = read(TABLE_RS);
    // try_inc_ref 错误路径必须用 `.into()` 从 KernelError 自动包装
    let try_inc_block_start = src.find("pub fn try_inc_ref").expect("try_inc_ref 存在");
    let try_inc_block = &src[try_inc_block_start..try_inc_block_start + 400];
    assert!(
        try_inc_block.contains("NoSuchProcess"),
        "try_inc_ref 错误路径必须返回 NoSuchProcess"
    );
    assert!(
        try_inc_block.contains(".into()"),
        "try_inc_ref 错误路径必须用 .into() 让 KernelError 自动包装为 TableError::Kernel(...)"
    );
}

#[test]
fn deny_unsafe_code_intact() {
    let src = read(TABLE_RS);
    let first_line = src.lines().next().expect("non-empty");
    assert_eq!(first_line, "#![deny(unsafe_code)]");
    let unsafe_count = src.matches("unsafe {").count() + src.matches("unsafe fn").count();
    assert_eq!(unsafe_count, 0, "table.rs 必须 0 unsafe 块");
}
