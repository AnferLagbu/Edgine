//! ELF 加载器 FFI 安全代理 — privileged TCB
//!
//! ## 职责
//!
//! 这是 functions 层与 `kernel::crate::privileged::proc::elf::elf_*` 之间的**唯一** unsafe 边界。
//! 所有 `unsafe { ... }` 块都集中在本模块处理, functions 层 0 unsafe。
//!
//! ## 设计原则
//!
//! 1. 每个 `unsafe { ... }` 块都带 SAFETY 注释
//! 2. 切片 API (`&[u8]`) 替代 `*const u8` + 长度
//! 3. 强类型 `Elf64Header` / `ElfLoadResult` 替代裸结构体
//!
//! 评估日期: 2026-06-04

use crate::privileged::mm::MmStruct;
use crate::privileged::proc;

// ============================================================================
// ELF 校验
// ============================================================================

/// 校验 ELF 头部
///
/// # Safety
///
/// `data` 必须为至少 64 字节的有效切片, 调用期间不释放。
pub fn elf_validate(data: *const u8, len: u64) -> Option<&'static proc::Elf64Header> {
    proc::elf_validate(data, len)
}

// ============================================================================
// ELF 加载
// ============================================================================

/// 加载 ELF 镜像到用户内存空间
///
/// # Safety
///
/// - `mm` 必须指向目标进程的有效 `MmStruct`
/// - 调用方保证 `mm` 在加载期间不被其他线程访问
/// - `data` 必须为有效切片, 调用期间不释放
///
/// # Errors
/// 错误与 `privileged::proc::elf_load` 相同: 包括 `"Invalid ELF header"`,
/// `"No program headers"`, `"ELF: vaddr + memsz overflow"`,
/// `"ELF: p_offset + p_filesz overflow"` 与 `"OOM loading ELF"`.
pub fn elf_load(
    mm: &MmStruct,
    data: *const u8,
    len: u64,
) -> Result<proc::ElfLoadResult, &'static str> {
    proc::elf_load(mm, data, len)
}
