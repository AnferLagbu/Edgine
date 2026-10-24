//! P-8: init 编排收口 + 孤儿编排入口防回归门禁
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-8 / P-12.
//!
//! 背景: `privileged/proc/sched_ops.rs` 里的 `#[unsafe(no_mangle)] pub extern "C"
//! fn scheduler_init()` 挂着 10 项子系统 init (tick_query 注册 / cgroup / numa /
//! pm / sgeg / CET / tickless / timesync / kexec / uefi), 但全仓无任何调用者 ——
//! 生产环境这 10 项初始化**从未执行**. 直接后果: NUMA 拓扑从未建立而
//! `SYS_getcpu`/`SYS_mbind` 已接线; `tick_query` 未注册使 freg 的 `current_tick()`
//! 恒返 0 (P-1); `sys_tickless` 未初始化即返错使 EG_TICKLESS/EG_CET/EG_TIMESYNC/
//! EG_UEFI 四条 syscall 全线失效.
//!
//! 收口后的形态 (本文件锁死):
//!   1. `lib.rs::kernel_init` 有一段「延迟子系统编排」块, 按依赖序接线 10 项 init,
//!      位置在 Scheduler 之后、aarch64 `start_interval` 之前;
//!   2. 孤儿 FFI 入口 (`scheduler_init` / `timer_init_ffi`) 已删除, `sgeg_init`
//!      降为普通 Rust 机制函数;
//!   3. unsafe 留在 privileged 机制侧 (`tick_query::install_builtin_tick_query`),
//!      编排层 0 unsafe;
//!   4. 5 个 functions 门面成为单一路径: EG_CET/EG_TICKLESS/EG_TIMESYNC/EG_UEFI
//!      四臂已从 privileged 回退层迁入 functions dispatch, 门面 `*_syscall` 不再
//!      是无人调用的死包裹;
//!   5. `scripts/audit_orphaned_init.py` 已接入 `ci/audit.sh`, 且检测器本身有夹具
//!      自检 (门禁不可信时宁可直接失败).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const KERNEL_LIB: &str = "../src/kernel/lib.rs";
const SCHED_OPS: &str = "../src/kernel/privileged/proc/sched_ops.rs";
const PROC_MECHANISM: &str = "../src/kernel/privileged/proc/mechanism.rs";
const TIMER_MOD: &str = "../src/kernel/privileged/timer/mod.rs";
const SGEG_MOD: &str = "../src/kernel/privileged/sgeg/mod.rs";
const TICK_QUERY: &str = "../src/kernel/privileged/tick_query.rs";
const PRIV_DISPATCH: &str = "../src/kernel/privileged/syscall/dispatch.rs";
const FUNC_DISPATCH: &str = "../src/kernel/functions/syscall/dispatch.rs";
const AUDIT_PY: &str = "../scripts/audit_orphaned_init.py";
const CI_AUDIT: &str = "../ci/audit.sh";
const FUNC_PROC_MOD: &str = "../src/kernel/functions/proc/mod.rs";
const FUNC_IPC_MOD: &str = "../src/kernel/functions/ipc/mod.rs";
const FUNC_IPC_SIGNAL: &str = "../src/kernel/functions/ipc/signal.rs";

/// 「延迟子系统编排」块应接线的 10 项 init (逐字对应 lib.rs 的调用行).
const DEFERRED_INIT_CALLS: &[&str] = &[
    "crate::privileged::tick_query::install_builtin_tick_query();",
    "crate::privileged::proc::cgroup::cgroup_init();",
    "crate::privileged::mm::numa_init(",
    "crate::privileged::driver::pm_init(",
    "crate::privileged::sgeg::sgeg_init();",
    "crate::functions::proc::shadow_stack::init();",
    "crate::functions::timer::tickless::init(",
    "crate::functions::timer::time_sync::init();",
    "crate::functions::driver::kexec::init();",
    "crate::functions::driver::uefi::init(0);",
];

/// 已由 functions 安全门面承接的机制项 (编排层禁止再直调 privileged 版).
const FACADE_ONLY_MECHANISMS: &[&str] = &[
    "privileged::timer::tickless_init(",
    "privileged::timer::timesync_init(",
    "privileged::driver::kexec_init(",
    "privileged::driver::uefi_init(",
    "privileged::arch::cet_init(",
    "privileged::tick_query::register_tick_query(",
];

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {}", path, e))
}

/// 剥掉整行注释, 只留实际生效的代码行.
///
/// 必须剥: 本轮修复在 lib.rs / sched_ops.rs / tick_query.rs 留下大量动机注释,
/// 其中逐字提到 `scheduler_init` / `start_interval` 等被断言的符号名. 不剥注释
/// 的话, 本测试锁的就成了「注释里不许提旧事」, 而不是代码形态.
fn code_lines(src: &str) -> Vec<String> {
    src.lines()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with("//") && !l.starts_with("///") && !l.starts_with("//!"))
        .filter(|l| !l.starts_with("/*") && !l.starts_with('*'))
        .map(String::from)
        .collect()
}

fn code_text(src: &str) -> String {
    code_lines(src).join("\n")
}

fn assert_contains(hay: &str, needle: &str, why: &str) {
    assert!(hay.contains(needle), "{why} (未找到 {needle:?})");
}

fn assert_before(hay: &str, a: &str, b: &str, why: &str) {
    let ia = hay
        .find(a)
        .unwrap_or_else(|| panic!("{a:?} 未找到 —— {why}"));
    let ib = hay
        .find(b)
        .unwrap_or_else(|| panic!("{b:?} 未找到 —— {why}"));
    assert!(ia < ib, "{why}: {a:?} 应在 {b:?} 之前");
}

fn repo_root() -> PathBuf {
    Path::new("..").canonicalize().expect("仓库根目录")
}

/// 在临时目录里复刻 `<root>/scripts` + `<root>/src/kernel`, 跑审计脚本, 返回退出码.
///
/// 审计脚本按自身文件位置推导 PROJECT_ROOT, 因此把脚本复制进临时 `scripts/`
/// 就能让它在夹具目录上工作 (与 td25_comment_language_test.rs 同构).
fn run_audit_on_fixture(files: &[(&str, &str)]) -> (i32, String) {
    let mut tmp = std::env::temp_dir();
    let uniq = format!(
        "edgine-orphaned-init-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );
    tmp.push(uniq);
    fs::create_dir_all(tmp.join("src/kernel")).expect("建夹具目录");
    fs::create_dir_all(tmp.join("scripts")).expect("建 scripts 目录");
    fs::copy(
        repo_root().join(AUDIT_PY.strip_prefix("../").unwrap()),
        tmp.join("scripts/audit_orphaned_init.py"),
    )
    .expect("复制审计脚本");
    for (rel, content) in files {
        let p = tmp.join("src/kernel").join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("建夹具子目录");
        }
        fs::write(p, content).expect("写夹具文件");
    }

    let out = Command::new("python3")
        .arg("scripts/audit_orphaned_init.py")
        .current_dir(&tmp)
        .output()
        .expect("启动 python3 失败 (审计脚本无法执行)");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = fs::remove_dir_all(&tmp);
    (out.status.code().unwrap_or(-1), stdout)
}

// ── 1. 编排块: 10 项 init 全部接线 ────────────────────────────────────────

#[test]
fn test_kernel_init_wires_all_deferred_subsystems() {
    let lib = code_text(&read(KERNEL_LIB));
    for call in DEFERRED_INIT_CALLS {
        assert_contains(&lib, call, "kernel_init 必须接线该项 init (P-8 收口)");
    }
}

#[test]
fn test_deferred_block_ordered_after_scheduler_before_timer_start() {
    // 依赖序: Scheduler/Timer 就绪后才能注册 tick_query 与 tickless 状态;
    // Generic Timer 一旦 start_interval, tick 路径即查 tickless 状态 ⇒ 编排必须更早.
    // 逐条校验 (而非只校首尾): 只校边界时, 把中间或首项单独移到 start_interval
    // 之后不会被发现 —— 负控制实测过这一点.
    let lib = code_text(&read(KERNEL_LIB));
    const TIMER_START: &str = "crate::privileged::arch::aarch64::timer::start_interval(";
    for call in DEFERRED_INIT_CALLS {
        assert_before(
            &lib,
            "crate::functions::proc::init();",
            call,
            "延迟子系统编排必须晚于 scheduler init (tick_query / cgroup 依赖它)",
        );
        assert_before(
            &lib,
            call,
            TIMER_START,
            "延迟子系统编排每一项都必须早于 aarch64 Generic Timer 启动",
        );
    }
}

#[test]
fn test_deferred_block_wired_exactly_once_in_real_boot_path() {
    // 两项可验形态约束:
    //   1. 编排块位于真机引导门控 (`not(any(kernel_test, host-test))`) 之后
    //      —— 测试维不应重复跑硬件子系统 init;
    //   2. 每项 init 在 lib.rs 里只出现一次 —— 防止旧 FFI 路径与编排块并存.
    let lib = code_text(&read(KERNEL_LIB));
    let gate = "#[cfg(not(any(feature = \"kernel_test\", feature = \"host-test\")))]";
    let gate_pos = lib
        .find(gate)
        .unwrap_or_else(|| panic!("真机引导块门控 {gate:?} 必须存在"));
    let first_call = lib
        .find(DEFERRED_INIT_CALLS[0])
        .expect("编排块首项调用必须存在");
    assert!(
        gate_pos < first_call,
        "延迟子系统编排必须处于真机引导块内 (门控在调用之前)"
    );
    for call in DEFERRED_INIT_CALLS {
        let n = lib.matches(call).count();
        assert_eq!(n, 1, "{call:?} 在 kernel_init 里应只接线一次 (实得 {n} 次)");
    }
}

#[test]
fn test_orchestration_layer_has_no_direct_mechanism_init() {
    // 门面项必须由 functions 门面承接, 编排层不得留 privileged 直调第二路径
    // (双路径 = 门面 `*_syscall` 变死包裹, 正是 P-8 要收口的形态).
    let lib = code_text(&read(KERNEL_LIB));
    for banned in FACADE_ONLY_MECHANISMS {
        assert!(
            !lib.contains(banned),
            "编排层不应直调 {banned:?} —— 改走对应 functions 门面/tick_query safe 封装"
        );
    }
}

// ── 2. 孤儿 FFI 入口已删除 ────────────────────────────────────────────────

#[test]
fn test_orphan_scheduler_init_entry_removed() {
    let code = code_text(&read(SCHED_OPS));
    assert!(
        !code.contains("fn scheduler_init("),
        "sched_ops.rs 不得再保留 scheduler_init 孤儿入口 (P-8 缺陷类)"
    );
    let mech = code_lines(&read(PROC_MECHANISM));
    assert!(
        !mech
            .iter()
            .any(|l| l.contains("pub use") && l.contains("scheduler_init")),
        "mechanism.rs 不得再 re-export 已删除的 scheduler_init"
    );
}

#[test]
fn test_orphan_timer_init_ffi_removed() {
    let code = code_text(&read(TIMER_MOD));
    assert!(
        !code.contains("fn timer_init_ffi("),
        "timer_init_ffi 无调用者, 必须删除 (真机 timer_init 由 kernel_init 直路)"
    );
}

#[test]
fn test_sgeg_init_downgraded_to_plain_rust_fn() {
    // 与 P-9 同判据: 唯一调用者是 crate root 编排 ⇒ 不该再占 C ABI.
    let sgeg = code_text(&read(SGEG_MOD));
    assert_contains(
        &sgeg,
        "pub fn sgeg_init()",
        "sgeg_init 应为普通 Rust 机制函数",
    );
    let lines = code_lines(&read(SGEG_MOD));
    let idx = lines
        .iter()
        .position(|l| l.contains("pub fn sgeg_init()"))
        .expect("sgeg_init 定义必须存在");
    // 签名之前的非空代码行 (属性行不算注释, 会保留在 code_lines 里).
    let nearby: Vec<String> = lines[..idx]
        .iter()
        .rev()
        .filter(|l| !l.is_empty())
        .take(6)
        .cloned()
        .collect();
    let nearby = nearby.join(" ");
    assert!(
        !nearby.contains("no_mangle") && !nearby.contains("extern \"C\""),
        "sgeg_init 不得残留 extern \"C\" / no_mangle 形态"
    );
}

// ── 3. unsafe 封在 privileged 机制侧 ──────────────────────────────────────

#[test]
fn test_tick_query_registration_wrapped_safe_in_privileged() {
    let tq = read(TICK_QUERY);
    let code = code_text(&tq);
    assert_contains(
        &code,
        "pub fn install_builtin_tick_query()",
        "机制侧必须提供 safe 封装",
    );
    assert_contains(&tq, "// SAFETY:", "unsafe 注册块必须配 SAFETY 注释 (F4)");
    // safe 封装之外, register_tick_query 不得再被任何编排层直调 (见上条测试).
    let lib = code_text(&read(KERNEL_LIB));
    assert!(
        !lib.contains("register_tick_query"),
        "编排层只能调 safe 封装, 不得自己开 unsafe 注册"
    );
}

// ── 4. 5 门面收口: EG_* 四臂迁 functions ──────────────────────────────────

#[test]
fn test_eg_arms_no_longer_in_privileged_fallback() {
    let priv_code = code_text(&read(PRIV_DISPATCH));
    for arm in [
        "EG_CET =>",
        "EG_TICKLESS =>",
        "EG_TIMESYNC =>",
        "EG_UEFI =>",
    ] {
        assert!(
            !priv_code.contains(arm),
            "{arm} 必须已从 privileged 回退层迁出 (否则门面是死包裹)"
        );
    }
}

#[test]
fn test_eg_arms_dispatch_through_facades() {
    let func = code_text(&read(FUNC_DISPATCH));
    let expected = [
        (
            "EG_CET =>",
            "crate::functions::proc::shadow_stack::cet_syscall(",
        ),
        (
            "EG_TICKLESS =>",
            "crate::functions::timer::tickless::tickless_syscall(",
        ),
        (
            "EG_TIMESYNC =>",
            "crate::functions::timer::time_sync::timesync_syscall(",
        ),
        (
            "EG_UEFI =>",
            "crate::functions::driver::uefi::uefi_syscall(",
        ),
    ];
    for (arm, facade) in expected {
        assert_contains(&func, arm, "functions dispatch 必须有该 EG_ 臂");
        assert_contains(&func, facade, "该臂必须经 functions 安全门面");
    }
}

// ── 5. UEFI 简化项必须留 SIMPLIFIED 标注 (§12.3) ──────────────────────────

#[test]
fn test_uefi_zero_system_table_is_marked_simplified() {
    let lib = read(KERNEL_LIB);
    assert_contains(
        &lib,
        "// SIMPLIFIED: UEFI 系统表地址固定传 0",
        "bootloader 未传 EFI system table 属有意简化, 必须可被审查识别",
    );
}

// ── 6. D: 防回归门禁本身 ──────────────────────────────────────────────────

#[test]
fn test_orphaned_init_audit_wired_into_ci() {
    let ci = read(CI_AUDIT);
    assert_contains(
        &ci,
        "audit_orphaned_init.py",
        "审计脚本必须接入本地权威门禁",
    );
    assert_contains(&ci, "--self-test", "门禁上线前必须先自证检测器有效");
    assert!(Path::new(AUDIT_PY).exists(), "审计脚本必须存在");
}

#[test]
fn test_audit_self_test_passes() {
    // 检测器退化 (如漏剥注释) 会让门禁静默全绿; 自检失败即门禁不可信.
    let out = Command::new("python3")
        .arg(repo_root().join(AUDIT_PY.strip_prefix("../").unwrap()))
        .arg("--self-test")
        .output()
        .expect("启动 python3 失败");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "审计脚本自检必须通过:\n{stdout}"
    );
}

#[test]
fn test_repo_scan_finds_no_orphan_aggregation_entry() {
    let out = Command::new("python3")
        .arg(repo_root().join(AUDIT_PY.strip_prefix("../").unwrap()))
        .output()
        .expect("启动 python3 失败");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "全仓扫描必须无 O1 孤儿编排入口 (P-8 不变式):\n{stdout}"
    );
}

#[test]
fn test_audit_detects_synthetic_orphan_entry() {
    // 负控制 (永久化): 复刻 scheduler_init 的形态 —— no_mangle + init 语义 +
    // 零调用 + 体内聚合多项 init, 必须被 O1 命中并 fail-closed.
    let fixture = r#"
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn synthetic_orphan_init() {
    crate::privileged::proc::cgroup::cgroup_init();
    crate::privileged::mm::numa_init(0, 1);
    crate::privileged::driver::pm_init(1);
}
"#;
    let (code, stdout) = run_audit_on_fixture(&[("privileged/proc/fixture.rs", fixture)]);
    assert_eq!(code, 1, "合成孤儿编排入口必须被 O1 拦截:\n{stdout}");
    assert!(
        stdout.contains("synthetic_orphan_init"),
        "报告必须点出违规入口名:\n{stdout}"
    );
}

#[test]
fn test_audit_ignores_wired_and_comment_only_entries() {
    // 正控制: 有调用者 / 仅注释提及, 都不构成违规 —— 防止门禁把存量注释当调用点.
    let wired = r#"
#[unsafe(no_mangle)]
pub extern "C" fn wired_ok_init() {
    a_init();
    b_init();
}
fn caller() {
    wired_ok_init();
}
"#;
    let (code, stdout) = run_audit_on_fixture(&[("privileged/proc/wired.rs", wired)]);
    assert_eq!(code, 0, "有调用者的入口不得误报:\n{stdout}");

    let comment_only = r#"
// 历史注释提到 legacy_agg_init, 它不构成调用点
#[unsafe(no_mangle)]
pub extern "C" fn legacy_agg_init() {
    a_init();
    b_init();
}
"#;
    let (code, stdout) = run_audit_on_fixture(&[("privileged/proc/note.rs", comment_only)]);
    assert_eq!(code, 1, "注释里的名字不得被当作调用点:\n{stdout}");
}

// ── 7. 跨层 init 接线顺序 (802b8298 修复的回归锁) ─────────────────────────

#[test]
fn test_boot_path_wires_mm_ipc_strategy_proc_in_order() {
    // 4 项此前无生产调用点 (mm::init / ipc::init / ipc 策略注册 / proc::init),
    // 缺失后果各自不同: MM 策略停留 fallback、首次 pipe() 越界 panic、策略未注册
    // 降级 ENOSYS、信号与 coredump 策略从不注册.
    let lib = code_text(&read(KERNEL_LIB));
    let gate = "#[cfg(not(any(feature = \"kernel_test\", feature = \"host-test\")))]";
    let gate_pos = lib
        .find(gate)
        .unwrap_or_else(|| panic!("真机引导块门控 {gate:?} 必须存在"));
    let boot = &lib[gate_pos..];

    const MM_INIT: &str = "crate::functions::mm::init();";
    const IPC_INIT: &str = "crate::privileged::ipc::ipc_init();";
    const IPC_STRATEGY: &str = "crate::functions::ipc::strategy::register_default_ipc_strategy()";
    const PROC_INIT: &str = "crate::functions::proc::init();";

    assert_contains(boot, MM_INIT, "真机引导必须注册 functions MM 策略");
    assert_contains(boot, IPC_INIT, "真机引导必须初始化 IPC 机制槽位");
    assert_contains(boot, IPC_STRATEGY, "真机引导必须注册 IPC 策略");
    assert_contains(boot, PROC_INIT, "真机引导必须走 proc::init 组合入口");

    // 槽位必须早于策略注册 (策略方法惰性调用, 但未分配槽位时首个 pipe() 即 panic);
    // 前三项必须在中断使能前完成 (单线程阶段, 无 IRQ 竞态). proc::init 依赖
    // SMP 上线后的 per-CPU 调度器, 故位于中断使能之后 (步骤 8).
    assert_before(boot, MM_INIT, IPC_INIT, "MM 策略注册应早于 IPC 机制初始化");
    assert_before(
        boot,
        IPC_INIT,
        IPC_STRATEGY,
        "IPC 槽位必须早于 IPC 策略注册",
    );
    assert_before(
        boot,
        IPC_STRATEGY,
        PROC_INIT,
        "IPC 策略注册应早于 scheduler 组合入口",
    );
    for call in [MM_INIT, IPC_INIT, IPC_STRATEGY] {
        assert_before(
            boot,
            call,
            "interrupt_late_init(",
            "策略注册 / IPC 槽位填充必须在中断使能前完成",
        );
    }

    // 真机路径不得再直调 privileged scheduler::init —— 由 proc::init 组合入口取代
    // (直调会跳过信号策略与 coredump sink 注册).
    assert!(
        !boot.contains("crate::privileged::proc::scheduler::init()"),
        "真机引导不得绕过 functions::proc::init 直调 privileged scheduler::init"
    );
}

#[test]
fn test_signal_policy_registered_only_by_proc_init() {
    // 信号决策策略的唯一权威实现是 `functions::proc::signal::StandardSignalPolicy`;
    // ipc 层曾另有一份语义不同的重复实现 (`FunctionsSignalPolicy`), 双实现会让
    // "改哪份生效" 取决于注册顺序, 必须保持零重复.
    let sig = code_text(&read(FUNC_IPC_SIGNAL));
    assert!(
        !sig.contains("impl SignalDecision for"),
        "ipc/signal.rs 不得再实现 SignalDecision (权威实现归 functions::proc::signal)"
    );
    assert!(
        !sig.contains("init_signal_decision"),
        "ipc/signal.rs 不得再提供信号决策注册入口"
    );

    let ipc_mod = code_text(&read(FUNC_IPC_MOD));
    assert!(
        !ipc_mod.contains("init_signal_decision"),
        "ipc::init_global 不得注册信号决策 (重复注册会 panic)"
    );

    let proc_mod = code_text(&read(FUNC_PROC_MOD));
    assert_contains(
        &proc_mod,
        "signal::register_standard_signal_policy()",
        "信号策略必须由 functions::proc::init 注册",
    );
}
