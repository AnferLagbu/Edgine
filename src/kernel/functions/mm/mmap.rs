#![deny(unsafe_code)]
#![deny(missing_docs)]
//! mmap/munmap/mprotect 系统调用实现 — functions 层策略主体
//!
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! ## 迁移记录
//!
//! 策略代码于 2026-06-17 从 `privileged::syscall::mmap` 迁移至此。
//! privileged 层仅保留 re-export 保持调用方兼容。
//!
//! ## 职责
//!
//! - mmap/munmap/mprotect 策略逻辑 (参数验证、VMA 创建决策)
//! - VFS 交互: fd → 文件映射后端凭据解析 (属于 functions 层职责)
//! - 文件映射 Page Cache 引用释放

use crate::functions::fs::MmapBacking;
use crate::privileged::mm::{FileBackedVma, MmStruct, Vma, VmaType};
use crate::privileged::mm::{PAGE_SIZE, PageFlags as VmaFlags};
use crate::privileged::syscall::Errno;

// ============================================================================
// mmap 标志位
// ============================================================================

/// `MAP_SHARED`: 写入回写文件
pub const MAP_SHARED: i32 = 0x01;
/// `MAP_PRIVATE`: 写入触发 COW, 不回写文件
pub const MAP_PRIVATE: i32 = 0x02;
/// `MAP_ANONYMOUS`: 匿名映射 (无文件后端)
pub const MAP_ANONYMOUS: i32 = 0x20;
/// `MAP_FIXED`: 强制使用指定地址, 并拆除该区间已有映射.
pub const MAP_FIXED: i32 = 0x10;
/// `MAP_FIXED_NOREPLACE`: 强制使用指定地址, 但区间已有映射时返回 `EEXIST`.
pub const MAP_FIXED_NOREPLACE: i32 = 0x0010_0000;

/// mmap 系统调用的标志位基准值 (0, 标志位由调用侧按需 OR 组合)。
pub const SYS_MMAP_FLAGS: u64 = 0;

// ============================================================================
// VFS 交互 (functions 层职责)
// ============================================================================

/// 从 fd 解析文件映射后端凭据
///
/// B-9.5: 经 per-process fd 表取 `OpenFile` handle, 再由对象自身声明是否可作
/// 文件映射 (见 `File::mmap_backing`). 判据在对象侧: 管道 / eventfd / signalfd
/// 等特殊对象也报告 `VfsFileType::File`, 但其 inode 编号是伪命名空间, 绝不能
/// 被当作页缓存键; 目录 / 设备 / 符号链接同样不可映射 (对齐 Linux `ENODEV`).
///
/// # Errors
///
/// - `EBADF`: `fd` 为负或未在当前进程 fd 表中打开.
/// - `ENODEV`: 对象未声明页缓存后端 (管道 / eventfd / signalfd / 目录等).
pub fn fd_mmap_backing(fd: i32) -> Result<MmapBacking, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let handle_id =
        crate::functions::fs::vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))
            .ok_or(Errno::Ebadf)?;
    crate::functions::fs::OPEN_FILE_TABLE
        .with_file(handle_id, |of| of.file().mmap_backing())
        .flatten()
        .ok_or(Errno::Enodev)
}

// ============================================================================
// mmap 策略实现
// ============================================================================

/// mmap 请求参数 (syscall 入口解析后的扁平参数集)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmapRequest {
    /// 用户地址提示 (0 表示由内核选择)
    pub addr_hint: u64,
    /// 映射长度 (字节)
    pub length: u64,
    /// 保护标志 (`PROT_*`)
    pub prot: i32,
    /// 映射标志 (`MAP_*`)
    pub flags: i32,
    /// 文件描述符 (匿名映射时忽略)
    pub fd: i32,
    /// 文件偏移 (文件映射时须按页对齐)
    pub offset: u64,
    /// 调用者 PWM 标识
    pub pwm: u64,
}

/// 执行 mmap 系统调用: 创建匿名或文件内存映射.
///
/// 根据 flags 区分匿名映射与文件映射, 对齐地址后插入 VMA.
///
/// # Errors
///
/// 以下情况返回 `Err`:
/// - `length == 0`、映射同时或都不带 `MAP_SHARED`/`MAP_PRIVATE`、或文件映射 `offset` 未按页对齐 → `EINVAL`
/// - 文件映射的 `fd` 无效 → `EBADF`
/// - 文件映射的对象不可作页缓存后端 (目录 / 管道 / 事件对象等) → `ENODEV`
/// - 地址分配或 VMA 插入失败(内存不足) → `ENOMEM`
#[inline]
pub fn mmap_syscall(mm: &MmStruct, req: MmapRequest) -> Result<usize, Errno> {
    mmap_syscall_inner(mm, req, None)
}

/// `mmap_syscall` 的地址空间变体: `cr3` 显式给出，供固定落点拆除旧映射时使用.
fn mmap_syscall_inner(mm: &MmStruct, req: MmapRequest, cr3: Option<u64>) -> Result<usize, Errno> {
    if req.length == 0 {
        return Err(Errno::Einval);
    }

    let page = crate::functions::intcast::usize_from_u64(PAGE_SIZE);
    let len_aligned = crate::functions::intcast::usize_from_u64(req.length)
        .checked_add(page - 1)
        .ok_or(Errno::Enomem)?
        & !(page - 1);

    let page_flags = prot_to_vma_flags(req.prot);

    let map_private = (req.flags & MAP_PRIVATE) != 0;
    let map_shared = (req.flags & MAP_SHARED) != 0;
    let map_anonymous = (req.flags & MAP_ANONYMOUS) != 0;

    if map_shared && map_private {
        return Err(Errno::Einval);
    }
    if !map_shared && !map_private {
        return Err(Errno::Einval);
    }

    let map_fixed = (req.flags & MAP_FIXED) != 0;
    let map_fixed_noreplace = (req.flags & MAP_FIXED_NOREPLACE) != 0;
    if map_fixed && map_fixed_noreplace {
        return Err(Errno::Einval);
    }

    // ── 固定落点 ──
    // MAP_FIXED / MAP_FIXED_NOREPLACE 不走 hint 搜索: 地址必须页对齐且落在
    // 用户 VA 区; 两者都要求精确落点, 区别只在已有映射时是拆除还是 EEXIST.
    let fixed_addr = if map_fixed || map_fixed_noreplace {
        Some(validate_fixed_addr(req.addr_hint, len_aligned)?)
    } else {
        None
    };

    // ── 匿名映射 ──
    if map_anonymous {
        let addr = if let Some(fixed) = fixed_addr {
            fixed
        } else {
            let addr = find_or_allocate_addr(mm, req.addr_hint, len_aligned)?;
            addr & !(crate::functions::intcast::usize_from_u64(PAGE_SIZE) - 1)
        };

        let final_flags = page_flags;
        prepare_fixed_range(mm, fixed_addr, addr, len_aligned, map_fixed_noreplace, cr3)?;
        let vma = Vma::new(addr, addr + len_aligned, final_flags, VmaType::Anonymous);
        match mm.insert_vma(vma) {
            Ok(()) => {}
            Err(_) => return Err(Errno::Enomem),
        }

        return Ok(addr);
    }

    // ── 文件映射 ──
    if req.fd < 0 {
        return Err(Errno::Ebadf);
    }

    if !req.offset.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::Einval);
    }

    let backing = fd_mmap_backing(req.fd)?;

    let addr = if let Some(fixed) = fixed_addr {
        fixed
    } else {
        let addr = find_or_allocate_addr(mm, req.addr_hint, len_aligned)?;
        addr & !(crate::functions::intcast::usize_from_u64(PAGE_SIZE) - 1)
    };

    let final_flags = if map_shared {
        page_flags
    } else {
        page_flags & !VmaFlags::WRITABLE
    };

    prepare_fixed_range(mm, fixed_addr, addr, len_aligned, map_fixed_noreplace, cr3)?;
    let vma = Vma::file_backed(&FileBackedVma {
        start: addr,
        end: addr + len_aligned,
        flags: final_flags,
        offset: req.offset,
        inode_id: backing.inode_id,
        pwm: req.pwm,
        shared: map_shared,
        mount_idx: backing.mount_idx,
    });

    match mm.insert_vma(vma) {
        Ok(()) => {}
        Err(_) => return Err(Errno::Enomem),
    }

    Ok(addr)
}

/// 校验固定落点: 非零、页对齐、不低于用户 VA 下界、不越过 `TASK_SIZE`.
fn validate_fixed_addr(addr: u64, len: usize) -> Result<usize, Errno> {
    use crate::privileged::constants::limits::USER_ADDR_MAX;
    if addr == 0 || !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::Einval);
    }
    if addr < crate::privileged::mm::user_va_lower_bound() {
        return Err(Errno::Einval);
    }
    let start = crate::functions::intcast::usize_from_u64(addr);
    let end = start.checked_add(len).ok_or(Errno::Enomem)?;
    if crate::functions::intcast::usize_from_u64(USER_ADDR_MAX) < end {
        return Err(Errno::Enomem);
    }
    Ok(start)
}

/// 固定落点覆盖已有映射时: NOREPLACE 返回 `EEXIST`, FIXED 先拆旧区间.
///
/// 拆除必须同时注销文件映射的页缓存持有并拆除进程用户页表项; `cr3` 由
/// `mmap_syscall` 在检测到重叠后取当前进程页表根. 无重叠时不触碰页表.
fn prepare_fixed_range(
    mm: &MmStruct,
    fixed_addr: Option<usize>,
    start: usize,
    len: usize,
    noreplace: bool,
    cr3: Option<u64>,
) -> Result<(), Errno> {
    if fixed_addr.is_none() {
        return Ok(());
    }
    let end = start.checked_add(len).ok_or(Errno::Enomem)?;
    let has_overlap = {
        let vmas = mm.vmas.lock();
        vmas.iter().any(|vma| vma.start < end && vma.end > start)
    };
    if !has_overlap {
        return Ok(());
    }
    if noreplace {
        return Err(Errno::Eexist);
    }
    let cr3 = match cr3 {
        Some(cr3) => cr3,
        None => current_user_cr3()?,
    };
    release_file_pages(mm, start, end, cr3);
    mm.remove_range(start, end, cr3);
    Ok(())
}

/// 当前进程用户页表根; 无当前进程 / cr3 为 0 时 fail-closed.
fn current_user_cr3() -> Result<u64, Errno> {
    let cr3 = crate::privileged::proc::process_get_current_cr3();
    if cr3 == 0 {
        return Err(Errno::Enomem);
    }
    Ok(cr3)
}

/// 查找或分配映射地址
///
/// 非零 hint 必须是**精确**落点 (本实现不忽略 hint), 故低于用户 VA 下界的
/// hint 直接 `EINVAL`: 该区间在 x86_64 kview 中是内核 boot 恒等窗口, 用户映射
/// 落进去会让内核态 `copy_*_user` / 缺页路径解析到恒等物理页 (I4 代理面失真).
/// hint-less 分配的搜索起点同样抬到下界 (见 `MmStruct::find_free_range`).
fn find_or_allocate_addr(
    mm: &MmStruct,
    addr_hint: u64,
    len_aligned: usize,
) -> Result<usize, Errno> {
    use crate::privileged::constants::limits::USER_ADDR_MAX;
    if addr_hint != 0 && addr_hint < USER_ADDR_MAX {
        if addr_hint < crate::privileged::mm::user_va_lower_bound() {
            return Err(Errno::Einval);
        }
        Ok(crate::functions::intcast::usize_from_u64(addr_hint))
    } else {
        mm.find_free_range(len_aligned).ok_or(Errno::Enomem)
    }
}

// ============================================================================
// munmap 策略实现
// ============================================================================

#[inline]
/// 执行 munmap 系统调用: 解除指定地址区间的内存映射.
///
/// 先释放文件映射区域的 Page Cache 引用, 再从进程地址空间移除对应 VMA
/// 并拆除其页表项.
///
/// # Errors
///
/// 当 `addr == 0` 或 `length == 0` 时返回 `EINVAL`;
/// 当无法取得当前进程的用户页表根 (无 mm 或 cr3 为 0) 时返回 `ENOMEM`
/// (fail-closed: 不静默退化为操作内核表).
pub fn munmap_syscall(mm: &MmStruct, addr: u64, length: u64) -> Result<(), Errno> {
    if addr == 0 || length == 0 {
        return Err(Errno::Einval);
    }

    let start = crate::functions::intcast::usize_from_u64(addr);
    let end = start + crate::functions::intcast::usize_from_u64(length);

    // 目标用户页表根: 用户数据页建在进程用户页表上, 拆除必须作用在同一张表.
    let cr3 = crate::privileged::proc::process_get_cr3(
        crate::privileged::proc::process_get_current_pid(),
    )
    .ok_or(Errno::Enomem)?;

    release_file_pages(mm, start, end, cr3);

    mm.remove_range(start, end, cr3);
    Ok(())
}

/// 释放文件映射区域的 Page Cache 引用
///
/// 仅注销「该 VA 确实持有该页缓存帧」的引用 (判据在 privileged 侧实现一份,
/// 与建立侧缺页路径共用): 从未缺页的页、已被 COW 换出的页不持有缓存帧,
/// 故不得注销他处条目. `cr3` 为该进程用户页表根.
fn release_file_pages(mm: &MmStruct, start: usize, end: usize, cr3: u64) {
    let vmas = mm.vmas.lock();
    for vma in vmas.iter() {
        if vma.kind != VmaType::FileBacked {
            continue;
        }
        let Some(inode_id) = vma.inode_id else {
            continue;
        };
        if vma.start >= end || vma.end <= start {
            continue;
        }

        let overlap_start = vma.start.max(start);
        let overlap_end = vma.end.min(end);

        let mut addr = overlap_start;
        while addr < overlap_end {
            let page_index = ((addr - vma.start) as u64 + vma.offset) / PAGE_SIZE;
            // 返回 None = 该 VA 未持有此缓存帧 (无需注销, 非错误)
            let _ = crate::privileged::mm::pcache::pcache_release_for_va(
                cr3,
                addr as u64,
                inode_id,
                page_index,
            );
            addr += crate::functions::intcast::usize_from_u64(PAGE_SIZE);
        }
    }
}

// ============================================================================
// mprotect 策略实现
// ============================================================================

#[inline]
/// 执行 mprotect 系统调用: 修改指定地址区间的内存保护属性.
///
/// 将区间内所有重叠 VMA 的保护标志更新为目标 prot.
///
/// # Errors
///
/// 当 `addr == 0` 或 `length == 0` 时返回 `EINVAL`.
pub fn mprotect_syscall(mm: &MmStruct, addr: u64, length: u64, prot: i32) -> Result<(), Errno> {
    if addr == 0 || length == 0 {
        return Err(Errno::Einval);
    }

    let start = crate::functions::intcast::usize_from_u64(addr);
    let end = start + crate::functions::intcast::usize_from_u64(length);
    let new_flags = prot_to_vma_flags(prot);

    let mut vmas = mm.vmas.lock();
    for vma in vmas.iter_mut() {
        if vma.start < end && vma.end > start {
            vma.flags = (vma.flags & VmaFlags::empty()) | new_flags;
        }
    }

    Ok(())
}

// ============================================================================
// 辅助函数
// ============================================================================

fn prot_to_vma_flags(prot: i32) -> VmaFlags {
    // PROT_NONE = 0: 保留 VMA 描述以占住地址区间, 但不置 PRESENT; 缺页路径
    // 见 VMA 无 PRESENT 即 SIGSEGV, 不会被匿名页分配"修复"成可读页.
    if prot == 0 {
        return VmaFlags::empty();
    }

    let mut flags = VmaFlags::USER | VmaFlags::PRESENT;
    if prot & 0x02 != 0 {
        flags |= VmaFlags::WRITABLE;
    }
    if prot & 0x04 == 0 {
        flags |= VmaFlags::NX;
    }
    flags
}

// ============================================================================
// syscall 入口 — 从 privileged::syscall::sys_mmap/sys_munmap 迁移的策略层
// ============================================================================

/// mmap syscall 策略入口
#[must_use]
pub fn mmap_syscall_entry(
    addr: u64,
    size: u64,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: u64,
) -> i64 {
    if size == 0 {
        return Errno::Einval.as_ret();
    }
    let pwm = crate::privileged::sgeg::pwm_get_current();
    if !crate::privileged::sgeg::pwm_has_capability(pwm, 7, 0x01) {
        return Errno::Eacces.as_ret();
    }

    // M3: 无 mm 一律 ENOMEM，不再回退到裸页分配。裸页返回值是物理地址，
    // 用户态把它当虚拟地址用会绕过 VMA/页表，动态链接器的 mmap 首步即失真。
    let Some(mm) = crate::privileged::mm::vma_get_current_mm() else {
        return Errno::Enomem.as_ret();
    };

    match mmap_syscall(
        mm,
        MmapRequest {
            addr_hint: addr,
            length: size,
            prot,
            flags,
            fd,
            offset,
            pwm,
        },
    ) {
        Ok(a) => {
            // M4: wasmi 腿用真实用户堆的 mmap 返回值断言用户 VA 下界; 默认镜像不打印.
            #[cfg(feature = "wasm_userspace")]
            crate::privileged::klog_info!(Process, "[WASMI] mmap=0x{:X}", a);
            crate::functions::intcast::i64_from_usize(a)
        }
        Err(e) => e.as_ret(),
    }
}

/// munmap syscall 策略入口
#[must_use]
pub fn munmap_syscall_entry(addr: u64, size: u64) -> i64 {
    if addr == 0 || size == 0 {
        return Errno::Einval.as_ret();
    }

    // M3: 无 mm 不再走裸页释放；没有 VMA/页表归属就无法判断该地址是否属于本进程，
    // 静默 free 会把别的地址空间帧抽走。
    let Some(mm) = crate::privileged::mm::vma_get_current_mm() else {
        return Errno::Enomem.as_ret();
    };

    match munmap_syscall(mm, addr, size) {
        Ok(()) => 0,
        Err(e) => e.as_ret(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prot_to_flags() {
        let none = prot_to_vma_flags(0);
        assert!(none.is_empty(), "PROT_NONE 不得置 PRESENT/WRITABLE/USER");

        let r = prot_to_vma_flags(0x01);
        assert!(r.contains(VmaFlags::PRESENT));
        assert!(!r.contains(VmaFlags::WRITABLE));
        assert!(r.contains(VmaFlags::USER));

        let rw = prot_to_vma_flags(0x03);
        assert!(rw.contains(VmaFlags::PRESENT));
        assert!(rw.contains(VmaFlags::WRITABLE));

        let rwx = prot_to_vma_flags(0x07);
        assert!(rwx.contains(VmaFlags::PRESENT));
        assert!(rwx.contains(VmaFlags::WRITABLE));
        assert!(!rwx.contains(VmaFlags::NX));
    }

    #[test]
    fn test_mmap_flags_constants() {
        assert_eq!(MAP_SHARED, 0x01);
        assert_eq!(MAP_PRIVATE, 0x02);
        assert_eq!(MAP_ANONYMOUS, 0x20);
        assert_eq!(MAP_FIXED, 0x10);
        assert_eq!(MAP_FIXED_NOREPLACE, 0x100000);
    }

    #[test]
    fn test_fixed_mapping_replaces_overlapping_vma() {
        let _ = crate::privileged::boot::init();
        let mm = MmStruct::new();
        let base: usize = 0x50_0000;
        let page = crate::privileged::glue::usize_from_u64(PAGE_SIZE);
        mm.insert_vma(Vma::new(
            base,
            base + 2 * page,
            VmaFlags::PRESENT | VmaFlags::USER | VmaFlags::NX,
            VmaType::Anonymous,
        ))
        .expect("预置 VMA");

        let req = MmapRequest {
            addr_hint: u64::try_from(base + page).expect("地址可表示"),
            length: PAGE_SIZE,
            prot: 0x03,
            flags: MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
            fd: -1,
            offset: 0,
            pwm: 0,
        };
        let addr = mmap_syscall_inner(&mm, req, Some(0)).expect("MAP_FIXED 必须成功");
        assert_eq!(addr, base + page);

        let vmas = mm.vmas.lock();
        assert_eq!(vmas.len(), 2, "旧 VMA 应被拆成前段 + 新映射");
        let old = vmas.iter().find(|v| v.start == base).expect("前段保留");
        assert_eq!(old.end, base + page, "旧 VMA 前段只保留未覆盖页");
        let new = vmas
            .iter()
            .find(|v| v.start == base + page)
            .expect("新映射");
        assert!(new.flags.contains(VmaFlags::WRITABLE), "新映射必须可写");
        assert!(new.flags.contains(VmaFlags::NX), "PROT_RW 必须不可执行");
    }
}
