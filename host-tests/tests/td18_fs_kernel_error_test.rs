// SPDX-License-Identifier: MPL-2.0
// TD-18: functions::fs::ramfs::FsError 收敛到 KernelError (TD-08 V4 fs域)
//
// 验收:
//   - functions/fs/ramfs.rs 不再独立定义 16 字段 FsError enum
//   - FsError 3 FS 特有字段 (NotInitialized/IoError/Overflow) + 1 Kernel 包装
//   - 旧变体 NotFound/AlreadyExists/NoSpace/PermissionDenied/InvalidArgument/
//     OutOfMemory/Busy/NotSupported/NotADirectory/IsDirectory/ReadOnly/
//     BadFileDescriptor/NameTooLong 全部废弃
//   - KernelError 新增 FS 特有 POSIX 变体 (FileNotFound/AlreadyExists/Busy/
//     NotADirectory/IsDirectory/ReadOnlyFilesystem/NameTooLong/NoSpace/CrossDevice)
//   - to_errno() 方法 4 变体全覆盖
//   - From<KernelError> for FsError 包装实现
//   - from_i32() 委托给 KernelError::from_i32
//   - 15 个 FsError 使用点全部改走 Kernel(KernelError) 包装
//
// 运行: cargo test -p host-tests --test td18_fs_kernel_error_test

use std::fs;
use std::path::Path;

const RAMFS_RS: &str = "src/kernel/functions/fs/ramfs.rs";
// B09-12/DECISION-H13 P0-2: KernelError 定义迁回 privileged/error.rs, functions 侧 re-export.
// 静态断言指向 privileged/error.rs (变体/映射定义所在).
const ERROR_RS: &str = "src/kernel/privileged/error.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn fs_error_is_thin_wrapper() {
    let src = read(RAMFS_RS);
    assert!(
        src.contains("Kernel(crate::functions::error::KernelError)"),
        "FsError 必须含 `Kernel(KernelError)` 共享包装字段"
    );
}

#[test]
fn no_legacy_fs_error_variants() {
    let src = read(RAMFS_RS);
    // 旧 16 字段变体全部不应再出现
    for legacy in &[
        "FsError::NotFound",
        "FsError::AlreadyExists",
        "FsError::NoSpace",
        "FsError::PermissionDenied",
        "FsError::InvalidArgument",
        "FsError::OutOfMemory",
        "FsError::Busy",
        "FsError::NotSupported",
        "FsError::NotADirectory",
        "FsError::IsDirectory",
        "FsError::ReadOnly",
        "FsError::BadFileDescriptor",
        "FsError::NameTooLong",
    ] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 FsError::Kernel(KernelError::...)",
            legacy
        );
    }
}

#[test]
fn fs_error_preserves_three_fs_specific_variants() {
    let src = read(RAMFS_RS);
    assert!(src.contains("NotInitialized"), "NotInitialized 应保留");
    assert!(src.contains("IoError"), "IoError 应保留");
    assert!(src.contains("Overflow"), "Overflow 应保留");
}

#[test]
fn to_errno_method_present() {
    let src = read(RAMFS_RS);
    assert!(
        src.contains("pub fn to_errno(self) -> crate::privileged::syscall::types::Errno")
            || src.contains("pub fn to_errno(self) -> crate::privileged::syscall::Errno"),
        "FsError 必须有 to_errno() 方法 (4 变体全覆盖)"
    );
    let to_errno_block_start = src.find("pub fn to_errno(self)").expect("to_errno 存在");
    let to_errno_block = &src[to_errno_block_start..to_errno_block_start + 600];
    for variant in &[
        "Self::NotInitialized",
        "Self::IoError",
        "Self::Overflow",
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
    let src = read(RAMFS_RS);
    assert!(
        src.contains("impl From<crate::functions::error::KernelError> for FsError"),
        "FsError 必须有 From<KernelError> 包装实现"
    );
}

#[test]
fn from_i32_delegates_to_kernel_error() {
    let src = read(RAMFS_RS);
    // from_i32 必须委托给 KernelError::from_i32
    let from_i32_block_start = src
        .find("pub fn from_i32(code: i32) -> Self")
        .expect("from_i32 存在");
    let from_i32_block = &src[from_i32_block_start..from_i32_block_start + 300];
    assert!(
        from_i32_block.contains("KernelError::from_i32(code)"),
        "FsError::from_i32 必须委托给 KernelError::from_i32"
    );
}

#[test]
fn kernel_error_exposes_all_fs_posix_variants() {
    let src = read(ERROR_RS);
    // 验证 9 个 FS 特有 POSIX 变体都暴露
    for variant in &[
        "FileNotFound",
        "AlreadyExists",
        "Busy",
        "NotADirectory",
        "IsDirectory",
        "ReadOnlyFilesystem",
        "NameTooLong",
        "NoSpace",
        "CrossDevice",
    ] {
        assert!(
            src.contains(variant),
            "KernelError 必须暴露 {} 变体",
            variant
        );
    }
    // 验证 from_i32 + as_errno 双向映射覆盖
    for errno_num in &[2, 16, 17, 18, 20, 21, 28, 30, 36] {
        assert!(
            src.contains(&format!("{} => Self::", errno_num)),
            "KernelError::from_i32({}) 必须显式映射",
            errno_num
        );
    }
}

#[test]
fn usages_all_use_kernel_wrapper() {
    let src = read(RAMFS_RS);
    // 至少 8 处用 FsError::Kernel(KernelError::...) 包装
    // (7×NotFound + 1×NotADirectory + 1×InvalidArgument = 9,
    //  减去 doc 注释中 1 行 引用 = 8 实际.
    //  原下界 10 含已删死的 ramfs 辅助 split_path/validate_path 之 3 处
    //  (2×InvalidArgument + 1×NameTooLong), 随 B09 冗余定型删除后下调为 8)
    // rustfmt 拆 FsError::Kernel(crate::...::KernelError::Xxx) 为多行; 鲁棒匹配
    // 拆行后: "FsError::Kernel(" 在前, "KernelError::" 在后, ")" 闭合.
    // 改为独立子串 count (klog 风格).
    // rustfmt 把 FsError::Kernel(crate::...::KernelError::Xxx) 拆为多行 (前缀和后缀各占一行).
    // 鲁棒计数: FsError::Kernel(\n (跨行) ...KernelError::\n 模式; 用前缀 (FsError::Kernel( 单 token) + 后缀 (KernelError:: 单 token) 最小值.
    let kernel_count = src
        .matches("FsError::Kernel(")
        .count()
        .min(src.matches("KernelError::").count());
    assert!(
        kernel_count >= 8,
        "FsError 至少应有 8 处用 Kernel(KernelError::...) 包装, 实际: {}",
        kernel_count
    );
}

#[test]
fn deny_unsafe_code_intact() {
    let src = read(RAMFS_RS);
    let first_line = src.lines().next().expect("non-empty");
    assert!(
        first_line.contains("#![deny(unsafe_code)]"),
        "ramfs.rs 必须含 #![deny(unsafe_code)]"
    );
    let unsafe_count = src.matches("unsafe {").count() + src.matches("unsafe fn").count();
    assert_eq!(unsafe_count, 0, "ramfs.rs 必须 0 unsafe 块");
}
