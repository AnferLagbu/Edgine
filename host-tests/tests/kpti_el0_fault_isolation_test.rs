// SPDX-License-Identifier: MPL-2.0
// KPTI-09 回归测试: EL0 越权访问内核高半区 → 终止该进程 的跨架构接线.
//
// 背景:
//   - 运行期判据由 QEMU 承担 (双架构 `qemu_boot_test.sh` grep
//     `[KPTI] EL0 kernel high-half access denied`), 但运行期只能证"当下通过",
//     不能防回归 —— 本文件用静态断言锁定三处接线契约, 任一处被改坏即 fail。
//   - 用户页表"不含内核 `.text`/`.data` 映射"的**装配面**断言由
//     `kpti_x86_user_table_test.rs` (KPTI-11) 承担, 本文件不重复。
//
// 验收:
//   1. x86_64: 用户态 #PF 收敛到 `RecoveryAction::TerminateProcess` —— 即
//      "不再返回用户态重执行故障指令", 内核态 not-present #PF 仍 `Panic`。
//   2. aarch64: `sync_exception_handler` 识别 EL0 来源 (`frame.spsr` 的 `M[3:0]`)
//      并 `process_exit` + `scheduler_yield`; 顺序上 EL0 终止分支必须**早于**
//      停机循环, 否则 EL0 故障会回落到 `wfi` 死循环 (挂死内核)。
//   3. `init` 探针: 双架构各自的内核高半区别名常量 + `read_volatile` 越权读取
//      + 父进程按退出码判定 + 里程碑串。
//   4. 里程碑串在 `init` 与 `qemu_boot_test.sh` 双架构分支中一致 (fail-closed:
//      脚本无判据 = 运行期验收空转)。

use std::fs;

const INIT: &str = "../src/user/init/src/main.rs";
const A64_EXCEPTION: &str = "../src/kernel/framework/arch/aarch64/exception.rs";
const X64_HANDLERS: &str = "../src/kernel/framework/idt/handlers.rs";
const QEMU_SCRIPT: &str = "../scripts/qemu_boot_test.sh";

/// 双架构共用的运行期里程碑 (由 init 打印, qemu_boot_test.sh 判定).
const MILESTONE: &str = "[KPTI] EL0 kernel high-half access denied";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释 (含 `///` 文档注释), 避免注释文本被误判为代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 取 `fn_text` 对应函数的源码窗口 (从签名起到下一次 `\n}` 为止).
fn fn_body(src: &str, signature: &str) -> String {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("未找到函数签名 {signature:?}"));
    let rest = &src[start..];
    let end = rest.find("\n}").map(|i| i + 2).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// 断言 `a` 在 `b` 之前出现.
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay.find(a).unwrap_or_else(|| panic!("{a:?} 未找到"));
    let ib = hay.find(b).unwrap_or_else(|| panic!("{b:?} 未找到"));
    assert!(ia < ib, "{a:?} 必须出现在 {b:?} 之前");
}

/// 1. x86_64: 故障的用户态异常不得返回到用户态重执行故障指令.
#[test]
fn test_x64_user_fault_terminates_process() {
    let src = code_only(&read(X64_HANDLERS));
    let body = fn_body(&src, "impl ExceptionHandler for PageFaultHandler");

    assert!(
        body.contains("PfResult::SignalSegv => return RecoveryAction::TerminateProcess(pid)"),
        "x86_64 用户态 #PF 未识别为可修复时必须终止进程 (而非返回用户态重执行故障指令)"
    );
    assert!(
        body.contains("return RecoveryAction::TerminateProcess(pid);"),
        "用户态 #PF 的兜底出口必须终止进程"
    );
    assert_eq!(
        body.matches("RecoveryAction::Recovered").count(),
        3,
        "用户态 #PF 只允许三条恢复路径 (栈扩展 / PfResult::Fixed / UffdWait); \
         新增恢复路径必须显式论证其不会命中内核高半区地址"
    );
    // 内核态 not-present #PF 仍须 Panic (内核缺陷不可被当作进程故障掩盖).
    assert!(
        body.contains("RecoveryAction::Panic(PanicInfo::new(")
            && body.contains("\"Kernel Page Fault: page not present\""),
        "内核态 not-present #PF 必须仍走 Panic"
    );
    // 用户态分派必须先于内核态分派 (否则用户态故障会被当作内核缺陷 panic).
    assert_before(&body, "is_user_mode()", "FaultCause::PageNotPresent");
}

/// 2. aarch64: EL0 同步异常 → 终止进程, 且不得回落到停机循环.
#[test]
fn test_a64_el0_sync_fault_terminates_process() {
    let src = code_only(&read(A64_EXCEPTION));
    let body = fn_body(&src, "pub extern \"C\" fn sync_exception_handler");

    assert!(
        body.contains("frame.spsr & 0xF == 0"),
        "aarch64 必须按 SPSR.M[3:0] 识别 EL0 来源 (否则无法区分内核态同 EL 异常)"
    );
    assert_before(&body, "frame.spsr & 0xF == 0", "process_exit");
    assert!(
        body.contains("crate::framework::proc::process_exit(pid)"),
        "EL0 故障分支必须终止当前进程 (与 x86_64 TerminateProcess 口径一致)"
    );
    assert!(
        body.contains("crate::framework::proc::scheduler_yield()"),
        "终止后必须调度离去 (不得返回 EL0 重执行故障指令)"
    );
    assert_before(&body, "process_exit", "loop {");
    assert!(
        body.contains("loop {"),
        "内核态 (非 EL0) 同步异常必须保留停机路径, 使内核缺陷暴露而非被掩盖"
    );
}

/// 3. `init` 探针: 双架构内核高半区别名常量 + 越权读取 + 退出码判定.
#[test]
fn test_init_probe_reads_kernel_high_half() {
    let src = read(INIT);

    assert!(
        src.contains("const KERNEL_IMAGE_ALIAS: u64"),
        "init 必须定义探针目标常量 KERNEL_IMAGE_ALIAS"
    );
    // 双架构常量须与内核侧基址一致 (fail-closed: 缺任一条即断言失败).
    assert!(
        src.contains("0xFFFF_8000_0000_0000 + 0x10_0000"),
        "x86_64 探针目标必须为 KERNEL_BASE + 内核镜像 LMA 基址 (link/x86_64.ld 的 . = 0x100000)"
    );
    assert!(
        src.contains("0xFFFF_0000_0000_0000 + 0x4008_0000"),
        "aarch64 探针目标必须为 KERNEL_BASE + 内核镜像 LMA 基址 (link/aarch64.ld 的 . = 0x40080000)"
    );
    assert!(
        src.contains("core::ptr::read_volatile(KERNEL_IMAGE_ALIAS as *const u8)"),
        "探针必须以 read_volatile 解引用内核高半区别名 (不得被优化掉)"
    );
    assert!(
        src.contains("if wait_pid(probe as i32) != 0"),
        "父进程必须按退出码判定 —— 非 0 即内核终止了探针子进程"
    );
    assert!(
        src.contains(MILESTONE),
        "init 必须打印运行期里程碑串 (与 qemu_boot_test.sh 判据一致)"
    );
    // 读到值 ⇒ 隔离失效: 打印 FAIL(非里程碑) 后以 0 退出, 使父进程判定为不通过.
    assert!(
        src.contains("[KPTI] FAIL: kernel high-half readable from EL0"),
        "读取成功必须显式报告隔离失效"
    );
    assert!(
        src.contains("[KPTI] FAIL: EL0 kernel high-half access was NOT denied"),
        "父进程观察到退出码 0 时必须显式报告隔离失效"
    );
}

/// 4. `qemu_boot_test.sh` 双架构分支均判定同一里程碑 (缺判据 = 验收空转).
#[test]
fn test_qemu_script_asserts_milestone_on_both_arches() {
    let src = read(QEMU_SCRIPT);
    // 脚本用 grep BRE, 方括号需转义 —— 由里程碑串自身派生, 防两处写法漂移.
    let escaped = MILESTONE.replace('[', "\\[").replace(']', "\\]");
    let hits: Vec<&str> = src.lines().filter(|l| l.contains(&escaped)).collect();
    assert_eq!(
        hits.len(),
        2,
        "qemu_boot_test.sh 必须分别在 x86_64 与 aarch64 分支判定里程碑串, 实得 {} 处",
        hits.len()
    );
    assert!(
        hits.iter().any(|l| l.contains("$X64_LOG")),
        "x86_64 分支的判据必须读 x86_64 日志"
    );
    assert!(
        hits.iter().any(|l| l.contains("$A64_LOG")),
        "aarch64 分支的判据必须读 aarch64 日志"
    );
}
