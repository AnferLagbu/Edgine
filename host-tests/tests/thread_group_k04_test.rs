// SPDX-License-Identifier: MPL-2.0
//! K-04 调度语义收口回归锁 (D1 + D7).
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-04 与 D5/D7.
//! 本文件锁定:
//!
//! - 进程级身份 (父组 / 子组账 / session / pgid) 归 `ThreadGroup`,
//!   `Process` 不再持有同名的进程级字段 (每线程 TCB 只承载调度实体);
//! - fork / clone / wait4 / getppid / 信号投递 / 会话 syscall 全部读组身份;
//! - 线程不进入父组 `children`, `wait4` 只见线程组;
//! - 退出拆卸 (会话释放 / 计数 / cgroup / 父唤醒 / 子组改挂) 仅由组最后成员触发,
//!   每线程只置自己的 Zombie 与退出码.
//!
//! 线程退出/`exit_group` 分离、组定向信号投递与 exec `de_thread` 归 K-06,
//! 本文件不越界锁定.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use edgine::kernel::privileged::proc::{Process, ProcessId, ThreadGroup};

const PROCESS_SRC: &str = "src/kernel/privileged/proc/process.rs";
const THREAD_GROUP_SRC: &str = "src/kernel/privileged/proc/thread_group.rs";
const PROC_OPS_SRC: &str = "src/kernel/privileged/proc/proc_ops.rs";
const CLONE_SRC: &str = "src/kernel/functions/proc/clone.rs";
const WAIT4_SRC: &str = "src/kernel/functions/proc/wait4.rs";
const SCHED_SRC: &str = "src/kernel/privileged/proc/scheduler.rs";
const SESSION_SRC: &str = "src/kernel/privileged/proc/session.rs";
const FS_SESSION_SRC: &str = "src/kernel/functions/proc/session.rs";
const SIGNAL_SRC: &str = "src/kernel/privileged/proc/signal.rs";
const COREDUMP_SRC: &str = "src/kernel/functions/proc/coredump.rs";
const PROCFS_SRC: &str = "src/kernel/functions/fs/procfs_core.rs";

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
fn process_identity_fields_live_on_thread_group() {
    let process = code_only(&read_src(PROCESS_SRC));
    for field in [
        "pub parent: Option<ProcessId>,",
        "pub children: Mutex<Vec<ProcessId>>,",
        "pub session_id: AtomicU64,",
        "pub pgid: AtomicU32,",
    ] {
        assert!(
            !has(&process, field),
            "K-04: 进程级身份不得再按 TCB 分裂 (Process 仍声明 {field})"
        );
    }
    assert!(
        has(&process, "pub thread_group: Arc<ThreadGroup>,"),
        "K-04: 组身份必须唯一由 ThreadGroup 承载"
    );

    let group = code_only(&read_src(THREAD_GROUP_SRC));
    for field in [
        "parent: AtomicU32,",
        "children: Mutex<Vec<Pid>>,",
        "session_id: AtomicU64,",
        "pgid: AtomicU32,",
    ] {
        assert!(
            has(&group, field),
            "K-04: ThreadGroup 缺少进程级身份字段 {field}"
        );
    }
}

#[test]
fn process_new_seeds_group_parent_from_argument() {
    let src = code_only(&read_src(PROCESS_SRC));
    let body = fn_body(
        &src,
        "pub fn new(pid: Pid, name: &str, parent: Option<ProcessId>)",
    );
    assert!(
        has(&body, "let group = ThreadGroup::new_leader(pid);"),
        "K-04: 新 TCB 必须自建单成员组"
    );
    assert!(
        has(&body, "group.set_parent(parent.0);"),
        "K-04: 父进程组必须在组构造时落账"
    );

    let process = Process::new(0x4D10, "k04-parent", Some(ProcessId(0x4D01)));
    assert_eq!(
        process.group().parent(),
        0x4D01,
        "getppid 语义: 父组 TGID 必须来自构造参数"
    );
    assert_eq!(process.tgid(), 0x4D10);
    assert_eq!(process.group().members(), vec![0x4D10]);
}

#[test]
fn fork_records_parent_group_and_child_group() {
    let src = code_only(&read_src(PROC_OPS_SRC));
    let fork = fn_body(&src, "pub extern \"C\" fn sys_fork");

    assert!(
        has(
            &fork,
            "let parent_tgid = PROCESS_TABLE
                 .with_process(parent_pid, Process::tgid)
                 .unwrap_or(parent_pid);"
        ),
        "K-04: fork 的子进程父字段必须取父线程组 tgid"
    );
    assert!(
        has(
            &fork,
            "let child_ptr = raw::alloc_process(child_pid, \"\", Some(ProcessId(parent_tgid)));"
        ),
        "K-04: 子进程组挂到父组 tgid, 不挂到父 TID"
    );

    let insert_at = index_of(&fork, "if !PROCESS_TABLE.insert(");
    let add_at = index_of(&fork, "p.group().add_child(child_pid);");
    assert!(
        insert_at < add_at,
        "K-04: 只有进程表登记成功才把子组挂入父组 children"
    );

    let inherit = fn_body(&src, "fn inherit_parent_state");
    assert!(
        has(
            &inherit,
            "let Some((sid, pgid, canary)) = PROCESS_TABLE.with_process(parent_pid, |p| {
                 (
                     p.group().session_id(),
                     p.group().pgid(),
                     p.stack_canary.load(Ordering::SeqCst),
                 )
             })"
        ),
        "K-04: fork 继承 session/pgid 必须读父线程组"
    );
    assert!(
        has(&inherit, "child.group().set_session_id(sid);")
            && has(&inherit, "child.group().set_pgid(pgid);"),
        "K-04: fork 子组必须从父组复制会话与进程组"
    );

    let ppid = fn_body(&src, "pub extern \"C\" fn proc_get_ppid");
    assert!(
        has(
            &ppid,
            "PROCESS_TABLE
                 .with_process(pid, |p| p.group().parent())
                 .unwrap_or(0)"
        ),
        "K-04: getppid 必须读线程组父字段"
    );
}

#[test]
fn clone_registers_only_process_children_on_parent_group() {
    let src = code_only(&read_src(CLONE_SRC));

    let alloc = fn_body(&src, "fn vm_child_alloc");
    assert!(
        has(
            &alloc,
            "let parent_record = if joins_group {
                 None
             } else {
                 Some(crate::privileged::proc::ProcessId(
                     api::process_with(parent_pid, Process::tgid).unwrap_or(parent_pid),
                 ))
             };"
        ),
        "K-04: 线程无父记录; 独立共享 VM 进程挂父组 tgid"
    );

    let register = fn_body(&src, "fn vm_child_register");
    assert!(
        has(
            &register,
            "if !joins_group {
                 api::process_with(parent_pid, |p| {
                     p.group().add_child(child_pid);
                 });
             }"
        ),
        "K-04: 只有独立进程入父组 children, 线程不得成为可 wait 子进程"
    );
    let insert_at = index_of(&register, "if !api::process_insert(");
    let add_at = index_of(&register, "p.group().add_child(child_pid);");
    assert!(insert_at < add_at, "K-04: 子组入账必须在进程表登记成功之后");
}

#[test]
fn wait4_reads_group_children_and_group_pgid() {
    let src = code_only(&read_src(WAIT4_SRC));
    let find = fn_body(&src, "fn find_waitable_child");

    assert!(
        has(
            &find,
            "let (children, caller_pgid) =
                 api::process_with(parent_pid, |p| (p.group().children(), p.group().pgid()))
                     .unwrap_or_default();"
        ),
        "K-04: wait4 的子进程账与调用方进程组必须来自线程组"
    );
    assert!(
        has(
            &find,
            "let pgid = api::process_with(child_tgid, |p| p.group().pgid()).unwrap_or(0);"
        ),
        "K-04: wait4 的进程组匹配必须读子线程组"
    );
}

#[test]
fn exit_teardown_is_gated_on_last_group_member() {
    let src = code_only(&read_src(SCHED_SRC));
    let body = fn_body(&src, "pub fn exit(&self, exit_code: u32)");

    assert!(
        has(
            &body,
            "if last_member { crate::privileged::proc::session_leader_exit(pid); }"
        ),
        "K-04: 会话释放只由组最后成员触发"
    );
    assert!(
        has(&body, "if last_member { self.dec_limit(pwm); }"),
        "K-04: 进程级计数只在组最后成员退出时撤销"
    );
    let dec_at = index_of(&body, "if last_member { self.dec_limit(pwm); }");
    let cg_at = index_of(&body, "cg.detach_proc(pid);");
    assert!(
        dec_at < cg_at,
        "K-04: cgroup 成员按 TCB 各自摘除, 不得被收进 last_member 计数块"
    );
    assert_eq!(
        body.matches("if last_member {").count(),
        3,
        "K-04: 会话 / 计数 / 子组改挂三处组级拆卸都必须受 last_member 门控"
    );
    assert!(
        has(
            &body,
            "if last_member && parent_tgid != 0 { Self::wake_child_waiters(parent_tgid); }"
        ),
        "K-04 / P-78: 只在线程组整体退出后唤醒父组 (经助手覆盖组内全部等待线程)"
    );
    assert!(
        has(
            &body,
            "let children: alloc::vec::Vec<Pid> = PROCESS_TABLE
                 .with_process(pid, |proc| proc.group().children())
                 .unwrap_or_default();"
        ),
        "K-04: 退出时改挂的是子进程组账"
    );
    assert!(
        has(&body, "if !PROCESS_TABLE.reap_zombie_group(child_tgid) {")
            && has(&body, "child.group().set_parent(1)")
            && has(&body, "proc.group().clear_children()"),
        "K-04/K-06: 子组已整体退出则整组回收, 否则改挂 pid 1; 全部处理后清空组子账"
    );

    assert!(
        has(
            &body,
            "let parent_tgid = PROCESS_TABLE
                 .with_process(pid, |proc| {"
        ) && has(&body, "proc.group().parent()"),
        "K-04: 父组唤醒目标必须取线程组父字段"
    );
    assert!(
        has(
            &body,
            "proc.exit_code.store(effective_code, Ordering::SeqCst);"
        ) && has(&body, "proc.mark_zombie();"),
        "K-06: 每个 TCB 记录组有效退出码, 并各自置 Zombie (R-4: 经不可拒绝终态写点)"
    );
    assert!(
        has(&body, "itimer_real_release_pid(pid);") && has(&body, "posix_timer_release_pid(pid);"),
        "K-04: 定时器撤防仍按 TID (组定向投递归 K-06)"
    );
}

#[test]
fn zombie_reaper_reads_group_parent() {
    let src = code_only(&read_src(SCHED_SRC));
    let body = fn_body(&src, "fn tick_reap_zombies(new_tick: u64)");
    assert!(
        has(
            &body,
            "(p.get_state() == ProcessState::Zombie)
                 .then(|| (p.thread_group.tgid(), p.group().parent()))"
        ),
        "K-04: 周期僵尸回收的父存活判定必须读线程组"
    );
}

#[test]
fn signal_broadcasts_read_group_pgid() {
    let signal = code_only(&read_src(SIGNAL_SRC));
    assert!(
        has(
            &signal,
            "let current_pgid = PROCESS_TABLE
                 .with_process(current, |p| p.group().pgid())
                 .unwrap_or(0);"
        ),
        "K-04: 组广播的当前 pgid 必须读线程组"
    );
    assert_eq!(
        signal
            .matches("collect_matching_tgids(|proc| proc.group().pgid() == target_pgid)")
            .count(),
        2,
        "K-04: kill(0) / kill(-pgid) 的进程组匹配必须读线程组"
    );
    assert_eq!(
        signal
            .matches("let effective_pgid = proc.group().pgid();")
            .count(),
        1,
        "K-04: sig=0 存在性探测的进程组匹配必须读线程组"
    );
}

#[test]
fn session_syscalls_read_group_identity() {
    let session = code_only(&read_src(SESSION_SRC));
    let setsid = fn_body(&session, "pub fn proc_setsid");
    assert!(
        has(
            &setsid,
            "let Some((tgid, effective_pgid)) = PROCESS_TABLE
                 .with_process(pid, |p| (p.tgid(), p.group().pgid()))"
        ),
        "K-04: setsid 的组长判定必须读线程组"
    );
    assert!(
        has(&setsid, "if effective_pgid == tgid {")
            && has(&setsid, "p.group().set_session_id(sid);")
            && has(&setsid, "p.group().set_pgid(tgid);"),
        "K-04: setsid 写线程组会话与进程组"
    );

    let getsid = fn_body(&session, "pub fn proc_getsid(pid: i32)");
    assert!(
        has(&getsid, "let sid = p.group().session_id();") && has(&getsid, "i64::from(p.tgid())"),
        "K-04: getsid 读线程组会话, 未入会话回落 tgid"
    );

    let setpgid = fn_body(&session, "pub fn proc_setpgid(pid: i32, pgid_arg: i32)");
    assert!(
        has(&setpgid, "p.group().parent() == current_tgid,"),
        "K-04: setpgid 的子进程判定必须读线程组父字段"
    );
    assert!(
        has(
            &setpgid,
            "if proc.group().pgid() == new_pgid && proc.group().session_id() == sid_current {"
        ),
        "K-04: setpgid 的目标组归属扫描必须读线程组"
    );

    let getpgid = fn_body(&session, "pub fn proc_getpgid(pid: i32)");
    assert!(
        has(&getpgid, "i64::from(proc.group().pgid())"),
        "K-04: getpgid 必须读线程组"
    );

    assert!(
        !session.contains("fn proc_init_pgid"),
        "K-04: 独立 pgid 初始化必须退役 (组构造已落 pgid)"
    );

    let tiocsctty = fn_body(&session, "pub fn sys_tiocsctty(fd: i32)");
    assert!(
        has(
            &tiocsctty,
            "let (tgid, sid) = PROCESS_TABLE
                 .with_process(pid, |p| (p.tgid(), p.group().session_id()))"
        ) && has(&tiocsctty, "sid != u64::from(tgid)"),
        "K-04: 控制终端只认会话 leader (SID == tgid)"
    );

    let leader_exit = fn_body(&session, "pub fn session_leader_exit(pid: u32)");
    assert!(
        has(
            &leader_exit,
            "let Some((tgid, sid)) = PROCESS_TABLE
                 .with_process(pid, |p| (p.tgid(), p.group().session_id()))"
        ) && has(&leader_exit, "sid != u64::from(tgid)"),
        "K-04: 会话 leader 退出判定必须读线程组"
    );

    let tcsetpgrp = fn_body(
        &code_only(&read_src(FS_SESSION_SRC)),
        "pub fn tcsetpgrp_syscall(_fd: i32, pgid: i32)",
    );
    assert!(
        has(
            &tcsetpgrp,
            "process_with(proc_id, |p| p.group().session_id())"
        ) && has(
            &tcsetpgrp,
            "proc.group().pgid() == pgid.cast_unsigned()
                     && proc.group().session_id() == sid"
        ),
        "K-04: tcsetpgrp 的会话与进程组校验必须读线程组"
    );
}

#[test]
fn dump_paths_read_group_parent() {
    let coredump = code_only(&read_src(COREDUMP_SRC));
    assert!(
        has(
            &coredump,
            "prstatus.pr_ppid = p.group().parent().cast_signed();"
        ) && has(
            &coredump,
            "prstatus.pr_pgrp = p.group().pgid().cast_signed();"
        ) && has(
            &coredump,
            "prstatus.pr_sid =
                     crate::functions::intcast::low_i32_from_u64(p.group().session_id());"
        ),
        "K-04: coredump 的 ppid/pgrp/sid 必须读线程组"
    );

    let procfs = code_only(&read_src(PROCFS_SRC));
    assert_eq!(
        procfs
            .matches("let parent_pid = proc.group().parent();")
            .count(),
        2,
        "K-04: procfs 的 PPid 输出两处都必须读线程组"
    );
}

#[test]
fn joined_thread_shares_group_identity() {
    let group = Arc::new(ThreadGroup::new_leader(0x4D20));
    group.set_parent(0x4D02);
    group.add_child(0x4D21);
    group.set_session_id(0x4D20);
    group.set_pgid(0x4D22);

    let mut thread = Process::new(0x4D23, "k04-thread", None);
    assert!(
        thread.join_thread_group(Arc::clone(&group)),
        "K-06: 非 exec de_thread 状态下入组必须成功"
    );

    assert_eq!(thread.tgid(), 0x4D20, "入组后 getpid 必须读组长 tgid");
    assert!(
        Arc::ptr_eq(thread.group(), &group),
        "组内所有 TCB 必须共享同一组实例"
    );
    assert_eq!(thread.group().parent(), 0x4D02);
    assert_eq!(thread.group().children(), vec![0x4D21]);
    assert_eq!(thread.group().session_id(), 0x4D20);
    assert_eq!(thread.group().pgid(), 0x4D22);
    assert_eq!(group.members(), vec![0x4D20, 0x4D23]);

    // 组内任一线程的改动对全组可见
    thread.group().set_pgid(0x4D24);
    assert_eq!(group.pgid(), 0x4D24);
}

#[test]
fn new_group_defaults_to_own_process_group() {
    let group = ThreadGroup::new_leader(0x4D30);
    assert_eq!(group.parent(), 0, "新建组无父进程");
    assert!(group.children().is_empty(), "新建组无子组账");
    assert_eq!(group.session_id(), 0, "新建组未加入会话");
    assert_eq!(group.pgid(), 0x4D30, "POSIX: 新进程自成一组");

    group.set_pgid(0);
    assert_eq!(group.pgid(), 0x4D30, "pgid=0 必须回落 tgid");

    group.set_pgid(0x4D31);
    assert_eq!(group.pgid(), 0x4D31);
    assert_eq!(group.tgid(), 0x4D30, "迁移进程组不得改变组身份");
}
