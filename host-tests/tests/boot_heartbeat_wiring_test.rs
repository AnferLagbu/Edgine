// SPDX-License-Identifier: MPL-2.0
//! 启动挂死区分读数接线锁 (任务 #130, 裁定⑩「单开一批只做区分」).
//!
//! 口径来源: `docs/plan/multithreading-project.md` §7 2026-11-05 裁定位清零轮
//! 裁定⑩. 本文件只锁源码形状, 不重跑内核路径 (挂死需真机启动现场才能复现):
//!
//! 1. **单一发射点**: `[HB]` 行只在 `heartbeat_emit` 内输出, 且该函数只被
//!    `on_timer_interrupt` 调用一次 —— 防止第二个手写打印点长出来.
//! 2. **顺序**: 心跳必须排在 `TICK_IRQ_COUNT` 递增之后 (读数是本 tick 的累计值),
//!    且在两架构共用的 tick 入口里, 不另开架构分支.
//! 3. **两架构都经共用入口**: x86_64 `timer_tick_handler` 与 aarch64 timer IRQ
//!    各恰好一处 `on_timer_interrupt()` 调用 (读数覆盖两架构).
//! 4. **低频 + 默认构建可见**: 间隔常量参与判据且不藏 `debug_assertions` /
//!    `feature = "lockdep"` 开关 —— 挂死发生在默认镜像上, 诊断必须常开.
//! 5. **零新增锁**: 发射体只允许 klog 自身的锁, 不得再取任何锁
//!    (`docs/explain/ref-lock-order.md`: hardirq 发射点不持其它全局锁).

use std::fs;
use std::path::PathBuf;

const TICK_SRC: &str = "src/kernel/privileged/timer/tick.rs";
const TIMER_IRQ_SRC: &str = "src/kernel/privileged/timer/irq.rs";
const A64_EXC_SRC: &str = "src/kernel/privileged/arch/aarch64/exception.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {e}", path.display()))
}

/// 去掉整行注释 (含 `///`), 避免文档里对 `[HB]` / 函数名的引用被当成接线。
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
fn heartbeat_has_a_single_emission_point_in_the_common_tick_entry() {
    let tick = code_only(&read_src(TICK_SRC));

    // 全树只有一个 `[HB]` 发射点, 且落在 heartbeat_emit 内。
    assert_eq!(
        tick.matches("[HB]").count(),
        1,
        "心跳发射点必须唯一 (第二个手写打印点即口径分裂)"
    );
    let emit = fn_body(&tick, "fn heartbeat_emit(total: u64)");
    assert!(
        has(&emit, "[HB] tick={} cpu={}"),
        "发射体必须输出 tick 与 cpu 两个字段"
    );
    assert!(
        has(&emit, "crate::privileged::smp::current_cpu_index()"),
        "cpu 必须取本核逻辑索引"
    );

    // 共用入口只调用一次, 且排在 TICK_IRQ_COUNT 递增之后。
    let entry = fn_body(&tick, "fn on_timer_interrupt()");
    assert_eq!(
        entry.matches("heartbeat_emit(total)").count(),
        1,
        "on_timer_interrupt 必须恰好一处心跳调用"
    );
    assert_eq!(
        entry.matches("TICK_IRQ_COUNT.fetch_add").count(),
        1,
        "tick 计数必须单点递增"
    );
    let count_at = entry.find("TICK_IRQ_COUNT.fetch_add").unwrap();
    let beat_at = entry.find("heartbeat_emit(total)").unwrap();
    assert!(
        count_at < beat_at,
        "心跳必须读本 tick 的累计值 (排在计数递增之后)"
    );
    assert!(
        has(&entry, "heartbeat_emit(total)"),
        "心跳接线参数必须是本 tick 的累计值"
    );

    // 判据本身走纯函数, 间隔常量参与取模。
    let due = fn_body(&tick, "fn heartbeat_due(total: u64)");
    assert!(
        has(&due, "HEARTBEAT_INTERVAL_TICKS"),
        "判据必须使用间隔常量"
    );
    assert!(has(&due, "total == 1"), "第 1 tick 必须发射 (接线证明)");
}

#[test]
fn heartbeat_is_default_on_and_takes_no_extra_locks() {
    let tick = code_only(&read_src(TICK_SRC));
    let emit = fn_body(&tick, "fn heartbeat_emit(total: u64)");

    // 挂死发生在默认镜像上: 读数不得藏在调试/feature 开关后面。
    for gate in ["debug_assertions", "feature = \"lockdep\"", "#[cfg("] {
        assert!(!emit.contains(gate), "心跳发射不得被条件编译藏起: {gate}");
    }

    // hardirq 发射点除 klog 自身外不得取锁 (否则把读数变成新的锁序风险)。
    for lock in [".lock()", ".lock_guard", "IrqSpinLock::new", "Mutex::new"] {
        assert!(!emit.contains(lock), "发射体不得新增取锁: {lock}");
    }

    // 证据走 klog, 与其余启动读数同一出口。
    assert!(
        has(&emit, "crate::klog_info!(Boot,"),
        "心跳必须经 klog Boot 分类输出"
    );
}

#[test]
fn both_architectures_reach_the_common_tick_entry_exactly_once() {
    let x86 = code_only(&read_src(TIMER_IRQ_SRC));
    assert_eq!(
        x86.matches("crate::privileged::timer::on_timer_interrupt();")
            .count(),
        1,
        "x86_64 timer_tick_handler 必须恰好一处进入共用 tick 入口"
    );

    let a64 = code_only(&read_src(A64_EXC_SRC));
    assert_eq!(
        a64.matches("crate::privileged::timer::on_timer_interrupt();")
            .count(),
        1,
        "aarch64 timer IRQ 必须恰好一处进入共用 tick 入口"
    );
}
