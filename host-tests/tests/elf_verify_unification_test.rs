//! ELF 验证双份复制修复测试 (P1-I-33)
//!
//! ## 验证契约
//!
//! 1. **单一来源**: `privileged::proc::elf::verify::verify_elf` 是 ELF magic / class / machine /
//!    phentsize / phnum / phdr-bounds 校验的**唯一**入口, 旧版本在 `elf.rs::elf_validate` 与
//!    `user_proc.rs::load_elf_from_memory` 各写一份, I-33 统一抽到 `verify.rs`.
//! 2. **解析一致**: 两处实现不再独立 (host-test 通过源码静态文本扫描确认两份独立
//!    `e_ident[0..4] != 0x7F/0x45/0x4c/0x46` 字符串字面量已消除).
//! 3. **跨架构**: x86_64 (0x3E) 与 aarch64 (0xB7) 均接受, 其它机器码拒绝.
//! 4. **错误细分**: 7 类错误 (TooSmall / BadMagic / BadClass / BadMachine /
//!    BadPhentsize / TooManyPhdr / PhdrOutOfBounds) 行为可观测.
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `verify_elf` 算法复刻 + `VerifyError` / `VerifyResult` / `Elf64Header`
//! 平行镜像, 改引内核真实源码 `edgine::kernel::privileged::proc::elf::verify::verify_elf`
//! (pub unsafe fn, host 可测, 传 host 缓冲区指针 + 长度). 内核 `verify_elf` 为
//! **unsafe** 函数, 测试调用需 unsafe 块.
//! - `EM_X86_64` / `EM_AARCH64` / `ET_DYN` 为内核 pub const, 直接引用.
//! - `ELF_MAGIC` / `ELF_CLASS_64` / `MAX_PHDR_COUNT` 为内核私有常量, 测试侧保留
//!   镜像并标注同步 (参考 lib_string_strlen_safe 的 STRLEN_MAX 做法).
//!
//! ## 保留
//! 静态契约用例 (源码文本扫描 user_proc.rs / elf/mod.rs 无重复 magic 字面量,
//! verify 子模块声明与委托) 为 B08-20 混合型文件的 include_str 部分, 原样保留.

use edgine::kernel::privileged::proc::elf::verify::{
    EM_AARCH64, EM_X86_64, ET_DYN, VerifyError, VerifyResult, verify_elf,
};
use edgine::kernel::privileged::proc::elf::{Elf64Header, Elf64Phdr};

// =============================================================================
// 镜像内核私有常量 (verify.rs, 非 pub)
// =============================================================================

/// 内核私有常量镜像 (verify.rs `const ELF_MAGIC`)
const ELF_MAGIC: &[u8; 4] = b"\x7FELF";
/// 内核私有常量镜像 (verify.rs `const ELF_CLASS_64`)
const ELF_CLASS_64: u8 = 2;
/// 内核私有常量镜像 (elf/mod.rs `const MAX_PHDR_COUNT`, verify.rs 经 use 访问)
const MAX_PHDR_COUNT: usize = 128;

// =============================================================================
// 测试装置: 构造合法 ELF64 header + 任意 phdr 表
// =============================================================================

/// 构造合法 ELF64 header + 任意 phdr 表 (用内核 `Elf64Header` 布局)
fn make_elf(machine: u16, e_type: u16, phnum: u16, phoff: u64, phentsize: u16) -> Vec<u8> {
    let header_size = core::mem::size_of::<Elf64Header>();
    let phdr_table_size = phnum as usize * phentsize as usize;
    let total = header_size + phdr_table_size;
    let mut buf = vec![0u8; total];

    // SAFETY: buf 长度已 ≥ header
    let header = unsafe { &mut *(buf.as_mut_ptr() as *mut Elf64Header) };
    header.e_ident[0..4].copy_from_slice(ELF_MAGIC);
    header.e_ident[4] = ELF_CLASS_64;
    header.e_type = e_type;
    header.e_machine = machine;
    header.e_entry = 0x400000;
    header.e_phoff = phoff;
    header.e_phentsize = phentsize;
    header.e_phnum = phnum;
    buf
}

/// 调用内核 `verify_elf` (unsafe: 传入 host 缓冲区指针 + 长度)
///
/// # Safety
/// `elf_data` 必须是 `elf_size` 字节的可读 host 内存 (由调用方 Vec 保证).
unsafe fn call_verify_elf(elf: &[u8]) -> Result<VerifyResult, VerifyError> {
    unsafe { verify_elf(elf.as_ptr(), elf.len() as u64) }
}

// =============================================================================
// 静态契约 (源码文本扫描)
// =============================================================================

/// 镜像旧 `user_proc.rs` 实现的 magic 字符串字面量 (0x7F/E/L/F 各字节比较)
const USER_PROC_OLD_MAGIC_LITERALS: &str = "0x7F, b'E', b'L', b'F'";

#[test]
fn elf_source_files_do_not_duplicate_magic_literal() {
    // P1-I-33: 源码扫描 — user_proc.rs 不应再出现 4 字节独立 magic 字符串字面量
    let user_proc = include_str!("../../src/kernel/privileged/proc/user_proc.rs");
    assert!(
        !user_proc.contains(USER_PROC_OLD_MAGIC_LITERALS),
        "P1-I-33: user_proc.rs 仍含独立 magic 字面量 `{USER_PROC_OLD_MAGIC_LITERALS}`, 需委托给 elf::verify::verify_elf"
    );

    // 同样 elf/mod.rs 的 elf_validate 不应再内联 magic/class/machine 检查
    let elf_mod = include_str!("../../src/kernel/privileged/proc/elf/mod.rs");
    assert!(
        !elf_mod.contains("ELF_MAGIC") || elf_mod.contains("verify::verify_elf"),
        "P1-I-33: elf/mod.rs 仍内联 ELF_MAGIC 字面量, 应委托给 verify::verify_elf"
    );
}

#[test]
fn elf_mod_declares_verify_submodule() {
    let elf_mod = include_str!("../../src/kernel/privileged/proc/elf/mod.rs");
    assert!(
        elf_mod.contains("pub mod verify"),
        "P1-I-33: elf/mod.rs 必须声明 `pub mod verify`"
    );
    assert!(
        elf_mod.contains("verify::verify_elf"),
        "P1-I-33: elf::elf_validate 应委托给 verify::verify_elf"
    );
}

#[test]
fn user_proc_load_elf_uses_verify_submodule() {
    let user_proc = include_str!("../../src/kernel/privileged/proc/user_proc.rs");
    assert!(
        user_proc.contains("elf::verify::verify_elf"),
        "P1-I-33: user_proc::load_elf_from_memory 必须调用 elf::verify::verify_elf"
    );
}

// =============================================================================
// 内核 verify_elf 行为验证 (7 类校验)
// =============================================================================

#[test]
fn verify_x86_64_elf64_succeeds() {
    let elf = make_elf(
        EM_X86_64,
        2, /* ET_EXEC */
        1,
        64,
        core::mem::size_of::<Elf64Phdr>() as u16,
    );
    // SAFETY: elf 是完整 host Vec
    let v = unsafe { call_verify_elf(&elf) }.expect("x86_64 ELF64 must verify");
    assert_eq!(v.machine, EM_X86_64);
    assert!(!v.is_pie);
    assert_eq!(v.entry, 0x400000);
    assert_eq!(v.phnum, 1);
}

#[test]
fn verify_aarch64_elf64_succeeds() {
    let elf = make_elf(
        EM_AARCH64,
        ET_DYN,
        0,
        64,
        core::mem::size_of::<Elf64Phdr>() as u16,
    );
    // SAFETY: elf 是完整 host Vec
    let v = unsafe { call_verify_elf(&elf) }.expect("aarch64 ELF64 must verify");
    assert_eq!(v.machine, EM_AARCH64);
    assert!(v.is_pie);
    assert_eq!(v.phnum, 0);
}

#[test]
fn verify_rejects_bad_magic() {
    let mut elf = make_elf(
        EM_X86_64,
        2,
        0,
        64,
        core::mem::size_of::<Elf64Phdr>() as u16,
    );
    elf[0] = b'X'; // 破坏 magic
    // SAFETY: elf 是完整 host Vec
    assert_eq!(
        unsafe { call_verify_elf(&elf) }.unwrap_err(),
        VerifyError::BadMagic
    );
}

#[test]
fn verify_rejects_bad_class() {
    let mut elf = make_elf(
        EM_X86_64,
        2,
        0,
        64,
        core::mem::size_of::<Elf64Phdr>() as u16,
    );
    elf[4] = 1; // ELFCLASS32
    // SAFETY: elf 是完整 host Vec
    assert_eq!(
        unsafe { call_verify_elf(&elf) }.unwrap_err(),
        VerifyError::BadClass
    );
}

#[test]
fn verify_rejects_bad_machine() {
    // 0x03 (i386) 不在白名单
    let elf = make_elf(0x03, 2, 0, 64, core::mem::size_of::<Elf64Phdr>() as u16);
    // SAFETY: elf 是完整 host Vec
    assert_eq!(
        unsafe { call_verify_elf(&elf) }.unwrap_err(),
        VerifyError::BadMachine
    );
}

#[test]
fn verify_rejects_bad_phentsize() {
    let elf = make_elf(EM_X86_64, 2, 0, 64, 32); // phentsize 错
    // SAFETY: elf 是完整 host Vec
    assert_eq!(
        unsafe { call_verify_elf(&elf) }.unwrap_err(),
        VerifyError::BadPhentsize
    );
}

#[test]
fn verify_rejects_too_many_phdr() {
    let elf = make_elf(
        EM_X86_64,
        2,
        (MAX_PHDR_COUNT as u16) + 1,
        64,
        core::mem::size_of::<Elf64Phdr>() as u16,
    );
    // SAFETY: elf 是完整 host Vec
    assert_eq!(
        unsafe { call_verify_elf(&elf) }.unwrap_err(),
        VerifyError::TooManyPhdr
    );
}

#[test]
fn verify_rejects_phdr_out_of_bounds() {
    // phoff=64, phnum=2, phentsize=56, total phdr = 112, 加上 header 64 = 176 总需求
    // 实际只给 100 字节 (header 64 + 36 phdr 字节), 不够 → OutOfBounds
    let mut elf = vec![0u8; 100];
    // SAFETY: 长度 ≥ header
    let header = unsafe { &mut *(elf.as_mut_ptr() as *mut Elf64Header) };
    header.e_ident[0..4].copy_from_slice(ELF_MAGIC);
    header.e_ident[4] = ELF_CLASS_64;
    header.e_type = 2;
    header.e_machine = EM_X86_64;
    header.e_phoff = 64;
    header.e_phentsize = core::mem::size_of::<Elf64Phdr>() as u16;
    header.e_phnum = 2;
    // SAFETY: elf 是完整 host Vec
    assert_eq!(
        unsafe { call_verify_elf(&elf) }.unwrap_err(),
        VerifyError::PhdrOutOfBounds
    );
}

#[test]
fn verify_rejects_too_small() {
    let elf = vec![0u8; 10]; // 远小于 sizeof(Elf64Header)=64
    // SAFETY: elf 是完整 host Vec
    assert_eq!(
        unsafe { call_verify_elf(&elf) }.unwrap_err(),
        VerifyError::TooSmall
    );
}

#[test]
fn verify_null_ptr_is_too_small() {
    // 内核 verify_elf: null 指针 → TooSmall
    // SAFETY: 显式 null 检查路径
    assert_eq!(
        unsafe { verify_elf(core::ptr::null(), 0) }.unwrap_err(),
        VerifyError::TooSmall
    );
}
