#![deny(unsafe_code)]
//! mprotect — functions 层策略主体
//!
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! ## 迁移记录
//!
//! 策略代码于 2026-06-17 从 `privileged::syscall::mprotect` 迁移至此。
//! privileged 层仅保留 re-export 保持调用方兼容。
//!
//! ## 职责
//!
//! - mprotect 参数验证 (addr 页对齐, len > 0, prot 合法)
//! - prot → `PageFlags` 转换 (策略决策)
//! - 委托 privileged 层执行页表修改 (机制)

use crate::privileged::mm::{PageFlags, vma_get_current_mm};
use crate::privileged::syscall::Errno;

/// PROT 常量
pub const PROT_NONE: i32 = 0x0;
pub const PROT_READ: i32 = 0x1;
pub const PROT_WRITE: i32 = 0x2;
pub const PROT_EXEC: i32 = 0x4;

/// 将 POSIX prot 转换为 `PageFlags` (策略决策)
pub fn prot_to_page_flags(prot: i32) -> PageFlags {
    let mut flags = PageFlags::empty();

    if prot & PROT_READ != 0 || prot & PROT_WRITE != 0 || prot & PROT_EXEC != 0 {
        flags.insert(PageFlags::PRESENT);
    }

    if prot & PROT_WRITE != 0 {
        flags.insert(PageFlags::WRITABLE);
    }

    if prot != PROT_NONE {
        flags.insert(PageFlags::USER);
    }

    if prot & PROT_EXEC == 0 && prot != PROT_NONE {
        flags.insert(PageFlags::NX);
    }

    flags
}

/// mprotect 系统调用策略实现
///
/// 验证参数 + 转换 prot → `PageFlags` + 委托 privileged 执行页表修改.
///
/// # Errors
///
/// 当 `addr` 未按页对齐、`len == 0` 或 `prot` 含非法位时返回 `EINVAL`;
/// 当无法取得当前进程的 mm(内存描述符缺失)或其用户页表根(无 mm / cr3 为 0)时返回 `ENOMEM`.
pub fn mprotect_syscall(addr: u64, len: u64, prot: i32) -> Result<usize, Errno> {
    // 验证 addr 页对齐
    if addr & 0xFFF != 0 {
        return Err(Errno::EINVAL);
    }
    // 验证 len > 0
    if len == 0 {
        return Err(Errno::EINVAL);
    }
    // 验证 prot 合法
    let valid_prot = PROT_NONE | PROT_READ | PROT_WRITE | PROT_EXEC;
    if prot & !valid_prot != 0 {
        return Err(Errno::EINVAL);
    }

    // 转换 prot → PageFlags
    let new_flags = prot_to_page_flags(prot);

    // 目标用户页表根: 用户页的权限位只存在于进程用户页表中,
    // 改内核表 (全局单表变体) 不会影响用户页权限.
    let cr3 = crate::privileged::proc::process_get_cr3(
        crate::privileged::proc::process_get_current_pid(),
    )
    .ok_or(Errno::ENOMEM)?;

    // 委托 privileged 层执行页表修改
    vma_get_current_mm().map_or(Err(Errno::ENOMEM), |mm| {
        match mm.mprotect(addr as usize, len as usize, new_flags, cr3) {
            Ok(()) => Ok(0),
            Err(e) => Err(e),
        }
    })
}
