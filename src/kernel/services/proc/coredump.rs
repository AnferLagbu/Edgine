#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! Core Dump 生成器 — services 层实现 (自 framework/proc/coredump.rs 下沉, §6.2)
//!
//! 当进程收到 Core 类信号 (SIGQUIT/SIGILL/SIGABRT/SIGBUS/SIGFPE/SIGSEGV 等)
//! 时, 生成 ELF 格式的 core 文件, 包含进程的寄存器状态和内存映射。
//!
//! ## ELF Core 文件格式
//!
//! ```text
//! ELF Header (ET_CORE)
//! ├── PT_NOTE 段: NT_PRSTATUS (寄存器) + NT_SIGINFO (信号信息)
//! ├── PT_LOAD 段: 可读内存区域 (每个 VMA 一个)
//! └── ...
//! ```
//!
//! ## safe 机制来源 (framework 安全代理)
//!
//! - 中断/异常帧寄存器: [`read_interrupt_regs`] (framework 提供 POD 快照)
//! - 当前进程 VMA 枚举: [`vma_snapshot_current`] (framework 提供 POD 快照)
//! - 用户内存读取: [`copy_from_user_in_mm`] (跨进程安全代理)
//! - core 文件写入: `framework::fs::vfs::api` 的 `vfs_*_safe` / `vfs_write_pod`
//!
//! ## 限制
//!
//! - `RLIMIT_CORE`: core 文件大小上限 (0 = 禁止), 写入路径按此截断
//! - 仅转储可读 VMA (跳过只执行/不可读)
//! - 最大转储 64 个内存段

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use core::mem::size_of;
use core::sync::atomic::Ordering;

use crate::framework::fs::vfs::api::{vfs_close_safe, vfs_open_safe, vfs_write_pod, vfs_write_safe};
use crate::framework::mm::{PAGE_SIZE, PageFlags, copy_from_user_in_mm, vma_snapshot_current};
use crate::framework::proc::{
    CoredumpSink, Elf64Header, Elf64Phdr, RLIM_INFINITY, RLIMIT_CORE, process_get_cr3,
    process_get_current_pid, process_with, read_interrupt_regs, register_coredump_sink,
};

// ============================================================================
// ELF Core 常量
// ============================================================================

const ELFMAG: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_CORE: u16 = 4;
const EV_CURRENT: u32 = 1;

#[cfg(target_arch = "x86_64")]
const EM_MACHINE: u16 = 62; // EM_X86_64
#[cfg(target_arch = "aarch64")]
const EM_MACHINE: u16 = 183; // EM_AARCH64

const PT_NOTE: u32 = 4;
const PT_LOAD: u32 = 1;

const NT_PRSTATUS: u32 = 1;
const NT_SIGINFO: u32 = 0x5349_4749;

const PF_R: u32 = 4;
const PF_W: u32 = 2;
const PF_X: u32 = 1;

/// 最大转储内存段数
const MAX_CORE_SEGMENTS: usize = 64;

/// core 文件打开标志: O_WRONLY | O_CREAT | O_TRUNC
const OPEN_FLAGS: u32 = 0x0002 | 0x0100 | 0x0200;

/// note name "CORE\0" 声明长度 (含 NUL, 共 5 字节)
const NOTE_NAMESZ: u32 = 5;
/// note name 按 4 字节对齐后的长度 ("CORE\0" 补齐至 8)
const NOTE_NAME_ALIGNED: u64 = 8;
/// 补齐到 8 字节的 name 字节 — 修复原实现按 8 字节读写 5 字节数组的越界读
const NOTE_NAME_PADDED: [u8; 8] = *b"CORE\0\0\0\0";

/// 寄存器组有效长度: x86_64 27 项 / aarch64 34 项
#[cfg(target_arch = "x86_64")]
const REGSET_LEN: usize = 27;
#[cfg(target_arch = "aarch64")]
const REGSET_LEN: usize = 34;

// ============================================================================
// ELF Core 结构体 (纯 POD, derive Copy 以适配 vfs_write_pod 约束)
// ============================================================================

/// `siginfo_t` 简化 (仅用于 core dump note)
#[repr(C)]
#[derive(Copy, Clone)]
struct CoreSiginfo {
    si_signo: i32,
    si_code: i32,
    si_errno: i32,
}

/// 进程状态 note (Linux `elf_prstatus` 布局子集)
#[repr(C)]
#[derive(Copy, Clone)]
struct PrStatus {
    siginfo: CoreSiginfo,
    _pad0: u16,
    pr_cursig: u16,
    _pad1: u32,
    pr_sigpend: u64,
    pr_sighold: u64,
    pr_pid: i32,
    pr_ppid: i32,
    pr_pgrp: i32,
    pr_sid: i32,
    pr_utime: u64,
    pr_stime: u64,
    pr_cutime: u64,
    pr_cstime: u64,
    /// 寄存器组: x86_64 27 项 / aarch64 34 项
    regs: [u64; REGSET_LEN],
}

/// 单个 ELF Note 头
#[repr(C)]
#[derive(Copy, Clone)]
struct Elf64Note {
    namesz: u32,
    descsz: u32,
    note_type: u32,
}

/// 内存段信息 (用于构建 `PT_LOAD`)
struct CoreSegment {
    start: u64,
    end: u64,
    flags: u32,     // 权限标志: PF_R | PF_W | PF_X
    file_size: u64, // 实际写入大小
}

/// 计算对齐后的 note 大小 (12 = sizeof(Elf64Note))
fn note_size(namesz: u32, descsz: u32) -> u64 {
    let name_aligned = (u64::from(namesz) + 3) & !3;
    let desc_aligned = (u64::from(descsz) + 3) & !3;
    12 + name_aligned + desc_aligned
}

// ============================================================================
// 策略注入 (CoredumpSink) — framework 在信号默认动作为 Core 时调用
// ============================================================================

/// 标准核心转储实现 — 委托 [`do_coredump`] 生成 ELF core 文件
pub struct StandardCoredumpSink;

impl CoredumpSink for StandardCoredumpSink {
    fn coredump(&self, pid: u32, sig: u8, frame_addr: u64) -> bool {
        do_coredump(pid, sig, frame_addr)
    }
}

/// 注册标准核心转储实现到 framework
///
/// 由 `services::proc::init()` 调用. 只能成功一次, 后续调用返回 `Err(())`.
///
/// # Errors
///
/// 当核心转储实现已被注册时返回 `Err(())`.
pub fn register_standard_coredump_sink() -> Result<(), ()> {
    static SINK: StandardCoredumpSink = StandardCoredumpSink;
    register_coredump_sink(&SINK).map_err(|_| ())
}

// ============================================================================
// Core Dump 写入
// ============================================================================

/// 生成 core dump
///
/// 调用时机: 进程收到 Core 类信号, framework 经 [`CoredumpSink`] 调用本函数。
///
/// # 参数
/// - `pid`: 目标进程 PID
/// - `sig`: 导致 core dump 的信号编号
/// - `frame_addr`: 中断帧地址 (寄存器快照, 可为 0)
///
/// # 返回
/// - `true`: core dump 成功写入
/// - `false`: core dump 失败 (`RLIMIT_CORE=0`, 非当前进程, 磁盘满等)
fn do_coredump(pid: u32, sig: u8, frame_addr: u64) -> bool {
    // SIMPLIFIED: 仅支持"当前进程"转储 — framework 未提供 pid→mm 映射边
    //   (见 docs/plan/cr3-lifetime-ownership.md)。
    //   影响面: 非当前进程的 Core 请求被拒绝 (修正 P0-17: 原实现误用当前 mm 枚举目标进程 VMA);
    //   何时需扩展: framework 补全 pid→mm 映射边后, 改用目标进程 mm 枚举。
    if pid != process_get_current_pid() {
        crate::slog_warn!(Process, "coredump: pid={} 非当前进程, 拒绝转储", pid);
        return false;
    }

    let core_limit = core_limit_for(pid);
    if core_limit == 0 {
        crate::slog_info!(Process, "coredump: RLIMIT_CORE=0, 跳过 pid={}", pid);
        return false;
    }

    let segments = collect_segments();
    if segments.is_empty() {
        crate::slog_info!(Process, "coredump: 无可转储段 pid={}", pid);
        return false;
    }

    // 计算 note 段大小与 program header 数量/偏移
    let note_total = note_size(NOTE_NAMESZ, size_of::<PrStatus>() as u32)
        + note_size(NOTE_NAMESZ, size_of::<CoreSiginfo>() as u32);
    let phnum = 1 + segments.len() as u16; // PT_NOTE + PT_LOADs
    let note_offset =
        size_of::<Elf64Header>() as u64 + size_of::<Elf64Phdr>() as u64 * u64::from(phnum);

    // 打开 core 文件
    let core_path = build_core_path(pid);
    let fd = vfs_open_safe(&core_path, OPEN_FLAGS, 0);
    if fd < 0 {
        crate::slog_warn!(Process, "coredump: 打开 {} 失败", core_path);
        return false;
    }
    let fd = fd as u32;

    let mut offset = 0u64;

    // 写入 ELF 头 + 程序头表 (note 段 + load 段)
    write_struct(fd, &build_ehdr(phnum), &mut offset);
    write_struct(fd, &build_note_phdr(note_offset, note_total), &mut offset);

    let mut data_offset = note_offset + note_total;
    for seg in &segments {
        write_struct(fd, &build_load_phdr(seg, data_offset), &mut offset);
        data_offset += seg.file_size;
    }

    // note 段 (PRSTATUS + SIGINFO)
    write_note_prstatus(fd, pid, sig, frame_addr, &mut offset);
    write_note_siginfo(fd, sig, &mut offset);

    // 对齐 note 段
    let note_end = note_offset + note_total;
    if offset < note_end {
        pad_zeros(fd, note_end - offset);
        offset = note_end;
    }

    // 写入内存段数据 (按 RLIMIT_CORE 截断)
    let cr3 = process_get_cr3(pid).unwrap_or(0);
    for seg in &segments {
        write_segment_data(fd, cr3, seg, &mut offset, core_limit);
    }

    vfs_close_safe(fd);
    crate::slog_info!(Process, "coredump: 已写 {} 字节 pid={}", offset, pid);
    true
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 构建 core 文件路径: "core.<pid>"
fn build_core_path(pid: u32) -> String {
    format!("core.{pid}")
}

/// 收集当前进程的可读内存段
fn collect_segments() -> Vec<CoreSegment> {
    let mut segments = Vec::new();

    for vma in vma_snapshot_current() {
        if segments.len() >= MAX_CORE_SEGMENTS {
            break;
        }

        let flags = PageFlags::from_bits_truncate(vma.flags_bits);
        let pf_r = if flags.contains(PageFlags::PRESENT) {
            PF_R
        } else {
            0
        };
        let pf_w = if flags.contains(PageFlags::WRITABLE) {
            PF_W
        } else {
            0
        };
        let pf_x = if flags.contains(PageFlags::NX) { 0 } else { PF_X };

        // 跳过不可读段
        if pf_r == 0 {
            continue;
        }

        segments.push(CoreSegment {
            start: vma.start,
            end: vma.end,
            flags: pf_r | pf_w | pf_x,
            file_size: vma.end - vma.start,
        });
    }

    segments
}

/// 构建 ELF header
fn build_ehdr(phnum: u16) -> Elf64Header {
    let mut e_ident = [0u8; 16];
    e_ident[0..4].copy_from_slice(&ELFMAG);
    e_ident[4] = ELFCLASS64;
    e_ident[5] = ELFDATA2LSB;
    e_ident[6] = EV_CURRENT as u8;

    Elf64Header {
        e_ident,
        e_type: ET_CORE,
        e_machine: EM_MACHINE,
        e_version: EV_CURRENT,
        e_entry: 0,
        e_phoff: size_of::<Elf64Header>() as u64,
        e_shoff: 0,
        e_flags: 0,
        e_ehsize: size_of::<Elf64Header>() as u16,
        e_phentsize: size_of::<Elf64Phdr>() as u16,
        e_phnum: phnum,
        e_shentsize: 0,
        e_shnum: 0,
        e_shstrndx: 0,
    }
}

/// 构建 `PT_NOTE` program header
fn build_note_phdr(offset: u64, size: u64) -> Elf64Phdr {
    Elf64Phdr {
        p_type: PT_NOTE,
        p_flags: 0,
        p_offset: offset,
        p_vaddr: 0,
        p_paddr: 0,
        p_filesz: size,
        p_memsz: size,
        p_align: 4,
    }
}

/// 构建 `PT_LOAD` program header
fn build_load_phdr(seg: &CoreSegment, offset: u64) -> Elf64Phdr {
    Elf64Phdr {
        p_type: PT_LOAD,
        p_flags: seg.flags,
        p_offset: offset,
        p_vaddr: seg.start,
        p_paddr: 0,
        p_filesz: seg.file_size,
        p_memsz: seg.end - seg.start,
        p_align: PAGE_SIZE,
    }
}

/// 构建进程状态 note (全字段显式初始化, 规避 `mem::zeroed`)
fn build_prstatus(pid: u32, sig: u8, frame_addr: u64) -> PrStatus {
    let mut prstatus = PrStatus {
        siginfo: CoreSiginfo {
            si_signo: i32::from(sig),
            si_code: 0,
            si_errno: 0,
        },
        _pad0: 0,
        pr_cursig: u16::from(sig),
        _pad1: 0,
        pr_sigpend: 0,
        pr_sighold: 0,
        pr_pid: pid as i32,
        pr_ppid: 0,
        pr_pgrp: 0,
        pr_sid: 0,
        pr_utime: 0,
        pr_stime: 0,
        pr_cutime: 0,
        pr_cstime: 0,
        regs: [0; REGSET_LEN],
    };

    // 填充进程信息 (pid 不存在时 process_with 返回 None, 保持初值)
    let _ = process_with(pid, |p| {
        prstatus.pr_ppid = p.parent.map_or(0, |pp| pp.0 as i32);
        prstatus.pr_pgrp = p.pgid.load(Ordering::SeqCst) as i32;
        prstatus.pr_sid = p.session_id.load(Ordering::SeqCst) as i32;
        prstatus.pr_sigpend = p.signal_pending_get();
        prstatus.pr_utime = p.user_time.load(Ordering::SeqCst);
        prstatus.pr_stime = p.sys_time.load(Ordering::SeqCst);
    });

    // 从中断帧填充寄存器 (frame_addr == 0 时返回 None)
    if let Some(snapshot) = read_interrupt_regs(frame_addr) {
        let src = snapshot.as_slice();
        let n = src.len().min(REGSET_LEN);
        prstatus.regs[..n].copy_from_slice(&src[..n]);
    }

    prstatus
}

/// 写入 `NT_PRSTATUS` note
fn write_note_prstatus(fd: u32, pid: u32, sig: u8, frame_addr: u64, offset: &mut u64) {
    let descsz = size_of::<PrStatus>() as u32;
    write_note_header(fd, descsz, NT_PRSTATUS, offset);
    write_desc(fd, &build_prstatus(pid, sig, frame_addr), offset);
}

/// 写入 `NT_SIGINFO` note
fn write_note_siginfo(fd: u32, sig: u8, offset: &mut u64) {
    let descsz = size_of::<CoreSiginfo>() as u32;
    write_note_header(fd, descsz, NT_SIGINFO, offset);
    let siginfo = CoreSiginfo {
        si_signo: i32::from(sig),
        si_code: 0,
        si_errno: 0,
    };
    write_desc(fd, &siginfo, offset);
}

/// 写入 note 头 + 4 字节对齐的 name
fn write_note_header(fd: u32, descsz: u32, note_type: u32, offset: &mut u64) {
    let note = Elf64Note {
        namesz: NOTE_NAMESZ,
        descsz,
        note_type,
    };
    write_struct(fd, &note, offset);
    vfs_write_safe(fd, &NOTE_NAME_PADDED);
    *offset += NOTE_NAME_ALIGNED;
}

/// 写入 note 描述体 + 4 字节对齐填充
fn write_desc<T: Copy>(fd: u32, desc: &T, offset: &mut u64) {
    let size = size_of::<T>();
    vfs_write_pod(fd, desc);
    *offset += size as u64;

    let aligned = (size + 3) & !3;
    if aligned > size {
        let pad = (aligned - size) as u64;
        pad_zeros(fd, pad);
        *offset += pad;
    }
}

/// 按 POD 位视图写入任意结构体
fn write_struct<T: Copy>(fd: u32, data: &T, offset: &mut u64) {
    vfs_write_pod(fd, data);
    *offset += size_of::<T>() as u64;
}

/// 写入内存段数据 (按页读取, 按 `core_limit` 实质截断)
fn write_segment_data(fd: u32, cr3: u64, seg: &CoreSegment, offset: &mut u64, core_limit: u64) {
    let mut buf = [0u8; PAGE_SIZE as usize];
    let mut written = 0u64;
    let mut addr = seg.start;

    while written < seg.file_size {
        if core_limit != RLIM_INFINITY && *offset >= core_limit {
            break;
        }

        let mut chunk = (seg.file_size - written).min(PAGE_SIZE);
        if core_limit != RLIM_INFINITY {
            chunk = chunk.min(core_limit - *offset);
        }
        let n = chunk as usize;

        // 读取用户内存; 不可读部分写零
        let read = copy_from_user_in_mm(cr3, addr, &mut buf[..n], n).unwrap_or(0);
        if read < n {
            buf[read..n].fill(0);
        }
        vfs_write_safe(fd, &buf[..n]);

        written += chunk;
        addr += chunk;
        *offset += chunk;
    }
}

/// 写入 `n` 个零字节 (16 字节块循环)
fn pad_zeros(fd: u32, mut n: u64) {
    const ZEROS: [u8; 16] = [0u8; 16];
    while n > 0 {
        let chunk = n.min(16) as usize;
        vfs_write_safe(fd, &ZEROS[..chunk]);
        n -= chunk as u64;
    }
}

/// 读取指定进程的 `RLIMIT_CORE` 当前值 (进程不存在返回 0)
fn core_limit_for(pid: u32) -> u64 {
    process_with(pid, |p| {
        let table = p.rlimit_table.lock();
        table.get(RLIMIT_CORE).map_or(0, |r| r.cur)
    })
    .unwrap_or(0)
}

// ============================================================================
// 公共 API
// ============================================================================

/// 检查当前进程是否允许生成 core dump (`RLIMIT_CORE` > 0)
pub fn coredump_allowed() -> bool {
    let pid = process_get_current_pid();
    if pid == 0 {
        return false;
    }
    process_with(pid, |p| {
        p.rlimit_table
            .lock()
            .get(RLIMIT_CORE)
            .is_some_and(|r| r.cur > 0)
    })
    .unwrap_or(false)
}

/// 获取当前进程的 core dump 大小限制 (`RLIMIT_CORE` 当前值)
pub fn coredump_limit() -> u64 {
    let pid = process_get_current_pid();
    if pid == 0 {
        return 0;
    }
    core_limit_for(pid)
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_note_size_alignment() {
        assert_eq!(note_size(5, 0), 20);
        assert_eq!(note_size(5, 12), 32);
        assert_eq!(note_size(5, 13), 36);
        assert_eq!(note_size(5, 304), 324);
    }

    #[test]
    fn test_build_core_path() {
        assert_eq!(build_core_path(1234), "core.1234");
        assert_eq!(build_core_path(0), "core.0");
    }

    #[test]
    fn test_pod_layout() {
        assert_eq!(size_of::<Elf64Header>(), 64);
        assert_eq!(size_of::<Elf64Phdr>(), 56);
        assert_eq!(size_of::<CoreSiginfo>(), 12);
        assert_eq!(size_of::<Elf64Note>(), 12);
        assert_eq!(core::mem::offset_of!(PrStatus, regs), 88);
        assert_eq!(size_of::<PrStatus>(), 88 + 8 * REGSET_LEN);
    }

    #[test]
    fn test_build_ehdr() {
        let ehdr = build_ehdr(3);
        let mut expected = [0u8; 16];
        expected[0..4].copy_from_slice(&ELFMAG);
        expected[4] = ELFCLASS64;
        expected[5] = ELFDATA2LSB;
        expected[6] = EV_CURRENT as u8;
        assert_eq!(ehdr.e_ident, expected);
        assert_eq!(ehdr.e_type, ET_CORE);
        assert_eq!(ehdr.e_machine, EM_MACHINE);
        assert_eq!(ehdr.e_version, EV_CURRENT);
        assert_eq!(ehdr.e_phoff, 64);
        assert_eq!(ehdr.e_ehsize, 64);
        assert_eq!(ehdr.e_phentsize, 56);
        assert_eq!(ehdr.e_phnum, 3);
    }

    #[test]
    fn test_build_load_phdr() {
        let seg = CoreSegment {
            start: 0x1000,
            end: 0x3000,
            flags: PF_R | PF_X,
            file_size: 0x2000,
        };
        let phdr = build_load_phdr(&seg, 0x4000);
        assert_eq!(phdr.p_type, PT_LOAD);
        assert_eq!(phdr.p_flags, PF_R | PF_X);
        assert_eq!(phdr.p_offset, 0x4000);
        assert_eq!(phdr.p_vaddr, 0x1000);
        assert_eq!(phdr.p_paddr, 0);
        assert_eq!(phdr.p_filesz, 0x2000);
        assert_eq!(phdr.p_memsz, 0x2000);
        assert_eq!(phdr.p_align, PAGE_SIZE);
    }

    #[test]
    fn test_build_prstatus_no_frame() {
        // pid=4321 >= MAX_PROCESSES(256) ⇒ process_with 返 None (host 测试不触发进程表锁)
        let ps = build_prstatus(4321, 11, 0);
        assert_eq!(ps.pr_pid, 4321);
        assert_eq!(ps.pr_cursig, 11);
        assert_eq!(ps.siginfo.si_signo, 11);
        assert!(ps.regs.iter().all(|&r| r == 0));
    }
}
