// SPDX-License-Identifier: MPL-2.0
//! K-03a 组共享资源回归锁 (`CLONE_FILES` / `CLONE_SIGHAND` / `CLONE_FS`).
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-03a 与 D7.
//! 本文件锁定:
//!
//! - clone 置位时共享同一 `Arc`, 未置位时 fork 语义深拷贝;
//! - fd 表按显式 `users` 计数, 只有最后持有者退出才关闭句柄;
//! - fork 复制 fd 表时在源表锁内递增 `OpenFile` 引用, 失败路径撤销;
//! - exec 换新信号动作表后再清零;
//! - 路径解析 / chroot / umask 走进程 FS 上下文, 全局量仅作无进程缺省.
//! - K-07a 起 `CLONE_*` 位值单源在 `edgine-abi::thread`, clone.rs 不得再本地定义.
//!
//! 线程退出语义 (`exit` / `exit_group`) 与 exec 的 `de_thread` 归 K-06,
//! 本文件不越界锁定; 多线程 exec 自 K-06 起由 `de_thread` 接管, 不再拒绝.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use edgine::kernel::privileged::proc::{DEFAULT_UMASK, Process};

const CLONE_SRC: &str = "src/kernel/functions/proc/clone.rs";
const FD_TABLE_SRC: &str = "src/kernel/privileged/proc/fd_table.rs";
const PROCESS_SRC: &str = "src/kernel/privileged/proc/process.rs";
const PROC_OPS_SRC: &str = "src/kernel/privileged/proc/proc_ops.rs";
const EXEC_SRC: &str = "src/kernel/functions/proc/exec.rs";
const VFS_MANAGER_SRC: &str = "src/kernel/functions/fs/vfs_manager.rs";
const ABI_THREAD_SRC: &str = "src/rust/edgine-abi/src/thread.rs";

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
fn clone_files_requires_thread_and_fs_requires_shared_vm() {
    let src = code_only(&read_src(CLONE_SRC));
    // K-07a: CLONE_* 位值不再在 clone.rs 内定义, 改由 ABI 单源持有, clone 经
    // `functions::syscall::types` 出口消费. 两侧都锁: 单源里有正确数值,
    // clone.rs 里不得再出现本地定义 (防止漂移回双写).
    let abi = code_only(&read_src(ABI_THREAD_SRC));
    assert!(
        has(&abi, "pub const CLONE_FS: u64 = 0x0000_0200;"),
        "K-03a: CLONE_FS 位值必须由 edgine-abi::thread 单源持有"
    );
    assert!(
        has(&abi, "pub const CLONE_FILES: u64 = 0x0000_0400;"),
        "K-03a: CLONE_FILES 位值必须由 edgine-abi::thread 单源持有"
    );
    assert!(
        !src.contains("const CLONE_FS"),
        "K-07a: clone.rs 不得再本地定义 CLONE_FS"
    );
    assert!(
        src.contains("use crate::functions::syscall::types::"),
        "K-07a: clone.rs 的 CLONE_* 一律经 syscall::types 出口取用"
    );

    let body = fn_body(&src, "pub fn clone_syscall");
    assert!(
        has(
            &body,
            "if (flags & CLONE_FILES) != 0 && (flags & CLONE_THREAD) == 0 {
                 return Err(Errno::Einval);
             }"
        ),
        "K-03a/K-06: fd 表 unshare 未实装, 非线程共享仍须拒绝"
    );
    assert!(
        has(
            &body,
            "if (flags & CLONE_FS) != 0 && (flags & CLONE_VM) == 0 {
                 return Err(Errno::Einval);
             }"
        ),
        "K-06: CLONE_FS 共享以共享地址空间为前提"
    );
    assert!(
        has(
            &body,
            "if (flags & CLONE_SIGHAND) != 0 && (flags & CLONE_VM) == 0 {
                 return Err(Errno::Einval);
             }"
        ),
        "K-03a: CLONE_SIGHAND 以共享地址空间为前提"
    );

    let raw = read_src(CLONE_SRC);
    assert!(
        raw.contains("SIMPLIFIED: CLONE_FILES 仅接受线程"),
        "K-03a: fd 表拒绝路径必须带 SIMPLIFIED 说明与扩展时点"
    );
    assert!(
        raw.contains("SIMPLIFIED: fork 路径 (无 CLONE_VM) 不接受 CLONE_FS"),
        "K-06: CLONE_FS 拒绝路径必须带 SIMPLIFIED 说明"
    );
}

#[test]
fn clone_adopts_group_resources_after_registration() {
    let src = code_only(&read_src(CLONE_SRC));

    let register = fn_body(&src, "fn vm_child_register");
    let insert_at = index_of(&register, "api::process_insert(");
    let adopt_at = index_of(
        &register,
        "vm_child_adopt_resources(flags, child, parent_pid);",
    );
    assert!(
        insert_at < adopt_at,
        "K-03a: 资源归属改写必须在进程表登记成功之后 (此前失败路径无需回滚共享计数)"
    );
    assert_eq!(
        src.matches("vm_child_adopt_resources(flags, child, parent_pid);")
            .count(),
        1,
        "K-03a: 资源接管只能有一个调用点"
    );

    let inherit = fn_body(&src, "fn vm_child_inherit");
    assert!(
        !inherit.contains("vm_child_adopt_resources"),
        "K-03a: 继承阶段不得提前改写资源归属"
    );

    let adopt = fn_body(&src, "fn vm_child_adopt_resources");
    for branch in [
        "child.share_signal_actions(&p.sigaction_table);",
        "child.copy_signal_actions_from(&p.sigaction_table);",
        "child.share_fd_table(&p.fd_table);",
        "child.copy_fd_table_from(&p.fd_table, |handle| ops.inc_open_file_ref(handle));",
        "child.share_fs_context(&p.fs_context);",
        "child.copy_fs_context_from(&p.fs_context);",
    ] {
        assert!(has(&adopt, branch), "K-03a: 资源接管缺少分支 {branch}");
    }
    assert!(
        has(&adopt, "if flags & CLONE_SIGHAND != 0")
            && has(&adopt, "if flags & CLONE_FILES != 0")
            && has(&adopt, "if flags & CLONE_FS != 0"),
        "K-03a: 三类资源必须分别按标志位分流"
    );
}

#[test]
fn process_resource_fields_are_shared_arcs() {
    let src = code_only(&read_src(PROCESS_SRC));
    for field in [
        "pub fd_table: Arc<FdTable>,",
        "pub sigaction_table: Arc<Mutex<[u64; 64]>>,",
        "pub fs_context: Arc<FsContext>,",
    ] {
        assert!(has(&src, field), "K-03a: Process 缺少共享资源字段 {field}");
    }

    let table_share_body = fn_body(&src, "pub fn share_fd_table");
    let add_user_at = index_of(&table_share_body, "table.add_user();");
    let assign_at = index_of(&table_share_body, "self.fd_table = Arc::clone(table);");
    assert!(
        add_user_at < assign_at,
        "K-03a: 必须先登记持有者再切换 Arc (计数不遗漏)"
    );

    let table_copy_body = fn_body(&src, "pub fn copy_fd_table_from");
    assert!(
        has(&table_copy_body, "self.fd_table.copy_from(src, on_handle);"),
        "K-03a: fork 复制委托给 FdTable::copy_from"
    );

    let copy_sig = fn_body(&src, "pub fn copy_signal_actions_from");
    assert!(
        has(&copy_sig, "let snapshot = *src.lock();")
            && has(&copy_sig, "*self.sigaction_table.lock() = snapshot;"),
        "K-03a: 信号表复制先快照再写本表, 不嵌套两把同层锁"
    );

    let context_share_body = fn_body(&src, "pub fn share_fs_context");
    assert!(
        has(&context_share_body, "self.fs_context = Arc::clone(ctx);"),
        "K-03a: CLONE_FS 必须共享同一 FsContext 实例"
    );
    let context_copy_body = fn_body(&src, "pub fn copy_fs_context_from");
    assert!(
        has(&context_copy_body, "self.fs_context.copy_from(src);"),
        "K-03a: fork 必须深拷贝 FS 上下文"
    );
}

#[test]
fn fd_table_counts_users_and_incrs_refs_under_source_lock() {
    let src = code_only(&read_src(FD_TABLE_SRC));
    let new_body = fn_body(&src, "pub const fn new");
    assert!(
        has(&new_body, "users: AtomicU32::new(1)"),
        "K-03a: 构造方即首个持有者"
    );

    let release = fn_body(&src, "pub fn release_user");
    assert!(
        has(
            &release,
            "let prev = self.users.fetch_sub(1, Ordering::AcqRel);"
        ) && has(&release, "prev == 1"),
        "K-03a: 最后用户判定必须来自显式计数"
    );

    let copy = fn_body(&src, "pub fn copy_from");
    let lock_at = index_of(&copy, "let e = src.entries.lock();");
    let callback_at = index_of(&copy, "on_handle(handle_id);");
    let snapshot_at = index_of(&copy, "(*e, *c)");
    assert!(
        lock_at < callback_at && callback_at < snapshot_at,
        "K-03a: 句柄回调必须在源表锁内完成 (先快照后 inc 会被并发 close 击穿)"
    );
}

#[test]
fn process_exit_closes_fds_only_for_last_user() {
    let src = code_only(&read_src(PROC_OPS_SRC));
    let body = fn_body(&src, "fn process_exit");
    let release_at = index_of(&body, "p.fd_table.release_user()");
    let close_at = index_of(&body, "close_all_fds()");
    assert!(
        release_at < close_at,
        "K-03a: 先释放持有者身份, 再决定是否关闭句柄"
    );
    assert!(
        has(&body, "if last_fd_user {") && has(&body, "unwrap_or(false)"),
        "K-03a: 只有最后持有者退出才允许关闭 fd 表"
    );
}

#[test]
fn fork_copies_fd_and_signal_and_fs_and_rolls_back_refs() {
    let src = code_only(&read_src(PROC_OPS_SRC));

    let inherit_fds = fn_body(&src, "fn inherit_parent_fds");
    assert!(
        has(
            &inherit_fds,
            "child.copy_fd_table_from(&p.fd_table, |handle| ops.inc_open_file_ref(handle));"
        ),
        "K-03a: fork 复制 fd 表时必须逐句柄递增 OpenFile 引用"
    );

    let release_refs = fn_body(&src, "fn release_inherited_fd_refs");
    assert!(
        has(&release_refs, "child.fd_table.get_all_fds()")
            && has(&release_refs, "ops.dec_open_file_ref(handle)"),
        "K-03a: fork 失败回滚必须撤销继承取得的 OpenFile 引用"
    );

    let fork = fn_body(&src, "fn sys_fork");
    assert_eq!(
        fork.matches("release_inherited_fd_refs(child);").count(),
        2,
        "K-03a: 进程表登记失败与 UserProc 注册失败两条路径都必须回滚"
    );

    let inherit_state = fn_body(&src, "fn inherit_parent_state");
    assert!(
        has(
            &inherit_state,
            "child.copy_signal_actions_from(&p.sigaction_table);"
        ) && has(&inherit_state, "child.copy_fs_context_from(&p.fs_context);"),
        "K-03a: fork 必须深拷贝信号动作表与 FS 上下文"
    );
}

#[test]
fn exec_swaps_signal_table_before_reset() {
    // K-06: 多线程 exec 改由 privileged de_thread 接管, functions 层不再拒绝.
    let exec_src = code_only(&read_src(EXEC_SRC));
    let execve = fn_body(&exec_src, "pub fn execve_syscall");
    assert!(
        !execve.contains("member_count") && !execve.contains("group_members"),
        "K-06: functions exec 不得重新引入多线程拒绝守卫 (de_thread 已接管)"
    );

    let proc_ops = code_only(&read_src(PROC_OPS_SRC));
    let replace = fn_body(&proc_ops, "fn proc_exec_replace");
    let fresh_arc_at = index_of(
        &replace,
        "p.sigaction_table =
             alloc::sync::Arc::new(crate::privileged::sync::IrqSpinLock::new([0u64; 64]));",
    );
    let reset_at = index_of(&replace, "reset_signal_state_on_exec(current_pid);");
    assert!(
        fresh_arc_at < reset_at,
        "K-03a: exec 必须先换新动作表再清零, 不得改写潜在共享实例"
    );
}

#[test]
fn path_resolution_and_umask_use_process_fs_context() {
    let manager = code_only(&read_src(VFS_MANAGER_SRC));
    assert!(
        has(
            &manager,
            "VFS_MAX_PATH == crate::privileged::proc::fs_context::FS_MAX_PATH"
        ),
        "K-03a: 两份路径缓冲上限必须由编译期断言绑定"
    );

    let current_cwd = fn_body(&manager, "pub fn current_cwd");
    assert!(
        has(
            &current_cwd,
            "crate::privileged::proc::with_current_fs_context(
                 crate::privileged::proc::FsContext::cwd_string,
             )
             .unwrap_or_else(|| self.get_cwd())"
        ),
        "K-03a: 有进程上下文时 cwd 以进程 FS 上下文为准, 全局量仅作缺省"
    );

    let set_cwd = fn_body(&manager, "pub fn set_current_cwd");
    assert!(
        has(
            &set_cwd,
            "if crate::privileged::proc::with_current_fs_context(|ctx| ctx.set_cwd(path)).is_none() {
                 self.set_cwd(path);
             }"
        ),
        "K-03a: chdir 必须写进程 FS 上下文并保留无进程回退"
    );

    let normalize = fn_body(&manager, "fn normalize_view_path_into");
    assert!(
        has(&normalize, "let cwd = self.current_cwd();"),
        "K-03a: 相对路径解析必须基于当前进程 cwd"
    );

    let resolve = fn_body(&manager, "fn resolve_user_path");
    assert!(
        has(&resolve, "let root = self.current_root_bytes();"),
        "K-03a: 路径映射必须基于当前进程根前缀"
    );

    let capture = fn_body(&manager, "pub fn capture_snapshot");
    assert!(
        has(
            &capture,
            "let process_fs = crate::privileged::proc::with_current_fs_context("
        ) && has(&capture, "ctx.cwd_into(&mut cwd)")
            && has(&capture, "ctx.root_into(&mut root)"),
        "K-03a: freg 快照必须包含进程 FS 上下文"
    );

    let restore = fn_body(&manager, "pub fn restore_from_snapshot");
    let root_at = index_of(&restore, "ctx.set_root(root);");
    let cwd_at = index_of(&restore, "ctx.set_cwd(cwd);");
    assert!(
        root_at < cwd_at,
        "K-03a: 回滚必须先恢复根再恢复 cwd (set_root 会重置 cwd)"
    );

    let vfs_path = code_only(&read_src("src/kernel/functions/fs/vfs_path.rs"));
    assert!(
        has(&vfs_path, "VFS_MANAGER.set_current_cwd(view);")
            && has(&vfs_path, "let cwd = VFS_MANAGER.current_cwd();"),
        "K-03a: chdir / getcwd 必须接线进程 FS 上下文"
    );

    let path = code_only(&read_src("src/kernel/functions/fs/path.rs"));
    assert!(
        has(&path, "VFS_MANAGER.set_current_root(real_root);")
            && has(
                &path,
                "let current_root = VFS_MANAGER.current_root_string();"
            ),
        "K-03a: chroot / pivot_root 必须接线进程 FS 上下文"
    );

    let mode = code_only(&read_src("src/kernel/functions/fs/mode.rs"));
    assert!(
        has(
            &mode,
            "crate::privileged::proc::with_current_fs_context(|ctx| ctx.set_umask(mask))
                 .unwrap_or_else(|| FALLBACK_UMASK.swap(mask, Ordering::SeqCst))"
        ),
        "K-03a: umask 必须存进程 FS 上下文并保留无进程回退"
    );

    let sgeg = code_only(&read_src("src/kernel/privileged/sgeg/api.rs"));
    assert!(
        !has(&sgeg, "fn umask_set") && !has(&sgeg, "fn umask_get"),
        "K-03a: 全局 umask 访问器必须退役 (改由 Process::fs_context 承载)"
    );
}

#[test]
fn process_share_fd_table_shares_space_and_user_count() {
    let leader = Process::new(0x4C20, "k03-fd-leader", None);
    let mut thread = Process::new(0x4C21, "k03-fd-thread", None);

    assert!(
        thread.fd_table.alloc_fd(0x40, false).is_some(),
        "线程自建表应可分配 fd"
    );
    let fd = leader
        .fd_table
        .alloc_fd(0x41, true)
        .expect("leader fd alloc");

    thread.share_fd_table(&leader.fd_table);

    assert!(
        Arc::ptr_eq(&thread.fd_table, &leader.fd_table),
        "CLONE_FILES 必须共享同一 FdTable 实例"
    );
    assert_eq!(
        thread.fd_table.get_entry(fd),
        Some((0x41, true)),
        "共享表必须立即看到父线程的 fd 映射"
    );
    assert!(
        !leader.fd_table.release_user(),
        "两位持有者时不得判定为最后用户"
    );
    assert!(
        thread.fd_table.release_user(),
        "最后一位持有者退出才允许关闭句柄"
    );
}

#[test]
fn process_copy_fd_table_is_independent_snapshot() {
    let parent = Process::new(0x4C22, "k03-fd-parent", None);
    let mut child = Process::new(0x4C23, "k03-fd-child", None);

    let fd_a = parent.fd_table.alloc_fd(0x51, false).expect("fd_a alloc");
    let fd_b = parent.fd_table.alloc_fd(0x52, true).expect("fd_b alloc");

    let mut seen = Vec::new();
    child.copy_fd_table_from(&parent.fd_table, |handle| seen.push(handle));
    seen.sort_unstable();

    assert_eq!(seen, [0x51, 0x52], "每个占用槽位必须回调一次");
    assert_eq!(
        child.fd_table.get_entry(fd_a),
        Some((0x51, false)),
        "fork 必须复制句柄映射与 cloexec"
    );
    assert_eq!(child.fd_table.get_entry(fd_b), Some((0x52, true)));

    child.fd_table.close_fd(fd_a);
    assert!(
        parent.fd_table.get_entry(fd_a).is_some(),
        "复制后父子 fd 表各自独立"
    );
    assert!(
        child.fd_table.release_user(),
        "复制不改写目标表用户计数 (仍只有一个持有者)"
    );
    assert!(
        parent.fd_table.release_user(),
        "父表用户计数不因子进程复制而增加"
    );
}

#[test]
fn process_signal_actions_share_or_copy() {
    let leader = Process::new(0x4C24, "k03-sig-leader", None);
    let mut thread = Process::new(0x4C25, "k03-sig-thread", None);
    leader.sigaction_table.lock()[3] = 0x1122_3344;

    thread.share_signal_actions(&leader.sigaction_table);
    assert!(
        Arc::ptr_eq(&thread.sigaction_table, &leader.sigaction_table),
        "CLONE_SIGHAND 必须共享同一动作表"
    );
    assert_eq!(
        thread.sigaction_table.lock()[3],
        0x1122_3344,
        "共享表必须立即看到父线程动作"
    );
    leader.sigaction_table.lock()[4] = 0x5566_7788;
    assert_eq!(
        thread.sigaction_table.lock()[4],
        0x5566_7788,
        "共享表必须双向可见"
    );

    let mut fork_child = Process::new(0x4C26, "k03-sig-fork", None);
    fork_child.copy_signal_actions_from(&leader.sigaction_table);
    assert!(
        !Arc::ptr_eq(&fork_child.sigaction_table, &leader.sigaction_table),
        "fork 必须持有独立动作表"
    );
    assert_eq!(fork_child.sigaction_table.lock()[3], 0x1122_3344);

    leader.sigaction_table.lock()[3] = 0;
    assert_eq!(
        fork_child.sigaction_table.lock()[3],
        0x1122_3344,
        "副本不得随源表后续修改而漂移"
    );
}

#[test]
fn process_fs_context_share_or_copy() {
    let leader = Process::new(0x4C27, "k03-fs-leader", None);
    let mut thread = Process::new(0x4C28, "k03-fs-thread", None);

    leader.fs_context.set_cwd("/srv/app");
    assert_eq!(
        leader.fs_context.set_umask(0o077),
        DEFAULT_UMASK,
        "新上下文 umask 必须取默认值"
    );

    thread.share_fs_context(&leader.fs_context);
    assert!(
        Arc::ptr_eq(&thread.fs_context, &leader.fs_context),
        "CLONE_FS 必须共享同一 FsContext"
    );
    assert_eq!(thread.fs_context.cwd_string(), "/srv/app");
    leader.fs_context.set_cwd("/srv/data");
    assert_eq!(
        thread.fs_context.cwd_string(),
        "/srv/data",
        "共享上下文必须对全组可见"
    );

    let mut fork_child = Process::new(0x4C29, "k03-fs-fork", None);
    fork_child.copy_fs_context_from(&leader.fs_context);
    assert!(
        !Arc::ptr_eq(&fork_child.fs_context, &leader.fs_context),
        "fork 必须持有独立 FS 上下文"
    );
    assert_eq!(fork_child.fs_context.cwd_string(), "/srv/data");
    assert_eq!(fork_child.fs_context.root_string(), "/");
    assert_eq!(
        fork_child.fs_context.set_umask(0o022),
        0o077,
        "umask 必须随 fork 继承"
    );

    leader.fs_context.set_cwd("/srv/other");
    assert_eq!(
        fork_child.fs_context.cwd_string(),
        "/srv/data",
        "父进程后续 chdir 不得影响 fork 子进程"
    );
}

#[test]
fn process_fs_context_root_copy_is_independent() {
    let parent = Process::new(0x4C2A, "k03-root-parent", None);
    parent.fs_context.set_cwd("/tmp");
    parent.fs_context.set_root("/jail");
    parent.fs_context.set_cwd("/work");

    let mut child = Process::new(0x4C2B, "k03-root-child", None);
    child.copy_fs_context_from(&parent.fs_context);

    assert_eq!(child.fs_context.root_string(), "/jail");
    assert_eq!(child.fs_context.cwd_string(), "/work");

    parent.fs_context.set_root("/newjail");
    assert_eq!(parent.fs_context.cwd_string(), "/", "切根必须重置 cwd");
    assert_eq!(
        child.fs_context.root_string(),
        "/jail",
        "父进程切根不得影响 fork 子进程"
    );
    assert_eq!(child.fs_context.cwd_string(), "/work");
}
