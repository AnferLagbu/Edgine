#![deny(unsafe_code)]
//! mremap — functions 层安全代理
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `privileged::mm::MmStruct::mremap`。
//!
//! ## 职责
//!
//! - 参数验证 (`old_addr` / `old_size` / `new_size` / flags 合法性)
//! - 类型转换 (usize ↔ u64)
//! - 委托 privileged 层执行 VMA 描述符搬迁
//!
//! ## Linux 语义
//!
//! - `MREMAP_MAYMOVE (1)`: 允许搬迁到新地址
//! - `MREMAP_FIXED (2)`: 不支持 (由 glibc 在用户态模拟)

use crate::privileged::mm::MmStruct;
use crate::privileged::syscall::Errno;

/// mremap 系统调用安全代理
///
/// 成功返回新映射的虚拟地址; 失败返回 `Errno`。
///
/// # Errors
///
/// 当 `old_addr == 0`、`old_size == 0`、`new_size == 0`、`old_addr` 未按页对齐
/// 或 flags 含非法位时返回 `EINVAL`; 当映射尺寸超过 1 GiB 上限时返回 `ENOMEM`;
/// 当无法取得当前进程的用户页表根 (无 mm 或 cr3 为 0) 时返回 `ENOMEM`.
pub fn mremap_syscall(
    mm: &MmStruct,
    old_addr: u64,
    old_size: u64,
    new_size: u64,
    flags: i32,
) -> Result<usize, Errno> {
    const MAX_REMAP: u64 = 1 << 30;
    const MREMAP_MAYMOVE: i32 = 1;
    // 1. 参数验证
    if old_addr == 0 || old_size == 0 || new_size == 0 {
        return Err(Errno::Einval);
    }
    if old_addr & 0xFFF != 0 {
        return Err(Errno::Einval);
    }
    // 限制最大搬迁大小: 1 GiB 防止恶意/错误请求耗尽地址空间
    if old_size > MAX_REMAP || new_size > MAX_REMAP {
        return Err(Errno::Enomem);
    }
    // flags 仅允许 MAYMOVE=1
    if flags & !MREMAP_MAYMOVE != 0 {
        return Err(Errno::Einval);
    }

    // 2. 委托 privileged 层执行搬迁
    // 目标用户页表根: 被裁掉区间的页表项必须从进程用户页表拆除.
    let cr3 = crate::privileged::proc::process_get_cr3(
        crate::privileged::proc::process_get_current_pid(),
    )
    .ok_or(Errno::Enomem)?;
    mm.mremap(
        crate::functions::intcast::usize_from_u64(old_addr),
        crate::functions::intcast::usize_from_u64(old_size),
        crate::functions::intcast::usize_from_u64(new_size),
        flags,
        cr3,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上限与 `mremap_syscall` 内的同名常量必须同步; 单独持有一份是为了在断言里
    /// 表达「边界值本身合法」, 而不必暴露函数内部常量.
    const MAX_REMAP: u64 = 1 << 30;

    fn empty_mm() -> MmStruct {
        MmStruct::new()
    }

    #[test]
    fn test_mremap_rejects_zero_and_unaligned() {
        let mm = empty_mm();
        assert_eq!(
            mremap_syscall(&mm, 0, 0x1000, 0x1000, 0),
            Err(Errno::Einval),
            "old_addr == 0 必须 EINVAL"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1000, 0, 0x1000, 0),
            Err(Errno::Einval),
            "old_size == 0 必须 EINVAL"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1000, 0x1000, 0, 0),
            Err(Errno::Einval),
            "new_size == 0 必须 EINVAL"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1001, 0x1000, 0x1000, 0),
            Err(Errno::Einval),
            "非页对齐 old_addr 必须 EINVAL"
        );
    }

    #[test]
    fn test_mremap_size_cap_is_enomem() {
        let mm = empty_mm();
        assert_eq!(
            mremap_syscall(&mm, 0x1000, MAX_REMAP + 1, 0x1000, 0),
            Err(Errno::Enomem),
            "超大 old_size 必须 ENOMEM (护栏, 而非 EINVAL)"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1000, 0x1000, MAX_REMAP + 1, 0),
            Err(Errno::Enomem),
            "超大 new_size 必须 ENOMEM"
        );
    }

    #[test]
    fn test_mremap_rejects_unknown_flags() {
        let mm = empty_mm();
        assert_eq!(
            mremap_syscall(&mm, 0x1000, 0x1000, 0x1000, 2),
            Err(Errno::Einval),
            "MREMAP_FIXED 未实装, 必须拒绝而不是静默忽略"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1000, 0x1000, 0x1000, -1),
            Err(Errno::Einval),
            "flags 全 1 必须 EINVAL"
        );
    }

    #[test]
    fn test_mremap_size_cap_boundary_is_inclusive() {
        let mm = empty_mm();
        // 探针取非法 flags: 容量护栏先拦 ⇒ ENOMEM, 护栏放行 ⇒ flags 校验 ⇒ EINVAL.
        // 因此 EINVAL 才能证明「上限值本身被接受」; 若把 `>` 改成 `>=`, 这里会变成 ENOMEM 而红.
        assert_eq!(
            mremap_syscall(&mm, 0x1000, MAX_REMAP, 0x1000, 2),
            Err(Errno::Einval),
            "old_size == MAX_REMAP 必须越过容量护栏 (边界是闭区间)"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1000, MAX_REMAP + 1, 0x1000, 2),
            Err(Errno::Enomem),
            "old_size 超上限必须被容量护栏先拦下"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1000, 0x1000, MAX_REMAP, 2),
            Err(Errno::Einval),
            "new_size == MAX_REMAP 必须越过容量护栏"
        );
        assert_eq!(
            mremap_syscall(&mm, 0x1000, 0x1000, MAX_REMAP + 1, 2),
            Err(Errno::Enomem),
            "new_size 超上限必须被容量护栏先拦下"
        );
    }
}
