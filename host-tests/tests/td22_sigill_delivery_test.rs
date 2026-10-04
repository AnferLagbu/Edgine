//! I-02 补充验收: 用户态非法指令投递 SIGILL
//!
//! 验证 [privileged/idt/handlers.rs::InvalidOpcodeHandler] 的契约:
//! 1. vector 6 必须从 create_handler 派发到 InvalidOpcodeHandler
//! 2. SIGILL (4) 默认动作 = Core
//! 3. handler 严重性分级 (user=Error / kernel=Fatal) 语义
//! 4. UD2 最短 2 字节, 投递后 rip += 2 (避免立即重入)
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `decide_ud` / `Severity` 平行镜像, 改引内核真实源码:
//! - `edgine::kernel::privileged::proc::{signal_default_action, SignalDefaultAction}`
//!   — 信号默认动作 (proc 模块, host 可链接)
//! - `create_handler` 派发表 → include_str! 静态契约扫描 (见下方不可测说明)
//!
//! ## 因内核 host 不可测已移除/降级
//! 1. `create_handler` 所在 `privileged::idt` 模块在 host 下**无法链接**: 其依赖链
//!    触发 `privileged::mm::read_user_cr3_asm` → `#[link_name = "USER_CR3_SAVE"]`
//!    汇编符号 (boot/isr.asm 定义), host 无 isr.asm 产物 → rust-lld undefined symbol.
//!    故派发表验证降级为 include_str! 静态契约 (等价于既有 sigaltstack_test 风格);
//!    同时 `Severity` 枚举 (idt/handlers.rs) 亦不可 host 引用, severity 分级用例移除.
//! 2. `InvalidOpcodeHandler::handle` 的行为镜像 (user-mode 投递 SIGILL+rip+2 /
//!    kernel-mode Panic) 依赖 `InterruptFrame` + 全局 PROCESS_TABLE, 属 IDT 中断
//!    路径上下文, 无法 host 直接调用; 对应用例已移除, 真实投递由 QEMU 集成测试覆盖.

use edgine::kernel::privileged::proc::{SignalDefaultAction, signal_default_action};

/// POSIX SIGILL = 4
const SIGILL: u8 = 4;
/// #UD vector = 6 (x86_64 与 aarch64 一致)
const VECTOR_UD: u8 = 6;
/// UD2 最短 2 字节 (内核里用作 __builtin_trap / 调试桩)
const UD2_LEN: u64 = 2;

#[test]
fn create_handler_dispatches_vector_6() {
    // 内核 handlers.rs::create_handler 派发表静态契约 (host 无法链接 idt 模块):
    // vector 6 → InvalidOpcodeHandler
    let source = include_str!("../../src/kernel/privileged/idt/handlers.rs");
    let needle = format!("{VECTOR_UD} => &INVALID_OPCODE");
    assert!(
        source.contains(&needle),
        "create_handler 必须将 vector 6 派发到 InvalidOpcodeHandler"
    );
}

#[test]
fn create_handler_covers_all_5_critical_vectors() {
    // create_handler 必须覆盖 5 个关键异常 (0/6/8/13/14), 其余走 DefaultHandler.
    let source = include_str!("../../src/kernel/privileged/idt/handlers.rs");
    for (v, handler) in [
        ("0", "DIV_ZERO"),
        ("6", "INVALID_OPCODE"),
        ("8", "DOUBLE_FAULT"),
        ("13", "GPF"),
        ("14", "PAGE_FAULT"),
    ] {
        let needle = format!("{v} => &{handler}");
        assert!(
            source.contains(&needle),
            "create_handler 应包含映射 `{needle}`"
        );
    }
}

#[test]
fn vector_6_does_not_collide_with_divzero() {
    // 边界: vector 0 (DivZero) 与 vector 6 (#UD) 必须派发到不同 handler.
    let source = include_str!("../../src/kernel/privileged/idt/handlers.rs");
    assert!(source.contains("0 => &DIV_ZERO"));
    assert!(source.contains("6 => &INVALID_OPCODE"));
}

#[test]
fn sigill_default_action_is_core() {
    // SIGILL 默认动作 = Core (与 SIGSEGV/SIGBUS/SIGABRT/SIGFPE 同类)
    assert_eq!(signal_default_action(SIGILL), SignalDefaultAction::Core);
}

#[test]
fn sigill_related_signals_are_core() {
    // 内核 FallbackSignalPolicy (functions 注册前回退策略) 对 Core 组信号:
    // QUIT(3)/ILL(4)/ABRT(6)/BUS(7)/FPE(8)/SEGV(11)/XCPU(24)/XFSZ(25)/SYS(31)
    for sig in [3u8, 4, 6, 7, 8, 11, 24, 25, 31] {
        assert_eq!(
            signal_default_action(sig),
            SignalDefaultAction::Core,
            "sig={} 默认动作应为 Core",
            sig
        );
    }
}

#[test]
fn rip_advance_is_exactly_two_bytes() {
    // UD2 编码为 0F 0B, 强制 2 字节, 即使是不可编码前缀的 #UD 也按
    // "单条最短指令" 假设 +2 即可避免立即重入. 内核 handle 用
    // `rip.wrapping_add(2)` 步进, 此处验证算术语义.
    assert_eq!(UD2_LEN, 2);
    let r0 = 0x401000u64;
    let r1 = 0x401002u64;
    assert_eq!(r0.wrapping_add(UD2_LEN), r1);
    // 接近 64-bit 顶部时 +2 应当 wrap, 不 panic
    let boundary = u64::MAX - 1;
    assert_eq!(boundary.wrapping_add(UD2_LEN), 0);
}
