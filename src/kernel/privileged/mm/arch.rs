//! 内存管理层架构抽象封装
//!
//! 封装 `Arch` trait 方法 — MMU/TLB 操作.
//! 所有与 MMU 相关的架构操作集中在此文件。
//!
//! ## Phase 1 状态
//! - [x] `tlb_flush_page()` — 刷新单个虚拟地址 TLB
//! - [x] `tlb_flush_all()` — 刷新全部 TLB
//! - [x] `read_page_table_base()` — 读取页表基地址 (`CR3/TTBR0_EL1`)
//! - [x] `write_page_table_base()` — 写入页表基地址
//! - [x] `read_fault_address()` — 读取页错误地址 (`CR2/FAR_EL1`)
//!
//! ## 设计原则
//! - 零开销: 所有调用通过 `arch!()` 宏展开为静态分发
//! - 零依赖: 直接调用 Arch trait，无中间层
//! - 可替换: Phase 2/3 只需更新 Arch impl，此处无需改动

use crate::privileged::arch::Arch;

/// 刷新单个虚拟地址的 TLB 条目 (invlpg / tlbi vaae1)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn tlb_flush_page(vaddr: usize) {
    <crate::privileged::arch::CurrentArch as Arch>::tlb_flush_page(vaddr);
}

/// 刷新整个 TLB (写 CR3 / tlbi vmalle1)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn tlb_flush_all() {
    <crate::privileged::arch::CurrentArch as Arch>::tlb_flush_all();
}

/// 读取当前页表基地址 (CR3 / `TTBR0_EL1`)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn read_page_table_base() -> u64 {
    <crate::privileged::arch::CurrentArch as Arch>::read_page_table_base()
}

/// 写入页表基地址 (写 CR3 / `TTBR0_EL1`)。
///
/// # Safety
///
/// `paddr` 必须指向有效的页表结构。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn write_page_table_base(paddr: u64) {
    <crate::privileged::arch::CurrentArch as Arch>::write_page_table_base(paddr);
}

/// 读取触发页错误的地址 (CR2 / `FAR_EL1`)。
#[inline(always)]
pub fn read_fault_address() -> usize {
    <crate::privileged::arch::CurrentArch as Arch>::read_fault_address()
}
