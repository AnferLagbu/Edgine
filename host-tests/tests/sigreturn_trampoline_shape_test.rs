//! x86_64 `rt_sigreturn` 跳板页形状锁。
//!
//! 根因：信号返回跳板曾被写到用户栈（`thread_stack`/BSS 数据页）。BSS 是 NX，
//! 执行跳板触发 instruction-fetch #PF；页故障处理器把它当数据缺页反复“修复”，
//! 形成逐页 TLB shootdown 风暴，x86 e2e 永远到不了 `[tcp] Listening`。
//!
//! 锁定契约：x86_64 跳板必须是固定、只读可执行的独立页；不得再复制到用户栈。

const SIGNAL: &str = include_str!("../../src/kernel/privileged/proc/signal.rs");
const USER_PROC: &str = include_str!("../../src/kernel/privileged/proc/user_proc.rs");

#[test]
fn x86_sigreturn_trampoline_has_fixed_executable_page() {
    assert!(
        SIGNAL.contains("pub const SIGRETURN_TRAMPOLINE_ADDR: u64 = 0x500000;"),
        "x86_64 跳板必须固定在 0x500000（USER_CODE_BASE 与 heap base 之间的独立页）"
    );
    assert!(
        USER_PROC.contains("pub fn map_sigreturn_trampoline(cr3: u64) -> bool"),
        "必须由进程侧负责映射固定跳板页"
    );
    assert!(
        USER_PROC.contains("PageFlags::PRESENT | PageFlags::USER"),
        "跳板页必须是 PRESENT|USER 且不带 NX/WRITABLE"
    );
    assert!(
        !USER_PROC.contains("PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE"),
        "跳板页不得映射为可写"
    );
}

#[test]
fn x86_signal_return_does_not_copy_trampoline_to_stack() {
    assert!(
        SIGNAL.contains("let trampoline_addr = SIGRETURN_TRAMPOLINE_ADDR;"),
        "x86_64 的 handler 返回地址必须指向固定跳板页"
    );
    assert!(
        SIGNAL.contains("#[cfg(not(target_arch = \"x86_64\"))]\n    let tramp_ok = crate::privileged::mm::copy_to_user("),
        "只有非 x86 架构允许把跳板复制到用户栈"
    );
}
