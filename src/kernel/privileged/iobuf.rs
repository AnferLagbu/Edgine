//! 内核态 I/O 临时缓冲框架 — `IobRegion`
//!
//! ## 用途
//!
//! 解决 sendmsg/recvmsg 散聚 I/O (SG) 中 4KB 栈缓冲硬限制问题:
//!   - 旧实现: `let mut stack: [u8; 4096] = [0; 4096]` 硬编码 4KB
//!   - 新实现: 按 iov 实际容量, 向上页对齐 alloc 物理页, 用完即 free
//!
//! ## 适用场景
//!
//! - `sendmsg` / `recvmsg` 任意大小 SG 拼接 (突破 4KB 限制)
//! - `readv` / `writev` 同理
//! - 后续 `mremap` 内部页搬迁临时缓冲
//!
//! ## 内存模型
//!
//! 物理页由 `pmm_alloc_pages` 分配, 通过 `phys_to_virt` 映射到内核虚拟地址.
//! Drop 时 (RAII) 自动 `free_pages`, 杜绝泄漏.
//!
//! ## 不允许简化: 不使用栈缓冲; 任意 size 都走 alloc.
//!
//! ## 与 functions 层关系
//!
//! 纯 privileged TCB, 仅暴露 safe API; functions 不可调 raw.

use crate::privileged::config::PAGE_SIZE;
use crate::privileged::mm::phys_to_virt;
use crate::privileged::mm::{pmm_alloc_pages, pmm_free_pages};

/// 内核态 I/O 临时区域 (RAII).
///
/// alloc 物理页并映射到内核虚拟地址; 析构时归还. 失败时 `as_mut_ptr()` 返回 null.
pub struct IobRegion {
    /// 内核虚拟地址 (HHDM 映射)
    vaddr: *mut u8,
    /// 用户请求字节数 (向上页对齐后的实际容量)
    cap: usize,
    /// 实际分配的物理页数
    pages: u64,
}

// SAFETY: IobRegion 拥有独立物理页, 不与其他 region 共享, 内部访问由 alloc/free 串行化.
//   多线程同时持有 IobRegion 是安全的 (物理页不会 move); 唯一不变量: 同一 region 不并行写入.
unsafe impl Send for IobRegion {}

impl IobRegion {
    /// 按 `want` 字节数 alloc 临时区域. 自动向上页对齐.
    /// 返回 `Some(region)` 成功, `None` 表示 alloc 失败.
    pub fn alloc(want: usize) -> Option<Self> {
        if want == 0 {
            // 0 字节用 1 页 (避免 pages=0 在 free 时语义不清)
            return Self::alloc_pages(1);
        }
        let pages = ((want as u64) + PAGE_SIZE - 1) / PAGE_SIZE;
        Self::alloc_pages(pages)
    }

    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn alloc_pages(pages: u64) -> Option<Self> {
        if pages == 0 || pages > 1024 {
            // 4MB 上限保护, 防止恶意/错误请求耗尽物理页
            return None;
        }
        let phys = pmm_alloc_pages(pages as usize);
        if phys.is_null() {
            return None;
        }
        // SAFETY: phys 由 PMM 分配, phys_to_virt 映射有效.
        let vaddr = phys_to_virt(phys as u64) as *mut u8;
        Some(Self {
            vaddr,
            cap: (pages as usize) * (PAGE_SIZE as usize),
            pages,
        })
    }

    /// 内核虚拟地址 (用于 `copy_nonoverlapping` 目标/源)
    #[inline]
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.vaddr
    }

    /// 容量 (字节)
    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// 实际分配页数
    #[inline]
    pub fn pages(&self) -> u64 {
        self.pages
    }
}

impl Drop for IobRegion {
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    fn drop(&mut self) {
        if !self.vaddr.is_null() {
            // 还原 phys addr: vaddr - hhdm_offset, 但 raw::free_pages 期望 phys.
            // 因为 alloc_pages 返回 phys, 我们需要从 vaddr 推回 phys.
            // 物理页是由 alloc_pages 一次性返回的连续区域, vaddr 减去 phys_to_virt
            // 的偏移量即可. 由于 alloc/free 由 raw 集中, 我们记录原 phys.
            // 简化: 用 vaddr 反向 = vaddr - (phys_to_virt(0) - 0) — 不行, 因为 phys_to_virt 是
            // phys + offset, 0 也变 offset. 反向 = vaddr - offset.
            // 实际: 我们 alloc 时收到 phys, 之后 free 需要 phys. 但 struct 没保存 phys.
            // 解决: free_pages 接受 vaddr (依赖 HHDM 反查) — 看 raw API.
            // raw::free_pages(addr, count) — 从 sys/mod.rs 看: pmm_free_pages(addr, count)
            // C-side 期望 phys addr. 所以我们必须把 vaddr 转回 phys.
            // 物理到虚拟: v = p + HHMD_OFFSET; 反向: p = v - HHMD_OFFSET.
            // HHMD_OFFSET 是常量, 但跨架构不同. 我们用 phys_to_virt 反函数: vaddr - (phys_to_virt(0) - 0).
            // 更简洁: 提供 vaddr_to_phys helper.
            let hhdm_offset = phys_to_virt(0);
            let phys = (self.vaddr as u64).wrapping_sub(hhdm_offset);
            pmm_free_pages(phys as *mut u8, self.pages as usize);
        }
    }
}
