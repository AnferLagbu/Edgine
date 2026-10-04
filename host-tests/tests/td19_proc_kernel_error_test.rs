// SPDX-License-Identifier: MPL-2.0
// TD-19: functions::proc::ElfError / MlockError / ProcError 收敛到 KernelError (TD-08 V5)
//
// 验收:
//   - proc/elf.rs: ElfError 12 字段 → 7 ELF 特有 + 1 Kernel 包装
//   - proc/madvise_mlock.rs: MlockError 7 字段 → 1 mlock 特有 + 1 Kernel 包装
//   - proc/mod.rs: ProcError 6 字段 → 1 proc 特有 + 1 Kernel 包装
//   - 旧变体全部废弃, 共享 POSIX 错误走 Kernel(KernelError) 包装
//   - 3 个 to_errno() 方法 全部变体 → POSIX Errno 双向映射
//
// 运行: cargo test -p host-tests --test td19_proc_kernel_error_test

use std::fs;
use std::path::Path;

const ELF_RS: &str = "src/kernel/functions/proc/elf.rs";
const MLOCK_RS: &str = "src/kernel/functions/proc/madvise_mlock.rs";
const PROC_MOD_RS: &str = "src/kernel/functions/proc/mod.rs";

fn read(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ============================================================================
// ElfError
// ============================================================================

#[test]
fn elf_error_is_thin_wrapper() {
    let src = read(ELF_RS);
    assert!(
        src.contains("Kernel(crate::functions::error::KernelError)"),
        "ElfError 必须含 `Kernel(KernelError)` 共享包装字段"
    );
}

#[test]
fn no_legacy_elf_error_variants() {
    let src = read(ELF_RS);
    for legacy in &[
        "ElfError::NoLoadableSegment",
        "ElfError::AddressOverflow",
        "ElfError::InvalidSize",
        "ElfError::Other",
    ] {
        assert!(
            !src.contains(legacy),
            "{} 已废弃, 应改走 ElfError::Kernel(KernelError::...)",
            legacy
        );
    }
}

#[test]
fn elf_error_preserves_seven_specific_variants() {
    let src = read(ELF_RS);
    for variant in &[
        "BadMagic",
        "NotElf64",
        "UnsupportedMachine",
        "Truncated",
        "PhdrOutOfRange",
        "TooManyPhdr",
        "MapFailed",
    ] {
        assert!(src.contains(variant), "ElfError 应保留 {} 变体", variant);
    }
}

#[test]
fn elf_error_to_errno_present() {
    let src = read(ELF_RS);
    assert!(
        src.contains("pub fn to_errno(self) -> Errno"),
        "ElfError 必须有 to_errno() 方法"
    );
    let to_errno_block_start = src.find("pub fn to_errno(self)").expect("to_errno 存在");
    let block = &src[to_errno_block_start..to_errno_block_start + 600];
    for variant in &[
        "Self::BadMagic",
        "Self::NotElf64",
        "Self::UnsupportedMachine",
        "Self::Truncated",
        "Self::PhdrOutOfRange",
        "Self::TooManyPhdr",
        "Self::MapFailed",
        "Self::Kernel",
    ] {
        assert!(
            block.contains(variant),
            "ElfError::to_errno 必须映射 {} 变体",
            variant
        );
    }
    // 验证 POSIX 目标: ENOEXEC=8, EINVAL=22, ENOMEM=12
    assert!(
        block.contains("E::ENOEXEC"),
        "ElfError 格式错误应映射 ENOEXEC"
    );
    assert!(
        block.contains("E::EINVAL"),
        "ElfError 解析错误应映射 EINVAL"
    );
    assert!(
        block.contains("E::ENOMEM"),
        "ElfError::MapFailed 应映射 ENOMEM"
    );
}

#[test]
fn elf_error_from_kernel_str_uses_kernel_wrapper() {
    let src = read(ELF_RS);
    let from_block_start = src
        .find("pub fn from_kernel_str(s: &'static str) -> Self")
        .expect("from_kernel_str 存在");
    let block = &src[from_block_start..from_block_start + 800];
    // ELF 溢出错误应改走 Kernel(K::InvalidArgument) 包装
    let kernel_count = block.matches("Self::Kernel(K::").count()
        + block
            .matches("Self::Kernel(crate::functions::error::KernelError::")
            .count();
    assert!(
        kernel_count >= 2,
        "ElfError::from_kernel_str 至少应有 2 处使用 Kernel(K::...) 包装, 实际: {}",
        kernel_count
    );
}

// ============================================================================
// MlockError
// ============================================================================

#[test]
fn mlock_error_is_thin_wrapper() {
    let src = read(MLOCK_RS);
    assert!(
        src.contains("Kernel(crate::functions::error::KernelError)"),
        "MlockError 必须含 `Kernel(KernelError)` 共享包装字段"
    );
}

#[test]
fn no_legacy_mlock_error_variants() {
    let src = read(MLOCK_RS);
    for legacy in &[
        "MlockError::InvalidArgument",
        "MlockError::BadAddress",
        "MlockError::OutOfMemory",
        "MlockError::NoResources",
        "MlockError::PermissionDenied",
    ] {
        // 容忍 mlock_error::from_errno 中映射的定义 (Kernel(K::InvalidArgument)),
        // 禁用 MlockError::InvalidArgument / MlockError::OutOfMemory 等独立变体
        // 即: 在 MlockError 字段定义中, 不应再出现 "InvalidArgument," 等作为独立 enum 变体
        // 简化: 直接检查 .rs 文件中不存在 enum 字段变体声明
        let has_variant_decl = src
            .contains(&format!("    {},\n", legacy.replace("MlockError::", "")))
            || src.contains(&format!("    {},", legacy.replace("MlockError::", "")));
        assert!(
            !has_variant_decl,
            "{} 旧变体声明应废弃, 应改走 Kernel(KernelError::...) 包装",
            legacy
        );
    }
}

#[test]
fn mlock_error_preserves_not_mapped() {
    let src = read(MLOCK_RS);
    let enum_block_start = src.find("pub enum MlockError {").expect("MlockError 存在");
    let search_from = enum_block_start + "pub enum MlockError {".len();
    let enum_block_end = src[search_from..].find("}").expect("enum 闭合") + search_from;
    let enum_block = &src[enum_block_start..enum_block_end];
    assert!(
        enum_block.contains("NotMapped"),
        "MlockError 应保留 NotMapped 变体 (kernel thread 路径)"
    );
    assert!(
        enum_block.contains("Kernel(crate::functions::error::KernelError)"),
        "MlockError 应含 Kernel 包装"
    );
}

#[test]
fn mlock_error_to_errno_present() {
    let src = read(MLOCK_RS);
    assert!(
        src.contains("pub fn to_errno(self) -> Errno"),
        "MlockError 必须有 to_errno() 方法"
    );
    let to_errno_block_start = src.find("pub fn to_errno(self)").expect("to_errno 存在");
    let block = &src[to_errno_block_start..to_errno_block_start + 400];
    assert!(
        block.contains("Self::NotMapped"),
        "MlockError::to_errno 必须映射 NotMapped"
    );
    assert!(
        block.contains("Self::Kernel"),
        "MlockError::to_errno 必须映射 Kernel"
    );
    assert!(
        block.contains("E::ESRCH"),
        "MlockError::NotMapped 应映射 ESRCH"
    );
}

#[test]
fn mlock_error_from_errno_uses_kernel_wrapper() {
    let src = read(MLOCK_RS);
    let from_block_start = src
        .find("pub fn from_errno(e: Errno) -> Self")
        .expect("from_errno 存在");
    let block = &src[from_block_start..from_block_start + 600];
    let kernel_count = block.matches("Self::Kernel(K::").count();
    assert!(
        kernel_count >= 6,
        "MlockError::from_errno 至少应有 6 处使用 Kernel(K::...) 包装 (EINVAL/EFAULT/ENOMEM/ENOSPC/EAGAIN/EPERM/兜底), 实际: {}",
        kernel_count
    );
}

#[test]
fn mlock_error_mincore_uses_kernel_wrapper() {
    let src = read(MLOCK_RS);
    // rustfmt 拆 MlockError::Kernel(crate::...::KernelError::InvalidArgument) 为多行;
    // contains() 不跨行, 用 normalized 字符串 (删除空白) 匹配.
    // 注意: rustfmt 在 InvalidArgument, 后再加 ), 所以匹配片段为 InvalidArgument,
    let normalized: String = src.split_whitespace().collect::<Vec<_>>().join("");
    assert!(
        normalized
            .contains("MlockError::Kernel(crate::functions::error::KernelError::InvalidArgument,"),
        "mincore 函数中的 MlockError 使用点应改走 Kernel(K::InvalidArgument) 包装"
    );
}

// ============================================================================
// ProcError
// ============================================================================

#[test]
fn proc_error_is_thin_wrapper() {
    let src = read(PROC_MOD_RS);
    assert!(
        src.contains("Kernel(crate::functions::error::KernelError)"),
        "ProcError 必须含 `Kernel(KernelError)` 共享包装字段"
    );
}

#[test]
fn no_legacy_proc_error_variants() {
    let src = read(PROC_MOD_RS);
    for legacy in &[
        "ProcError::NotFound",
        "ProcError::PermissionDenied",
        "ProcError::NoResources",
        "ProcError::InvalidArgument",
    ] {
        // 类似 MlockError 检查, 不在 enum 字段定义中出现
        let has_variant_decl = src
            .contains(&format!("    {},\n", legacy.replace("ProcError::", "")))
            || src.contains(&format!("    {},", legacy.replace("ProcError::", "")));
        assert!(
            !has_variant_decl,
            "{} 旧变体声明应废弃, 应改走 ProcError::Kernel(KernelError::...) 包装",
            legacy
        );
    }
}

#[test]
fn proc_error_preserves_exited() {
    let src = read(PROC_MOD_RS);
    let enum_block_start = src.find("pub enum ProcError {").expect("ProcError 存在");
    let search_from = enum_block_start + "pub enum ProcError {".len();
    let enum_block_end = src[search_from..].find("}").expect("enum 闭合") + search_from;
    let enum_block = &src[enum_block_start..enum_block_end];
    assert!(
        enum_block.contains("Exited"),
        "ProcError 应保留 Exited 变体"
    );
}

#[test]
fn proc_error_to_errno_present() {
    let src = read(PROC_MOD_RS);
    assert!(
        src.contains("pub fn to_errno(self) -> Errno"),
        "ProcError 必须有 to_errno() 方法"
    );
    let to_errno_block_start = src.find("pub fn to_errno(self)").expect("to_errno 存在");
    let block = &src[to_errno_block_start..to_errno_block_start + 400];
    assert!(
        block.contains("Self::Exited"),
        "ProcError::to_errno 必须映射 Exited"
    );
    assert!(
        block.contains("Self::Kernel"),
        "ProcError::to_errno 必须映射 Kernel"
    );
    assert!(block.contains("E::ESRCH"), "ProcError::Exited 应映射 ESRCH");
}

#[test]
fn proc_error_from_i32_uses_kernel_wrapper() {
    let src = read(PROC_MOD_RS);
    let from_block_start = src
        .find("pub fn from_i32(rc: i32) -> Self")
        .expect("from_i32 存在");
    let block = &src[from_block_start..from_block_start + 500];
    let kernel_count = block.matches("Self::Kernel(K::").count();
    assert!(
        kernel_count >= 4,
        "ProcError::from_i32 至少应有 4 处使用 Kernel(K::...) 包装 (-1/-2/-3/-22), 实际: {}",
        kernel_count
    );
    // 验证 -1 → NoSuchProcess
    assert!(
        block.contains("-1 => Self::Kernel(K::NoSuchProcess)"),
        "ProcError::from_i32(-1) 应映射到 NoSuchProcess"
    );
}

#[test]
fn deny_unsafe_code_intact() {
    for (name, path) in &[("elf", ELF_RS), ("mlock", MLOCK_RS), ("proc", PROC_MOD_RS)] {
        let src = read(path);
        let first = src.lines().next().expect("non-empty");
        assert!(
            first.contains("#![deny(unsafe_code)]"),
            "{} 顶部必须含 #![deny(unsafe_code)]",
            name
        );
        // 过滤 doc 注释行 (以 //! 开头) 避免误判
        let code_lines: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//!"))
            .collect::<Vec<_>>()
            .join("\n");
        let unsafe_count =
            code_lines.matches("unsafe {").count() + code_lines.matches("unsafe fn").count();
        assert_eq!(
            unsafe_count, 0,
            "{} 必须 0 unsafe 块 (code 区域, 排除 doc 注释)",
            name
        );
    }
}
