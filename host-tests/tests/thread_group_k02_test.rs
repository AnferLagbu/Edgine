// SPDX-License-Identifier: MPL-2.0
//! K-02 `CLONE_THREAD` 入组接线回归锁.
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-02 与 D7.
//! 本文件锁定:
//!
//! - `CLONE_THREAD` 必须同时置 `CLONE_VM` (否则非法组合, 不静默走 fork);
//! - 线程 TCB 不进入 `parent.children`, 父字段不指向创建者;
//! - 线程在登记进程表前加入父线程组 (`getpid` 首次上核即读到 tgid);
//! - 进程表登记失败时回滚组成员身份与 PCB;
//! - `Process::join_thread_group` 共享组身份并登记成员.
//!
//! 线程退出语义 (exit / `exit_group` 分离) 归 K-06, 本文件不越界锁定.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use edgine::kernel::privileged::proc::Process;

const CLONE_SRC: &str = "src/kernel/functions/proc/clone.rs";

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

fn index_of(haystack: &str, needle: &str) -> usize {
    let norm = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    norm(haystack)
        .find(&norm(needle))
        .unwrap_or_else(|| panic!("未找到片段: {needle}"))
}

#[test]
fn clone_thread_requires_shared_vm() {
    let src = code_only(&read_src(CLONE_SRC));
    let body = fn_body(&src, "pub fn clone_syscall");
    assert!(
        has(
            &body,
            "if (flags & CLONE_THREAD) != 0 && (flags & CLONE_VM) == 0 {
                 return Err(Errno::Einval);
             }"
        ),
        "K-02: CLONE_THREAD 缺 CLONE_VM 必须是非法组合"
    );
}

#[test]
fn clone_vm_path_splits_thread_from_shared_vm_process() {
    let src = code_only(&read_src(CLONE_SRC));
    let body = fn_body(&src, "fn clone_vm_path");
    assert!(
        has(&body, "let joins_group = flags & CLONE_THREAD != 0;"),
        "K-02: 入组判定必须来自 CLONE_THREAD 标志"
    );
    for callee in [
        "vm_child_alloc(parent_pid, child_pid, parent_cr3, joins_group)",
        "vm_child_inherit(parent_pid, &mut *child, parent_cr3, child_stack, tls)",
        "vm_child_register(",
    ] {
        assert!(has(&body, callee), "K-02: clone_vm_path 必须接线 {callee}");
    }
    assert!(
        has(&body, "if let Err(ret) = vm_child_register("),
        "K-02: 登记失败必须回传错误而非当作成功"
    );
}

#[test]
fn thread_tcb_has_no_parent_record_and_skips_children() {
    let src = code_only(&read_src(CLONE_SRC));

    let alloc = fn_body(&src, "fn vm_child_alloc");
    assert!(
        has(
            &alloc,
            "let parent_record = if joins_group {
                 None
             } else {
                 Some(crate::privileged::proc::ProcessId(parent_pid))
             };"
        ),
        "K-02: 线程 TCB 不得登记创建者为其父"
    );

    let register = fn_body(&src, "fn vm_child_register");
    assert!(
        has(
            &register,
            "if !joins_group {
                 api::process_with_mut(parent_pid, |p| {
                     p.children
                         .lock()
                         .push(crate::privileged::proc::ProcessId(child_pid));
                 });
             }"
        ),
        "K-02: 只有独立进程才进入 parent.children"
    );
    assert_eq!(
        register.matches("children").count(),
        1,
        "K-02: 线程路径不得在守卫之外再写 children"
    );

    let inherit = fn_body(&src, "fn vm_child_inherit");
    assert!(
        !inherit.contains("children"),
        "K-02: 继承阶段不得写 children (登记成功后才挂父账)"
    );
}

#[test]
fn thread_registration_joins_group_before_insert_and_rolls_back() {
    let src = code_only(&read_src(CLONE_SRC));
    let body = fn_body(&src, "fn vm_child_register");

    let join_at = index_of(&body, "child.join_thread_group(group);");
    let insert_at = index_of(&body, "api::process_insert(");
    assert!(
        join_at < insert_at,
        "K-02: 入组必须先于进程表登记 (getpid 首次运行即读对 tgid)"
    );

    assert!(
        has(
            &body,
            "let Some(group) = api::process_with(parent_pid, |p| alloc::sync::Arc::clone(p.group())) else {"
        ),
        "K-02: 入组必须取自父线程的组 Arc"
    );
    assert!(
        has(&body, "let _ = child.thread_group.leave(child_pid);"),
        "K-02: 登记失败必须撤回组成员身份"
    );
    assert!(
        has(&body, "raw::drop_boxed_process("),
        "K-02: 登记失败必须释放半成品 PCB"
    );
    assert!(
        has(&body, "Err(Errno::Esrch.as_ret())") && has(&body, "Err(Errno::Enomem.as_ret())"),
        "K-02: 父组缺失 / 登记失败必须回传错误"
    );
    assert!(
        body.trim_end().ends_with("Ok(()) }") || has(&body, "Ok(())"),
        "K-02: 登记成功必须返回 Ok"
    );
}

#[test]
fn join_thread_group_shares_identity_and_registers_member() {
    let leader = Process::new(0x4C01, "k02-leader", None);
    let mut thread = Process::new(0x4C02, "k02-thread", None);

    let group = Arc::clone(leader.group());
    assert!(!Arc::ptr_eq(thread.group(), &group), "入组前线程自建独立组");

    thread.join_thread_group(Arc::clone(&group));

    assert_eq!(thread.tgid(), 0x4C01, "线程 tgid 必须取目标组组长");
    assert!(Arc::ptr_eq(thread.group(), &group), "线程必须共享组 Arc");
    assert_eq!(group.members(), vec![0x4C01, 0x4C02]);
    assert_eq!(group.member_count(), 2);
    assert_eq!(leader.group().member_count(), 2);
    assert!(!group.has_exited(), "入组不得改变组退出状态");
}
