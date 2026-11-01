// SPDX-License-Identifier: MPL-2.0
//! TLS 基址随上下文切换恢复的接线契约 (K-00).
//!
//! 背景: `Process.tls_base` 此前只被 `arch_prctl` / `clone(CLONE_SETTLS)` 归档,
//! 切换路径从不恢复硬件寄存器, 用户态线程库拿到的 `FS/TPIDR_EL0` 基址恒为 0.
//!
//! x86_64 的关键约束: `mov fs, sel` 会把 `IA32_FS_BASE` 重置为描述符基址 (0),
//! 因此 MSR 写入**必须**位于段装载之后 —— 本测试按源码次序锁定该契约.
//!
//! 覆盖点:
//! 1. `process_switch_asm` 接收 `user_tls_base` 并在 `mov fs, ax` 之后写 MSR
//! 2. `enter_user_asm` 接收第 6 参数并在 `mov fs, cx` 之后写 MSR
//! 3. 调度器两处 `context_switch` 都传入目标任务 `tls_base`
//! 4. `MmuArch::set_user_tls_base` 在两架构均已实现
//! 5. `arch_prctl(ARCH_SET_FS)` 走统一 setter (aarch64 也立即生效)
//! 6. `execve` 清零 TLS, `fork` / `clone` 按语义继承
//! 7. `Process.tls_base` 文档不再声称"切换恢复未实装"

use std::fs;
use std::path::PathBuf;

const SWITCH_ASM: &str = "src/kernel/privileged/proc/switch.asm";
const X86_ARCH: &str = "src/kernel/privileged/arch/x86_64/mod.rs";
const AARCH64_ARCH: &str = "src/kernel/privileged/arch/aarch64/mod.rs";
const ARCH_TRAIT: &str = "src/kernel/privileged/arch/mod.rs";
const SCHED: &str = "src/kernel/privileged/proc/scheduler.rs";
const USER_PROC: &str = "src/kernel/privileged/proc/user_proc.rs";
const PROCESS: &str = "src/kernel/privileged/proc/process.rs";
const PROC_OPS: &str = "src/kernel/privileged/proc/proc_ops.rs";
const SYSCALL_DISPATCH: &str = "src/kernel/privileged/syscall/dispatch.rs";
const CLONE: &str = "src/kernel/functions/proc/clone.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释, 避免注释里的历史名词被误判为代码
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 源码片段匹配统一忽略空白
fn has(haystack: &str, needle: &str) -> bool {
    let norm = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    norm(haystack).contains(&norm(needle))
}

/// 取 `sig` 所在函数的体 (含签名行), 按花括号配对截取
fn fn_body(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let bytes: Vec<char> = src[start..].chars().collect();
    let mut depth = 0usize;
    let mut seen_open = false;
    for (i, c) in bytes.iter().enumerate() {
        match c {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    return bytes[..=i].iter().collect();
                }
            }
            _ => {}
        }
    }
    panic!("函数体花括号不配对: {sig}");
}

/// x86_64 切换必须用第三参数恢复 FS 基址, 且写入次序在 `mov fs, ax` 之后。
#[test]
fn switch_asm_restores_tls_after_fs_load() {
    let src = read_src(SWITCH_ASM);
    assert!(
        has(
            &src,
            "process_switch_asm(ProcessContext* prev, const ProcessContext* next,"
        ),
        "switch.asm: process_switch_asm 必须接收第三个参数"
    );
    assert!(
        has(&src, "u64 user_tls_base"),
        "switch.asm: 第三参数必须命名为 user_tls_base"
    );
    let fs_load = src.find("mov fs, ax").expect("switch.asm 缺少 mov fs, ax");
    let msr_write = src
        .find("0xC0000100")
        .expect("switch.asm 缺少 IA32_FS_BASE 写入");
    assert!(
        fs_load < msr_write,
        "switch.asm: IA32_FS_BASE 必须在 mov fs 之后写 (段装载会清零基址)"
    );
    assert!(has(&src, "wrmsr"), "switch.asm: 缺少 wrmsr");
}

/// 首次进入用户态 (init / exec / 新 fork 子进程) 同样恢复 TLS。
#[test]
fn enter_user_asm_restores_tls_after_fs_load() {
    let src = read_src(X86_ARCH);
    let asm = &src[src.find("enter_user_asm:").expect("缺少 enter_user_asm")..];
    assert!(
        has(
            asm,
            "rdi=entry, rsi=stack, rdx=arg, rcx=user_cr3, r8=kstack, r9=tls_base"
        ),
        "enter_user_asm: 第 6 参数必须经 r9 传入 tls_base"
    );
    assert!(
        has(asm, "mov r13, r9"),
        "enter_user_asm: 必须在清除寄存器前暂存 r9"
    );
    let fs_load = asm
        .find("mov fs, cx")
        .expect("enter_user_asm 缺少 mov fs, cx");
    let msr_write = asm
        .find("0xC0000100")
        .expect("enter_user_asm 缺少 IA32_FS_BASE 写入");
    assert!(
        fs_load < msr_write,
        "enter_user_asm: IA32_FS_BASE 必须在 mov fs 之后写"
    );
}

/// 调度器两处上下文切换都必须把目标任务的 `tls_base` 交给切换原语。
#[test]
fn scheduler_passes_tls_to_every_switch() {
    let sched = code_only(&read_src(SCHED));
    let tail = fn_body(&sched, "fn context_switch_tail(");
    assert!(
        has(&tail, "tls_base.load"),
        "context_switch_tail: 必须读取目标任务 tls_base"
    );
    assert_eq!(
        tail.matches("context_switch(").count(),
        2,
        "context_switch_tail: 正常切换与单向保活切换都必须传 TLS"
    );
    assert!(
        tail.matches("next_tls_base").count() >= 2,
        "context_switch_tail: 两处切换都必须使用 next_tls_base"
    );
}

/// 首次进入用户态的 `enter_user` 调用必须带上 `tls_base`。
#[test]
fn user_proc_enter_passes_tls() {
    let src = code_only(&read_src(USER_PROC));
    let enter = fn_body(&src, "pub unsafe fn enter(&self");
    assert!(
        has(&enter, "load_tls_base()"),
        "UserProcManager::enter: 必须读取 tls_base"
    );
    assert!(
        has(&enter, "enter_user(") && has(&enter, "tls_base"),
        "UserProcManager::enter: enter_user 调用必须传 tls_base"
    );
}

/// 架构层必须提供统一的 TLS 基址写入原语, 两架构各自实现。
#[test]
fn arch_trait_and_impls_expose_tls_setter() {
    let trait_src = code_only(&read_src(ARCH_TRAIT));
    assert!(
        has(&trait_src, "fn set_user_tls_base(base: u64);"),
        "MmuArch: 缺少 set_user_tls_base 契约"
    );
    assert!(
        has(
            &trait_src,
            "unsafe fn context_switch(from: *mut u8, to: *const u8, user_tls_base: u64);"
        ),
        "MmuArch: context_switch 必须接收 user_tls_base"
    );

    let x86 = code_only(&read_src(X86_ARCH));
    assert!(
        has(&x86, "fn set_user_tls_base(base: u64)"),
        "x86_64: 缺少 set_user_tls_base 实现"
    );
    assert!(
        has(&x86, "IA32_FS_BASE"),
        "x86_64: set_user_tls_base 必须写 IA32_FS_BASE"
    );

    let arm = code_only(&read_src(AARCH64_ARCH));
    assert!(
        has(&arm, "fn set_user_tls_base(base: u64)"),
        "aarch64: 缺少 set_user_tls_base 实现"
    );
    assert!(
        has(&arm, "msr tpidr_el0"),
        "aarch64: set_user_tls_base 必须写 TPIDR_EL0"
    );
    let switch_fn = fn_body(&arm, "unsafe fn context_switch(from: *mut u8");
    assert!(
        has(&switch_fn, "set_user_tls_base(user_tls_base)"),
        "aarch64: context_switch 必须在切换前写入目标 TLS 基址"
    );
}

/// `arch_prctl(ARCH_SET_FS)` 必须走统一 setter, 不得只对 x86_64 生效。
#[test]
fn arch_prctl_uses_unified_tls_setter() {
    let src = code_only(&read_src(SYSCALL_DISPATCH));
    let body = fn_body(&src, "pub(crate) fn sys_arch_prctl(");
    assert!(
        has(&body, "crate::arch!(set_user_tls_base(addr))"),
        "arch_prctl: ARCH_SET_FS 必须经统一 set_user_tls_base 立即生效"
    );
    assert!(
        !body.contains("write_msr"),
        "arch_prctl: 不得再直接写 x86_64 MSR (aarch64 会漏掉)"
    );
}

/// execve 不继承旧映像 TLS; fork / clone 按语义继承。
#[test]
fn exec_clears_tls_and_fork_clone_inherit() {
    let proc_ops = code_only(&read_src(PROC_OPS));
    let exec = fn_body(&proc_ops, "pub extern \"C\" fn proc_exec_replace(");
    assert!(
        has(&exec, "tls_base.store(0") && has(&exec, "set_user_tls_base(0)"),
        "proc_exec_replace: execve 必须清零 TLS 基址 (进程字段 + 硬件寄存器)"
    );
    let inherit = fn_body(&proc_ops, "fn inherit_parent_state(");
    assert!(
        has(&inherit, "tls_base") && has(&inherit, "child.tls_base.store"),
        "inherit_parent_state: fork 子进程必须继承父进程 TLS 基址"
    );

    let clone = code_only(&read_src(CLONE));
    let fragment = fn_body(&clone, "fn vm_child_inherit(");
    assert!(
        has(&fragment, "parent_tls_base") && has(&fragment, "child.tls_base.store"),
        "vm_child_inherit: 未指定 CLONE_SETTLS 时必须继承父线程 TLS"
    );
    let init_ctx = fn_body(&clone, "fn vm_child_init_context(");
    assert!(
        has(&init_ctx, "if tls != 0") && has(&init_ctx, "tls_base"),
        "vm_child_init_context: CLONE_SETTLS 必须覆盖继承值"
    );
}

/// 字段文档不得再声称"切换恢复未实装"。
#[test]
fn process_tls_field_documented_as_live() {
    let src = read_src(PROCESS);
    assert!(
        has(&src, "pub tls_base: AtomicU64,"),
        "Process: 缺少 tls_base 字段"
    );
    assert!(
        !src.contains("切换恢复未实装") && !src.contains("待用户态线程库出现时实装"),
        "Process.tls_base: 字段文档仍声称切换恢复未实装"
    );
}
