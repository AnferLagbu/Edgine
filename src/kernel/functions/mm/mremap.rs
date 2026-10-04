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
    // 1. 参数验证
    if old_addr == 0 || old_size == 0 || new_size == 0 {
        return Err(Errno::EINVAL);
    }
    if old_addr & 0xFFF != 0 {
        return Err(Errno::EINVAL);
    }
    // 限制最大搬迁大小: 1 GiB 防止恶意/错误请求耗尽地址空间
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const MAX_REMAP: u64 = 1 << 30;
    if old_size > MAX_REMAP || new_size > MAX_REMAP {
        return Err(Errno::ENOMEM);
    }
    // flags 仅允许 MAYMOVE=1
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const MREMAP_MAYMOVE: i32 = 1;
    if flags & !MREMAP_MAYMOVE != 0 {
        return Err(Errno::EINVAL);
    }

    // 2. 委托 privileged 层执行搬迁
    // 目标用户页表根: 被裁掉区间的页表项必须从进程用户页表拆除.
    let cr3 = crate::privileged::proc::process_get_cr3(
        crate::privileged::proc::process_get_current_pid(),
    )
    .ok_or(Errno::ENOMEM)?;
    mm.mremap(
        old_addr as usize,
        old_size as usize,
        new_size as usize,
        flags,
        cr3,
    )
}
