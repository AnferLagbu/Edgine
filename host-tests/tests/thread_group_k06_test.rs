// SPDX-License-Identifier: MPL-2.0
//! K-06 线程组语义系统调用回归锁.
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-06 与 D4/D7.
//! 本文件锁定:
//!
//! - `exit` / `exit_group` 走两条独立路径: 前者只结束调用 TCB, 后者登记组退出码
//!   并向其余成员投递不可屏蔽的 `SIGKILL`;
//! - `getpid` 读 tgid, `gettid` 读 TID;
//! - `tgkill` 先校验 TID 对 tgid 的归属再投递, `sig == 0` 探测也受同一校验;
//! - `kill(pid)` / `kill(0)` / `kill(-1)` / `kill(-pgid)` 按线程组分发, 组内
//!   择线程遵守每线程屏蔽字与显式忽略; 不可捕获信号无视屏蔽投给首个存活成员;
//! - `wait4` 只收割已整体退出的线程组, 收割前先从父组子账摘除 tgid;
//! - 多线程 `exec` 的 `de_thread`: 置标记并快照成员后拒绝新入组 (`EAGAIN`), 终止
//!   兄弟线程、保留组长锚点, 调用线程补建镜像后接管新映像; exec 后 FS 与信号
//!   动作表拆出私有副本;
//! - 线程切换的用户页表取 `Process.cr3` 权威值, 镜像缺失不再阻止切换;
//! - 致命信号默认动作在 `de_thread` 期间只结束目标线程, 其余场景终止整组;
//! - 内核等待循环经 `Scheduler::yield_current` 感知组退出或 `de_thread` 的 SIGKILL;
//! - `exit_group` 与 `clone` 并发: 入组与退出请求在成员表锁内互斥, 快照漏掉的
//!   线程入队前自挂 SIGKILL, 登记前失败路径归还 PID 槽位;
//! - §6③a 收割取证: 「已请求组退出但整组未收齐」才落 `pid|state` 取证行, 签名去重,
//!   取证名单按 tgid 扫进程表 (成员账在 `leave` 后已空, 看不到「收齐却被卡在非
//!   Zombie」的 L-2 形态), 且取证不改收割判据
//!   (`reap_evidence_records_signature_only_after_exit_is_requested` 与
//!   `reap_evidence_diag_shape_contract_locked`).
//!
//! 镜像 `destroy` 与 `Process::drop` 对同一 cr3 的重复 `frame_dec` 是 K-06 阶段
//! 发现的预存问题 (§11.5), 已由 A-8 收口 (`Process::drop` 唯一减计点); 回归锁见
//! `user_proc_reclaim_contract_test.rs` 与 `user_proc_frame_release_test.rs`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::Ordering;

use edgine::kernel::privileged::proc::signal::do_signal_send_group;
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, ProcessState, ThreadGroup};

const FUNC_DISPATCH_SRC: &str = "src/kernel/functions/syscall/dispatch.rs";
const LIFECYCLE_SRC: &str = "src/kernel/functions/proc/lifecycle.rs";
const INFO_SRC: &str = "src/kernel/functions/proc/info.rs";
const FUNC_SIGNAL_SRC: &str = "src/kernel/functions/proc/signal.rs";
const FUNC_EXEC_SRC: &str = "src/kernel/functions/proc/exec.rs";
const CLONE_SRC: &str = "src/kernel/functions/proc/clone.rs";
const WAIT4_SRC: &str = "src/kernel/functions/proc/wait4.rs";
const PRIV_DISPATCH_SRC: &str = "src/kernel/privileged/syscall/dispatch.rs";
const PRIV_SIGNAL_SRC: &str = "src/kernel/privileged/proc/signal.rs";
const PROC_OPS_SRC: &str = "src/kernel/privileged/proc/proc_ops.rs";
const PROCESS_SRC: &str = "src/kernel/privileged/proc/process.rs";
const THREAD_GROUP_SRC: &str = "src/kernel/privileged/proc/thread_group.rs";
const SCHED_SRC: &str = "src/kernel/privileged/proc/scheduler.rs";
const USER_PROC_SRC: &str = "src/kernel/privileged/proc/user_proc.rs";
const ROBUST_SRC: &str = "src/kernel/privileged/proc/robust.rs";

/// 进程表 / PID 位图是全局态, 行为用例串行执行.
static TABLE_LOCK: StdMutex<()> = StdMutex::new(());

fn lock_table() -> std::sync::MutexGuard<'static, ()> {
    TABLE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 已登记进全局进程表的 TCB; Drop 时回收 (幂等).
struct TableProc {
    pid: u32,
    armed: bool,
}

impl TableProc {
    fn insert_owned(process: Process) -> Self {
        let pid = process.pid.0;
        assert!(
            PROCESS_TABLE.insert(Box::into_raw(Box::new(process))),
            "进程表登记失败 (pid={pid})"
        );
        Self { pid, armed: true }
    }

    /// 交给别处 (组收割) 释放, Drop 不再重复回收.
    fn disarm(&mut self) {
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

/// 状态机要求 Created → Ready → Running, 不能一步跳到运行/僵尸.
fn set_running(pid: u32) {
    PROCESS_TABLE.with_process(pid, |p| {
        p.set_state_safe(ProcessState::Ready)
            .expect("Created → Ready");
        p.set_state_safe(ProcessState::Running)
            .expect("Ready → Running");
    });
}

fn set_zombie(pid: u32) {
    PROCESS_TABLE.with_process(pid, |p| {
        p.set_state_safe(ProcessState::Ready)
            .expect("Created → Ready");
        p.set_state_safe(ProcessState::Running)
            .expect("Ready → Running");
        p.set_state_safe(ProcessState::Zombie)
            .expect("Running → Zombie");
    });
}

// ============================================================================
// 行为用例
// ============================================================================

/// `de_thread` 期间入组被拒, 线程保留自身单线程组; 结束后可正常入组.
#[test]
fn join_thread_group_refused_during_exec_de_thread() {
    let group = Arc::new(ThreadGroup::new_leader(0x6A00));
    let snapshot = group.begin_exec_de_thread_snapshot();
    assert_eq!(snapshot, vec![0x6A00], "快照包含现有全部成员");
    assert!(group.exec_de_thread());

    let mut thread = Process::new(0x6A01, "k06-exec-joiner", None);
    assert!(
        !thread.join_thread_group(Arc::clone(&group)),
        "de_thread 期间新线程不得入组 (否则 exec 后会留在旧映像)"
    );
    assert_eq!(thread.tgid(), 0x6A01, "被拒线程必须保留自身单线程组");
    assert_eq!(group.members(), vec![0x6A00], "被拒线程不得留在成员账");

    group.end_exec_de_thread();
    assert!(!group.exec_de_thread());
    assert!(
        thread.join_thread_group(Arc::clone(&group)),
        "de_thread 结束后入组应恢复"
    );
    assert_eq!(thread.tgid(), 0x6A00, "入组后 getpid 读组长 tgid");
    assert_eq!(group.members(), vec![0x6A00, 0x6A01]);
}

/// 组收割: 必须全员 Zombie 才就绪; 收割时先摘父组子账再释放 TCB, 且只成功一次.
#[test]
fn group_reap_requires_all_members_and_clears_parent_ledger() {
    let _serial = lock_table();
    let parent_pid = PROCESS_TABLE.allocate_pid().expect("parent PID");
    let child_pid = PROCESS_TABLE.allocate_pid().expect("child leader PID");
    let tid = PROCESS_TABLE.allocate_pid().expect("child thread PID");

    let parent = Process::new(parent_pid, "k06-parent", None);
    let parent_group = Arc::clone(parent.group());
    let _parent = TableProc::insert_owned(parent);

    let leader = Process::new(child_pid, "k06-child-leader", None);
    let child_group = Arc::clone(leader.group());
    child_group.set_parent(parent_pid);
    let mut thread = Process::new(tid, "k06-child-thread", None);
    assert!(thread.join_thread_group(Arc::clone(&child_group)));
    let mut leader_guard = TableProc::insert_owned(leader);
    let mut thread_guard = TableProc::insert_owned(thread);
    parent_group.add_child(child_pid);

    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        None,
        "组未退出时不可收割"
    );
    assert!(!PROCESS_TABLE.reap_zombie_group(child_pid));

    child_group.request_exit(42);
    child_group.mark_exited(42);
    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        None,
        "组已退出但成员仍在退出途中时不可收割"
    );
    set_zombie(child_pid);
    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        None,
        "仍有成员未落 Zombie 时不可收割"
    );
    assert!(!PROCESS_TABLE.reap_zombie_group(child_pid));
    assert_eq!(
        parent_group.children(),
        vec![child_pid],
        "未完成收割不得从父组子账摘除"
    );

    set_zombie(tid);
    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        Some(42),
        "全员 Zombie 后返回组退出码"
    );
    leader_guard.disarm();
    thread_guard.disarm();
    assert!(
        PROCESS_TABLE.reap_zombie_group(child_pid),
        "首个认领者应完成收割"
    );
    assert!(
        PROCESS_TABLE.with_process(child_pid, |_| ()).is_none(),
        "组长 TCB 必须被回收"
    );
    assert!(
        PROCESS_TABLE.with_process(tid, |_| ()).is_none(),
        "成员 TCB 必须被回收"
    );
    assert!(
        parent_group.children().is_empty(),
        "收割必须先摘父组子账, 避免 tgid 复用时误删新子组"
    );
    assert!(
        !PROCESS_TABLE.reap_zombie_group(child_pid),
        "同一组不得被重复收割"
    );
}

/// §6③a 甲 收割取证: 「已请求组退出但整组未收齐」落取证签名, 未请求时不落;
/// 诊断全程只读 —— 收割判据 (`None` → 收齐后 `Some(code)`) 逐字未变.
///
/// 取证覆盖 `group_zombie_exit_code` 的两个 `None` 出口: 出口①组未整体退出 (兄弟
/// 线程始终没走到退出收尾), 出口②组已收齐 (成员账空) 但某个 TCB 仍非 Zombie ——
/// 已登记的 L-2「Zombie 被重新投入」形态只有进程表扫描看得见, 成员账为空表.
///
/// host-test 下 klog 是 no-op (`privileged::klog` 的 B08-14 前置), 打印本身不可观测,
/// 故用哨兵签名反证「内核路径是否写了签名」: 写了 ⇒ 哨兵被替换 ⇒ 再写哨兵返回
/// `true`; 没写 ⇒ 哨兵仍在 ⇒ 再写哨兵返回 `false`.
#[test]
fn reap_evidence_records_signature_only_after_exit_is_requested() {
    const SENTINEL: u64 = u64::MAX;
    let _serial = lock_table();
    let parent_pid = PROCESS_TABLE.allocate_pid().expect("parent PID");
    let child_pid = PROCESS_TABLE.allocate_pid().expect("child leader PID");
    let tid = PROCESS_TABLE.allocate_pid().expect("child thread PID");

    let parent = Process::new(parent_pid, "reap-diag-parent", None);
    let _parent = TableProc::insert_owned(parent);

    let leader = Process::new(child_pid, "reap-diag-leader", None);
    let child_group = Arc::clone(leader.group());
    let mut thread = Process::new(tid, "reap-diag-thread", None);
    assert!(thread.join_thread_group(Arc::clone(&child_group)));
    let _leader_guard = TableProc::insert_owned(leader);
    let _thread_guard = TableProc::insert_owned(thread);

    assert_eq!(
        PROCESS_TABLE.group_non_zombie_members(child_pid),
        vec![
            (child_pid, ProcessState::Created),
            (tid, ProcessState::Created)
        ],
        "取证名单按 tgid 扫进程表, 组长与兄弟线程都在内"
    );

    // 判据收窄①: 未请求组退出的轮询 (整组仍活着) 不得落取证签名.
    assert!(child_group.note_reap_diag(SENTINEL), "哨兵写入应放行");
    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        None,
        "组未退出不可收割"
    );
    assert!(
        !child_group.note_reap_diag(SENTINEL),
        "§6③a: 未请求组退出时诊断不得写签名 (否则哨兵已被替换)"
    );

    // 出口①: 请求组退出 + 组长已收尾离开成员账并落 Zombie, 兄弟线程仍未退出.
    child_group.request_exit(42);
    assert!(
        !child_group.leave(child_pid),
        "组长离开后仍有兄弟线程, `leave` 返回的是「组是否已无成员」"
    );
    set_zombie(child_pid);
    assert_eq!(
        PROCESS_TABLE.group_non_zombie_members(child_pid),
        vec![(tid, ProcessState::Created)],
        "已落 Zombie 的组长不得再出现在取证名单"
    );
    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        None,
        "§6③a: 取证行不改变收割判据"
    );
    assert!(
        child_group.note_reap_diag(SENTINEL),
        "§6③a: 出口① (组未整体退出) 的未收齐态必须由内核路径记下签名 (诊断已接线)"
    );

    // 出口② (L-2 形态): 成员账已清空且组已标退出, 兄弟线程 TCB 却被重新投入 Ready.
    assert!(child_group.leave(tid), "最后成员离开成员账");
    child_group.mark_exited(42);
    assert!(
        child_group.members().is_empty(),
        "成员账已空: 取证名单不能取自成员账"
    );
    PROCESS_TABLE.with_process(tid, |p| {
        p.set_state_safe(ProcessState::Ready)
            .expect("Created → Ready (重新投入)");
    });
    assert_eq!(
        PROCESS_TABLE.group_non_zombie_members(child_pid),
        vec![(tid, ProcessState::Ready)],
        "§6③a: 表扫描必须列出「组已收齐却被卡在非 Zombie」的 TCB"
    );
    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        None,
        "§6③a: 出口② 仍判不可收割 (判据未变)"
    );
    assert!(
        child_group.note_reap_diag(SENTINEL),
        "§6③a: 出口② 的未收齐态同样必须记下签名 (取证覆盖两个 None 出口)"
    );

    // 整组落 Zombie: 判据按原口径返回组退出码, 取证名单清空.
    PROCESS_TABLE.with_process(tid, |p| {
        p.set_state_safe(ProcessState::Running)
            .expect("Ready → Running");
        p.set_state_safe(ProcessState::Zombie)
            .expect("Running → Zombie");
    });
    assert!(
        PROCESS_TABLE.group_non_zombie_members(child_pid).is_empty(),
        "全员落 Zombie 后取证名单为空"
    );
    assert_eq!(
        PROCESS_TABLE.group_zombie_exit_code(child_pid),
        Some(42),
        "§6③a: 收齐后的组退出码读数不受诊断影响"
    );
}

/// 组定向信号: 组内择线程遵守每线程屏蔽字; 全部屏蔽时挂起到首个未忽略成员;
/// 不可捕获信号无视屏蔽; 全员退出返回 `-3`, 组不存在返回 `-2`.
#[test]
fn group_signal_delivery_honors_per_thread_blocked_mask() {
    let _serial = lock_table();
    let leader_pid = PROCESS_TABLE.allocate_pid().expect("leader PID");
    let thread_pid = PROCESS_TABLE.allocate_pid().expect("thread PID");

    let leader = Process::new(leader_pid, "k06-sig-leader", None);
    let group = Arc::clone(leader.group());
    let mut thread = Process::new(thread_pid, "k06-sig-thread", None);
    assert!(thread.join_thread_group(Arc::clone(&group)));
    let _leader = TableProc::insert_owned(leader);
    let _thread = TableProc::insert_owned(thread);

    // Running 在 unblock 判据中是"已在核上", 测试不触碰运行队列.
    set_running(leader_pid);
    set_running(thread_pid);

    let sigusr1 = 10u8;
    let bit = 1u64 << sigusr1;
    let mask_of = |pid: u32| {
        PROCESS_TABLE
            .with_process(pid, |p| p.blocked_mask.load(Ordering::Acquire))
            .unwrap()
    };
    let pending_of = |pid: u32| {
        PROCESS_TABLE
            .with_process(pid, Process::signal_pending_get)
            .unwrap()
    };
    let clear_pending = |pid: u32, bits: u64| {
        PROCESS_TABLE.with_process(pid, |p| p.signal_pending_clear(bits));
    };

    assert_eq!(
        do_signal_send_group(u32::MAX, sigusr1),
        Err(-2),
        "不存在的组应返回 -2"
    );

    // 组长屏蔽, 成员未屏蔽 → 只投给成员.
    PROCESS_TABLE.with_process(leader_pid, |p| {
        p.blocked_mask.store(bit, Ordering::Release);
    });
    assert_eq!(mask_of(leader_pid) & bit, bit);
    assert_eq!(do_signal_send_group(leader_pid, sigusr1), Ok(1));
    assert_eq!(pending_of(leader_pid) & bit, 0, "屏蔽线程不得收到该信号");
    assert_eq!(pending_of(thread_pid) & bit, bit, "未屏蔽成员必须收到");

    // 全员屏蔽 → 挂起到首个未忽略成员 (成员账顺序 = leader 先), 不唤醒.
    clear_pending(thread_pid, bit);
    PROCESS_TABLE.with_process(thread_pid, |p| {
        p.blocked_mask.store(bit, Ordering::Release);
    });
    assert_eq!(do_signal_send_group(leader_pid, sigusr1), Ok(1));
    assert_eq!(
        pending_of(leader_pid) & bit,
        bit,
        "全部屏蔽时挂起到首个成员"
    );
    assert_eq!(pending_of(thread_pid) & bit, 0);

    // 不可捕获信号 (SIGKILL) 无视屏蔽, 投给首个存活成员.
    clear_pending(leader_pid, bit);
    let sigkill = 9u8;
    let kill_bit = 1u64 << sigkill;
    assert_eq!(do_signal_send_group(leader_pid, sigkill), Ok(1));
    assert_eq!(pending_of(leader_pid) & kill_bit, kill_bit);
    assert_eq!(pending_of(thread_pid) & kill_bit, 0);

    // 全员 Zombie → 无可投递目标.
    clear_pending(leader_pid, kill_bit | bit);
    set_zombie(leader_pid);
    set_zombie(thread_pid);
    assert_eq!(
        do_signal_send_group(leader_pid, sigusr1),
        Err(-3),
        "组内全员退出应返回 -3"
    );
}

// ============================================================================
// 源码契约
// ============================================================================

/// `exit` / `exit_group` 不再合并: dispatch 分派两个策略, 策略调用不同机制入口.
#[test]
fn exit_and_exit_group_take_separate_paths() {
    let dispatch = code_only(&read_src(FUNC_DISPATCH_SRC));
    assert!(
        has(
            &dispatch,
            "SYS_EXIT => crate::functions::proc::lifecycle::exit_syscall("
        ),
        "K-06: SYS_EXIT 必须独立分派到 exit_syscall"
    );
    assert!(
        has(
            &dispatch,
            "SYS_EXIT_GROUP => crate::functions::proc::lifecycle::exit_group_syscall("
        ),
        "K-06: SYS_EXIT_GROUP 必须独立分派到 exit_group_syscall"
    );
    assert!(
        !has(&dispatch, "SYS_EXIT | SYS_EXIT_GROUP"),
        "K-06: 两个系统调用不得再共用同一匹配臂"
    );

    let lifecycle = code_only(&read_src(LIFECYCLE_SRC));
    let exit = fn_body(&lifecycle, "pub fn exit_syscall(status: i32) -> i64");
    assert!(
        has(&exit, "process_exit(status.cast_unsigned());") && !has(&exit, "process_exit_group"),
        "K-06: exit() 只结束调用线程"
    );
    let group_exit = fn_body(&lifecycle, "pub fn exit_group_syscall(status: i32) -> i64");
    assert!(
        has(&group_exit, "process_exit_group(status.cast_unsigned());"),
        "K-06: exit_group() 走组退出机制入口"
    );
}

/// getpid 读 tgid, gettid 读 TID.
#[test]
fn getpid_reads_tgid_and_gettid_reads_tid() {
    let info = code_only(&read_src(INFO_SRC));
    let getpid = fn_body(&info, "pub fn getpid_syscall() -> usize");
    assert!(
        has(&getpid, "api::process_get_current_tgid() as usize"),
        "K-06: getpid 必须返回线程组 tgid"
    );
    let gettid = fn_body(&info, "pub fn gettid_syscall() -> usize");
    assert!(
        has(&gettid, "api::process_get_current_pid() as usize"),
        "K-06: gettid 必须返回本 TCB 的 TID"
    );

    let ops = code_only(&read_src(PROC_OPS_SRC));
    let current_tgid = fn_body(&ops, "pub fn process_get_current_tgid() -> u32");
    assert!(
        has(
            &current_tgid,
            "PROCESS_TABLE.with_process(pid, super::process::Process::tgid)"
        ),
        "K-06: tgid 出口必须委托 Process::tgid (组身份唯一权威)"
    );
}

/// tgkill 先校验 TID 对 tgid 的归属, 再按线程定向投递; sig=0 探测也校验.
#[test]
fn tgkill_validates_group_membership_before_delivery() {
    let signal = code_only(&read_src(FUNC_SIGNAL_SRC));
    let tgkill = fn_body(&signal, "pub fn tgkill_syscall(");
    assert!(
        has(
            &tgkill,
            "let owner_group = crate::privileged::proc::api::process_with(target_tid, |p| p.group().tgid()).ok_or(Errno::Esrch)?;"
        ),
        "K-06: tgkill 的归属判定必须查询目标 TID 所属线程组"
    );
    assert!(
        has(&tgkill, "if owner_group != tgid.cast_unsigned() {")
            && has(&tgkill, "return Err(Errno::Esrch);"),
        "K-06: tgid 不匹配必须返回 ESRCH"
    );
    assert!(
        has(
            &tgkill,
            "crate::privileged::proc::do_signal_send(target_tid, crate::privileged::glue::u8_from_i32(sig))"
        ),
        "K-06: tgkill 必须线程定向投递 (kill 才是组定向)"
    );
    let membership_at = index_of(&tgkill, "process_with(target_tid");
    let probe_at = index_of(&tgkill, "if sig == 0 {");
    assert!(
        membership_at < probe_at,
        "K-06: sig=0 存在性探测同样必须先通过 tgid 归属校验"
    );
}

/// kill 4 种 pid 语义按线程组分发, 信号范围 0..=63.
#[test]
fn kill_routes_by_thread_group() {
    let signal = code_only(&read_src(FUNC_SIGNAL_SRC));
    let kill = fn_body(&signal, "pub fn kill_syscall(pid: i32, sig: i32)");
    assert!(
        has(&kill, "if !(0..=63).contains(&sig) {")
            && has(&kill, "crate::privileged::syscall::api::sys_kill(pid, sig)"),
        "K-06: kill 接受标准 + RT 信号并委托 privileged"
    );

    let dispatch = code_only(&read_src(PRIV_DISPATCH_SRC));
    let sys_kill = fn_body(
        &dispatch,
        "pub(crate) fn sys_kill(pid: i32, sig: i32) -> i64",
    );
    assert!(
        has(&sys_kill, "if !(0..=63).contains(&sig) {")
            && has(&sys_kill, "do_signal_send_extended("),
        "K-06: privileged kill 入口同步放行 RT 信号"
    );

    let priv_signal = code_only(&read_src(PRIV_SIGNAL_SRC));
    let extended = fn_body(
        &priv_signal,
        "pub fn do_signal_send_extended(pid: i32, sig: u8) -> Result<usize, i32>",
    );
    assert!(
        has(
            &extended,
            "let tgid = PROCESS_TABLE.with_process(target, |proc| proc.thread_group.tgid()).ok_or(-2)?;"
        ) && has(&extended, "do_signal_send_group(tgid, sig)"),
        "K-06: kill(tid/tgid) 必须按目标所属线程组投递"
    );
    assert_eq!(
        extended
            .matches("collect_matching_tgids(|proc| proc.group().pgid() == target_pgid)")
            .count(),
        2,
        "K-06: kill(0)/kill(-pgid) 的进程组匹配必须读线程组 pgid"
    );
    assert!(
        has(&extended, "collect_matching_tgids(|proc| proc.pid.0 != 1)"),
        "K-06: kill(-1) 广播到全部进程 (init 除外)"
    );
    assert_eq!(
        extended.matches("deliver_to_groups(&tgids, sig)").count(),
        3,
        "K-06: 三条广播路径逐个线程组投递 (组内只命中一次)"
    );

    let deliver = fn_body(
        &priv_signal,
        "fn deliver_to_groups(tgids: &[Pid], sig: u8) -> Result<usize, i32>",
    );
    assert!(
        has(&deliver, "if do_signal_send_group(tgid, sig).is_ok() {")
            && has(&deliver, "if count > 0 { Ok(count) } else { Err(-2) }"),
        "K-06: 广播按 tgid 逐组投递并统计命中组数"
    );
}

/// 组内择线程: 不可捕获信号优先, 忽略的成员不接收, 屏蔽成员只作挂起候选.
#[test]
fn group_sender_selection_follows_per_thread_state() {
    let priv_signal = code_only(&read_src(PRIV_SIGNAL_SRC));
    let group_send = fn_body(
        &priv_signal,
        "pub fn do_signal_send_group(tgid: Pid, sig: u8) -> Result<usize, i32>",
    );
    assert!(
        has(&group_send, "let members = group.members();")
            && has(&group_send, "if members.is_empty() {")
            && has(&group_send, "return Err(-3);"),
        "K-06: 投递选集来自线程组成员账, 空组返回 ESRCH"
    );
    assert!(
        has(&group_send, "let uncatchable = is_uncatchable(sig);")
            && has(&group_send, "if uncatchable { return 1u8; }"),
        "K-06: SIGKILL/SIGSTOP 无视屏蔽投给首个存活成员"
    );
    assert!(
        has(
            &group_send,
            "let ignored = proc.sigaction_table.lock()[(sig - 1) as usize] == SIG_IGN;"
        ) && has(&group_send, "if ignored { 2u8 }"),
        "K-06: 显式忽略的成员不接收, 但 kill 仍算成功"
    );
    assert!(
        has(
            &group_send,
            "proc.blocked_mask.load(Ordering::Acquire) & bit != 0"
        ) && has(
            &group_send,
            "} else if proc.blocked_mask.load(Ordering::Acquire) & bit != 0 {"
        ),
        "K-06: 每线程屏蔽字决定直接投递还是挂起"
    );
    assert!(
        has(&group_send, "if let Some(tid) = blocked_candidate {")
            && has(&group_send, "return Ok(1);"),
        "K-06: 全员屏蔽时挂起到首个未忽略成员且不唤醒"
    );
    assert!(
        has(
            &group_send,
            "if state == ProcessState::Zombie || state == ProcessState::Terminated { return 0u8; }"
        ),
        "K-06: 已退出成员不是投递目标"
    );
}

/// wait4 只收割线程组: 就绪 = 组退出且全员 Zombie; 认领 CAS 失败视为已收割.
#[test]
fn wait4_reaps_thread_groups_only() {
    let wait4 = code_only(&read_src(WAIT4_SRC));
    let wait_reap = fn_body(
        &wait4,
        "pub fn wait_reap(target_pid: i32, non_blocking: bool, keep_zombie: bool) -> WaitOutcome",
    );
    assert!(
        has(
            &wait_reap,
            "let exit_code = api::process_group_exit_code(child_tgid);"
        ),
        "K-06: wait4 就绪判定读组退出码"
    );
    assert!(
        has(
            &wait_reap,
            "if api::process_with(child_tgid, |p| p.group().tgid()).is_none() {"
        ),
        "K-06: 组长 TCB 消失即视为已被并发等待者收割"
    );
    assert!(
        has(&wait_reap, "return reap_group(")
            && has(
                &wait_reap,
                "if non_blocking { return WaitOutcome::Running; }"
            ),
        "K-06: 非阻塞等待只返回组就绪状态"
    );

    let reap_group = fn_body(
        &wait4,
        "fn reap_group(child_tgid: u32, exit_code: u32, keep_zombie: bool) -> WaitOutcome",
    );
    assert!(
        has(
            &reap_group,
            "if !keep_zombie && !api::process_reap_group(child_tgid) {"
        ) && has(&reap_group, "pid: child_tgid,"),
        "K-06: 收割单位是线程组 tgid, WNOWAIT 不释放 Zombie"
    );

    let find = fn_body(
        &wait4,
        "fn find_waitable_child(parent_pid: u32, target_pid: i32) -> Option<u32>",
    );
    assert!(
        has(
            &find,
            "if api::process_with(child_tgid, |p| p.group().tgid()).is_none() { continue; }"
        ),
        "K-06: 非组长 TID 不是可等待对象, 已回收组跳过"
    );

    let process = code_only(&read_src(PROCESS_SRC));
    let zombie_code = fn_body(
        &process,
        "pub fn group_zombie_exit_code(&self, tgid: Pid) -> Option<u32>",
    );
    assert!(
        has(&zombie_code, "if !group.has_exited() {")
            && has(&zombie_code, "return None;")
            && has(
                &zombie_code,
                "if p.thread_group.tgid() == tgid && p.get_state() != ProcessState::Zombie {"
            ),
        "K-06: 组未退出或仍有成员退出途中都不可收割"
    );
    // §6③a 取证行自身的形状锁见 `reap_evidence_diag_shape_contract_locked`.

    let reap = fn_body(
        &process,
        "pub fn reap_zombie_group(&self, tgid: Pid) -> bool",
    );
    assert!(
        has(
            &reap,
            "if !all_zombie || tids.is_empty() || !group.claim_reap() { return false; }"
        ),
        "K-06: 收割必须全员 Zombie 且认领 CAS 唯一获胜"
    );
    let detach_at = index_of(&reap, "p.group().remove_child(tgid)");
    let free_at = index_of(&reap, "self.remove_and_free(tid);");
    assert!(
        detach_at < free_at,
        "K-06: 必须先摘父组子账再释放 TCB (tgid 释放后可复用)"
    );
    assert!(
        has(&reap, "let _ = p.set_state_safe(ProcessState::Terminated);"),
        "K-06: 回收前先落 Terminated (Zombie → Terminated 是唯一合法迁移)"
    );
}

/// §6③a 甲 取证形状锁: `group_zombie_exit_code` 的两个 `None` 出口都调取证, 收窄判据
/// (`exit_requested` / 空名单 / 签名去重) 与只读性 (不触碰认领与回收) 逐条钉住;
/// 取证名单来源钉为进程表扫描, 不得退回成员账.
#[test]
fn reap_evidence_diag_shape_contract_locked() {
    let process = code_only(&read_src(PROCESS_SRC));
    let zombie_code = fn_body(
        &process,
        "pub fn group_zombie_exit_code(&self, tgid: Pid) -> Option<u32>",
    );
    assert!(
        has(
            &zombie_code,
            "if !group.has_exited() { self.diag_unreaped_group(tgid, &group); return None; }"
        ) && has(
            &zombie_code,
            "if all_zombie { return Some(group.exit_code()); } \
             self.diag_unreaped_group(tgid, &group); None"
        ),
        "§6③a: 未收齐的两类现场都要取证, 且随后照旧返回 None"
    );

    let diag = fn_body(
        &process,
        "fn diag_unreaped_group(&self, tgid: Pid, group: &ThreadGroup)",
    );
    assert!(
        has(&diag, "if !group.exit_requested() { return; }")
            && has(&diag, "if stuck.is_empty() { return; }")
            && has(&diag, "if !group.note_reap_diag(sig) { return; }")
            && has(&diag, "\"[REAP] tgid={} unreaped pid={} state={}\"")
            && has(&diag, "state.name()"),
        "§6③a: 取证行必须 exit_requested 收窄 + 空名单静默 + 签名去重后才打印 pid|state"
    );
    assert!(
        !has(&diag, "claim_reap(") && !has(&diag, "remove_and_free("),
        "§6③a: 取证行只读, 不得触碰认领与回收"
    );

    let stuck_list = fn_body(
        &process,
        "pub fn group_non_zombie_members(&self, tgid: Pid) -> alloc::vec::Vec<(Pid, ProcessState)>",
    );
    assert!(
        has(
            &stuck_list,
            "if proc.thread_group.tgid() == tgid { let state = proc.get_state(); \
             if state != ProcessState::Zombie { out.push((proc.pid.0, state)); } }"
        ) && !has(&stuck_list, ".members()"),
        "§6③a: 取证名单必须按 tgid 扫进程表 (成员账在 L-2 形态下是空表, 看不到现场)"
    );
}

/// exec `de_thread`: 标记+快照同锁域, 兄弟线程逐个 SIGKILL, 组长 TCB 留作锚点.
#[test]
fn exec_de_thread_terminates_siblings_and_anchors_leader() {
    let ops = code_only(&read_src(PROC_OPS_SRC));
    let de = fn_body(&ops, "fn exec_de_thread(caller: Pid) -> bool");
    assert!(
        has(&de, "let members = group.begin_exec_de_thread_snapshot();"),
        "K-06: 标记与成员快照必须在同一成员表锁域完成"
    );
    assert!(
        has(
            &de,
            "if members.len() <= 1 { group.end_exec_de_thread(); return true; }"
        ),
        "K-06: 单线程组无需 de_thread, 直接撤销标记"
    );
    assert!(
        has(
            &de,
            "p.signal_pending_set(u32::from(super::signal::SIGKILL));"
        ) && has(&de, "Scheduler::unblock(tid);"),
        "K-06: 兄弟线程收到 SIGKILL 并由调度器唤醒"
    );
    assert!(
        has(&de, "None if !still_member => {}") && has(&de, "None => all_gone = false,"),
        "K-06: 等待循环区分已回滚成员与登记途中的新成员"
    );
    assert!(
        has(
            &de,
            "if tid != caller && tid != tgid { PROCESS_TABLE.remove_and_free(tid); }"
        ),
        "K-06: 回收非组长兄弟; 组长 TCB 保留为 tgid 锚点"
    );
    assert!(
        has(&de, "group.end_exec_de_thread();") && has(&de, "true"),
        "K-06: de_thread 完成后撤销标记, 恢复入组"
    );

    let exec = fn_body(&ops, "pub extern \"C\" fn proc_exec_replace(");
    let de_at = index_of(&exec, "if !exec_de_thread(current_pid) {");
    let replace_at = index_of(&exec, "if !USER_PROC_MANAGER.replace_user_space(");
    assert!(de_at < replace_at, "K-06: 必须先结束兄弟线程再更换地址空间");
    assert!(
        has(
            &exec,
            "p.sigaction_table = alloc::sync::Arc::new(crate::privileged::sync::IrqSpinLock::new([0u64; 64]));"
        ),
        "K-06: exec 后信号动作表必须是私有新实例"
    );
    assert!(
        has(
            &exec,
            "let new_fs = Arc::new(super::fs_context::FsContext::new_default()); new_fs.copy_from(&old_fs);"
        ),
        "K-06: exec 后 FS 上下文 unshare 为私有副本"
    );
    assert!(
        has(
            &exec,
            "crate::privileged::proc::reset_signal_state_on_exec(current_pid);"
        ),
        "K-06: exec 后清除每线程待处理信号与屏蔽字"
    );

    let func_exec = code_only(&read_src(FUNC_EXEC_SRC));
    assert!(
        !has(&func_exec, "member_count") && !has(&func_exec, "threads().len()"),
        "K-06: exec 不再以多线程 fail-fast 拒绝"
    );
}

/// clone 入组被 `de_thread` 拒绝时按 EAGAIN 可重试; `CLONE_FS` 已放开, `CLONE_FILES` 仍限线程.
#[test]
fn clone_refuses_group_join_during_exec() {
    let clone = code_only(&read_src(CLONE_SRC));
    let register = fn_body(&clone, "fn vm_child_register(");
    assert!(
        has(&register, "if !child.join_thread_group(group) {")
            && has(&register, "return Err(Errno::Eagain.as_ret());"),
        "K-06: de_thread 期间入组失败必须回滚 PCB 并按 EAGAIN 返回"
    );
    assert!(
        has(
            &clone,
            "if (flags & CLONE_FILES) != 0 && (flags & CLONE_THREAD) == 0 {"
        ),
        "K-06: fd 表 unshare 尚未实装, CLONE_FILES 仍只接受线程"
    );
    assert!(
        !has(
            &clone,
            "if (flags & CLONE_FS) != 0 && (flags & CLONE_THREAD) == 0 {"
        ) && has(
            &clone,
            "if (flags & CLONE_FS) != 0 && (flags & CLONE_VM) == 0 {"
        ),
        "K-06: CLONE_FS 对共享 VM 的独立进程放开 (exec 总会拆私有副本)"
    );

    let group = code_only(&read_src(THREAD_GROUP_SRC));
    let join = fn_body(&group, "pub fn join(&self, tid: Pid) -> bool");
    assert!(
        has(
            &join,
            "let mut members = self.members.lock(); if self.exec_de_thread.load(Ordering::Acquire) || self.exit_requested.load(Ordering::Acquire) { return false; }"
        ) && has(&join, "members.push(tid); true"),
        "K-06: join 与 de_thread 标记 / 组退出请求共用成员表锁, 杜绝幽灵入组"
    );
    let snapshot = fn_body(
        &group,
        "pub fn begin_exec_de_thread_snapshot(&self) -> Vec<Pid>",
    );
    assert!(
        has(
            &snapshot,
            "let members = self.members.lock(); self.exec_de_thread.store(true, Ordering::Release); members.clone()"
        ),
        "K-06: 置标记与快照必须在同一临界区"
    );

    let process = code_only(&read_src(PROCESS_SRC));
    let join_group = fn_body(
        &process,
        "pub fn join_thread_group(&mut self, group: Arc<ThreadGroup>) -> bool",
    );
    assert!(
        has(
            &join_group,
            "if !group.join(self.pid.0) { return false; } self.thread_group = group; true"
        ),
        "K-06: 入组被拒时保留原组, 不得半途改写身份"
    );
}

/// 线程切换的用户页表取权威 `Process.cr3`; 镜像仅为 current 查询缓存.
#[test]
fn context_switch_uses_authority_cr3_not_mirror() {
    let sched = code_only(&read_src(SCHED_SRC));
    let update = fn_body(
        &sched,
        "fn update_user_context(next: Pid, next_kernel_stack: u64)",
    );
    assert!(
        has(
            &update,
            "let user_cr3 = PROCESS_TABLE.with_process(next, |p| p.cr3.load(Ordering::SeqCst)).unwrap_or(0);"
        ),
        "K-06: 切换页表必须读权威 Process.cr3, 线程无镜像也能切换"
    );
    assert!(
        has(
            &update,
            "set_current(super::user_proc::USER_PROC_MANAGER.get(next));"
        ),
        "K-06: 镜像只作为 current 镜像缓存, 缺失即为 None"
    );
    assert!(
        !has(&update, "let Some("),
        "K-06: 镜像缺失不得成为切换用户的门条件"
    );
    assert!(
        has(&update, "if user_cr3 == 0 { return; }"),
        "K-06: 无用户地址空间 (内核线程) 不切换 cr3"
    );

    let user_proc = code_only(&read_src(USER_PROC_SRC));
    let replace = fn_body(&user_proc, "pub fn replace_user_space(");
    assert!(
        has(
            &replace,
            "let existing = self.processes.lock().get(&pid).copied();"
        ) && has(
            &replace,
            "let proc = if let Some(proc) = existing { proc } else {"
        ) && has(
            &replace,
            "let Some(proc_ptr) = raw::alloc_user_process(kproc_nn) else { return false; };"
        ) && has(&replace, "self.processes.lock().insert(pid, proc_nn);"),
        "K-06: exec 的线程调用方缺镜像时必须先补建再接管新映像"
    );
}

/// 致命信号默认动作: `de_thread` 期间只结束目标线程, 其余终止整组.
#[test]
fn fatal_signal_routes_to_group_exit_except_de_thread() {
    let priv_signal = code_only(&read_src(PRIV_SIGNAL_SRC));
    let terminate = fn_body(
        &priv_signal,
        "fn terminate_from_fatal_signal(pid: Pid, sig: u8)",
    );
    assert!(
        has(&terminate, "let code = u32::from(sig) << 8 | 0x7f;"),
        "K-06: 信号致死退出码 = 128 + signum"
    );
    assert!(
        has(
            &terminate,
            "let exec_de_thread = PROCESS_TABLE.with_process(pid, |proc| proc.group().exec_de_thread()).unwrap_or(false);"
        ),
        "K-06: 终止范围必须读线程组 de_thread 标记"
    );
    let single_at = index_of(&terminate, "crate::privileged::proc::process_exit(code);");
    let group_at = index_of(
        &terminate,
        "crate::privileged::proc::process_exit_group(code);",
    );
    assert!(
        single_at < group_at,
        "K-06: de_thread 分支只结束目标线程, 否则会带走正在 exec 的调用方"
    );

    let ops = code_only(&read_src(PROC_OPS_SRC));
    let exit_group = fn_body(
        &ops,
        "pub extern \"C\" fn process_exit_group(exit_code: u32)",
    );
    assert!(
        has(
            &exit_group,
            "let Some(group) = PROCESS_TABLE.with_process(current_pid, |p| { let group = Arc::clone(p.group()); group.request_exit(exit_code); group }) else {"
        ),
        "K-06: 组退出请求必须在进程表锁内发布 (Scheduler::exit 的成对读取持同一把锁)"
    );
    assert!(
        has(&exit_group, "group.request_exit(exit_code);"),
        "K-06: exit_group 首个退出码是组权威退出码"
    );
    assert!(
        has(&exit_group, "for tid in group.members() {")
            && has(
                &exit_group,
                "p.signal_pending_set(u32::from(super::signal::SIGKILL));"
            ),
        "K-06: 组退出向其余成员登记不可屏蔽 SIGKILL"
    );
    assert!(
        has(&exit_group, "process_exit(exit_code);"),
        "K-06: 调用线程自身经单线程退出路径收口"
    );
}

/// 内核等待循环: yield 感知组退出或 `de_thread` 的 SIGKILL, 立即退出不返回用户态.
#[test]
fn yield_aborts_kernel_waiters_on_group_kill() {
    let sched = code_only(&read_src(SCHED_SRC));
    let yield_body = fn_body(&sched, "pub fn yield_current()");
    assert!(
        has(
            &yield_body,
            "p.signal_pending_get() & (1u64 << super::signal::SIGKILL) != 0"
        ),
        "K-06: 等待循环只对 SIGKILL 提前退出 (不可捕获/屏蔽)"
    );
    assert!(
        has(
            &yield_body,
            "&& (p.group().exit_requested() || p.group().exec_de_thread())"
        ),
        "K-06: 提前退出限定组退出 / exec de_thread, 普通 SIGKILL 仍走用户态投递"
    );
    assert!(
        has(
            &yield_body,
            "crate::privileged::proc::process_exit(u32::from(super::signal::SIGKILL) << 8 | 0x7f"
        ),
        "K-06: 等待方直接走单线程退出 (de_thread 的兄弟只结束自身)"
    );
}

/// `clear_child_tid` 与 futex 唤醒按 TID 在销毁地址空间前收口.
#[test]
fn clear_child_tid_is_per_thread_and_woken() {
    let robust = code_only(&read_src(ROBUST_SRC));
    let cleanup = fn_body(&robust, "pub fn exit_cleanup(pid: u32)");
    assert!(
        has(
            &cleanup,
            "let clear_tid = api::process_with(pid, |p| p.clear_child_tid.load(Ordering::Acquire)).unwrap_or(0);"
        ),
        "K-06: clear_child_tid 按退出 TID 读取 (每线程字段)"
    );
    assert!(
        has(&cleanup, "write_u32_user(clear_tid, 0);")
            && has(
                &cleanup,
                "crate::privileged::syscall::futex::futex_wake(clear_tid, 1);"
            ),
        "K-06: 清零后必须唤醒该地址上的 futex 等待者"
    );
    assert!(
        has(
            &cleanup,
            "api::process_with(pid, |p| p.clear_child_tid.store(0, Ordering::Release));"
        ),
        "K-06: 清理后复位字段, 避免 TID 复用时误写旧地址"
    );
}

/// `exit_group` 与 `clone` 并发: 入组窗口互斥, 快照漏掉的线程入队前自挂 SIGKILL,
/// 登记前失败路径归还 PID 位图槽位.
#[test]
fn concurrent_clone_cannot_escape_group_exit() {
    let group_src = code_only(&read_src(THREAD_GROUP_SRC));
    let request = fn_body(&group_src, "pub fn request_exit(&self, exit_code: u32)");
    assert!(
        has(
            &request,
            "let _members = self.members.lock(); if !self.exit_requested.swap(true, Ordering::AcqRel) { self.requested_exit_code.store(exit_code, Ordering::Release); }"
        ),
        "K-06: 退出请求与 join 共用成员表锁, 成功入组者必进退出快照"
    );
    let join = fn_body(&group_src, "pub fn join(&self, tid: Pid) -> bool");
    assert!(
        has(&join, "self.exit_requested.load(Ordering::Acquire)"),
        "K-06: 已发起组退出的组不得再接纳新线程"
    );

    let clone_src = code_only(&read_src(CLONE_SRC));
    let register = fn_body(&clone_src, "fn vm_child_register(");
    let kill_at = index_of(&register, "child.thread_group.exit_requested()");
    let enqueue_at = index_of(&register, "api::scheduler_add_to_run_queue(child_pid);");
    assert!(
        kill_at < enqueue_at,
        "K-06: 入组早于退出请求、登记晚于成员快照的线程必须在入队前自挂 SIGKILL"
    );

    let vm_path = fn_body(&clone_src, "fn clone_vm_path(");
    assert_eq!(
        vm_path.matches("api::proc_free_pid(child_pid);").count(),
        3,
        "K-06: alloc 后、进程表登记前的每条失败路径都要归还 PID 槽位"
    );
    assert!(
        !clone_src.contains("PROCESS_TABLE"),
        "K-06: PID 槽位归还经 privileged api 门面, functions 层不直接引用进程表全局"
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
