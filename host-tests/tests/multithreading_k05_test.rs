// SPDX-License-Identifier: MPL-2.0
//! K-05 owner 口径回归锁 (D2 + D7).
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-05 与 D2/D7.
//! 本文件锁定:
//!
//! - 内核 `Mutex` 的持有者按 **TID** 记账: 同一线程组的不同线程 (同 tgid)
//!   不得重入, 同 TID 重入只递增 depth;
//! - 完全释放后 owner 回到 `-1` 哨兵 — 0 是"无进程上下文"的合法 PID,
//!   不能当作无主标记 (FFI 路径同口径);
//! - `PiMutex` 退出登记在完全释放 / 移交 / 对象析构时摘除, 进程退出路径
//!   按持有人强制释放, 重复调用幂等;
//! - PID 复用负控制: 旧化身的退出清理先于 PID 回收, 复用的同一数值 PID
//!   拿到的是无主锁, 而不是旧化身仍持有的锁;
//! - futex 等待队列 / robust futex / `clear_child_tid` 的持有者比较均按 TID.
//!
//! 线程退出与 `exit_group` 的语义分离, 以及组定向信号投递归 K-06,
//! 本文件不越界锁定.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::Ordering;

use edgine::kernel::privileged::config::MAX_PROCESSES;
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};
use edgine::kernel::privileged::sync::pi_mutex::{self, PiMutex};
use edgine::kernel::privileged::sync::{Mutex, MutexInner, mutex_lock, mutex_unlock};

const PROCESS_SRC: &str = "src/kernel/privileged/proc/process.rs";
const PROC_OPS_SRC: &str = "src/kernel/privileged/proc/proc_ops.rs";
const ROBUST_SRC: &str = "src/kernel/privileged/proc/robust.rs";
const PROCESS_CLEANUP_SRC: &str = "src/kernel/privileged/process_cleanup.rs";
const PI_MUTEX_SRC: &str = "src/kernel/privileged/sync/pi_mutex.rs";
const MUTEX_SRC: &str = "src/kernel/privileged/sync/mutex.rs";
const SYNC_MOD_SRC: &str = "src/kernel/privileged/sync/mod.rs";
const FUTEX_SRC: &str = "src/kernel/privileged/syscall/futex.rs";

/// 进程表 / 调度器 current 都是全局态, 文件内测试串行执行.
static TABLE_LOCK: StdMutex<()> = StdMutex::new(());

fn lock_table() -> std::sync::MutexGuard<'static, ()> {
    TABLE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 本核 current 复位 (测试结束或 panic 都必须回到无进程上下文).
struct CurrentGuard;

impl CurrentGuard {
    fn set(pid: u32) -> Self {
        Scheduler::set_current(pid);
        Self
    }
}

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        Scheduler::set_current(0);
    }
}

/// 已登记进全局进程表的 TCB; Drop 时回收 (幂等).
struct TableProc {
    pid: u32,
    armed: bool,
}

impl TableProc {
    fn insert(pid: u32, name: &str) -> Self {
        Self::insert_owned(Process::new(pid, name, None))
    }

    fn insert_owned(process: Process) -> Self {
        let pid = process.pid.0;
        assert!(
            PROCESS_TABLE.insert(Box::into_raw(Box::new(process))),
            "进程表登记失败 (pid={pid})"
        );
        Self { pid, armed: true }
    }

    fn remove_now(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
        self.armed = false;
    }
}

impl Drop for TableProc {
    fn drop(&mut self) {
        if self.armed {
            PROCESS_TABLE.remove_and_free(self.pid);
        }
    }
}

/// 排空 PID 位图并在 Drop 时归还全部 PID, 供"唯一空位"复用负控制使用.
struct PidDrain {
    pids: Vec<u32>,
}

impl PidDrain {
    fn take_all() -> Self {
        let mut pids = Vec::with_capacity(MAX_PROCESSES);
        while let Some(pid) = PROCESS_TABLE.allocate_pid() {
            pids.push(pid);
            assert!(pids.len() <= MAX_PROCESSES, "PID 位图分配数超过容量");
        }
        Self { pids }
    }
}

impl Drop for PidDrain {
    fn drop(&mut self) {
        for &pid in &self.pids {
            PROCESS_TABLE.free_pid(pid);
        }
    }
}

fn owner_of(pid: u32) -> i32 {
    i32::try_from(pid).expect("测试 TID 应可由 i32 表示")
}

/// 同一线程组的不同 TID 互斥, 同 TID 重入只增 depth.
#[test]
fn mutex_owner_is_tid_and_reentrant_only_for_same_tid() {
    let _serial = lock_table();
    let a_pid = PROCESS_TABLE.allocate_pid().expect("leader PID 分配失败");
    let b_pid = PROCESS_TABLE.allocate_pid().expect("thread PID 分配失败");

    let leader = Process::new(a_pid, "k05-leader", None);
    let group = Arc::clone(leader.group());
    let _leader = TableProc::insert_owned(leader);

    let mut thread = Process::new(b_pid, "k05-thread", None);
    thread.join_thread_group(Arc::clone(&group));
    let _thread = TableProc::insert_owned(thread);

    assert_eq!(
        PROCESS_TABLE.with_process(b_pid, Process::tgid),
        Some(a_pid),
        "两个 TID 必须属于同一线程组 (同 tgid)"
    );

    let _current = CurrentGuard::set(a_pid);
    let m = Mutex::new(0u32);

    let g1 = m.lock();
    assert_eq!(m.owner(), owner_of(a_pid), "持有者必须记为当前 TID");
    assert_eq!(m.depth(), 1);

    let g2 = m.lock();
    assert_eq!(m.depth(), 2, "同 TID 重入只递增 depth");

    Scheduler::set_current(b_pid);
    assert!(
        m.try_lock().is_none(),
        "同一线程组内的另一 TID 不得重入 (owner 不是 tgid)"
    );
    assert_eq!(m.owner(), owner_of(a_pid), "争用失败不得改写持有者");

    Scheduler::set_current(a_pid);
    drop(g2);
    assert_eq!(m.depth(), 1, "重入层退出只递减 depth");
    assert_eq!(m.owner(), owner_of(a_pid));

    drop(g1);
    assert!(!m.is_locked(), "最后一层释放后必须解锁");
    assert_eq!(m.owner(), -1, "完全释放后 owner 必须回到 -1 哨兵");
    assert_eq!(m.depth(), 0);
}

/// FFI 原始 mutex 路径: pid 0 是可记账的合法持有者, 无主哨兵必须是 -1.
#[test]
fn ffi_mutex_unlock_restores_no_owner_sentinel() {
    let _serial = lock_table();
    let _current = CurrentGuard::set(0);

    let inner = MutexInner::new();
    mutex_lock(&raw const inner);
    assert_eq!(
        inner.owner.load(Ordering::Acquire),
        0,
        "无进程上下文时持有者记为 PID 0"
    );

    mutex_unlock(&raw const inner);
    assert_eq!(
        inner.owner.load(Ordering::Acquire),
        -1,
        "完全释放必须写 -1; 用 0 作空标记会让 pid 0 误判为持有者"
    );
    assert_eq!(inner.locked.load(Ordering::Acquire), 0);
    assert_eq!(inner.depth.load(Ordering::Acquire), 0);
}

/// 进程退出强制释放按 PID 归属, 重复调用幂等, 移交后随新持有者走.
#[test]
fn pi_exit_cleanup_is_pid_scoped_and_idempotent() {
    let _serial = lock_table();
    let holder = 9_501u32;
    let next = 9_502u32;
    let other = 9_503u32;

    let m = PiMutex::new(0u32);
    assert!(m.try_lock(holder, 1), "holder 应先取得锁");
    assert!(!m.try_lock(next, 10), "next 应作为等待者注册");

    pi_mutex::pi_mutex_process_exit(other);
    assert!(m.is_locked(), "非持有者退出不得释放他人的锁");
    assert_eq!(m.holder(), holder);
    assert_eq!(m.waiter_count(), 1);

    pi_mutex::pi_mutex_process_exit(holder);
    assert_eq!(m.holder(), next, "持有者退出时按优先级定向移交给等待者");
    assert_eq!(pi_mutex::held_lock_count(holder), 0);
    assert_eq!(pi_mutex::held_lock_count(next), 1);

    pi_mutex::pi_mutex_process_exit(holder);
    assert_eq!(m.holder(), next, "重复的旧退出回调不得影响新持有者");

    pi_mutex::pi_mutex_process_exit(next);
    assert!(!m.is_locked(), "新持有者退出后锁必须完全释放");
    assert_eq!(m.holder(), pi_mutex::PID_NONE);
    assert_eq!(pi_mutex::held_lock_count(next), 0);

    pi_mutex::pi_mutex_process_exit(next);
    assert!(!m.is_locked(), "退出清理必须幂等");
}

/// PID 复用负控制: 退出清理先于 PID 回收, 新化身不得继承旧化身的 PI 持有.
#[test]
fn pid_reuse_after_exit_cannot_inherit_pi_ownership() {
    let _serial = lock_table();
    let _current = CurrentGuard::set(0);

    let drain = PidDrain::take_all();
    assert_eq!(
        drain.pids.len(),
        MAX_PROCESSES,
        "负控制需要排空 PID 位图以制造唯一空位"
    );
    let reused = drain.pids[MAX_PROCESSES / 2];

    PROCESS_TABLE.free_pid(reused);
    let realloc = PROCESS_TABLE.allocate_pid().expect("唯一空位应可重分配");
    assert_eq!(realloc, reused, "PID 分配必须复用唯一空槽");

    let mut old = TableProc::insert(reused, "k05-reuse-old");
    let m = PiMutex::new(0u32);
    assert!(m.try_lock(reused, 5), "旧化身应能持锁");
    assert_eq!(pi_mutex::held_lock_count(reused), 1);

    old.remove_now();
    assert!(!m.is_locked(), "退出清理必须释放旧化身持有的 PI 锁");
    assert_eq!(m.holder(), pi_mutex::PID_NONE);
    assert_eq!(
        pi_mutex::held_lock_count(reused),
        0,
        "退出清理必须摘除旧化身的登记"
    );

    let realloc2 = PROCESS_TABLE.allocate_pid().expect("回收后的 PID 应可复用");
    assert_eq!(realloc2, reused, "旧 TID 回收后必须复用同一数值 PID");

    let mut new = TableProc::insert(realloc2, "k05-reuse-new");
    assert!(
        m.try_lock(realloc2, 5),
        "新化身必须拿到无主锁, 而不是继承旧化身的持有权"
    );
    assert_eq!(m.holder(), realloc2);

    pi_mutex::pi_mutex_process_exit(realloc2);
    assert!(!m.is_locked(), "新化身退出后锁应完全释放");
    new.remove_now();
}

/// PI 登记生命周期: 释放/移交/析构/退出四条路径都必须关闭登记.
#[test]
fn pi_registry_lifecycle_is_closed() {
    let src = code_only(&read_src(PI_MUTEX_SRC));

    let unlock = fn_body(&src, "fn do_unlock(&self)");
    assert_eq!(
        unlock.matches("untrack_hold(self, my_pid);").count(),
        2,
        "K-05: 完全释放与定向移交两条路径都必须摘除原持有者登记"
    );
    assert!(
        has(&unlock, "track_hold(self, next_pid);"),
        "K-05: 定向移交必须为新持有者登记"
    );
    let holder_at = index_of(
        &unlock,
        "self.inner.holder.store(next_pid, Ordering::Release);",
    );
    let handoff_at = index_of(
        &unlock,
        "self.inner.handoff.store(next_pid, Ordering::Release);",
    );
    assert!(
        holder_at < handoff_at,
        "K-05: 移交标记必须最后写, 目标看到标记时所有权已就绪"
    );
    let release_holder_at = index_of(
        &unlock,
        "self.inner.holder.store(PID_NONE, Ordering::Release);",
    );
    let release_locked_at = index_of(
        &unlock,
        "self.inner.locked.store(false, Ordering::Release);",
    );
    assert!(
        release_holder_at < release_locked_at,
        "K-05: 完全释放先清 holder 再清 locked, 不给并发 CAS 留下双持有窗口"
    );

    let try_lock = fn_body(
        &src,
        "pub fn try_lock(&self, my_pid: u32, my_base_priority: u32)",
    );
    let accept_at = index_of(&try_lock, "if self.accept_handoff(my_pid) {");
    let cas_at = index_of(&try_lock, "compare_exchange(false, true, Ordering::AcqRel");
    assert!(accept_at < cas_at, "K-05: 定向移交必须先于自由竞争被确认");

    let drop_impl = fn_body(&src, "impl<T: ?Sized> Drop for PiMutex<T> {");
    assert!(
        has(&drop_impl, "untrack_all(self);"),
        "K-05: PiMutex 析构必须兜底摘除所有指向本实例的登记"
    );

    let exit = fn_body(&src, "pub fn pi_mutex_process_exit(pid: u32)");
    let remove_at = index_of(&exit, "PI_MUTEX_HELD.lock().remove(&pid)");
    let dispatch_at = index_of(&exit, "(h.dispatch)(h.ptr, pid)");
    assert!(
        remove_at < dispatch_at,
        "K-05: 退出回调先整体摘除该 pid 的登记, 再逐个强制解锁"
    );
}

/// 进程退出路径: PI 清理必须发生在 PID 回收之前 (复用窗口负控制的源码面).
#[test]
fn process_exit_cleanup_precedes_pid_reuse() {
    let process = code_only(&read_src(PROCESS_SRC));
    let remove = fn_body(&process, "pub fn remove_and_free(&self, pid: Pid)");
    let notify_at = index_of(
        &remove,
        "crate::privileged::process_cleanup::notify_process_exit(pid);",
    );
    let free_at = index_of(&remove, "self.free_pid(pid);");
    assert!(
        notify_at < free_at,
        "K-05: PI/驱动清理必须先于 PID 回收, 否则复用方会继承陈旧持有"
    );

    let cleanup = code_only(&read_src(PROCESS_CLEANUP_SRC));
    let notify = fn_body(&cleanup, "pub fn notify_process_exit(pid: u32)");
    let pi_at = index_of(&notify, "pi_mutex_process_exit(pid);");
    let callback_at = index_of(&notify, "func(pid);");
    assert!(pi_at < callback_at, "K-05: PI 锁释放先于外部驱动资源清理");
}

/// owner 记账口径: 内核各原语统一取 TID (`process_get_current_pid`), 不取 tgid.
#[test]
fn owner_paths_use_tid_not_tgid() {
    let process = code_only(&read_src(PROCESS_SRC));
    let proc_ops = code_only(&read_src(PROC_OPS_SRC));
    let current = fn_body(
        &proc_ops,
        "pub extern \"C\" fn process_get_current_pid() -> u32",
    );
    assert!(
        has(&current, "Scheduler::current().unwrap_or(0)") && !has(&current, "tgid"),
        "K-05: process_get_current_pid 必须返回线程级 TID"
    );
    let current_tgid = fn_body(&proc_ops, "pub fn process_get_current_tgid() -> u32");
    assert!(
        has(&current_tgid, "Process::tgid"),
        "K-05: getpid 语义单独走 tgid 出口, 不与 owner 记账混用"
    );

    let mutex = code_only(&read_src(MUTEX_SRC));
    let raw_lock = fn_body(&mutex, "fn raw_lock(&self)");
    assert!(
        has(
            &raw_lock,
            "let cur = (unsafe { process_get_current_pid() }).cast_signed();"
        ) && has(
            &raw_lock,
            "if self.inner.owner.load(Ordering::Acquire) == cur {"
        ),
        "K-05: Mutex 重入判定必须比较当前 TID"
    );
    let acquire = fn_body(&mutex, "fn acquire_lock_internal(&self)");
    assert!(
        has(&acquire, "let pid = unsafe { process_get_current_pid() };")
            && has(
                &acquire,
                "self.inner.owner.store(pid.cast_signed(), Ordering::Release);"
            ),
        "K-05: Mutex 持有者记录必须写 TID"
    );

    let ffi = code_only(&read_src(SYNC_MOD_SRC));
    let unlock = fn_body(
        &ffi,
        "pub extern \"C\" fn mutex_unlock(m: *const MutexInner)",
    );
    assert!(
        has(&unlock, "raw::mutex_owner(m).store(-1, Ordering::Release);")
            && !has(&unlock, "raw::mutex_owner(m).store(0,"),
        "K-05: FFI 完全释放必须写 -1 无主哨兵"
    );

    let futex = code_only(&read_src(FUTEX_SRC));
    let wait = fn_body(&futex, "fn futex_wait(uaddr: u64, val: i32, _timeout: u64)");
    assert!(
        has(
            &wait,
            "let current_pid = crate::privileged::proc::process_get_current_pid();"
        ) && has(&wait, "pid: current_pid,")
            && has(&wait, "bucket.remove_by_pid(current_pid);"),
        "K-05: futex 等待者按 TID 入队/出队"
    );

    let robust = code_only(&read_src(ROBUST_SRC));
    let handle = fn_body(&robust, "fn handle_futex_word(pid: u32, uaddr: u64)");
    assert!(
        has(
            &handle,
            "if word & FUTEX_TID_MASK != pid || word & FUTEX_OWNER_DIED != 0 {"
        ),
        "K-05: robust futex 持有者比较必须按传入 TID"
    );
    assert!(
        has(&process, "pub clear_child_tid: AtomicU64,"),
        "K-05: clear_child_tid 属于每线程 TCB, 而不是线程组"
    );
}

// ============================================================================
// 源码契约辅助
// ============================================================================

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
