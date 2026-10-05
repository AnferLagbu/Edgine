//! 用户态 Stack Canary (P1 #14)
//!
//! 双架构完整实现: `copy_to_user` 写用户内存,
//! `PROCESS_TABLE.with_process` 读 per-process canary.
//!
//! ## 历史问题 (已修复)
//!
//! LLVM 22 aarch64 codegen bug 曾导致含 inline asm label 的函数被 inline
//! 进大函数时触发 `invalid fixup for movz/movk`. 已通过将 `copy_user.rs`
//! 中的 asm label 逻辑拆分到 `#[inline(never)]` 函数 (`setup_recovery` /
//! `teardown_recovery`) 中解决, 阻止 inline chain 传播.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::privileged::mm;
use crate::privileged::proc::{process_get_current_pid, process_with};

static ENTROPY_POOL: AtomicU64 = AtomicU64::new(0x1234_5678_DEAD_BEEFu64);
static PER_PROC_SEED: AtomicU64 = AtomicU64::new(0x5A5A_5A5A_5A5A_5A5Au64);

#[expect(
    clippy::needless_continue,
    reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
)]
/// LFSR-64 推进一步并返回新值.
///
/// 多项式: x^64 + x^63 + x^61 + x^60 + 1 (最大周期 2^64 - 1).
/// CAS 保证多核安全.
pub fn next_random_u64() -> u64 {
    loop {
        let old = ENTROPY_POOL.load(Ordering::Acquire);
        let bit = (old >> 63) ^ (old >> 62) ^ (old >> 60) ^ (old >> 59);
        let new = (old << 1) | (bit & 1);
        match ENTROPY_POOL.compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return new,
            Err(_) => continue,
        }
    }
}

/// 生成 8 字节 canary (低字节强制为 0, Linux/glibc 兼容).
pub fn generate_canary() -> u64 {
    next_random_u64() & 0xFFFF_FFFF_FFFF_FF00
}

/// 设置每进程 canary 种子 — 供进程创建时注入熵源, 使各进程的栈保护值互异。
pub fn set_per_proc_seed(seed: u64) {
    PER_PROC_SEED.store(seed, Ordering::Release);
}

/// 从内核熵源填充用户 buffer.
///
/// 单次最大 256 字节. 返回实际写入字节数; 失败返回 0.
pub fn get_random_bytes(buf: u64, len: usize) -> usize {
    if len == 0 || len > 256 {
        return 0;
    }
    if !mm::is_user_buf(buf, len) {
        return 0;
    }
    let mut random_bytes = [0u8; 256];
    let mut i = 0;
    while i + 8 <= len {
        let val = next_random_u64();
        random_bytes[i..i + 8].copy_from_slice(&val.to_le_bytes());
        i += 8;
    }
    if i < len {
        let val = next_random_u64();
        random_bytes[i..len].copy_from_slice(&val.to_le_bytes()[..len - i]);
    }
    match mm::copy_to_user(buf, &random_bytes[..len], len) {
        Ok(n) => n,
        Err(()) => 0,
    }
}

/// 写 8 字节 canary 到用户 buffer.
///
/// 返回 0 (成功) / -1 (失败).
pub fn write_canary_to_user(buf: u64, len: usize) -> i64 {
    if len < 8 {
        return -1;
    }
    if !mm::is_user_buf(buf, 8) {
        return -1;
    }
    let canary = process_get_current_canary();
    let bytes = canary.to_le_bytes();
    match mm::copy_to_user(buf, &bytes, 8) {
        Ok(_) => 0,
        Err(()) => -1,
    }
}

/// 读取当前进程 per-process canary.
///
/// 从 `Process::stack_canary` (`AtomicU64`) 读取; 若进程不存在则
/// 回退到 `generate_canary()`.
#[inline(never)]
pub fn process_get_current_canary() -> u64 {
    let pid = process_get_current_pid();
    process_with(pid, |p| p.stack_canary.load(Ordering::Acquire)).unwrap_or_else(generate_canary)
}
