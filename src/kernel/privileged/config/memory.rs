//! 内存布局常量 + ASLR 运行时函数 — privileged 机制常量
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量定义 (页/栈/堆/用户地址空间/ASLR 基址) 于 T6-9 (2026-06-16) 迁至
//! `functions::config::memory`, 本文件仅 re-export + ASLR 运行时函数。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：页大小/栈布局/
//! ASLR 基址等被 privileged arch/mm/proc 机制直接消费, 属机制常量, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::config::memory::*` 保持 API 兼容。
//! 本文件 0 unsafe (纯常量 + 运行时函数).

// ============================================================================
// 页大小
// ============================================================================

/// 页面大小 (字节, 4 KiB). 必须是 2 的幂.
pub const PAGE_SIZE: u64 = 4096;

/// `log2(PAGE_SIZE)`. 编译期保证为 12。
pub const PAGE_SHIFT: u64 = 12;

// ============================================================================
// 大页 (Huge Page)
// ============================================================================

/// 2 MiB huge page.
pub const HUGE_PAGE_2M_SIZE: u64 = 2 * 1024 * 1024;
pub const HUGE_PAGE_2M_SHIFT: u64 = 21;

/// 1 GiB huge page.
pub const HUGE_PAGE_1G_SIZE: u64 = 1024 * 1024 * 1024;
pub const HUGE_PAGE_1G_SHIFT: u64 = 30;

// ============================================================================
// 用户态栈
// ============================================================================

/// 用户态栈初始大小 (字节).
pub const USER_STACK_SIZE: u64 = 65536;

/// 栈保护区域 (字节), 用于捕获栈溢出.
pub const USER_STACK_GUARD: u64 = PAGE_SIZE;

/// 用户栈顶 (可映射地址).
pub const USER_STACK_TOP: u64 = 0x7FFFFFFFE000;

/// 用户进程的内核栈大小 (字节).
pub const USER_KSTACK_SIZE: u64 = 16384;

/// 用户栈的最大自动扩展大小.
pub const USER_STACK_MAX_SIZE: u64 = 8 * 1024 * 1024;

/// 用户 ELF 可执行文件的默认加载地址 (非 PIE, `ET_EXEC`).
pub const USER_CODE_BASE: u64 = 0x400000;

// ============================================================================
// 用户态 ASLR (Address Space Layout Randomization)
// ============================================================================

/// ASLR 熵位数 — 栈/mmap/堆各区域的随机偏移位数.
///
/// 8 位 = 256 种偏移, 偏移范围 = 256 * `PAGE_SIZE` = 1 MiB.
/// Linux `x86_64` 默认 28 位 (mmap), 此处保守取 8 位以简化实现.
pub const ASLR_STACK_BITS: u64 = 8;
pub const ASLR_MMAP_BITS: u64 = 8;
pub const ASLR_HEAP_BITS: u64 = 8;
pub const ASLR_PIE_BITS: u64 = 8;

/// mmap 区域基址 (ASLR 偏移前).
///
/// 位于栈下方, 向下增长. 典型值: 0x7FFFF7xxx000 (glibc 区域).
pub const USER_MMAP_BASE: u64 = 0x7FFFF7000000;

/// 堆区域基址 (ASLR 偏移前).
///
/// 位于代码段上方, 由 `brk()` 扩展.
pub const USER_HEAP_BASE: u64 = 0x600000;

/// PIE 加载基址 (ASLR 偏移前).
///
/// `ET_DYN` ELF 在此基址 + 随机偏移处加载.
pub const USER_PIE_BASE: u64 = 0x555555554000;

// ============================================================================
// 内核栈
// ============================================================================

/// Per-process kernel stack size (bytes).
pub const KERNEL_STACK_SIZE: usize = 65536;

// ============================================================================
// 用户态 ASLR 随机偏移生成
// ============================================================================

/// 生成 ASLR 随机偏移 (页对齐).
///
/// 使用 TSC 作为熵源, 取低 `bits` 位作为偏移, 再乘以 `PAGE_SIZE`.
/// 偏移范围 = [0, (2^bits - 1) * `PAGE_SIZE`].
///
/// # Arguments
/// * `bits` - 熵位数 (如 `ASLR_STACK_BITS` = 8)
///
/// # Returns
/// 页对齐的随机偏移 (字节)
#[inline]
pub fn aslr_random_offset(bits: u64) -> u64 {
    let tsc = crate::privileged::cpu::read_tsc();
    let mask = (1u64 << bits) - 1;
    (tsc & mask) * PAGE_SIZE
}

/// 生成带 ASLR 随机偏移的栈顶地址.
///
/// 栈顶 = `USER_STACK_TOP` - `aslr_random_offset(ASLR_STACK_BITS)`
#[inline]
pub fn aslr_stack_top() -> u64 {
    USER_STACK_TOP - aslr_random_offset(ASLR_STACK_BITS)
}

/// 生成带 ASLR 随机偏移的 mmap 基址.
#[inline]
pub fn aslr_mmap_base() -> u64 {
    USER_MMAP_BASE - aslr_random_offset(ASLR_MMAP_BITS)
}

/// 生成带 ASLR 随机偏移的堆基址.
#[inline]
pub fn aslr_heap_base() -> u64 {
    USER_HEAP_BASE + aslr_random_offset(ASLR_HEAP_BITS)
}

/// 生成带 ASLR 随机偏移的 PIE 加载基址.
#[inline]
pub fn aslr_pie_base() -> u64 {
    USER_PIE_BASE + aslr_random_offset(ASLR_PIE_BITS)
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_aslr_constants() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // ASLR 偏移必须是页对齐的
    check!(PAGE_SIZE > 0, "PAGE_SIZE > 0");
    check!(USER_STACK_TOP > USER_STACK_SIZE, "stack_top > stack_size");
    check!(USER_PIE_BASE > 0, "PIE base > 0");
    check!(USER_MMAP_BASE > USER_PIE_BASE, "mmap > PIE");
    check!(USER_HEAP_BASE >= USER_CODE_BASE, "heap >= code");
    // ASLR 偏移范围不超过 1 MiB (8 bits * 4KB = 1MB)
    let max_offset = ((1u64 << ASLR_STACK_BITS) - 1) * PAGE_SIZE;
    check!(max_offset <= 1024 * 1024, "ASLR offset <= 1MiB");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_aslr_random_offset_range() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // 多次调用应产生不同偏移 (概率性测试, 可能偶尔失败)
    let o1 = aslr_random_offset(8);
    let o2 = aslr_random_offset(8);
    // 偏移应页对齐
    check!(o1.is_multiple_of(PAGE_SIZE), "offset1 page-aligned");
    check!(o2.is_multiple_of(PAGE_SIZE), "offset2 page-aligned");
    // 偏移应在有效范围内
    let max = ((1u64 << 8) - 1) * PAGE_SIZE;
    check!(o1 <= max, "offset1 in range");
    check!(o2 <= max, "offset2 in range");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_aslr_stack_top_range() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    let top = aslr_stack_top();
    // 栈顶应低于 USER_STACK_TOP
    check!(top <= USER_STACK_TOP, "stack_top <= USER_STACK_TOP");
    // 栈顶应足够高 (在用户空间上半部分)
    check!(top > 0x7FFF00000000, "stack_top in upper half");
    // 栈顶应页对齐
    check!(top.is_multiple_of(PAGE_SIZE), "stack_top page-aligned");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_aslr_pie_base_range() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    let base = aslr_pie_base();
    // PIE 基址应 >= USER_PIE_BASE
    check!(base >= USER_PIE_BASE, "PIE base >= USER_PIE_BASE");
    // PIE 基址应在用户空间
    check!(base < USER_STACK_TOP, "PIE base < stack_top");
    // 页对齐
    check!(base.is_multiple_of(PAGE_SIZE), "PIE base page-aligned");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
pub fn register_aslr_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("aslr", "constants", test_aslr_constants);
    r.register("aslr", "random_offset_range", test_aslr_random_offset_range);
    r.register("aslr", "stack_top_range", test_aslr_stack_top_range);
    r.register("aslr", "pie_base_range", test_aslr_pie_base_range);
}
