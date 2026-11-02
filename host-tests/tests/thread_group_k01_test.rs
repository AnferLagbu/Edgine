// SPDX-License-Identifier: MPL-2.0
//! K-01 线程组基础结构回归锁.
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-01 与 D7
//! (`Process` = 每线程 TCB, PID = TID; `ThreadGroup` 独立). 本文件锁定:
//!
//! - `Process::new` 自建单成员组, `tgid == pid`, 组长即首个线程;
//! - 组身份由 `Process::thread_group` (Arc) 承载, 不同 TCB 的组互不相同;
//! - 成员离开与组退出状态语义 (幂等、首个退出码生效);
//! - `getpid` 读 tgid、`gettid` 读 TID 的接线形状;
//! - 退出路径在进程表锁临界区内闭合成组 (层级 7 → 9).
//!
//! 多线程入组 (join) 与共享资源归 K-02/K-03, 本文件不越界锁定.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use edgine::kernel::privileged::proc::Process;

const INFO: &str = "src/kernel/functions/proc/info.rs";
const PROC_OPS: &str = "src/kernel/privileged/proc/proc_ops.rs";
const SCHED: &str = "src/kernel/privileged/proc/scheduler.rs";

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

/// 源码片段匹配统一忽略空白 (rustfmt 折行不影响契约)
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
    panic!("函数体未闭合: {sig}");
}

#[test]
fn process_new_creates_single_member_group() {
    let process = Process::new(0x4B01, "k01-single", None);
    assert_eq!(process.tgid(), 0x4B01, "单线程进程的 tgid 必须等于 pid");
    assert_eq!(process.group().tgid(), 0x4B01);
    assert_eq!(process.group().members(), vec![0x4B01]);
    assert_eq!(process.group().member_count(), 1);
    assert!(!process.group().has_exited(), "新建立线程组不得处于退出态");
}

#[test]
fn process_groups_are_private_per_tcb() {
    let a = Process::new(0x4B02, "k01-a", None);
    let b = Process::new(0x4B03, "k01-b", None);
    assert!(
        !Arc::ptr_eq(a.group(), b.group()),
        "每个新 TCB 必须自建独立线程组"
    );
    assert_eq!(a.group().members(), vec![0x4B02]);
    assert_eq!(b.group().members(), vec![0x4B03]);
    assert_ne!(a.tgid(), b.tgid());
}

#[test]
fn last_member_leaving_closes_group() {
    let process = Process::new(0x4B04, "k01-exit", None);
    let group = Arc::clone(process.group());

    assert!(group.leave(0x4B04), "移除唯一成员后组应为空");
    assert_eq!(group.member_count(), 0);
    assert!(
        !group.has_exited(),
        "成员离开只报空, 退出状态由退出路径显式置位"
    );

    group.mark_exited(7);
    assert!(group.has_exited());
    assert_eq!(group.exit_code(), 7);

    // 首个退出码生效: 后到者不覆盖
    group.mark_exited(9);
    assert_eq!(group.exit_code(), 7);

    // 重复离开幂等, 不得复活成员
    assert!(group.leave(0x4B04));
    assert_eq!(group.member_count(), 0);
}

#[test]
fn getpid_reads_tgid_and_gettid_reads_pid() {
    let src = code_only(&read_src(INFO));

    let getpid = fn_body(&src, "pub fn getpid_syscall");
    assert!(
        has(&getpid, "process_get_current_tgid()"),
        "K-01: getpid 必须返回 tgid"
    );
    assert!(
        !has(&getpid, "process_get_current_pid()"),
        "K-01: getpid 不得再直接返回 TID"
    );

    let gettid = fn_body(&src, "pub fn gettid_syscall");
    assert!(
        has(&gettid, "process_get_current_pid()"),
        "K-01: gettid 必须返回 TID"
    );
    assert!(
        !has(&gettid, "process_get_current_tgid()"),
        "K-01: gettid 不得返回 tgid"
    );
}

#[test]
fn current_tgid_accessor_reads_process_group() {
    let src = code_only(&read_src(PROC_OPS));
    let body = fn_body(&src, "pub fn process_get_current_tgid");
    assert!(
        has(&body, "Scheduler::current()"),
        "tgid 访问器必须从当前调度 TCB 出发"
    );
    assert!(
        has(
            &body,
            "PROCESS_TABLE.with_process(pid, super::process::Process::tgid)"
        ),
        "tgid 访问器必须在进程表锁内读取 Process::tgid"
    );
}

#[test]
fn exit_path_leaves_group_under_process_table_lock() {
    let src = code_only(&read_src(SCHED));
    let body = fn_body(&src, "pub fn exit(&self, exit_code: u32)");
    assert!(
        has(&body, "let last = group.leave(pid);"),
        "K-01: 退出路径必须移除线程组成员身份"
    );
    assert!(
        has(&body, "if last { group.mark_exited(code); }"),
        "K-01: 仅最后成员离开时才置组退出状态"
    );
    assert!(
        has(&body, "let (effective_code, last_member) = PROCESS_TABLE"),
        "K-01/K-04: 最后成员判定必须由退出路径持有"
    );
    // K-06: 组退出码以 `exit_group` / 致命信号的请求值为权威, 无请求时取本线程
    // 退出码; 每个 TCB 的 `exit_code` 记录同一有效值.
    assert!(
        has(
            &body,
            "let code = if group.exit_requested() {
                 group.requested_exit_code()
             } else {
                 exit_code
             };"
        ),
        "K-06: 退出路径必须按组请求码收口有效退出码"
    );
    // 成员账锁只能在 `with_process` 临界区内取 (层级 7 → 9);
    // 反向 (先成员账后进程表) 是 AB-BA.
    assert!(
        has(
            &body,
            "let (effective_code, last_member) = PROCESS_TABLE
                 .with_process(pid, |proc| {
                     let group = proc.group();
                     let code = if group.exit_requested() {
                         group.requested_exit_code()
                     } else {
                         exit_code
                     };
                     let last = group.leave(pid);
                     if last {
                         group.mark_exited(code);
                     }
                     (code, last)
                 })
                 .unwrap_or((exit_code, false));"
        ),
        "K-01/K-04: 成组必须闭合在同一次进程表锁临界区内 (层级 7 → 9)"
    );
}
