// SPDX-License-Identifier: MPL-2.0
//! L-2 重新投入取证接线锁 (任务 #129, 裁定「纯取证读数」).
//!
//! 口径来源: `docs/plan/multithreading-project.md` §7 #129 条目. 本文件只锁源码
//! 形状, 不重跑内核路径 (症状需真机启动日志才能复现):
//!
//! 1. **唯一发射点**: `[REDISPATCH]` 行只在 `Scheduler::redispatch_emit` 内输出,
//!    其余站点一律经该函数转出 —— 防止第二个手写打印点长出来.
//! 2. **站点普查**: 五个调度路径站点 (`schedule` / `unblock` / `requeue` / `exit` /
//!    `iret`) 与两个信号站点 (`signal-cont` / `signal-stop`) 各恰好一处, 且各自
//!    持有独立预算静态量 (共享预算会让一个站点吃掉另一个的读数名额).
//! 3. **语义未改**: 信号 `Cont`/`Stop` 的原始状态写入逐字保留, 且次序必须是
//!    「读判据 → 写状态 → 锁外发射」—— 取证不改变默认动作语义.
//! 4. **终止路径先后**: 两架构的 `redispatch_diag_iret()` 都必须在
//!    `scheduler_yield()` 之后 (先尝试切离, 未切离才是证据).
//! 5. **未装点处**: 持运行队列锁的 `place_locked` / `pick_cfs_task` 不得出现
//!    发射调用 (`docs/explain/ref-lock-order.md`: 发射点不持其它全局锁).

use std::fs;
use std::path::PathBuf;

const SCHED_SRC: &str = "src/kernel/privileged/proc/scheduler.rs";
const SIGNAL_SRC: &str = "src/kernel/privileged/proc/signal.rs";
const TABLE_SRC: &str = "src/kernel/privileged/idt/table.rs";
const A64_EXC_SRC: &str = "src/kernel/privileged/arch/aarch64/exception.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {e}", path.display()))
}

/// 去掉整行注释 (含 `///`), 避免文档里对 `[REDISPATCH]` 的引用被当成发射点。
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 源码片段匹配统一忽略空白 (rustfmt 折行不影响契约)。
fn has(haystack: &str, needle: &str) -> bool {
    let norm = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    norm(haystack).contains(&norm(needle))
}

/// 取函数体 (含签名行), 按花括号配对截取。
fn fn_body(src: &str, sig: &str) -> String {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("未找到函数签名: {sig}"));
    let chars: Vec<char> = src[start..].chars().collect();
    let mut depth = 0usize;
    let mut seen_open = false;
    for (idx, ch) in chars.iter().enumerate() {
        match ch {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    return chars[..=idx].iter().collect();
                }
            }
            _ => {}
        }
    }
    panic!("函数体未闭合: {sig}");
}

#[test]
fn redispatch_has_a_single_emission_point_and_every_site_is_wired() {
    let sched = code_only(&read_src(SCHED_SRC));

    assert_eq!(
        sched.matches("[REDISPATCH]").count(),
        1,
        "发射点必须唯一 (第二个手写打印点即口径分裂)"
    );

    for via in ["schedule", "unblock", "requeue", "exit", "iret"] {
        assert_eq!(
            sched.matches(&format!("\"{via}\"")).count(),
            1,
            "调度侧站点 via={via} 必须恰好一处"
        );
    }

    // 每个站点各持一份预算: 声明 + 调用各一次。
    for budget in [
        "RD_SCHEDULE",
        "RD_UNBLOCK",
        "RD_REQUEUE",
        "RD_EXIT",
        "RD_IRET",
    ] {
        assert_eq!(
            sched.matches(budget).count(),
            2,
            "站点预算 {budget} 必须独立 (声明 + 唯一调用)"
        );
    }

    let signal = code_only(&read_src(SIGNAL_SRC));
    assert_eq!(
        signal.matches("\"signal-cont\"").count(),
        1,
        "信号 Cont 站点必须恰好一处"
    );
    assert_eq!(
        signal.matches("\"signal-stop\"").count(),
        1,
        "信号 Stop 站点必须恰好一处"
    );
}

#[test]
fn signal_raw_state_stores_are_unchanged_and_ordered_before_emission() {
    let signal = code_only(&read_src(SIGNAL_SRC));

    assert_eq!(
        signal
            .matches(".store(ProcessState::Ready as u32, Ordering::Release)")
            .count(),
        1,
        "§5 语义未改: Cont 绕过状态机的原始写入必须原样保留"
    );
    assert_eq!(
        signal
            .matches(".store(ProcessState::Blocked as u32, Ordering::Release)")
            .count(),
        1,
        "§5 语义未改: Stop 绕过状态机的原始写入必须原样保留"
    );

    // 次序契约: 判据快照 → 原始写入 → 锁外发射 (信号路径不持全局锁)。
    assert!(
        has(
            &signal,
            "let state = proc.get_state();
             let tgid = proc.group().tgid();
             let group_exited = proc.group().has_exited();
             proc.state
                 .store(ProcessState::Ready as u32, Ordering::Release);
             super::scheduler::Scheduler::redispatch_emit(
                 &RD_SIGNAL_CONT,
                 \"signal-cont\",
                 pid,
                 tgid,
                 state,
                 group_exited,
             );"
        ),
        "Cont 站点必须「先取判据 → 再写状态 → 最后发射」"
    );
    assert!(
        has(
            &signal,
            "let state = proc.get_state();
             let tgid = proc.group().tgid();
             let group_exited = proc.group().has_exited();
             proc.state
                 .store(ProcessState::Blocked as u32, Ordering::Release);
             super::scheduler::Scheduler::redispatch_emit(
                 &RD_SIGNAL_STOP,
                 \"signal-stop\",
                 pid,
                 tgid,
                 state,
                 group_exited,
             );"
        ),
        "Stop 站点必须「先取判据 → 再写状态 → 最后发射」"
    );
}

#[test]
fn terminate_paths_probe_only_after_the_switch_attempt() {
    for (label, rel) in [("x86_64", TABLE_SRC), ("aarch64", A64_EXC_SRC)] {
        let src = code_only(&read_src(rel));
        let yield_at = src
            .rfind("scheduler_yield();")
            .unwrap_or_else(|| panic!("{label}: 终止路径缺少 scheduler_yield"));
        let probe_at = src
            .find("redispatch_diag_iret()")
            .unwrap_or_else(|| panic!("{label}: 终止路径缺少 redispatch_diag_iret"));
        assert!(
            probe_at > yield_at,
            "{label} 终止路径: 探测必须在 scheduler_yield 之后 (未切离才是 L-2 证据)"
        );
    }
}

#[test]
fn rq_lock_holders_stay_uninstrumented() {
    let sched = code_only(&read_src(SCHED_SRC));

    // 两处都在持有 cfs_rq 锁期间取进程表快照; 在此打印会违反
    // 「发射点不持其它全局锁」并把串口窗口撑进层级 8 临界区。
    for sig in ["fn place_locked(", "fn pick_cfs_task("] {
        let body = fn_body(&sched, sig);
        assert!(
            !body.contains("redispatch_emit") && !body.contains("redispatch_probe"),
            "{sig} 持运行队列锁, 不得就地发射 [REDISPATCH] (锁序口径)"
        );
    }

    // 其效应由 `via=schedule` 的 `gexit=1` 读数兜住 (文档已写明)。
    let body = fn_body(&sched, "pub fn schedule()");
    assert!(
        has(
            &body,
            "redispatch_emit(&RD_SCHEDULE, \"schedule\", next, tgid, state, group_exited);"
        ),
        "schedule() 必须在切上核前记录被判中任务的判据"
    );
}
