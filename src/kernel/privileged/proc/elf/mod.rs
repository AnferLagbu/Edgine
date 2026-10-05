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

// P1-I-33: ELF 验证抽到 verify 子模块, 单一来源
pub mod verify;

use crate::privileged::mm::{MmStruct, Vma, VmaType};
use crate::privileged::mm::{PAGE_SIZE, PageFlags, VirtAddr};

/// ELF64 文件头 (Elf64_Ehdr) — 描述文件类型、目标机器、入口地址与各表偏移。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Elf64Header {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

/// ELF64 程序头 (Elf64_Phdr) — 描述一个程序段 (PT_LOAD 等) 的偏移、地址、大小与权限。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

const PT_LOAD: u32 = 1;
/// `PT_INTERP`: 动态链接器路径 (指向 ELF 解释器)
const PT_INTERP: u32 = 3;
const PT_GNU_STACK: u32 = 0x6474E551;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;
const MAX_PHDR_COUNT: usize = 128;

/// `ET_DYN`: 共享对象 / PIE 可执行文件
const ET_DYN: u16 = 3;

/// ELF 加载结果 — 返回入口地址、程序头表位置、brk 基址与用户栈顶。
pub struct ElfLoadResult {
    pub entry: u64,
    pub phdr_addr: u64,
    pub phdr_count: u16,
    pub brk_base: u64,
    pub stack_top: u64,
}

impl ElfLoadResult {
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
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
/// 校验 ELF 数据有效性并返回指向其文件头的引用; 校验失败返回 `None`。
pub fn elf_validate(elf_data: *const u8, elf_size: u64) -> Option<&'static Elf64Header> {
    // SAFETY: 调用方保证 elf_data 有效, verify_elf 内部仅读借用
    let _ = unsafe { verify::verify_elf(elf_data, elf_size) }.ok()?;

    // SAFETY: 已通过 verify_elf 校验
    Some(unsafe { &*(elf_data as *const Elf64Header) })
}

/// 加载 ELF 文件到当前进程地址空间 (`ET_EXEC`, 固定地址加载, 等价于 `load_bias = 0`).
///
/// # Errors
/// 当 ELF 头校验失败时返回 `Err("Invalid ELF header")`; 当无 program header 时返回
/// `Err("No program headers")`; 当段地址/大小溢出或物理页分配失败 (OOM) 时分别返回
/// `Err("ELF: vaddr + memsz overflow")`, `Err("ELF: p_offset + p_filesz overflow")`,
/// `Err("OOM loading ELF")`.
pub fn elf_load(
    mm: &MmStruct,
    elf_data: *const u8,
    elf_size: u64,
) -> Result<ElfLoadResult, &'static str> {
    elf_load_with_bias(mm, elf_data, elf_size, 0)
}

#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
/// 加载 ELF 文件到指定地址空间, 支持可选加载偏移 (PIE/ASLR).
///
/// `load_bias` = 0 表示 `ET_EXEC` (固定地址加载).
/// `load_bias` > 0 表示 `ET_DYN/PIE` (在随机基址加载).
///
/// # Errors
/// 当 ELF 头校验失败时返回 `Err("Invalid ELF header")`; 当无 program header 时返回
/// `Err("No program headers")`; 当段地址/大小算术溢出时返回
/// `Err("ELF: vaddr + memsz overflow")` 或 `Err("ELF: p_offset + p_filesz overflow")`;
/// 当物理页分配失败时返回 `Err("OOM loading ELF")`.
pub fn elf_load_with_bias(
    mm: &MmStruct,
    elf_data: *const u8,
    elf_size: u64,
    load_bias: u64,
) -> Result<ElfLoadResult, &'static str> {
    let header = elf_validate(elf_data, elf_size).ok_or("Invalid ELF header")?;

    // PIE (ET_DYN) 需要非零 load_bias; ET_EXEC 使用固定地址
    let is_pie = header.e_type == ET_DYN;
    let bias = if is_pie { load_bias } else { 0 };

    let entry = header.e_entry + bias;

    if header.e_phoff == 0 || header.e_phnum == 0 {
        return Err("No program headers");
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let phdr_base = unsafe { elf_data.add(header.e_phoff as usize) };
    let phdr_count = header.e_phnum;

    let mut max_vaddr: u64 = 0;
    let mut result = ElfLoadResult {
        entry,
        phdr_addr: phdr_base as u64 + bias,
        phdr_count,
        brk_base: 0,
        stack_top: crate::privileged::config::aslr_stack_top(),
    };

    for i in 0..phdr_count {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let phdr = unsafe {
            &*(phdr_base.add(i as usize * core::mem::size_of::<Elf64Phdr>()) as *const Elf64Phdr)
        };

        if phdr.p_filesz > phdr.p_memsz {
            continue;
        }

        if phdr.p_type == PT_GNU_STACK {
            if phdr.p_flags & PF_X != 0 {
                crate::klog_warn!(
                    Process,
                    "ELF: PT_GNU_STACK requests executable stack (flags={:#x})",
                    phdr.p_flags
                );
            }
            continue;
        }

        if phdr.p_type != PT_LOAD {
            continue;
        }

        if phdr.p_memsz == 0 {
            continue;
        }

        let vaddr_start = (phdr.p_vaddr + bias) & !(PAGE_SIZE - 1);
        let vaddr_end_raw = phdr
            .p_vaddr
            .checked_add(phdr.p_memsz)
            .ok_or("ELF: vaddr + memsz overflow")?
            + bias;
        let vaddr_end = ((vaddr_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)) as usize;
        let filesz = phdr.p_filesz as usize;
        let file_offset = phdr.p_offset as usize;

        let file_data_end = phdr
            .p_offset
            .checked_add(phdr.p_filesz)
            .ok_or("ELF: p_offset + p_filesz overflow")?;
        if file_data_end > elf_size {
            continue;
        }

        if vaddr_end as u64 > max_vaddr {
            max_vaddr = vaddr_end as u64;
        }

        let mut page_flags = PageFlags::USER;
        if phdr.p_flags & PF_R != 0 {
            page_flags |= PageFlags::PRESENT;
        }
        if phdr.p_flags & PF_W != 0 {
            page_flags |= PageFlags::WRITABLE;
        }
        if phdr.p_flags & PF_X == 0 {
            page_flags |= PageFlags::NX;
        }

        let vma = Vma::new(
            vaddr_start as usize,
            vaddr_end,
            page_flags,
            VmaType::Anonymous,
        );
        mm.insert_vma(vma).map_err(|_| "VMA insertion failed")?;

        // 复制段数据到物理页
        let vmm_inst = crate::privileged::mm::get_vmm();
        let pml4 = crate::privileged::mm::get_current_pml4();

        let file_end = file_offset + filesz;
        let mut cur = vaddr_start;

        while cur < vaddr_end as u64 {
            let phys = crate::privileged::mm::pmm_alloc_page_phys().ok_or("OOM loading ELF")?;

            let page_virt = phys.to_virt();
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                core::ptr::write_bytes(page_virt.0 as *mut u8, 0, PAGE_SIZE as usize);
            }

            let copy_start = if (cur as usize) < file_end {
                file_offset + (cur - vaddr_start) as usize
            } else {
                file_end
            };
            let copy_len = if copy_start < file_end {
                (file_end - copy_start).min(PAGE_SIZE as usize)
            } else {
                0
            };

            if copy_len > 0 {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        elf_data.add(copy_start),
                        page_virt.0 as *mut u8,
                        copy_len,
                    );
                }
            }

            vmm_inst.map_page_in_table(pml4, VirtAddr(cur), phys, page_flags | PageFlags::PRESENT);
            cur += PAGE_SIZE;
        }
    }

    result.brk_base = (max_vaddr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    // 连接 MmStruct 到当前进程
    crate::privileged::mm::vma_set_current_mm(mm as *const MmStruct);

    Ok(result)
}

/// edgine 动态链接器路径 (替代 Linux ld-linux-*.so.2)
const EDGINE_INTERP: &[u8] = b"/usr/libexec/elfld.so\0";

/// Linux 动态链接器路径前缀 (用于检测)
const LINUX_INTERP_PREFIXES: &[&[u8]] = &[
    b"/lib64/ld-linux-x86-64.so.2",
    b"/lib/ld-linux-x86-64.so.2",
    b"/lib/ld-linux-aarch64.so.1",
    b"/lib/ld-linux.so.2",
    b"/lib/ld-linux.so.3",
    b"/lib/ld-musl-x86_64.so.1",
    b"/lib/ld-musl-aarch64.so.1",
];

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 扫描 ELF program headers, 检测 `PT_INTERP` 是否为 Linux 动态链接器.
///
/// 返回 true 表示需要改写 `PT_INTERP` (Linux 二进制).
pub fn needs_interp_rewrite(elf_data: *const u8, elf_size: u64) -> bool {
    let header = match elf_validate(elf_data, elf_size) {
        Some(h) => h,
        None => return false,
    };

    if header.e_phoff == 0 || header.e_phnum == 0 {
        return false;
    }

    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
    let phdr_base = unsafe { elf_data.add(header.e_phoff as usize) };

    for i in 0..header.e_phnum {
        // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
        let phdr = unsafe {
            &*(phdr_base.add(i as usize * core::mem::size_of::<Elf64Phdr>()) as *const Elf64Phdr)
        };

        if phdr.p_type != PT_INTERP || phdr.p_filesz == 0 {
            continue;
        }

        // 读取 interp 路径字符串
        let interp_offset = phdr.p_offset as usize;
        let interp_len = phdr.p_filesz as usize;
        if interp_offset + interp_len > elf_size as usize {
            continue;
        }
        // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
        let interp_path =
            unsafe { core::slice::from_raw_parts(elf_data.add(interp_offset), interp_len) };

        // 检查是否为 Linux 动态链接器
        for prefix in LINUX_INTERP_PREFIXES {
            if interp_path.starts_with(prefix) {
                return true;
            }
        }
    }

    false
}

#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 改写 ELF 数据中的 `PT_INTERP` 路径为 edgine 动态链接器.
///
/// # Safety
/// `elf_data` 必须指向可写的有效 ELF 数据 (内核拷贝缓冲区).
pub unsafe fn rewrite_interp_path(elf_data: *mut u8, elf_size: u64) {
    unsafe {
        let header = match elf_validate(elf_data as *const u8, elf_size) {
            Some(h) => h,
            None => return,
        };

        if header.e_phoff == 0 || header.e_phnum == 0 {
            return;
        }

        let phdr_base = elf_data.add(header.e_phoff as usize);

        for i in 0..header.e_phnum {
            let phdr = &*(phdr_base.add(i as usize * core::mem::size_of::<Elf64Phdr>())
                as *const Elf64Phdr);

            if phdr.p_type != PT_INTERP || phdr.p_filesz == 0 {
                continue;
            }

            let interp_offset = phdr.p_offset as usize;
            let interp_len = phdr.p_filesz as usize;
            if interp_offset + interp_len > elf_size as usize {
                continue;
            }

            let interp_dst = elf_data.add(interp_offset);

            // 计算可写入长度 (不超过原 interp 段大小)
            let write_len = EDGINE_INTERP.len().min(interp_len);

            // SAFETY: elf_data 是内核拷贝缓冲区, interp_offset+interp_len 在 ELF 范围内
            core::ptr::copy_nonoverlapping(EDGINE_INTERP.as_ptr(), interp_dst, write_len);

            // 如果 edgine interp 比原 interp 短, 用 null 填充剩余空间
            if write_len < interp_len {
                core::ptr::write_bytes(interp_dst.add(write_len), 0, interp_len - write_len);
            }
        }
    }
}

/// 准备 ELF 映像: 检测并在必要时把 `PT_INTERP` 改写为 edgine 动态链接器路径.
///
/// 返回 `true` 表示发生了改写 (Linux 二进制), `false` 表示无需改写.
///
/// functions 层通过本 safe 入口完成 `PT_INTERP` 改写, 无需接触裸指针.
pub fn prepare_elf_image(elf: &mut [u8]) -> bool {
    let size = elf.len() as u64;
    if !needs_interp_rewrite(elf.as_ptr(), size) {
        return false;
    }
    // SAFETY: elf 是可写的有效切片, size 为其真实字节长度;
    // rewrite_interp_path 仅在该切片范围内原地改写 PT_INTERP 路径.
    unsafe {
        rewrite_interp_path(elf.as_mut_ptr(), size);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elf_magic_validation() {
        let mut data = [0u8; 64];
        // Invalid magic
        assert!(elf_validate(data.as_ptr(), 64).is_none());

        // Valid magic
        data[0] = 0x7F;
        data[1] = b'E';
        data[2] = b'L';
        data[3] = b'F';
        data[4] = 2; // ELFCLASS64
        assert!(elf_validate(data.as_ptr(), 64).is_none()); // machine=0 not valid

        // Set valid machine
        // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
        let hdr = unsafe { &mut *(data.as_mut_ptr() as *mut Elf64Header) };
        hdr.e_machine = 0x3E; // x86_64
        hdr.e_phentsize = core::mem::size_of::<Elf64Phdr>() as u16;
        assert!(elf_validate(data.as_ptr(), 64).is_some());
    }

    #[test]
    fn test_elf64_header_sizes() {
        assert_eq!(core::mem::size_of::<Elf64Header>(), 64);
        assert_eq!(core::mem::size_of::<Elf64Phdr>(), 56);
    }
}
