#![deny(unsafe_code)]
//! ELF 加载器 — functions 层安全代理
//!
//! ## 状态 (v2.14, 2026-06-04)
//!
//! Phase 2.5 进程迁移 3/4 (ELF 加载):
//! - [x] ELF 头校验 (`validate`)
//! - [x] ELF 段加载 (`load`) — 由 privileged 层 `MmStruct` 状态机驱动
//! - [x] 段类型 / 段权限 / 段重定位
//! - [x] brk / `stack_top` 计算
//! - [x] 强类型 `ElfLoadResult` 透传
//!
//! ## 迁移方法
//!
//! 1. 内部把 `&[u8]` 切片转换为 `(*const u8, u64)` 指针长度对
//! 2. functions 层 0 unsafe — 所有 `unsafe { &*ptr }` 局限于 privileged `elf_*` 函数内部
//! 3. 错误码翻译 `ElfError`, 替代内核 `&'static str`
//!
//! 评估日期: 2026-06-04

use crate::privileged::mm::MmStruct;
use crate::privileged::proc_elf;
use crate::privileged::syscall::Errno;

// ============================================================================
// 强类型 re-export
// ============================================================================

/// ELF 64 字节头 (与 Linux ELF64 布局一致, 64 字节)
pub use crate::privileged::proc::Elf64Header;

/// ELF 64 程序头 (56 字节)
pub use crate::privileged::proc::Elf64Phdr;

/// ELF 加载结果 (entry / `phdr_addr` / `phdr_count` / brk / `stack_top`)
pub use crate::privileged::proc::ElfLoadResult;

// ============================================================================
// 错误
// ============================================================================

/// ELF 加载错误 — TD-19: 收敛到 `KernelError`, 7 字段 ELF 特有 + 1 共享包装.
///
/// 字段说明:
///   - `BadMagic` / `NotElf64` / `UnsupportedMachine`: ELF 格式校验失败 (POSIX ENOEXEC=8)
///   - `Truncated` / `PhdrOutOfRange` / `TooManyPhdr`: 解析阶段越界/超限
///   - `MapFailed`: 用户态 VMA 添加失败
///   - `Kernel(KernelError)`: 共享错误 (`AddressOverflow` → `InvalidArgument` 等) 走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// ELF 魔数错误
    BadMagic,
    /// 不是 64 位 ELF
    NotElf64,
    /// 不支持的机器类型 (非 `x86_64` / aarch64)
    UnsupportedMachine,
    /// ELF 头不完整
    Truncated,
    /// 程序头表越界
    PhdrOutOfRange,
    /// 程序头数量超限 (> 128)
    TooManyPhdr,
    /// 用户内存映射失败 (`MmStruct` 添加 VMA 失败)
    MapFailed,
    /// 共享 `KernelError` 包装
    Kernel(crate::functions::error::KernelError),
}

impl ElfError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::BadMagic | Self::NotElf64 | Self::UnsupportedMachine => E::ENOEXEC,
            Self::Truncated | Self::PhdrOutOfRange | Self::TooManyPhdr => E::EINVAL,
            Self::MapFailed => E::ENOMEM,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 从内核返回的 `&'static str` 翻译为 `ElfError`
    pub fn from_kernel_str(s: &'static str) -> Self {
        use crate::functions::error::KernelError as K;
        match s {
            "Invalid ELF header" => Self::Truncated,
            "No program headers" => Self::BadMagic,
            "ELF: vaddr + memsz overflow" => Self::Kernel(K::InvalidArgument),
            "ELF: p_offset + p_filesz overflow" => Self::Kernel(K::InvalidArgument),
            other => {
                // 通用兜底: 归类为 NoSuchProcess 之外的通用错误
                // 实际未知字符串属低频路径, 走 NoSuchProcess 占位不准确,
                // 改走 InvalidArgument 表达 "未知 ELF 头失败"
                let _ = other;
                Self::Kernel(K::InvalidArgument)
            }
        }
    }
}

pub type ElfResult<T> = Result<T, ElfError>;

// ============================================================================
// 校验
// ============================================================================

/// 校验 ELF 头 (魔数 / 类 / 机器类型 / 段表大小 / 段数)
///
/// **参数**:
/// - `elf_data`: 完整 ELF 镜像字节切片
///
/// **返回**:
/// - `Ok(Elf64Header)`: 校验通过
/// - `Err(ElfError)`: 校验失败
///
/// # Errors
///
/// 当 ELF 镜像过短或头部字段非法时返回 `ElfError::Truncated`.
pub fn validate(elf_data: &[u8]) -> ElfResult<Elf64Header> {
    let header = proc_elf::elf_validate(elf_data.as_ptr(), elf_data.len() as u64);
    header.ok_or(ElfError::Truncated).map(|h| Elf64Header {
        e_ident: h.e_ident,
        e_type: h.e_type,
        e_machine: h.e_machine,
        e_version: h.e_version,
        e_entry: h.e_entry,
        e_phoff: h.e_phoff,
        e_shoff: h.e_shoff,
        e_flags: h.e_flags,
        e_ehsize: h.e_ehsize,
        e_phentsize: h.e_phentsize,
        e_phnum: h.e_phnum,
        e_shentsize: h.e_shentsize,
        e_shnum: h.e_shnum,
        e_shstrndx: h.e_shstrndx,
    })
}

// ============================================================================
// 加载
// ============================================================================

/// 加载 ELF 镜像到用户内存空间
///
/// 遍历 `PT_LOAD` 段, 为每个段建立 VMA 并复制数据。
///
/// **参数**:
/// - `mm`: 目标进程的内存描述符 (由调用方持有可变借用)
/// - `elf_data`: 完整 ELF 镜像字节切片
///
/// **返回**:
/// - `Ok(ElfLoadResult)`: 加载成功, 包含 entry / `phdr_addr` / `brk_base` / `stack_top`
/// - `Err(ElfError)`: 加载失败
///
/// **Safety**:
/// - `mm` 必须指向目标进程的有效 `MmStruct`
/// - 调用方保证 `mm` 在加载期间不被其他线程访问
///
/// # Errors
///
/// 当 ELF 加载失败(段越界、内存分配失败等)时返回对应的 `ElfError`.
pub fn load(mm: &MmStruct, elf_data: &[u8]) -> ElfResult<ElfLoadResult> {
    proc_elf::elf_load(mm, elf_data.as_ptr(), elf_data.len() as u64)
        .map_err(ElfError::from_kernel_str)
}

// ============================================================================
// 段类型常量 (透传)
// ============================================================================

/// `PT_LOAD`: 可加载段
pub const PT_LOAD: u32 = 1;

/// `PT_GNU_STACK`: GNU 栈属性
pub const PT_GNU_STACK: u32 = 0x6474E551;

/// `PF_X`: 可执行
pub const PF_X: u32 = 1;

/// `PF_W`: 可写
pub const PF_W: u32 = 2;

/// `PF_R`: 可读
pub const PF_R: u32 = 4;

// ============================================================================
// 强类型便利函数
// ============================================================================

/// ELF 头是否合法 (直接布尔, 不返回错误细节)
#[inline]
pub fn is_valid(elf_data: &[u8]) -> bool {
    validate(elf_data).is_ok()
}

/// 获取入口地址
#[inline]
pub fn entry_point(elf_data: &[u8]) -> u64 {
    validate(elf_data).map_or(0, |h| h.e_entry)
}

/// 获取机器类型 (0x3E = `x86_64`, 0xB7 = aarch64)
#[inline]
pub fn machine(elf_data: &[u8]) -> u16 {
    validate(elf_data).map_or(0, |h| h.e_machine)
}

/// 是否为 64 位 ELF
#[inline]
pub fn is_64bit(elf_data: &[u8]) -> bool {
    elf_data.len() >= 5 && elf_data[4] == 2
}

/// 是否为可执行文件 (`e_type` == 2: `ET_EXEC`)
#[inline]
pub fn is_executable(elf_data: &[u8]) -> bool {
    if elf_data.len() < 16 {
        return false;
    }
    let e_type = u16::from_le_bytes([elf_data[16], elf_data[17]]);
    e_type == 2
}
