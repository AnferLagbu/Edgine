// SPDX-License-Identifier: MPL-2.0
//! P-23 回归: `per_cpu()` 热路径的 CPU 逻辑索引缓存契约.
//!
//! 缺陷形态: `current_cpu_index()` 每次调用都要读一笔 LAPIC ID MMIO, 再线性
//! 扫描 `CPU_HW_IDS`; P-17 实测调度表锁取次数达 9 千万级, 每次调度相关调用
//! 都付这笔开销 (fx3 gdb 快照曾见一核停在 `apic::get_id()`)。
//!
//! 修复形态: `SyscallPerCpu` 尾部 (`offset 64`, 与汇编已用的 `0..56` 不冲突)
//! 存放本核逻辑索引, BSP/AP 在 `gdt_init` / `gdt_init_ap` 写 GS 基址后一次性
//! 写入并自校验; 热路径改为「就绪标志读 + 一条 `gs:[64]` 读」, 就绪标志未置位
//! (BSP `gdt_init` 之前 / host-test) 或自校验失败时回落硬件 id 反查。
//!
//! 运行期到达由 QEMU 启动日志 `[GDT] cpu_index fast path armed: cpu=N` 见证;
//! 本文件锁定接线形态与回落安全网本身。

use std::fs;
use std::path::PathBuf;

const GDT: &str = "src/kernel/privileged/arch/x86_64/gdt.rs";
const SMP: &str = "src/kernel/privileged/smp/mod.rs";
const ARCH: &str = "src/kernel/privileged/arch/mod.rs";
const X86: &str = "src/kernel/privileged/arch/x86_64/mod.rs";
const AARCH64: &str = "src/kernel/privileged/arch/aarch64/mod.rs";

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

/// 空白不敏感 + 尾随逗号不敏感 (rustfmt 会折行长参数列表)
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .replace(",)", ")")
}

fn has(hay: &str, needle: &str) -> bool {
    norm(hay).contains(&norm(needle))
}

/// `first` 的末次出现早于 `second` 的首次出现.
fn before(hay: &str, first: &str, second: &str) -> bool {
    let hay = norm(hay);
    let (Some(f), Some(s)) = (hay.rfind(&norm(first)), hay.find(&norm(second))) else {
        return false;
    };
    f < s
}

#[test]
fn hot_path_uses_cached_index_before_hardware_id() {
    let smp = code_only(&read_src(SMP));
    assert!(
        has(
            &smp,
            "if let Some(idx) = crate::arch!(cached_cpu_index()) { return idx; }"
        ),
        "快路径必须先于任何硬件 id 读取"
    );
    assert!(
        before(
            &smp,
            "if let Some(idx) = crate::arch!(cached_cpu_index())",
            "let hw_id = crate::arch!(cpu_id());"
        ),
        "缓存未命中时才允许走 LAPIC 硬件 id 反查"
    );
}

#[test]
fn x86_fast_path_gates_on_ready_flag_before_reading_gs() {
    let gdt = code_only(&read_src(GDT));
    assert!(
        has(
            &gdt,
            "if !CACHED_CPU_INDEX_READY.load(Ordering::Acquire) { return None; }"
        ),
        "GS 基址未就绪时必须自报不可用, 不得盲读 gs:[64]"
    );
    assert!(
        has(&gdt, r#""mov {}, gs:[64]""#),
        "快路径读取位移必须与 CPU_INDEX_OFFSET 同源 (64)"
    );
    assert!(
        before(
            &gdt,
            "CACHED_CPU_INDEX_READY.load(Ordering::Acquire)",
            r#""mov {}, gs:[64]""#
        ),
        "就绪检查必须先于 gs 读"
    );
}

#[test]
fn bsp_and_ap_write_their_own_logical_index() {
    let gdt = code_only(&read_src(GDT));
    assert!(has(&gdt, "gdt.syscall.cpu_index = 0;"), "BSP 索引恒 0");
    assert!(
        has(&gdt, "ap.syscall.cpu_index = u64::from(cpu_index);"),
        "AP 索引取调用方传入的逻辑索引, 不得沿用槽位 0"
    );
    assert!(
        before(
            &gdt,
            "ap.syscall.cpu_index = u64::from(cpu_index);",
            "&raw const ap.syscall as u64"
        ),
        "AP 必须先写索引槽再把 GS 基址指向该结构"
    );
}

#[test]
fn fast_path_armed_only_after_gs_base_written() {
    let gdt = code_only(&read_src(GDT));
    assert!(
        before(
            &gdt,
            "write_msr(IA32_GS_BASE, &raw const gdt.syscall as u64)",
            "CACHED_CPU_INDEX_READY.store(true, Ordering::Release);"
        ),
        "BSP 就绪标志必须在 GS 基址写入之后置位"
    );
    assert!(
        has(&gdt, "verify_cached_cpu_index(0);"),
        "BSP 必须做一次 gs:[64] 自校验"
    );
    assert!(
        has(&gdt, "verify_cached_cpu_index(cpu_index);"),
        "每个 AP 必须按自己的逻辑索引自校验"
    );
}

#[test]
fn mismatch_disables_fast_path_instead_of_running_stale_index() {
    let gdt = code_only(&read_src(GDT));
    assert!(
        has(
            &gdt,
            "CACHED_CPU_INDEX_READY.store(false, Ordering::Release);"
        ),
        "自校验失败必须关闭全局快路径 (全部核回落慢路径)"
    );
    assert!(
        before(
            &gdt,
            "CACHED_CPU_INDEX_READY.store(false, Ordering::Release);",
            "[GDT] cached cpu_index self-check FAILED"
        ),
        "先关闭快路径再告警"
    );
}

#[test]
fn offset_contract_is_compile_time_locked() {
    let gdt = code_only(&read_src(GDT));
    assert!(
        has(&gdt, "pub const CPU_INDEX_OFFSET: usize = 64;"),
        "位移常量必须显式 64 (汇编位移单源)"
    );
    assert!(
        has(
            &gdt,
            "core::mem::offset_of!(SyscallPerCpu, cpu_index) == CPU_INDEX_OFFSET"
        ),
        "字段偏移漂移必须在编译期拦截"
    );
}

#[test]
fn arch_hook_defaults_to_no_cache_and_only_x86_overrides() {
    let arch = code_only(&read_src(ARCH));
    assert!(
        has(&arch, "fn cached_cpu_index() -> Option<u32> { None }"),
        "无缓存架构默认返回 None"
    );
    assert!(
        has(
            &arch,
            "fn cached_cpu_index() -> Option<u32> { <Self as CoreArch>::cached_cpu_index() }"
        ),
        "Arch 必须委托 CoreArch 的缓存钩子"
    );
    let x86 = code_only(&read_src(X86));
    assert!(
        has(
            &x86,
            "fn cached_cpu_index() -> Option<u32> { crate::privileged::arch::x86_64::gdt::cached_cpu_index() }"
        ),
        "x86_64 必须覆写为 GS 快路径"
    );
    let aarch64 = code_only(&read_src(AARCH64));
    assert!(
        !has(&aarch64, "fn cached_cpu_index"),
        "aarch64 的 MPIDR_EL1 是系统寄存器读, 不设缓存 (保持默认 None)"
    );
}
