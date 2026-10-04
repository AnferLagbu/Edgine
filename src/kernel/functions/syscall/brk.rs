#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! brk — functions 层实现 (从 privileged/syscall/brk.rs 下沉, §6.1)
//!
//! 堆内存扩展/收缩系统调用. 纯策略: 经 privileged VMA / PMM 公共 API 操作,
//! 无 unsafe 代码。

use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::mm::{PAGE_SIZE, pmm_alloc_pages, vma_get_current_mm};
use crate::privileged::syscall::Errno;

/// 用户空间最大地址
#[cfg(target_arch = "x86_64")]
const USER_ADDR_MAX: u64 = 0x7FFF_FFFF_FFFF;

#[cfg(target_arch = "aarch64")]
const USER_ADDR_MAX: u64 = 0x0000_FFFF_FFFF_FFFF;

/// 全局静态 brk 回退 (无 `MmStruct` 时使用)
static BRK: AtomicU64 = AtomicU64::new(0x400000 + 65536);

/// brk 系统调用实现
pub fn sys_brk(addr: u64) -> i64 {
    if addr == 0 {
        // 返回当前 brk (VMA 优先)
        if let Some(mm) = vma_get_current_mm() {
            return mm.brk.load(Ordering::Acquire) as i64;
        }
        return BRK.load(Ordering::SeqCst) as i64;
    }

    if addr > USER_ADDR_MAX {
        return Errno::ENOMEM.as_ret();
    }

    // VMA 路径: 通过 MmStruct 扩展/收缩堆
    if let Some(mm) = vma_get_current_mm() {
        // 目标用户页表根: 堆收缩需拆除堆页, 拆除必须作用在进程用户页表上.
        // 取不到 (进程无 mm / cr3 为 0) 时 fail-closed, 不静默退化为操作内核表;
        // 扩展方向虽不使用 cr3, 但同样要求进程具备合法页表根.
        let Some(cr3) = crate::privileged::proc::process_get_cr3(
            crate::privileged::proc::process_get_current_pid(),
        ) else {
            return Errno::ENOMEM.as_ret();
        };
        match mm.set_brk(addr as usize, cr3) {
            Ok(new_brk) => return new_brk as i64,
            Err(_) => return Errno::ENOMEM.as_ret(),
        }
    }

    // 回退: 全局静态 brk (无 MmStruct 时使用)
    let current = BRK.load(Ordering::SeqCst);
    if addr > current {
        let extra = addr - current;
        let pages = extra.div_ceil(PAGE_SIZE);
        // 有意窄化: u64 → usize 在 64 位平台无损; pages 由 extra 计算不会溢出
        let ptr = pmm_alloc_pages(pages as usize);
        if ptr.is_null() {
            return Errno::ENOMEM.as_ret();
        }
    }
    BRK.store(addr, Ordering::SeqCst);
    addr as i64
}
