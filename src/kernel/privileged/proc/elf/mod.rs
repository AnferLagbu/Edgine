//! ELF64 程序加载器
//!
//! 解析 ELF64 可执行文件, 创建 VMA 映射, 为 exec/load 提供统一入口。
//!
//! ## 加载流程
//!
//! ```text
//! load_elf(mm, elf_data)
//!   ├── 验证 ELF header (magic, class, machine)
//!   ├── 遍历 PT_LOAD 段
//!   │   ├── 创建 VMA (Anonymous, 对应权限)
//!   │   ├── 映射物理页 (demand paging 可选)
//!   │   └── 复制段数据 (从 ELF 文件)
//!   ├── 创建用户栈 VMA
//!   └── 返回 entry point
//! ```
//!
//! ## SAFETY
//!
//! `elf_data` 必须是有效的内核虚拟地址，指向完整 ELF 文件。
//! 调用者负责保证 ELF 数据在加载期间不被修改。

#![deny(missing_docs)]

// P1-I-33: ELF 验证抽到 verify 子模块, 单一来源
pub mod verify;

use crate::privileged::mm::{MmStruct, Vma, VmaType};
use crate::privileged::mm::{PAGE_SIZE, PageFlags, VirtAddr};

/// ELF64 文件头 (`Elf64_Ehdr`) — 描述文件类型、目标机器、入口地址与各表偏移。
///
/// 字段名对应 C `Elf64_Ehdr` 的 `e_ident` / `e_type` / `e_machine` / `e_version` /
/// `e_entry` / `e_phoff` / `e_shoff` / `e_flags` / `e_ehsize` / `e_phentsize` /
/// `e_phnum` / `e_shentsize` / `e_shnum` / `e_shstrndx`（按序仅去前缀, 顺序与类型不变）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Elf64Header {
    /// ELF 魔数、文件类、字节序与版本标识 (16 字节).
    pub ident: [u8; 16],
    /// 文件类型 (`ET_EXEC` / `ET_DYN` 等).
    pub ty: u16,
    /// 目标指令集架构 (`EM_X86_64` / `EM_AARCH64` 等).
    pub machine: u16,
    /// ELF 版本号.
    pub version: u32,
    /// 程序入口虚拟地址.
    pub entry: u64,
    /// 程序头表在文件中的偏移.
    pub phoff: u64,
    /// 节头表在文件中的偏移.
    pub shoff: u64,
    /// 处理器相关的标志位.
    pub flags: u32,
    /// ELF 文件头自身的大小.
    pub ehsize: u16,
    /// 单个程序头条目的大小.
    pub phentsize: u16,
    /// 程序头表条目数量.
    pub phnum: u16,
    /// 单个节头条目的大小.
    pub shentsize: u16,
    /// 节头表条目数量.
    pub shnum: u16,
    /// 节头字符串表在节头表中的索引.
    pub shstrndx: u16,
}

/// ELF64 程序头 (`Elf64_Phdr`) — 描述一个程序段 (`PT_LOAD` 等) 的偏移、地址、大小与权限。
///
/// 字段名对应 C `Elf64_Phdr` 的 `p_type` / `p_flags` / `p_offset` / `p_vaddr` /
/// `p_paddr` / `p_filesz` / `p_memsz` / `p_align`（按序仅去前缀, 顺序与类型不变）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Elf64Phdr {
    /// 段类型 (`PT_LOAD` / `PT_INTERP` / `PT_GNU_STACK` 等).
    pub ty: u32,
    /// 段权限位 (`PF_R` / `PF_W` / `PF_X`).
    pub flags: u32,
    /// 段内容在文件中的偏移.
    pub offset: u64,
    /// 段在内存中的虚拟地址.
    pub vaddr: u64,
    /// 段在内存中的物理地址 (通常与虚拟地址相同).
    pub paddr: u64,
    /// 段在文件中的字节数.
    pub filesz: u64,
    /// 段在内存中的字节数 (可大于文件字节数, 差额置零).
    pub memsz: u64,
    /// 段的对齐要求.
    pub align: u64,
}

const PT_LOAD: u32 = 1;
/// `PT_INTERP`: 动态链接器路径 (指向 ELF 解释器)
const PT_INTERP: u32 = 3;
const PT_GNU_STACK: u32 = 0x6474_E551;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;
const MAX_PHDR_COUNT: usize = 128;

/// `ET_DYN`: 共享对象 / PIE 可执行文件
const ET_DYN: u16 = 3;

/// ELF 加载结果 — 返回入口地址、程序头表位置、brk 基址与用户栈顶。
pub struct ElfLoadResult {
    /// 程序入口地址 (已加上加载偏移).
    pub entry: u64,
    /// 程序头表在用户地址空间中的地址.
    pub phdr_addr: u64,
    /// 程序头表条目数量.
    pub phdr_count: u16,
    /// 堆区起始地址 (所有已加载段之后的页对齐地址).
    pub brk_base: u64,
    /// 用户栈顶地址.
    pub stack_top: u64,
}

impl ElfLoadResult {
    /// 构造全零的加载结果, 用于错误路径或占位.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            entry: 0,
            phdr_addr: 0,
            phdr_count: 0,
            brk_base: 0,
            stack_top: 0,
        }
    }
}

// P1-I-33: 委托给 `verify::verify_elf` 单一来源
/// 校验 ELF 数据有效性并按值返回文件头 (`Elf64Header`); 校验失败返回 `None`。
///
/// 返回**值**而非引用: ELF 缓冲由调用方持有, 可能是任意对齐的用户/内核拷贝,
/// 引用会要求 8 字节对齐并伪造调用方无法保证的生命周期。调用方按需复制字段。
#[must_use]
pub fn elf_validate(elf_data: &[u8]) -> Option<Elf64Header> {
    let elf_size = elf_data.len() as u64;
    // SAFETY: elf_data 是有效切片; verify_elf 仅只读访问其 [0, len) 范围.
    let _ = unsafe { verify::verify_elf(elf_data.as_ptr(), elf_size) }.ok()?;

    // SAFETY: verify_elf 已确认 `elf_size >= size_of::<Elf64Header>()`, 故头区间在
    // 切片内; `Elf64Header` 是 `#[repr(C)]` 且全字段为整数/字节数组, 任意位模式
    // 均合法; `read_unaligned` 不要求缓冲对齐, 故可安全按值读出文件头 ——
    // 不对缓冲区构造对齐引用, 也不把返回值伪装成 `'static`。
    Some(unsafe { elf_data.as_ptr().cast::<Elf64Header>().read_unaligned() })
}

/// 加载 ELF 文件到当前进程地址空间 (`ET_EXEC`, 固定地址加载, 等价于 `load_bias = 0`).
///
/// # Errors
/// 当 ELF 头校验失败时返回 `Err("Invalid ELF header")`; 当无 program header 时返回
/// `Err("No program headers")`; 当段地址/大小溢出或物理页分配失败 (OOM) 时分别返回
/// `Err("ELF: vaddr + memsz overflow")`, `Err("ELF: p_offset + p_filesz overflow")`,
/// `Err("OOM loading ELF")`; 段落起点低于用户 VA 下界时返回
/// `Err("ELF: PT_LOAD below user VA lower bound")`.
pub fn elf_load(mm: &MmStruct, elf_data: &[u8]) -> Result<ElfLoadResult, &'static str> {
    elf_load_with_bias(mm, elf_data, 0)
}

/// 加载 ELF 文件到指定地址空间, 支持可选加载偏移 (PIE/ASLR).
///
/// `load_bias` = 0 表示 `ET_EXEC` (固定地址加载).
/// `load_bias` > 0 表示 `ET_DYN/PIE` (在随机基址加载).
///
/// # Errors
/// 当 ELF 头校验失败时返回 `Err("Invalid ELF header")`; 当无 program header 时返回
/// `Err("No program headers")`; 当段地址/大小算术溢出时返回
/// `Err("ELF: vaddr + memsz overflow")` 或 `Err("ELF: p_offset + p_filesz overflow")`;
/// 当物理页分配失败时返回 `Err("OOM loading ELF")`; 段落起点低于用户 VA 下界时
/// 返回 `Err("ELF: PT_LOAD below user VA lower bound")`.
pub fn elf_load_with_bias(
    mm: &MmStruct,
    elf_data: &[u8],
    load_bias: u64,
) -> Result<ElfLoadResult, &'static str> {
    let elf_size = elf_data.len() as u64;
    let header = elf_validate(elf_data).ok_or("Invalid ELF header")?;

    // PIE (ET_DYN) 需要非零 load_bias; ET_EXEC 使用固定地址
    let is_pie = header.ty == ET_DYN;
    let bias = if is_pie { load_bias } else { 0 };

    let entry = header.entry + bias;

    if header.phoff == 0 || header.phnum == 0 {
        return Err("No program headers");
    }

    // SAFETY: elf_data 是有效切片, 偏移 phoff 已经 verify_elf 边界校验.
    let phdr_base = unsafe {
        elf_data
            .as_ptr()
            .add(crate::privileged::glue::usize_from_u64(header.phoff))
    };
    let phdr_count = header.phnum;

    let mut max_vaddr: u64 = 0;
    let mut result = ElfLoadResult {
        entry,
        phdr_addr: phdr_base as u64 + bias,
        phdr_count,
        brk_base: 0,
        stack_top: crate::privileged::config::aslr_stack_top(),
    };

    for i in 0..phdr_count {
        // SAFETY: `elf_validate` 已先调 `verify_elf`, 后者校验了
        // `phoff..phoff + phnum*phentsize` 落在 `elf_data` 内; 本处按 `i < phnum`
        // 读取单个程序头, 不越出该表.
        let phdr = unsafe {
            phdr_base
                .add(i as usize * core::mem::size_of::<Elf64Phdr>())
                .cast::<Elf64Phdr>()
                .read_unaligned()
        };

        if phdr.filesz > phdr.memsz {
            continue;
        }

        if phdr.ty == PT_GNU_STACK {
            if phdr.flags & PF_X != 0 {
                crate::klog_warn!(
                    Process,
                    "ELF: PT_GNU_STACK requests executable stack (flags={:#x})",
                    phdr.flags
                );
            }
            continue;
        }

        if phdr.ty != PT_LOAD {
            continue;
        }

        if phdr.memsz == 0 {
            continue;
        }

        let vaddr_end = elf_load_segment(mm, &phdr, bias, elf_data.as_ptr(), elf_size)?;
        if vaddr_end > max_vaddr {
            max_vaddr = vaddr_end;
        }
    }

    result.brk_base = (max_vaddr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    // 连接 MmStruct 到当前进程
    crate::privileged::mm::vma_set_current_mm(core::ptr::from_ref::<MmStruct>(mm));

    Ok(result)
}

/// 加载单个 `PT_LOAD` 段: 建 VMA + 分配物理页 + 拷贝段数据 + 建立映射.
///
/// 返回该段的页对齐结束地址 (`vaddr_end`), 供调用方累计 `max_vaddr`;
/// 返回 0 表示该段数据超出 ELF 文件范围而被跳过 (与调用方 `continue` 等价).
fn elf_load_segment(
    mm: &MmStruct,
    phdr: &Elf64Phdr,
    bias: u64,
    elf_data: *const u8,
    elf_size: u64,
) -> Result<u64, &'static str> {
    let vaddr_start = (phdr.vaddr + bias) & !(PAGE_SIZE - 1);
    let vaddr_end_raw = phdr
        .vaddr
        .checked_add(phdr.memsz)
        .ok_or("ELF: vaddr + memsz overflow")?
        + bias;
    let vaddr_end =
        crate::privileged::glue::usize_from_u64((vaddr_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    let filesz = crate::privileged::glue::usize_from_u64(phdr.filesz);
    let file_offset = crate::privileged::glue::usize_from_u64(phdr.offset);

    let file_data_end = phdr
        .offset
        .checked_add(phdr.filesz)
        .ok_or("ELF: p_offset + p_filesz overflow")?;
    if file_data_end > elf_size {
        return Ok(0);
    }

    // K-08: 用户 VA 下界之下 (x86_64 的 kview 恒等窗口 / ELF 装载基址之下) 的
    // 段落会让内核态经 kview 访问时命中恒等物理页而非用户帧 (I4 代理面失真),
    // 装载期 fail-closed 拒绝, 不留给缺页路径兜底.
    if vaddr_start < crate::privileged::mm::user_va_lower_bound() {
        return Err("ELF: PT_LOAD below user VA lower bound");
    }

    let mut page_flags = PageFlags::USER;
    if phdr.flags & PF_R != 0 {
        page_flags |= PageFlags::PRESENT;
    }
    if phdr.flags & PF_W != 0 {
        page_flags |= PageFlags::WRITABLE;
    }
    if phdr.flags & PF_X == 0 {
        page_flags |= PageFlags::NX;
    }

    let vma = Vma::new(
        crate::privileged::glue::usize_from_u64(vaddr_start),
        vaddr_end,
        page_flags,
        VmaType::Anonymous,
    );
    mm.insert_vma(vma).map_err(|_| "VMA insertion failed")?;

    // 复制段数据到物理页
    let pml4 = crate::privileged::mm::get_current_pml4();

    let file_end = file_offset + filesz;
    let mut cur = vaddr_start;

    while cur < vaddr_end as u64 {
        let phys = crate::privileged::mm::pmm_alloc_page_phys().ok_or("OOM loading ELF")?;

        let page_virt = phys.to_virt();
        // SAFETY: `pmm_alloc_page_phys` 返回独占的整页帧, `to_virt` 给出内核高半区
        // 别名; 清零长度恰为 PAGE_SIZE, 只触及本帧.
        unsafe {
            core::ptr::write_bytes(
                page_virt.0 as *mut u8,
                0,
                crate::privileged::glue::usize_from_u64(PAGE_SIZE),
            );
        }

        let copy_start = if crate::privileged::glue::usize_from_u64(cur) < file_end {
            file_offset + crate::privileged::glue::usize_from_u64(cur - vaddr_start)
        } else {
            file_end
        };
        let copy_len = if copy_start < file_end {
            (file_end - copy_start).min(crate::privileged::glue::usize_from_u64(PAGE_SIZE))
        } else {
            0
        };

        if copy_len > 0 {
            // SAFETY: `copy_start..file_end` 取自本段 phdr 的 `offset..offset+filesz`,
            // `verify_elf` 已确认该区间落在 `elf_data` 内; `copy_len` 不超过
            // PAGE_SIZE, 目标为本轮 `pmm_alloc_page_phys` 独占的页帧.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    elf_data.add(copy_start),
                    page_virt.0 as *mut u8,
                    copy_len,
                );
            }
        }

        crate::privileged::mm::Vmm::map_page_in_table(
            pml4,
            VirtAddr(cur),
            phys,
            page_flags | PageFlags::PRESENT,
        );
        cur += PAGE_SIZE;
    }
    Ok(vaddr_end as u64)
}

/// 计算 `AT_PHDR`：程序头表在用户地址空间中的实际地址。
///
/// 用包含 `e_phoff` 的 `PT_LOAD` 反推 `p_vaddr + (e_phoff - p_offset)`，
/// 再叠加 `load_bias`；不能直接拿 `e_phoff` 当虚拟地址（`PIE`/非零 `p_offset` 都会错）。
#[must_use]
pub fn phdr_addr(elf_data: &[u8], load_bias: u64) -> Option<u64> {
    let header = elf_validate(elf_data)?;
    if header.phoff == 0 || header.phnum == 0 {
        return None;
    }
    let phoff = crate::privileged::glue::usize_from_u64(header.phoff);
    // SAFETY: verify_elf 已校验 phdr 表在 `elf_data` 内；按值读取，不构造对齐引用.
    let phdr_base = unsafe { elf_data.as_ptr().add(phoff) };
    for i in 0..header.phnum {
        // SAFETY: 同 interp_path；i < phnum，phentsize 已由 verify 固定.
        let phdr = unsafe {
            phdr_base
                .add(i as usize * core::mem::size_of::<Elf64Phdr>())
                .cast::<Elf64Phdr>()
                .read_unaligned()
        };
        if phdr.ty != PT_LOAD || phdr.filesz == 0 {
            continue;
        }
        let offset = crate::privileged::glue::usize_from_u64(phdr.offset);
        let filesz = crate::privileged::glue::usize_from_u64(phdr.filesz);
        let Some(end) = offset.checked_add(filesz) else {
            continue;
        };
        if phoff < offset || phoff >= end {
            continue;
        }
        let delta = (phoff - offset) as u64;
        return load_bias.checked_add(phdr.vaddr)?.checked_add(delta);
    }
    None
}

/// `PT_INTERP` 路径解析失败: 段存在但不是一个落在文件内、NUL 结尾的绝对
/// ASCII 路径。调用方必须按 Linux `execve` 语义返回 `ENOEXEC`，不得退化成
/// 静态装载（那会把动态程序送进没有解释器的入口）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterpPathError {
    /// `PT_INTERP` 存在但内容畸形（空串、相对路径、非 ASCII、无 NUL 或越界）。
    Malformed,
}

/// 读取 ELF 的 `PT_INTERP` 路径（不修改原映像）。
///
/// Linux `execve` 的契约是“内核读出解释器路径并装载第二个 ELF”，不是把路径
/// 改写成一个内核自带的桩。返回的 `&str` 借用 `elf_data`，调用方需在读取
/// 解释器文件期间保持该缓冲有效。
///
/// - `Ok(None)`: 文件没有 `PT_INTERP`，按静态 ELF 处理；
/// - `Ok(Some(path))`: 合法的绝对 ASCII 路径；
/// - `Err(InterpPathError)`: `PT_INTERP` 存在但畸形，调用方必须拒绝装载。
///
/// 只接受绝对路径、NUL 结尾且为 ASCII 的 `PT_INTERP`。
///
/// # Errors
/// 当 `PT_INTERP` 存在但为空、相对路径、非 ASCII、无 NUL 或偏移/长度越界时
/// 返回 [`InterpPathError::Malformed`]；调用方应映射为 `ENOEXEC`。
pub fn interp_path(elf_data: &[u8]) -> Result<Option<&str>, InterpPathError> {
    let Some(header) = elf_validate(elf_data) else {
        return Err(InterpPathError::Malformed);
    };
    if header.phoff == 0 || header.phnum == 0 {
        return Ok(None);
    }

    // SAFETY: `verify_elf` 已校验 program header table 落在 `elf_data` 内；
    // `i < phnum` 且 phentsize 已由 verify 固定为 `size_of::<Elf64Phdr>()`.
    let phdr_base = unsafe {
        elf_data
            .as_ptr()
            .add(crate::privileged::glue::usize_from_u64(header.phoff))
    };
    for i in 0..header.phnum {
        // SAFETY: 同上方；按值读取，不构造对齐引用，也不越出已校验的 phdr 表.
        let phdr = unsafe {
            phdr_base
                .add(i as usize * core::mem::size_of::<Elf64Phdr>())
                .cast::<Elf64Phdr>()
                .read_unaligned()
        };
        if phdr.ty != PT_INTERP {
            continue;
        }
        if phdr.filesz == 0 {
            return Err(InterpPathError::Malformed);
        }

        let offset = crate::privileged::glue::usize_from_u64(phdr.offset);
        let len = crate::privileged::glue::usize_from_u64(phdr.filesz);
        let Some(end) = offset.checked_add(len) else {
            return Err(InterpPathError::Malformed);
        };
        if end > elf_data.len() {
            return Err(InterpPathError::Malformed);
        }

        let raw = &elf_data[offset..end];
        let Some(nul) = raw.iter().position(|b| *b == 0) else {
            return Err(InterpPathError::Malformed);
        };
        let path = &raw[..nul];
        if path.is_empty() || path[0] != b'/' || !path.is_ascii() {
            return Err(InterpPathError::Malformed);
        }
        let path = core::str::from_utf8(path).map_err(|_| InterpPathError::Malformed)?;
        return Ok(Some(path));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elf_magic_validation() {
        fn hdr_bytes(h: &Elf64Header) -> &[u8] {
            // SAFETY: `h` 是有效 `Elf64Header` 引用; 按同起点、长度恰为结构体大小构造 u8 只读切片.
            unsafe {
                core::slice::from_raw_parts(
                    core::ptr::from_ref(h).cast::<u8>(),
                    core::mem::size_of::<Elf64Header>(),
                )
            }
        }
        // SAFETY: `Elf64Header` 全字段都是整数与定长字节数组, 全零位模式合法.
        let mut hdr = unsafe { core::mem::zeroed::<Elf64Header>() };
        // Invalid magic
        assert!(elf_validate(hdr_bytes(&hdr)).is_none());

        // Valid magic
        hdr.ident[0] = 0x7F;
        hdr.ident[1] = b'E';
        hdr.ident[2] = b'L';
        hdr.ident[3] = b'F';
        hdr.ident[4] = 2; // ELFCLASS64
        assert!(elf_validate(hdr_bytes(&hdr)).is_none()); // machine=0 not valid

        // Set valid machine
        hdr.machine = 0x3E; // x86_64
        hdr.phentsize = core::mem::size_of::<Elf64Phdr>() as u16;
        assert!(elf_validate(hdr_bytes(&hdr)).is_some());
    }

    #[test]
    fn test_elf64_header_sizes() {
        assert_eq!(core::mem::size_of::<Elf64Header>(), 64);
        assert_eq!(core::mem::size_of::<Elf64Phdr>(), 56);
    }

    /// 8 字节对齐的 ELF 缓冲区，含一条 PT_INTERP + 路径字符串。
    #[repr(align(8))]
    struct AlignedElfWithPath([u8; 160]);

    fn elf_with_interp_path(path: &[u8]) -> AlignedElfWithPath {
        let mut buf = AlignedElfWithPath([0u8; 160]);
        // SAFETY: 160B 对齐缓冲足够容纳头 (64B)、一条 phdr (56B) 与 32B 路径；
        // 所有写入均落在缓冲区内，按值读写，不构造对齐引用.
        unsafe {
            let hdr = buf.0.as_mut_ptr().cast::<Elf64Header>();
            let mut ident = [0u8; 16];
            ident[0..4].copy_from_slice(b"\x7FELF");
            ident[4] = 2;
            (*hdr).ident = ident;
            (*hdr).machine = 0x3E;
            (*hdr).phentsize = core::mem::size_of::<Elf64Phdr>() as u16;
            (*hdr).phoff = 64;
            (*hdr).phnum = 1;

            let phdr = buf.0.as_mut_ptr().add(64).cast::<Elf64Phdr>();
            (*phdr).ty = PT_INTERP;
            (*phdr).offset = 128;
            (*phdr).filesz = path.len() as u64;

            core::ptr::copy_nonoverlapping(path.as_ptr(), buf.0.as_mut_ptr().add(128), path.len());
        }
        buf
    }

    #[repr(align(8))]
    struct AlignedLoadElf([u8; 160]);

    fn elf_with_first_load() -> AlignedLoadElf {
        let mut buf = AlignedLoadElf([0u8; 160]);
        // SAFETY: 160B 缓冲足够容纳头、一条 phdr 与路径；按值写入字段.
        unsafe {
            let hdr = buf.0.as_mut_ptr().cast::<Elf64Header>();
            let mut ident = [0u8; 16];
            ident[0..4].copy_from_slice(b"\x7FELF");
            ident[4] = 2;
            (*hdr).ident = ident;
            (*hdr).machine = 0x3E;
            (*hdr).phentsize = core::mem::size_of::<Elf64Phdr>() as u16;
            (*hdr).phoff = 64;
            (*hdr).phnum = 1;
            let phdr = buf.0.as_mut_ptr().add(64).cast::<Elf64Phdr>();
            (*phdr).ty = PT_LOAD;
            (*phdr).offset = 0;
            (*phdr).filesz = 128;
            (*phdr).vaddr = 0x1000;
        }
        buf
    }

    #[test]
    fn phdr_addr_uses_containing_pt_load_not_e_phoff() {
        let elf = elf_with_first_load();
        assert_eq!(phdr_addr(&elf.0, 0x10_000), Some(0x11_040));
    }

    #[test]
    fn interp_path_reads_absolute_path_without_rewriting() {
        let elf = elf_with_interp_path(b"/lib/ld-musl-x86_64.so.1\0");
        assert_eq!(interp_path(&elf.0), Ok(Some("/lib/ld-musl-x86_64.so.1")));
    }

    #[test]
    fn interp_path_rejects_relative_unterminated_and_oob() {
        let relative = elf_with_interp_path(b"lib/ld-musl-x86_64.so.1\0");
        assert_eq!(interp_path(&relative.0), Err(InterpPathError::Malformed));
        let unterminated = elf_with_interp_path(b"/lib/ld-musl-x86_64.so.1");
        assert_eq!(
            interp_path(&unterminated.0),
            Err(InterpPathError::Malformed)
        );

        let oob = elf_with_interp(200, 10);
        assert_eq!(interp_path(&oob.0), Err(InterpPathError::Malformed));
        let overflow = elf_with_interp(u64::MAX, 2);
        assert_eq!(interp_path(&overflow.0), Err(InterpPathError::Malformed));
    }

    /// 8 字节对齐的 ELF 缓冲区 (测试用构造器; `elf_validate` 本身按值读取, 不要求对齐).
    #[repr(align(8))]
    struct AlignedElf([u8; 120]);

    /// 构造「合法头 + 单条 PT_INTERP」的最小 ELF (头 64B + 程序头 56B = 120B).
    fn elf_with_interp(offset: u64, filesz: u64) -> AlignedElf {
        let mut buf = AlignedElf([0u8; 120]);
        // SAFETY: 缓冲区 8 字节对齐且长 120B >= Elf64Header(64B); 头尾与 PHDR 均
        // 落在缓冲区内, 只做按值字段写入 (不对整个缓冲构造共享引用).
        unsafe {
            let hdr = buf.0.as_mut_ptr().cast::<Elf64Header>();
            let mut ident = [0u8; 16];
            ident[0..4].copy_from_slice(b"\x7FELF");
            ident[4] = 2; // ELFCLASS64
            (*hdr).ident = ident;
            (*hdr).machine = 0x3E; // x86_64 (host 构建接受两架构)
            (*hdr).phentsize = core::mem::size_of::<Elf64Phdr>() as u16;
            (*hdr).phoff = core::mem::size_of::<Elf64Header>() as u64;
            (*hdr).phnum = 1;

            let phdr = buf.0.as_mut_ptr().add(64).cast::<Elf64Phdr>();
            (*phdr).ty = PT_INTERP;
            (*phdr).offset = offset;
            (*phdr).filesz = filesz;
        }
        buf
    }

    /// ㊼ 回归: `elf_validate` 全程按值 `read_unaligned`, 非 8 字节对齐的缓冲同样合法.
    #[test]
    fn elf_validate_accepts_unaligned_buffer() {
        let elf = elf_with_interp(119, 1);
        #[repr(align(8))]
        struct Padded([u8; 128]);
        let mut padded = Padded([0u8; 128]);
        // 8 字节对齐起点 + 1 字节偏移 ⇒ 视图起始地址必然非 `align_of::<Elf64Header>()`.
        padded.0[1..121].copy_from_slice(&elf.0);
        let view = &padded.0[1..121];
        assert_ne!(
            view.as_ptr() as usize % core::mem::align_of::<Elf64Header>(),
            0,
            "用例必须先构造出非对齐视图"
        );

        let header = elf_validate(view).expect("非对齐缓冲也应校验通过");
        assert_eq!(header.phnum, 1);
    }
}
