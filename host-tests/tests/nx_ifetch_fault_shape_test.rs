//! x86_64 NX 取指缺页语义锁。
//!
//! 背景：`instruction-fetch #PF` 若落在 NX VMA，继续走匿名页分配会映射同一
//! NX 页并重试，形成逐页 TLB shootdown 风暴；正确语义是 SIGSEGV。
//! 可执行 VMA 的 not-present 取指仍允许按需分配，不能一刀切。

const PAGE_FAULT: &str = include_str!("../../src/kernel/privileged/mm/page_fault.rs");

#[test]
fn nx_instruction_fetch_is_signal_segv() {
    assert!(
        PAGE_FAULT.contains(
            "if info.access.instruction && vma.flags.contains(PageFlags::NX) {\n        return PfResult::SignalSegv;"
        ),
        "NX VMA 上的取指缺页必须直接 SIGSEGV，不能继续匿名页分配"
    );
}

#[test]
fn executable_vma_demand_fetch_is_not_blocked() {
    assert!(
        !PAGE_FAULT.contains("if info.access.instruction {\n        return PfResult::SignalSegv;"),
        "不得无条件拦截所有取指缺页：可执行 VMA 的 not-present 取指仍需按需分配"
    );
}
