//! madvise / mlock / mincore 系统调用实现 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原策略代码于 T6-17 (2026-06-17) 迁至 `functions::mm::madvise_mlock`, 本文件仅
//! re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：`sys_madvise`
//! 等被 privileged proc 顶层 re-export (proc/mod.rs) + functions proc 包装消费,
//! 依赖闭包全在 privileged 内 (errno/mm::PAGE_SIZE/copy_user/vma_get_current_mm/
//! userptr) — 属机制项, 迁回 (同 io_uring 模式)。
//!
//! functions 侧改 `pub use crate::privileged::proc::madvise_mlock::*`
//! 保持 API 兼容。
//!
//! ## 职责
//!
//! - madvise: 内存访问模式建议 (参数验证 + 委托 privileged)
//! - mlock/munlock: 物理页锁定/解锁 (参数验证 + 委托 privileged)
//! - mlockall/munlockall: 进程级锁定 (委托 privileged)
//! - mincore: 每页驻留性查询 (参数验证 + copy_to_user)

use crate::privileged::errno::Errno;
use crate::privileged::mm::PAGE_SIZE;
use crate::privileged::mm::copy_user::copy_to_user;
use crate::privileged::mm::vma_get_current_mm;
use crate::privileged::userptr;

// ============================================================================
// madvise advice 常量
// ============================================================================

pub const MADV_NORMAL: u32 = 0;
pub const MADV_RANDOM: u32 = 1;
pub const MADV_SEQUENTIAL: u32 = 2;
pub const MADV_WILLNEED: u32 = 3;
pub const MADV_DONTNEED: u32 = 4;
pub const MADV_FREE: u32 = 5;
pub const MADV_REMOVE: u32 = 9;
pub const MADV_PAGEOUT: u32 = 21;

// ============================================================================
// mlockall 标志
// ============================================================================

pub const MCL_CURRENT: u32 = 1;
pub const MCL_FUTURE: u32 = 2;
pub const MCL_ONFAULT: u32 = 4;

// ============================================================================
// madvise
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `sys_madvise(addr, len, advice) -> 0/-errno` — 对当前地址空间指定区间应用内存建议。
pub fn sys_madvise(addr: u64, len: u64, advice: u64) -> i64 {
    let advice = advice as u32;
    if addr == 0 && len == 0 {
        return Errno::EINVAL.as_ret();
    }
    if addr & (PAGE_SIZE - 1) != 0 {
        return Errno::EINVAL.as_ret();
    }
    if advice > 21 {
        return Errno::EINVAL.as_ret();
    }
    if advice == MADV_REMOVE {
        return 0;
    }

    let mm = match vma_get_current_mm() {
        Some(m) => m,
        None => return Errno::EFAULT.as_ret(),
    };

    let len_usize = match (len as usize).checked_add(0usize) {
        Some(v) => v,
        None => return Errno::ENOMEM.as_ret(),
    };
    match mm.madvise_range(addr as usize, len_usize, advice) {
        Ok(_) => 0,
        Err(e) => e.as_ret(),
    }
}

// ============================================================================
// mlock
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `sys_mlock(addr, len) -> 0/-errno` — 锁定指定区间, 防止其被换出。
pub fn sys_mlock(addr: u64, len: u64) -> i64 {
    if addr & (PAGE_SIZE - 1) != 0 {
        return Errno::EINVAL.as_ret();
    }
    if len == 0 {
        return 0;
    }
    let len_usize = len as usize;

    let mm = match vma_get_current_mm() {
        Some(m) => m,
        None => return Errno::EFAULT.as_ret(),
    };

    match mm.mlock_range(addr as usize, len_usize) {
        Ok(_) => 0,
        Err(e) => e.as_ret(),
    }
}

// ============================================================================
// munlock
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `sys_munlock(addr, len) -> 0/-errno` — 解除指定区间的内存锁定。
pub fn sys_munlock(addr: u64, len: u64) -> i64 {
    if addr & (PAGE_SIZE - 1) != 0 {
        return Errno::EINVAL.as_ret();
    }
    if len == 0 {
        return 0;
    }
    let len_usize = len as usize;

    let mm = match vma_get_current_mm() {
        Some(m) => m,
        None => return Errno::EFAULT.as_ret(),
    };

    match mm.munlock_range(addr as usize, len_usize) {
        Ok(_) => 0,
        Err(e) => e.as_ret(),
    }
}

// ============================================================================
// mlockall
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `sys_mlockall(flags) -> 0/-errno` — 按 `flags` 锁定当前及未来的内存映射。
pub fn sys_mlockall(flags: u64) -> i64 {
    let flags = flags as u32;
    let mm = match vma_get_current_mm() {
        Some(m) => m,
        None => return Errno::EFAULT.as_ret(),
    };

    match mm.mlock_all(flags) {
        Ok(_) => 0,
        Err(e) => e.as_ret(),
    }
}

// ============================================================================
// munlockall
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `sys_munlockall() -> 0/-errno` — 解除本进程全部内存锁定。
pub fn sys_munlockall() -> i64 {
    let mm = match vma_get_current_mm() {
        Some(m) => m,
        None => return Errno::EFAULT.as_ret(),
    };

    match mm.munlock_all() {
        Ok(()) => 0,
        Err(e) => e.as_ret(),
    }
}

// ============================================================================
// mincore
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `sys_mincore(addr, len, vec_ptr) -> 0/-errno` — 查询区间各页是否驻留, 驻留位图写入 `vec_ptr`。
pub fn sys_mincore(addr: u64, len: u64, vec_ptr: u64) -> i64 {
    if addr & (PAGE_SIZE - 1) != 0 {
        return Errno::EINVAL.as_ret();
    }
    if len == 0 {
        return 0;
    }
    if vec_ptr == 0 {
        return Errno::EFAULT.as_ret();
    }

    let page_size = PAGE_SIZE as usize;
    let n_pages = ((len as usize) + page_size - 1) / page_size;
    let buf_bytes = n_pages;

    if !userptr::validate_user_buf(vec_ptr, buf_bytes as u64) {
        return Errno::EFAULT.as_ret();
    }

    let mm = match vma_get_current_mm() {
        Some(m) => m,
        None => return Errno::EFAULT.as_ret(),
    };

    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const MAX_STACK_PAGES: usize = 1024;
    if n_pages > MAX_STACK_PAGES {
        return Errno::ENOMEM.as_ret();
    }
    let mut stack_buf = [0u8; MAX_STACK_PAGES];

    match mm.mincore_range(addr as usize, len as usize, &mut stack_buf[..n_pages]) {
        Ok(_resident) => {
            // 使用 privileged 的 safe copy_to_user 替代 unsafe copy_nonoverlapping
            match copy_to_user(vec_ptr, &stack_buf[..buf_bytes], buf_bytes) {
                Ok(_) => 0,
                Err(()) => Errno::EFAULT.as_ret(),
            }
        }
        Err(e) => e.as_ret(),
    }
}
