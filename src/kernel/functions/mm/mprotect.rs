#![deny(unsafe_code)]
#![deny(missing_docs)]
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
/// 页可读。
pub const PROT_READ: i32 = 0x1;
/// 页可写。
pub const PROT_WRITE: i32 = 0x2;
/// 页可执行。
pub const PROT_EXEC: i32 = 0x4;

/// 将 POSIX prot 转换为 `PageFlags` (策略决策)
#[must_use]
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
        return Err(Errno::Einval);
    }
    // 验证 len > 0
    if len == 0 {
        return Err(Errno::Einval);
    }
    // 验证 prot 合法
    let valid_prot = PROT_NONE | PROT_READ | PROT_WRITE | PROT_EXEC;
    if prot & !valid_prot != 0 {
        return Err(Errno::Einval);
    }

    // 转换 prot → PageFlags
    let new_flags = prot_to_page_flags(prot);

    // 目标用户页表根: 用户页的权限位只存在于进程用户页表中,
    // 改内核表 (全局单表变体) 不会影响用户页权限.
    let cr3 = crate::privileged::proc::process_get_cr3(
        crate::privileged::proc::process_get_current_pid(),
    )
    .ok_or(Errno::Enomem)?;

    // 委托 privileged 层执行页表修改
    vma_get_current_mm().map_or(Err(Errno::Enomem), |mm| {
        match mm.mprotect(
            crate::functions::intcast::usize_from_u64(addr),
            crate::functions::intcast::usize_from_u64(len),
            new_flags,
            cr3,
        ) {
            Ok(()) => Ok(0),
            Err(e) => Err(e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prot_to_page_flags_bit_matrix() {
        let none = prot_to_page_flags(PROT_NONE);
        assert!(
            !none.contains(PageFlags::PRESENT),
            "PROT_NONE 不得置 PRESENT"
        );
        assert!(!none.contains(PageFlags::WRITABLE));
        assert!(!none.contains(PageFlags::USER), "PROT_NONE 不得置 USER");
        assert!(
            !none.contains(PageFlags::NX),
            "PROT_NONE 不置 NX —— 该位属于可读映射的取指策略, 对未映射页无意义"
        );

        let read = prot_to_page_flags(PROT_READ);
        assert!(read.contains(PageFlags::PRESENT | PageFlags::USER | PageFlags::NX));
        assert!(!read.contains(PageFlags::WRITABLE), "PROT_READ 不得可写");

        let write = prot_to_page_flags(PROT_WRITE);
        assert!(
            write.contains(
                PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER | PageFlags::NX
            ),
            "PROT_WRITE 必须可写且不可执行 (W^X 默认栈)"
        );

        let exec = prot_to_page_flags(PROT_EXEC);
        assert!(exec.contains(PageFlags::PRESENT | PageFlags::USER));
        assert!(
            !exec.contains(PageFlags::NX),
            "PROT_EXEC 必须清 NX —— 置 NX 会让取指 #PF"
        );

        let rwx = prot_to_page_flags(PROT_READ | PROT_WRITE | PROT_EXEC);
        assert!(rwx.contains(PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER));
        assert!(!rwx.contains(PageFlags::NX));
    }

    #[test]
    fn test_mprotect_syscall_rejects_invalid_args() {
        assert_eq!(
            mprotect_syscall(0x1001, 0x1000, PROT_READ),
            Err(Errno::Einval),
            "非页对齐 addr 必须 EINVAL"
        );
        assert_eq!(
            mprotect_syscall(0x1000, 0, PROT_READ),
            Err(Errno::Einval),
            "len == 0 必须 EINVAL"
        );
        assert_eq!(
            mprotect_syscall(0x1000, 0x1000, 0x8),
            Err(Errno::Einval),
            "prot 位外值 (0x8) 必须 EINVAL"
        );
        assert_eq!(
            mprotect_syscall(0x1000, 0x1000, -1),
            Err(Errno::Einval),
            "prot 全 1 必须 EINVAL (按无符号位与判定)"
        );
    }

    #[test]
    fn test_mprotect_syscall_needs_current_page_table() {
        // 参数全部合法 ⇒ 越过校验, 落到进程页表根查询; host 侧无当前进程 ⇒ ENOMEM.
        // 该断言的意义是「合法参数不会被校验误杀」, 同时钉住 fail-closed (不静默改内核表).
        assert_eq!(
            mprotect_syscall(0x1000, 0x1000, PROT_READ),
            Err(Errno::Enomem)
        );
    }
}
