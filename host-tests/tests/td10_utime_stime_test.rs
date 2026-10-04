// SPDX-License-Identifier: MPL-2.0
// TD-10: utime/stime 区分测试.
//
// 验收:
//   - user_time / sys_time 字段存在并独立
//   - proc_set_in_kern(0) → tick 累加 user_time
//   - proc_set_in_kern(1) → tick 累加 sys_time
//   - proc_account_tick(in_kern) 真实作用到 PROCESS_TABLE
//   - syscall_dispatch 入口 set 1, 出口 set 0

use std::fs;

const PROC_API: &str = "../src/kernel/privileged/proc/proc_ops.rs";
const SCHED: &str = "../src/kernel/privileged/proc/scheduler_ex.rs";
const SYSCALL_MOD: &str = "../src/kernel/privileged/syscall/dispatch.rs";
const PROC_STRUCT: &str = "../src/kernel/privileged/proc/process.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

#[test]
fn test_user_time_and_sys_time_fields_exist() {
    let src = read(PROC_STRUCT);
    assert!(
        src.contains("pub user_time: AtomicU64"),
        "Process 必须有 user_time 字段"
    );
    assert!(
        src.contains("pub sys_time: AtomicU64"),
        "Process 必须有 sys_time 字段"
    );
}

#[test]
fn test_proc_set_and_get_in_kern() {
    let src = read(PROC_API);
    assert!(
        src.contains("fn proc_set_in_kern(v: u32)"),
        "必须有 proc_set_in_kern 入口"
    );
    assert!(
        src.contains("fn proc_get_in_kern() -> u32"),
        "必须有 proc_get_in_kern 读取"
    );
    assert!(
        src.contains("CURRENT_IN_KERN: AtomicU64")
            || src.contains("CURRENT_IN_KERN: core::sync::atomic::AtomicU64"),
        "必须有 CURRENT_IN_KERN 状态"
    );
}

#[test]
fn test_proc_account_tick_uses_in_kern() {
    let src = read(PROC_API);
    // proc_account_tick 必须按 in_kern 分支累加 user_time / sys_time
    let body_start = src.find("pub fn proc_account_tick").expect("必须存在");
    let body_end_rel = src[body_start..].find("\n}\n").unwrap_or(usize::MAX);
    let body = &src[body_start..body_start + body_end_rel];
    assert!(
        body.contains("sys_time") && body.contains("user_time"),
        "proc_account_tick 必须同时更新 sys_time / user_time"
    );
    assert!(body.contains("if in_kern"), "必须有 if in_kern 分支");
}

#[test]
fn test_tick_accounting_calls_proc_account_tick() {
    let src = read(SCHED);
    let body_start = src.find("pub fn tick_accounting").expect("必须存在");
    let body_end_rel = src[body_start..].find("\n    }\n").unwrap_or(usize::MAX);
    let body = &src[body_start..body_start + body_end_rel];
    assert!(
        body.contains("proc_account_tick"),
        "tick_accounting 必须调用 proc_account_tick"
    );
    assert!(
        body.contains("proc_get_in_kern"),
        "tick_accounting 必须读取 in_kern 状态"
    );
}

#[test]
fn test_syscall_dispatch_wraps_in_kern() {
    let src = read(SYSCALL_MOD);
    // 精确匹配 fn syscall_dispatch( 而不是 _from_frame
    let body_start = src
        .find("pub unsafe extern \"C\" fn syscall_dispatch(")
        .expect("dispatch 必须存在");
    let body_end_rel = src[body_start..].find("\n}\n").unwrap_or(usize::MAX);
    let body = &src[body_start..body_start + body_end_rel];
    assert!(
        body.contains("proc_set_in_kern(1)"),
        "dispatch 入口必须 set 1"
    );
    assert!(
        body.contains("proc_set_in_kern(0)"),
        "dispatch 出口必须 set 0"
    );
    // 拆分为 syscall_dispatch_impl 包装
    assert!(
        src.contains("fn syscall_dispatch_impl"),
        "必须抽出实现为 syscall_dispatch_impl"
    );
}

#[test]
fn test_in_kern_toggle_round_trip() {
    // 静态验证: set 1 → get 1, set 0 → get 0 (基于源码)
    let src = read(PROC_API);
    // set 走 store (允许 v as u64 或 u64::from(v), 后者符合 §5.2 算术规范)
    assert!(
        src.contains("CURRENT_IN_KERN.store(v as u64, Ordering::SeqCst)")
            || src.contains("CURRENT_IN_KERN.store(u64::from(v), Ordering::SeqCst)"),
        "set 必须走 CURRENT_IN_KERN.store(_, SeqCst)"
    );
    assert!(
        src.contains("CURRENT_IN_KERN.load(Ordering::SeqCst) as u32"),
        "get 走 load"
    );
}

#[test]
fn test_proc_get_times_reads_user_and_sys() {
    let src = read(PROC_API);
    // proc_get_times 必须返回 (user_time, sys_time) 二元组
    assert!(src.contains("fn proc_get_times"), "必须有 proc_get_times");
    let body_start = src.find("pub fn proc_get_times").expect("必须存在");
    let body_end_rel = src[body_start..].find("\n}\n").unwrap_or(usize::MAX);
    let body = &src[body_start..body_start + body_end_rel];
    assert!(
        body.contains("user_time.load"),
        "get_times 必须读 user_time"
    );
    assert!(body.contains("sys_time.load"), "get_times 必须读 sys_time");
}
