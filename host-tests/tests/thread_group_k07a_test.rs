// SPDX-License-Identifier: MPL-2.0
//! K-07a 线程组运行时判据回归锁.
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-07a 与 D2/D4.
//!
//! 本阶段把线程组语义第一次放进**真跑**射程: 内嵌 `init` ELF 的线程探针在双架构
//! QEMU 上跑 `clone` / 每线程 TLS / `gettid`+`getpid` / futex 双向往返 /
//! `CLONE_CHILD_CLEARTID` join / `exit_group` 六条判据, 断言点在
//! `scripts/qemu_boot_test.sh`. 真跑已在 aarch64 侧抓出两个静态审查看不到的缺陷
//! (线程栈落 `.rodata` ⇒ 压栈即权限错误; 返回用户态前无信号投递 ⇒ 组退出的
//! `SIGKILL` 无人消费 ⇒ 整组收不齐), 本文件把这两处的修复面钉住, 并锁定 ABI
//! 单源与探针装配.
//!
//! 锁定项:
//!
//! 1. 线程族 ABI 常量单源 = `edgine_abi::thread`; 内核与用户态 wrapper 都引用它,
//!    `functions/proc/clone.rs` 不得再自带本地 `CLONE_*` 定义;
//! 2. `clone_thread` 双架构各一条腿, 子分支都在汇编块内直接跳入
//!    `thread_trampoline`, 且经被调用者保存寄存器偷渡 entry/arg;
//! 3. 线程用户栈与 helper 栈必须是带内部可变性的 `ThreadStack` (否则 rustc 归入
//!    `.rodata`, aarch64 该段无 `PF_W` ⇒ 子线程首次压栈即 EL0 数据错误);
//! 4. `process_block` 在唤醒返回处收口组退出 / `de_thread` 的致命 `SIGKILL`
//!    (aarch64 无返回用户态投递点, 兄弟线程醒来会重睡同一等待字);
//! 5. 探针的六条判据与 `[THREAD] PASS` 里程碑在位, 且 `qemu_boot_test.sh` 两架构
//!    分支都以 fail-closed 断言该里程碑; 专项压力脚本 `thread_stress_test.sh`
//!    以同一里程碑为判据, 并由 `ci/audit.sh` 的 full 档固定轮次接线（偶发缺陷靠
//!    重复启动才现形，单次启动断言不足以判稳）.

use std::fs;
use std::path::PathBuf;

const ABI_THREAD_SRC: &str = "src/rust/edgine-abi/src/thread.rs";
const ABI_LIB_SRC: &str = "src/rust/edgine-abi/src/lib.rs";
const PRIV_TYPES_SRC: &str = "src/kernel/privileged/syscall/types.rs";
const PRIV_FUTEX_SRC: &str = "src/kernel/privileged/syscall/futex.rs";
const FUNC_CLONE_SRC: &str = "src/kernel/functions/proc/clone.rs";
const PROC_OPS_SRC: &str = "src/kernel/privileged/proc/proc_ops.rs";
const USERLIB_SYS_SRC: &str = "src/user/lib/src/sys.rs";
const INIT_MAIN_SRC: &str = "src/user/init/src/main.rs";
const BOOT_SH: &str = "scripts/qemu_boot_test.sh";
const STRESS_SH: &str = "scripts/thread_stress_test.sh";
const AUDIT_SH: &str = "ci/audit.sh";

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

/// 压缩连续空白为单空格, 使断言不受列对齐影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 取从 `start` 到 `end` 之间的源码 (不含 `end`)。
fn slice_between(src: &str, start: &str, end: &str) -> String {
    let i = src
        .find(start)
        .unwrap_or_else(|| panic!("未找到 {start:?}"));
    let rest = &src[i..];
    let j = rest.find(end).unwrap_or_else(|| panic!("未找到 {end:?}"));
    rest[..j].to_string()
}

/// 统计 `needle` 出现次数.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

// ============================================================================
// 1. 线程族 ABI 单源
// ============================================================================

/// `edgine_abi::thread` 必须是线程族常量的唯一权威源, 且值域为 asm-generic
/// (双架构同一套编号), 与内核 `privileged::syscall::types` 的出口逐条对齐.
#[test]
fn k07a_thread_abi_single_source_is_reexported_by_kernel() {
    let abi_lib = read_src(ABI_LIB_SRC);
    assert!(
        abi_lib.contains("pub mod thread;"),
        "edgine-abi 未导出 thread 模块"
    );

    let abi = read_src(ABI_THREAD_SRC);
    for name in [
        "SYS_CLONE",
        "SYS_KILL",
        "SYS_GETTID",
        "SYS_FUTEX",
        "SYS_EXIT_GROUP",
        "SYS_TGKILL",
        "CLONE_VM",
        "CLONE_FS",
        "CLONE_FILES",
        "CLONE_SIGHAND",
        "CLONE_THREAD",
        "CLONE_SETTLS",
        "CLONE_PARENT_SETTID",
        "CLONE_CHILD_SETTID",
        "CLONE_CHILD_CLEARTID",
        "FUTEX_WAIT",
        "FUTEX_WAKE",
        "FUTEX_PRIVATE_FLAG",
    ] {
        assert!(
            abi.contains(&format!("pub const {name}:")),
            "edgine_abi::thread 缺少 {name} 的单源定义"
        );
    }

    // 内核出口逐条取自单源, 不得回退为本地字面量.
    let types = read_src(PRIV_TYPES_SRC);
    for name in [
        "SYS_CLONE",
        "SYS_KILL",
        "SYS_GETTID",
        "SYS_FUTEX",
        "SYS_EXIT_GROUP",
        "SYS_TGKILL",
    ] {
        assert!(
            types.contains(&format!("edgine_abi::thread::{name}")),
            "privileged::syscall::types 的 {name} 未取自 edgine_abi::thread"
        );
    }

    let futex = code_only(&read_src(PRIV_FUTEX_SRC));
    for name in [
        "FUTEX_WAIT",
        "FUTEX_WAKE",
        "FUTEX_REQUEUE",
        "FUTEX_PRIVATE_FLAG",
    ] {
        assert!(
            futex.contains(&format!("edgine_abi::thread::{name}")),
            "privileged::syscall::futex 的 {name} 未取自 edgine_abi::thread"
        );
    }

    // clone 标志位经 `functions::syscall::types` 的 re-export 消费, 本模块内
    // 不得再出现任何本地 `const CLONE_*` 定义 (单源防漂移).
    let clone = code_only(&read_src(FUNC_CLONE_SRC));
    assert!(
        !clone.contains("const CLONE_"),
        "functions/proc/clone.rs 又出现了本地 CLONE_* 常量定义, 破坏 edgine_abi::thread 单源"
    );
    assert!(
        clone.contains("CLONE_PARENT_SETTID"),
        "functions/proc/clone.rs 未从单源引入 CLONE_PARENT_SETTID"
    );
}

/// `tls != 0` 即视为 `CLONE_SETTLS` 是本工程的既定语义口径 (K-00 选定):
/// 内核不因 flags 未带 `CLONE_SETTLS` 而拒绝写入 TLS 基址, 故不装 TLS 的线程
/// 必须传 `tls = 0` 继承组长基址 —— 探针的 helper 腿正是走这条继承路径.
#[test]
fn k07a_tls_nonzero_is_the_settls_trigger() {
    let clone = read_src(FUNC_CLONE_SRC);
    assert!(
        clone.contains("tls != 0"),
        "clone 路径的 TLS 判定基准已漂移: 期望 `tls != 0` 作为 CLONE_SETTLS 语义"
    );
    let init = read_src(INIT_MAIN_SRC);
    assert!(
        init.contains("tls = 0"),
        "init 探针未记录 `tls = 0` 继承基址的口径说明 (helper 线程依赖该口径)"
    );
}

// ============================================================================
// 2. clone 子分支的汇编偷渡
// ============================================================================

/// `clone_thread` 双架构各一条腿, 子分支都在 `asm!` 块内直接跳入
/// `thread_trampoline`, entry/arg 经被调用者保存寄存器随子上下文复制.
#[test]
fn k07a_clone_thread_child_branch_jumps_into_trampoline() {
    let sys = code_only(&read_src(USERLIB_SYS_SRC));
    assert_eq!(
        count_occurrences(&sys, "pub unsafe fn clone_thread("),
        2,
        "clone_thread 应为 x86_64 与 aarch64 各一条腿"
    );
    assert_eq!(
        count_occurrences(&sys, "tramp = sym thread_trampoline"),
        2,
        "两条腿都必须把 thread_trampoline 交给汇编块"
    );
    // x86_64: 子分支只保证 rax=0 与 rsp 已换, 故 entry/arg 只能挂 r12/r13;
    // aarch64: x19/x20 被 LLVM 保留, x8..x18 不入 ProcessContext, 故挂 x21/x22.
    assert!(sys.contains("r12"), "x86_64 腿缺少 r12 偷渡寄存器");
    assert!(sys.contains("r13"), "x86_64 腿缺少 r13 偷渡寄存器");
    assert!(sys.contains("x21"), "aarch64 腿缺少 x21 偷渡寄存器");
    assert!(sys.contains("x22"), "aarch64 腿缺少 x22 偷渡寄存器");
}

/// 线程族 wrapper 齐备: `futex_wait` / `futex_wake` / `exit_group` / `tgkill`
/// 都经 `SYS_*` 单源编号下 syscall.
#[test]
fn k07a_userlib_thread_wrappers_present() {
    let sys = code_only(&read_src(USERLIB_SYS_SRC));
    for name in ["futex_wait(", "futex_wake(", "exit_group(", "tgkill("] {
        assert!(
            sys.contains(name),
            "userlib 缺少 {name} wrapper, 探针无法覆盖该判据"
        );
    }
    for name in ["SYS_FUTEX", "SYS_EXIT_GROUP", "SYS_TGKILL"] {
        assert!(
            sys.contains(name),
            "userlib 的 {name} 未取自 edgine_abi::thread 单源"
        );
    }
}

// ============================================================================
// 3. 线程栈的内部可变性 (aarch64 `.rodata` 无 PF_W)
// ============================================================================

/// 线程用户栈必须包在带内部可变性的 `ThreadStack` 里.
///
/// 真跑发现的缺陷: 无 `UnsafeCell` 的不可变 `static` 被 rustc 归入 `.rodata`,
/// 而 aarch64 的 `.rodata` `PT_LOAD` 不带 `PF_W` ⇒ 该页在 EL0 只读, 子线程首次压栈
/// 即 `ESR=0x9200004F` (level-3 permission fault). x86_64 的 `link.x` 把
/// `.rodata` 并入可写 `PT_LOAD`, 所以同形代码只在 aarch64 崩 —— 判据必须两架构都真跑.
#[test]
fn k07a_thread_stacks_are_interior_mutable() {
    let init = read_src(INIT_MAIN_SRC);
    assert!(
        init.contains("struct ThreadStack(UnsafeCell<"),
        "ThreadStack 必须以 UnsafeCell 包栈数组, 否则整块落 .rodata (aarch64 只读 ⇒ 子线程压栈即权限错误)"
    );
    assert!(
        init.contains("static THREAD_STACKS: [ThreadStack; THREAD_COUNT]"),
        "每线程栈应声明为 ThreadStack 数组"
    );
    assert!(
        init.contains("static GROUP_HELPER_STACK: ThreadStack"),
        "exit_group 腿的 helper 栈应声明为 ThreadStack"
    );
    assert!(
        init.contains("unsafe impl Sync for ThreadStack"),
        "ThreadStack 需显式 Sync 才能进 static (每栈一线程的独占约定)"
    );
    // 反控: 不得再出现裸 `[u8; THREAD_STACK_BYTES]` 的 static 栈.
    let bare = code_only(&init);
    assert!(
        !bare.contains("static THREAD_STACKS: [[u8; THREAD_STACK_BYTES]"),
        "线程栈回退为裸字节数组 static (会落 .rodata)"
    );
}

// ============================================================================
// 4. 阻塞唤醒点的致命信号收口
// ============================================================================

/// `process_block` 唤醒返回处必须收口「`SIGKILL` pending + 组退出/`de_thread`」.
///
/// 真跑发现的缺陷: aarch64 没有返回用户态前的 `do_signal_deliver` 消费点,
/// `process_exit_group` 对睡在 futex 上的兄弟线程只投递一次 `SIGKILL` +
/// `Scheduler::unblock`; 该线程醒来后立刻重睡同一等待字 ⇒ 永远停在队列外的
/// Blocked 态 ⇒ 整组收不齐, 父进程 `wait4` 永久挂死. 与
/// `Scheduler::yield_current` 的致命信号检查同源.
#[test]
fn k07a_process_block_consumes_fatal_signal_on_resume() {
    let ops = read_src(PROC_OPS_SRC);
    let body = norm(&slice_between(
        &ops,
        "pub extern \"C\" fn process_block",
        "解除进程阻塞",
    ));
    let schedule_at = body
        .find("Scheduler::schedule();")
        .unwrap_or_else(|| panic!("process_block 结构已漂移: {body}"));
    let after = &body[schedule_at..];
    assert!(
        after.contains("p.signal_pending_get() & (1u64 << super::signal::SIGKILL) != 0"),
        "process_block 唤醒后未检查 SIGKILL pending"
    );
    assert!(
        after.contains("p.group().exit_requested()"),
        "process_block 唤醒后未复查组退出请求"
    );
    assert!(
        after.contains("p.group().exec_de_thread()"),
        "process_block 唤醒后未复查 exec de_thread"
    );
    assert!(
        after.contains("process_exit(u32::from(super::signal::SIGKILL) << 8 | 0x7f);"),
        "process_block 唤醒后未以 SIGKILL 致命码退出"
    );
}

// ============================================================================
// 5. 探针判据与启动断言
// ============================================================================

/// init 探针覆盖六条判据并打印 `[THREAD] PASS` 里程碑; 失败路径只打印
/// `[THREAD] FAIL`, 故里程碑缺失即回归.
#[test]
fn k07a_init_probe_covers_six_criteria() {
    let init = read_src(INIT_MAIN_SRC);
    assert!(
        init.contains("[THREAD] PASS: clone+per-thread TLS+tid/tgid+futex+join+exit_group"),
        "init 探针的 PASS 里程碑文案漂移, qemu_boot_test.sh 的断言会失去锚点"
    );
    assert!(
        init.contains("[THREAD] FAIL: "),
        "init 探针缺少失败出口, 单点失败无法定位判据"
    );
    // 六条判据的落点各就各位.
    for anchor in [
        "fn thread_probe()",
        "fn thread_leg()",
        "fn thread_spawn_checks(",
        "fn thread_release_and_join()",
        "fn signal_probe(",
        "fn group_exit_leg()",
        "fn group_child_body()",
        "extern \"C\" fn thread_body(",
    ] {
        assert!(init.contains(anchor), "init 探针缺少判据函数 {anchor}");
    }
    // 探针必须在 busy 忙等子进程之前同步完成, 且必须被 `_start` 接线.
    assert!(
        init.contains("thread_probe();"),
        "thread_probe 未接入 _start 探针链"
    );
    let start_body = slice_between(&init, "pub extern \"C\" fn _start", "fn pipe_probe");
    let tls_at = start_body
        .find("tls_probe();")
        .expect("_start 缺少 tls_probe 接线");
    let thread_at = start_body
        .find("thread_probe();")
        .expect("_start 缺少 thread_probe 接线");
    assert!(
        tls_at < thread_at,
        "线程探针必须排在 TLS 探针之后 (共用每线程基址恢复链路)"
    );
    // 所有等待都有界: 失败只打印 FAIL, 绝不挂死启动.
    assert!(
        init.contains("const THREAD_RETRY_LIMIT: u32"),
        "线程探针缺少重试上界 (判据失败会挂死启动)"
    );
}

/// 双架构启动腿都以 fail-closed 断言 `[THREAD] PASS`, 且专项压力脚本以同一
/// 里程碑为判据.
#[test]
fn k07a_boot_and_stress_assert_thread_marker() {
    let boot = read_src(BOOT_SH);
    let parts: Vec<&str> = boot.split(r"\[THREAD\] PASS:").collect();
    assert_eq!(
        parts.len(),
        3,
        "qemu_boot_test.sh 应在 x86_64 与 aarch64 两条腿各断言一次 [THREAD] PASS"
    );
    for (idx, part) in parts.iter().skip(1).enumerate() {
        let head: String = part.chars().take(400).collect();
        assert!(
            head.contains("RESULT=1"),
            "第 {idx} 处 [THREAD] 断言不是 fail-closed (缺失时未置 RESULT=1)"
        );
    }

    let stress = read_src(STRESS_SH);
    assert!(
        stress.contains(r#"MARKER="\[SCHED\] SNAP END""#),
        "thread_stress_test.sh 的判据里程碑应与 qemu_boot_test.sh 的观测断言同源 \
         (K-07b 批3 起取 [SCHED] SNAP END, 它在 [THREAD] PASS 之后触发)"
    );
    assert!(
        stress.contains(r#"grep -aq "\[THREAD\] PASS:""#) && stress.contains("conserve=ok"),
        "thread_stress_test.sh 必须在里程碑之外后置检 [THREAD] PASS: 与 conserve=ok \
         (单次启动同时覆盖线程组与观测两条腿)"
    );
    assert!(
        stress.contains("THREAD_STRESS_SMP") && stress.contains("THREAD_STRESS_ARCH"),
        "thread_stress_test.sh 缺少核数 / 架构旋钮 (偶发缺陷需靠时序与载体变换复现)"
    );
    assert!(
        stress.contains("ITER=\"${1:-50}\""),
        "thread_stress_test.sh 的默认轮次应为 50"
    );
}

/// `ci/audit.sh` 的 full 档必须固定跑该压力脚本: 多线程偶发缺陷只靠重复启动才现形,
/// 单次启动的启动断言不足以判稳.
#[test]
fn k07a_full_gate_runs_thread_stress_rounds() {
    let audit = read_src(AUDIT_SH);
    let stress_call = slice_between(&audit, "STRESS_ROUNDS=", "report_unexecuted_legs");
    assert!(
        stress_call.contains("thread_stress_test.sh"),
        "ci/audit.sh 的 full 档未接线线程组压力脚本 (专项脚本沦为无人跑的孤儿装备)"
    );
    assert!(
        stress_call.contains("THREAD_STRESS_ROUNDS:-3"),
        "full 档的固定轮次缺少默认值 (应固定 3 轮/架构, 可由环境变量上调)"
    );
    assert!(
        stress_call.contains("STRESS_RC") && stress_call.contains("err "),
        "ci/audit.sh 的压力子腿不是 fail-closed (非零退出未阻断)"
    );
}
