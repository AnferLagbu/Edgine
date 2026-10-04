//! 同步原语层面架构抽象封装
//!
//! 对 `Arch` trait 方法的薄封装 — 同步原语.
//! 所有与架构相关的同步操作集中在此文件。
//!
//! ## Phase 1 状态
//! - [x] `spin_hint()` — CPU 自旋提示 (pause / yield)
//! - [x] `fence()` — 全内存屏障 (mfence / dsb sy)
//! - [x] `fence_w()` — 写内存屏障 (sfence / dmb st)
//! - [x] `interrupt_save()` — 禁用中断并返回标志
//! - [x] `interrupt_restore()` — 恢复中断标志
//! - [x] `interrupt_enable()` — 启用中断
//! - [x] `is_interrupt_enabled()` — 检查中断状态
//!
//! ## 设计原则
//! - 零开销: 所有调用通过 `arch!()` 宏展开为静态分发
//! - 零依赖: 直接调用 Arch trait，无中间层
//! - 可替换: Phase 2/3 只需更新 Arch impl，此处无需改动

use crate::privileged::arch::Arch;

/// CPU 自旋提示 — 告知 CPU 当前在自旋等待锁。
///
/// `x86_64`: `pause` 指令
/// `AArch64`: `yield` 提示指令 (#1)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn spin_hint() {
    // 使用 fence() 作为通用自旋提示: 强内存屏障防止 CPU 投机执行
    // Phase 2: x86_64 换为专用 pause 指令
    <crate::privileged::arch::CurrentArch as Arch>::fence();
}

/// 全内存屏障 (mfence / dsb sy)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn fence() {
    <crate::privileged::arch::CurrentArch as Arch>::fence();
}

/// 写内存屏障 (sfence / dmb st)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn fence_w() {
    <crate::privileged::arch::CurrentArch as Arch>::fence_w();
}

/// 禁用中断并返回之前的中断状态标志。
///
/// # Safety
///
/// 调用方必须确保 `interrupt_restore` 在适当时候被调用以恢复中断状态。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn interrupt_save() -> usize {
    <crate::privileged::arch::CurrentArch as Arch>::interrupt_disable()
}

/// 恢复之前保存的中断状态。
///
/// # Safety
///
/// `flags` 必须是从 `interrupt_save()` 获取的值。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn interrupt_restore(flags: usize) {
    <crate::privileged::arch::CurrentArch as Arch>::interrupt_restore(flags);
}

/// 启用中断 (sti / msr daifclr)。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn interrupt_enable() {
    <crate::privileged::arch::CurrentArch as Arch>::interrupt_enable();
}

/// 检查中断是否已启用。
#[inline(always)]
pub fn is_interrupt_enabled() -> bool {
    <crate::privileged::arch::CurrentArch as Arch>::is_interrupt_enabled()
}
