//! ELF 验证 — 单一来源 (P1-I-33)
//!
//! ## 框架责任分离
//!
//! - **机制 (privileged/TCB)**: 原始 ELF header 字段访问 (本模块允许 unsafe)
//! - **策略 (functions/proc/elf)**: 加载/解析/映射 (通过本模块)
//!
//! ## 历史
//!
//! I-33: 原 `privileged::proc::elf::elf_validate` 与 `privileged::proc::user_proc::load_elf_from_memory`
//! 各自实现一份 ELF magic / class / machine 校验, 解析方式不一致, 容易出现一处修复另一处遗漏.
//! 本模块抽出 `verify_elf` 作为**唯一**验证入口, 两个调用方都改用同一函数.
//!
//! 详见 [docs/plan/maintenance-2026-06-11.md] I-33.

use super::{Elf64Header, Elf64Phdr, MAX_PHDR_COUNT};

/// ELF64 验证结果 — 调用方据 `is_pie` / `machine` / `entry` 等字段做后续决策
#[derive(Debug, Clone, Copy)]
pub struct VerifyResult {
    /// `e_machine`: 0x3E (`x86_64`) / 0xB7 (aarch64)
    pub machine: u16,
    /// `e_type == ET_DYN (3)`: PIE 共享对象
    pub is_pie: bool,
    /// `e_entry`: 程序入口点虚拟地址 (未加 `load_bias`)
    pub entry: u64,
    /// `e_phoff`: program header table 偏移
    pub phoff: u64,
    /// `e_phentsize`: 单个 PHDR 大小 (字节), 必 == sizeof(Elf64Phdr)
    pub phentsize: u16,
    /// `e_phnum`: PHDR 数量 (≤ `MAX_PHDR_COUNT`)
    pub phnum: u16,
}

/// ELF magic
const ELF_MAGIC: &[u8; 4] = b"\x7FELF";
/// ELFCLASS64
const ELF_CLASS_64: u8 = 2;
/// `x86_64` 机器码
pub const EM_X86_64: u16 = 0x3E;
/// aarch64 机器码
pub const EM_AARCH64: u16 = 0xB7;
/// `ET_DYN`: 共享对象 / PIE
pub const ET_DYN: u16 = 3;

/// 本内核编译期目标架构的 `e_machine` (S-6 防线②).
///
/// 裸机 target 下由 `target_arch` 唯一确定; host 构建 (host-tests 与 kernel 的
/// host-test / kernel_test 维度) 不绑定单一架构 ⇒ `None`, 使同一套用例能覆盖两
/// 种机器码的格式校验语义.
#[cfg(all(target_os = "none", target_arch = "x86_64"))]
pub const EXPECTED_E_MACHINE: Option<u16> = Some(EM_X86_64);
/// 见 [`EXPECTED_E_MACHINE`].
#[cfg(all(target_os = "none", target_arch = "aarch64"))]
pub const EXPECTED_E_MACHINE: Option<u16> = Some(EM_AARCH64);
/// 见 [`EXPECTED_E_MACHINE`].
#[cfg(not(target_os = "none"))]
pub const EXPECTED_E_MACHINE: Option<u16> = None;

/// `e_machine` 是否可被本内核加载 — 抽为纯函数供 host 侧直接覆盖两架构分支.
///
/// - `expected = Some(em)`: 裸机构建, 只接受本目标架构的机器码.
/// - `expected = None`: host 构建 (不绑定架构), 接受两架构作为格式校验.
pub const fn machine_acceptable(expected: Option<u16>, actual: u16) -> bool {
    match expected {
        Some(em) => em == actual,
        None => actual == EM_X86_64 || actual == EM_AARCH64,
    }
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
/// 验证 ELF 文件头 + program header table 边界
///
/// ## 校验项
///
/// 1. `elf_data != null && elf_size >= sizeof(Elf64Header)`  // 缓冲长度检查
/// 2. `e_ident[0..4] == b"\x7FELF"`  // ELF 文件魔数
/// 3. `e_ident` `[4]` == 2 (ELFCLASS64)  // 64 位 ELF
/// 4. `machine_acceptable(EXPECTED_E_MACHINE, e_machine)` — 裸机只接受**本目标架构**
///    的机器码 (S-6 防线②), host 构建接受 `x86_64` / aarch64
/// 5. `e_phentsize == sizeof(Elf64Phdr)` (56)  // PHDR 项大小
/// 6. `e_phnum <= MAX_PHDR_COUNT` (128)  // PHDR 数量上限
/// 7. `e_phoff + e_phnum * e_phentsize <= elf_size` (PHDR 表不越界)  // 边界
///
/// # Errors
/// 校验失败时返回对应的 `VerifyError`: `TooSmall` (指针为空或尺寸不足),
/// `BadMagic` (魔数不匹配), `BadClass` (非 64 位 ELF), `BadMachine` (与本内核目标架构不符),
/// `BadPhentsize` (PHDR 项大小不符), `TooManyPhdr` (PHDR 数量超限),
/// `PhdrOutOfBounds` (PHDR 表越出文件边界), `Overflow` (PHDR 表尺寸算术溢出).
///
/// # Safety
///
/// - `elf_data` 必须指向 `elf_size` 字节的可读内核虚拟地址
/// - 调用方负责保证指针/类型有效 (此函数本身不持有所有权)
pub unsafe fn verify_elf(elf_data: *const u8, elf_size: u64) -> Result<VerifyResult, VerifyError> {
    if elf_data.is_null() || elf_size < core::mem::size_of::<Elf64Header>() as u64 {
        return Err(VerifyError::TooSmall);
    }

    // SAFETY: 调用方保证 elf_data 有效, 仅读借用
    let header = unsafe { &*(elf_data as *const Elf64Header) };

    // magic
    if &header.e_ident[0..4] != ELF_MAGIC {
        return Err(VerifyError::BadMagic);
    }
    // class
    if header.e_ident[4] != ELF_CLASS_64 {
        return Err(VerifyError::BadClass);
    }
    // machine: 必须与本内核编译期目标架构一致 (S-6 防线②). 旧写法只校
    // `∈ {0x3E, 0xB7}`, 使得“嵌 aarch64 用户态的 x86_64 镜像”照样 verify_elf OK,
    // 直到入口 0x400000 执行非法指令才 #PF — 报错点距根因极远.
    if !machine_acceptable(EXPECTED_E_MACHINE, header.e_machine) {
        return Err(VerifyError::BadMachine);
    }
    // phentsize
    if header.e_phentsize as usize != core::mem::size_of::<Elf64Phdr>() {
        return Err(VerifyError::BadPhentsize);
    }
    // phnum
    if header.e_phnum as usize > MAX_PHDR_COUNT {
        return Err(VerifyError::TooManyPhdr);
    }
    // phdr 表边界检查
    let phdr_table_size = u64::from(header.e_phnum)
        .checked_mul(u64::from(header.e_phentsize))
        .ok_or(VerifyError::Overflow)?;
    let phdr_end = header
        .e_phoff
        .checked_add(phdr_table_size)
        .ok_or(VerifyError::Overflow)?;
    if phdr_end > elf_size {
        return Err(VerifyError::PhdrOutOfBounds);
    }

    Ok(VerifyResult {
        machine: header.e_machine,
        is_pie: header.e_type == ET_DYN,
        entry: header.e_entry,
        phoff: header.e_phoff,
        phentsize: header.e_phentsize,
        phnum: header.e_phnum,
    })
}

/// ELF 验证错误 — 区分原因便于调试与 host-test 验证
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// 指针为空或 size < sizeof(Elf64Header)
    TooSmall,
    /// 魔数不匹配 (非 ELF 文件)
    BadMagic,
    /// 非 ELFCLASS64
    BadClass,
    /// `e_machine` 与本内核目标架构不符 (裸机), 或非 `x86_64`/aarch64 (host)
    BadMachine,
    /// phentsize 与 sizeof(Elf64Phdr) 不符
    BadPhentsize,
    /// phnum > `MAX_PHDR_COUNT`
    TooManyPhdr,
    /// PHDR 表溢出文件边界
    PhdrOutOfBounds,
    /// 算术溢出
    Overflow,
}
