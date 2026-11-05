#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。成员账只经 `IrqSpinLock` 与原子量访问。
//!
//! 线程组 — 多线程进程的共享身份与成员账（K-01）
//!
//! ## 口径（D7 / K-01）
//!
//! 线程映射为 1:1：每个用户线程对应一个 [`Process`](super::process::Process)
//! （TCB），其 `pid` 即 TID。`ThreadGroup` 是**组身份**的唯一载体：`tgid` 在一次
//! 组生命周期内不变，组内所有线程共享同一个 `Arc`。
//!
//! - 组长 = 首个线程；单线程进程的 `tgid == pid`。
//! - 成员表只保存 TID，**不持有** `Process` 引用 —— 组不能延长 TCB 生命，
//!   避免与调度器/进程表的回收协议成环（组的内存随最后一个 TCB 的 `Arc`
//!   引用消失而释放）。
//! - 退出状态只认首次置位者（`exit_group` 或最后一线程退出），后到者不覆盖
//!   退出码。
//! - 共享资源句柄（fd 表 / sigaction / FS 状态）归 K-03；父进程/子进程账与
//!   POSIX `session_id`/`pgid` 归 K-04 —— 这些是进程级身份，不能按 TCB 分裂。
//!
//! ## 锁域
//!
//! 成员表与子进程账是进程属性级锁（`docs/explain/ref-lock-order.md` 层级 9），
//! 与进程级 FS 上下文同层。持进程表锁（层级 7）时可以获取它们；反向（持成员
//! 账锁时取进程表锁或任何 rq 锁）禁止。`parent` / `session_id` / `pgid` 为
//! 原子量，无独立锁域。

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::privileged::proc::Pid;
use crate::privileged::sync::IrqSpinLock as Mutex;

/// 线程组 — 组身份（tgid）、成员账、进程关系与退出状态。
pub struct ThreadGroup {
    /// 组 ID（= 组长 TID），生命周期内不变。
    tgid: Pid,
    /// 组内存活线程的 TID 集合（受锁保护）。
    members: Mutex<Vec<Pid>>,
    /// 父进程组 TGID；0 表示无父进程（idle / init / 已脱离）。
    parent: AtomicU32,
    /// 子进程组 TGID 列表（受锁保护；线程不入账）。
    children: Mutex<Vec<Pid>>,
    /// POSIX 会话 ID（`sid`）；0 表示未加入会话。
    session_id: AtomicU64,
    /// POSIX 进程组 ID（`pgid`）；0 表示未显式设置，有效值回落 `tgid`。
    pgid: AtomicU32,
    /// 组是否已退出（全部线程已离开）；仅首次置位生效。
    exited: AtomicBool,
    /// 组退出码；仅在 `exited == true` 时有意义。
    exit_code: AtomicU32,
    /// 组退出请求是否已发起（`exit_group` / 致命信号）；仅首次置位生效。
    exit_requested: AtomicBool,
    /// 请求的组退出码；仅在 `exit_requested == true` 时有意义。
    requested_exit_code: AtomicU32,
    /// 组收割认领标志：CAS 成功者独占回收组内全部僵尸 TCB。
    ///
    /// 多个 `wait4` 等待者与 tick 回收可能同时看到"组已退出"；若各自调用
    /// `remove_and_free`，同一 TCB 会被重复递减引用计数（PCB 提前释放 = UAF）。
    /// 认领失败的一方按"已被他人收割"处理。
    reap_claimed: AtomicBool,
    /// `exec` `de_thread` 标记：兄弟线程收到 SIGKILL 后按单线程退出而非组退出，
    /// 避免把正在 exec 的调用线程一并带走。
    exec_de_thread: AtomicBool,
    /// 「组未收齐」取证行的上一条签名（§6③a）；0 表示尚未打印过。
    ///
    /// 只作诊断限流，**不参与任何收割判据**：签名由未收齐成员的 `tid|state`
    /// 导出，同一状态被轮询方反复读到时只放行首行。
    reap_diag: AtomicU64,
}

impl ThreadGroup {
    /// 以首个线程为组长创建单线程组（`tgid == leader`）。
    #[must_use]
    pub fn new_leader(leader: Pid) -> Self {
        Self {
            tgid: leader,
            members: Mutex::new(alloc::vec![leader]),
            parent: AtomicU32::new(0),
            children: Mutex::new(Vec::new()),
            session_id: AtomicU64::new(0),
            pgid: AtomicU32::new(leader),
            exited: AtomicBool::new(false),
            exit_code: AtomicU32::new(0),
            exit_requested: AtomicBool::new(false),
            requested_exit_code: AtomicU32::new(0),
            reap_claimed: AtomicBool::new(false),
            exec_de_thread: AtomicBool::new(false),
            reap_diag: AtomicU64::new(0),
        }
    }

    /// 返回组 ID（tgid）。
    #[must_use]
    pub fn tgid(&self) -> Pid {
        self.tgid
    }

    /// 返回父进程组 TGID；0 表示无父进程。
    ///
    /// 组内所有线程读到同一值（`getppid` 的进程语义）。
    #[must_use]
    pub fn parent(&self) -> Pid {
        self.parent.load(Ordering::Acquire)
    }

    /// 设置父进程组 TGID（0 表示无父进程）。
    pub fn set_parent(&self, parent: Pid) {
        self.parent.store(parent, Ordering::Release);
    }

    /// 返回子进程组 TGID 快照（组内线程在 `wait4` 中看到同一份账）。
    #[must_use]
    pub fn children(&self) -> Vec<Pid> {
        self.children.lock().clone()
    }

    /// 登记子进程组 TGID。
    ///
    /// 调用方保证 `tgid` 是新分配且未登记的组；重复登记会使子进程账失真。
    pub fn add_child(&self, tgid: Pid) {
        self.children.lock().push(tgid);
    }

    /// 清空子进程账（组退出且子组已改挂后调用）。
    pub fn clear_children(&self) {
        self.children.lock().clear();
    }

    /// 返回 POSIX 会话 ID；0 表示未加入会话。
    #[must_use]
    pub fn session_id(&self) -> u64 {
        self.session_id.load(Ordering::Acquire)
    }

    /// 设置 POSIX 会话 ID（0 表示未加入会话）。
    pub fn set_session_id(&self, sid: u64) {
        self.session_id.store(sid, Ordering::Release);
    }

    /// 返回有效进程组 ID：未显式设置（0）时回落 `tgid`。
    ///
    /// POSIX 新建进程自成一组，故有效值恒非 0。
    #[must_use]
    pub fn pgid(&self) -> Pid {
        let pgid = self.pgid.load(Ordering::Acquire);
        if pgid == 0 { self.tgid } else { pgid }
    }

    /// 设置进程组 ID；0 表示回落 `tgid`。
    pub fn set_pgid(&self, pgid: Pid) {
        self.pgid.store(pgid, Ordering::Release);
    }

    /// 返回成员 TID 快照。
    #[must_use]
    pub fn members(&self) -> Vec<Pid> {
        self.members.lock().clone()
    }

    /// 返回当前成员数。
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.members.lock().len()
    }

    /// 新线程入组时登记其 TID（K-02）；`exec` `de_thread` 或组退出请求发布后
    /// 拒绝入组。
    ///
    /// 与 [`ThreadGroup::begin_exec_de_thread_snapshot`] /
    /// [`ThreadGroup::request_exit`] 共用成员表锁，形成互斥窗口：要么本线程出现
    /// 在 `de_thread` / `exit_group` 的起始快照中（随后被终止），要么入组失败。
    /// 否则 `exec` 完成后会出现一个仍停留在旧映像的"幽灵线程"，`exit_group`
    /// 也会漏掉快照之后才入组的成员。
    ///
    /// 调用方保证 `tid` 是新分配且未登记的 TID；重复登记会使成员账失真。
    #[must_use]
    pub fn join(&self, tid: Pid) -> bool {
        let mut members = self.members.lock();
        if self.exec_de_thread.load(Ordering::Acquire)
            || self.exit_requested.load(Ordering::Acquire)
        {
            return false;
        }
        members.push(tid);
        true
    }

    /// 线程退出时移除其成员身份；返回移除后组是否已无成员。
    ///
    /// 幂等：成员不存在时按"已移除"处理（重复退出不改变结果）。
    #[must_use]
    pub fn leave(&self, tid: Pid) -> bool {
        let mut members = self.members.lock();
        members.retain(|member| *member != tid);
        members.is_empty()
    }

    /// 从子进程账中移除已收割的子组 TGID；返回是否存在。
    pub fn remove_child(&self, tgid: Pid) -> bool {
        let mut children = self.children.lock();
        if let Some(pos) = children.iter().position(|child| *child == tgid) {
            children.remove(pos);
            true
        } else {
            false
        }
    }

    /// 登记组退出请求（`exit_group` / 致命信号）；仅首次生效。
    ///
    /// 请求码是该组全部线程最终 `exit_code` 的一致来源；后到的请求不覆盖首个
    /// 请求码（与 [`ThreadGroup::mark_exited`] 的首次写入口径一致）。
    ///
    /// 持成员表锁发布：与 [`ThreadGroup::join`] 互斥，成功入组者必在请求之后
    /// 的成员快照内。退出标志与退出码在同一临界区写入，成对读取方
    /// （`process_exit_group` / `Scheduler::exit` 持进程表锁）不会看到中间态。
    pub fn request_exit(&self, exit_code: u32) {
        let _members = self.members.lock();
        if !self.exit_requested.swap(true, Ordering::AcqRel) {
            self.requested_exit_code.store(exit_code, Ordering::Release);
        }
    }

    /// 组退出请求是否已发起。
    ///
    /// 与 [`ThreadGroup::requested_exit_code`] 成对读取的调用方须在进程表锁内
    /// （[`ThreadGroup::request_exit`] 的发布点与该锁同域），否则会读到
    /// "标志已置、码未写"的中间态。
    #[must_use]
    pub fn exit_requested(&self) -> bool {
        self.exit_requested.load(Ordering::Acquire)
    }

    /// 请求的组退出码；仅在 [`ThreadGroup::exit_requested`] 为真时有意义。
    #[must_use]
    pub fn requested_exit_code(&self) -> u32 {
        self.requested_exit_code.load(Ordering::Acquire)
    }

    /// 标记组已退出；仅首次生效（后到的退出码不覆盖首个）。
    pub fn mark_exited(&self, exit_code: u32) {
        if !self.exited.swap(true, Ordering::AcqRel) {
            self.exit_code.store(exit_code, Ordering::Release);
        }
    }

    /// 组是否已退出（全部线程已离开）。
    #[must_use]
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// 组退出码；仅在 [`ThreadGroup::has_exited`] 为真时有意义。
    #[must_use]
    pub fn exit_code(&self) -> u32 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// 认领本组的僵尸 TCB 回收权；仅首个调用者返回 true。
    ///
    /// 认领后组内 TCB 只允许由认领者回收，其他等待者不得再调用
    /// `remove_and_free`（重复递减引用计数会使 PCB 提前释放）。
    pub fn claim_reap(&self) -> bool {
        !self.reap_claimed.swap(true, Ordering::AcqRel)
    }

    /// 记录「组未收齐」签名，返回它是否首次出现（§6③a 取证行限流）。
    ///
    /// 未收齐状态会被轮询方（`wait4` 非阻塞腿 / tick 兜底回收）反复读到，不去重
    /// 即每轮一行刷屏；签名由成员 `tid|state` 快照导出，成员增减或状态改变即重新
    /// 放行一行。两个核同时首见同一签名时认领失败者静默，最多丢一行重复打印。
    /// `sig` 约定非 0（0 是"尚未打印过"的初值）。
    pub fn note_reap_diag(&self, sig: u64) -> bool {
        let prev = self.reap_diag.load(Ordering::Acquire);
        prev != sig
            && self
                .reap_diag
                .compare_exchange(prev, sig, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    /// 置 `exec` `de_thread` 标记并返回当时的成员快照。
    ///
    /// 标记与快照在同一把成员表锁内完成：快照之后不可能再有人入组
    /// （[`ThreadGroup::join`] 会拒绝），快照之前的入组者一定在名单内。
    #[must_use]
    pub fn begin_exec_de_thread_snapshot(&self) -> Vec<Pid> {
        let members = self.members.lock();
        self.exec_de_thread.store(true, Ordering::Release);
        members.clone()
    }

    /// `exec` `de_thread` 是否正在进行。
    #[must_use]
    pub fn exec_de_thread(&self) -> bool {
        self.exec_de_thread.load(Ordering::Acquire)
    }

    /// 结束 `exec` `de_thread`（兄弟线程已全部离开）。
    pub fn end_exec_de_thread(&self) {
        self.exec_de_thread.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_thread_group_starts_with_leader_only() {
        let group = ThreadGroup::new_leader(7);
        assert_eq!(group.tgid(), 7);
        assert_eq!(group.members(), alloc::vec![7]);
        assert_eq!(group.member_count(), 1);
        assert!(!group.has_exited());
    }

    #[test]
    fn join_appends_member_and_keeps_tgid() {
        let group = ThreadGroup::new_leader(7);
        assert!(group.join(8));
        assert!(group.join(9));
        assert_eq!(group.members(), alloc::vec![7, 8, 9]);
        assert_eq!(group.member_count(), 3);
        assert_eq!(group.tgid(), 7, "入组不得改变组身份");
        assert!(!group.has_exited());
    }

    #[test]
    fn leave_is_idempotent_and_reports_empty() {
        let group = ThreadGroup::new_leader(7);
        assert!(group.leave(7), "移除唯一成员后组应为空");
        assert!(group.leave(7), "重复移除不得复活成员");
        assert_eq!(group.member_count(), 0);
    }

    #[test]
    fn leave_keeps_other_members() {
        let group = ThreadGroup::new_leader(7);
        assert!(group.join(8));
        assert!(!group.leave(7), "仍有成员时不得报空");
        assert_eq!(group.members(), alloc::vec![8]);
        assert!(group.leave(8));
        assert_eq!(group.member_count(), 0);
    }

    #[test]
    fn mark_exited_first_writer_wins() {
        let group = ThreadGroup::new_leader(7);
        group.mark_exited(3);
        group.mark_exited(9);
        assert!(group.has_exited());
        assert_eq!(group.exit_code(), 3);
    }

    #[test]
    fn request_exit_first_writer_wins() {
        let group = ThreadGroup::new_leader(7);
        assert!(!group.exit_requested());
        group.request_exit(0x0100);
        group.request_exit(0x0200);
        assert!(group.exit_requested());
        assert_eq!(
            group.requested_exit_code(),
            0x0100,
            "后到请求不得覆盖首个退出码"
        );
    }

    #[test]
    fn request_exit_does_not_mark_group_exited() {
        let group = ThreadGroup::new_leader(7);
        group.request_exit(0);
        assert!(group.exit_requested());
        assert!(!group.has_exited(), "请求退出 ≠ 成员全部离开");
    }

    #[test]
    fn exit_request_blocks_later_join() {
        let group = ThreadGroup::new_leader(7);
        assert!(group.join(8));
        group.request_exit(0x0100);
        assert!(
            !group.join(9),
            "组退出请求发布后入组必须被拒 (否则 exit_group 的成员快照会漏掉它)"
        );
        assert_eq!(group.members(), alloc::vec![7, 8], "被拒线程不得留在成员账");
    }

    #[test]
    fn group_starts_as_own_process_group() {
        let group = ThreadGroup::new_leader(7);
        assert_eq!(group.parent(), 0, "新建组无父进程");
        assert!(group.children().is_empty());
        assert_eq!(group.session_id(), 0, "新建组未加入会话");
        assert_eq!(group.pgid(), 7, "POSIX: 新进程自成一组");
    }

    #[test]
    fn parent_children_and_session_are_group_scoped() {
        let group = ThreadGroup::new_leader(7);
        group.set_parent(3);
        group.add_child(8);
        group.add_child(9);
        group.set_session_id(7);
        group.set_pgid(0);
        assert_eq!(group.parent(), 3);
        assert_eq!(group.children(), alloc::vec![8, 9]);
        assert_eq!(group.session_id(), 7);
        assert_eq!(group.pgid(), 7, "pgid=0 回落 tgid");
        group.clear_children();
        assert!(group.children().is_empty());
    }

    #[test]
    fn remove_child_drops_only_named_group() {
        let group = ThreadGroup::new_leader(7);
        group.add_child(8);
        group.add_child(9);
        assert!(group.remove_child(8));
        assert_eq!(group.children(), alloc::vec![9]);
        assert!(!group.remove_child(8), "重复移除应按不存在处理");
    }

    #[test]
    fn joined_thread_sees_same_group_attributes() {
        let group = ThreadGroup::new_leader(7);
        group.set_parent(3);
        group.add_child(8);
        group.set_session_id(7);
        group.set_pgid(7);
        assert!(group.join(9));
        assert_eq!(group.parent(), 3);
        assert_eq!(group.children(), alloc::vec![8]);
        assert_eq!(group.session_id(), 7);
        assert_eq!(group.pgid(), 7);
    }

    #[test]
    fn claim_reap_allows_single_winner() {
        let group = ThreadGroup::new_leader(7);
        assert!(group.claim_reap(), "首个认领者应成功");
        assert!(!group.claim_reap(), "后续认领者不得重复回收同一组");
    }

    #[test]
    fn note_reap_diag_passes_only_new_signatures() {
        let group = ThreadGroup::new_leader(7);
        assert!(group.note_reap_diag(0x100), "首个签名应放行");
        assert!(
            !group.note_reap_diag(0x100),
            "同一未收齐状态被轮询方反复读到, 不得重复放行"
        );
        assert!(
            group.note_reap_diag(0x200),
            "成员快照变化 (新签名) 必须重新放行一行"
        );
        assert!(!group.note_reap_diag(0x200), "新签名之后回到重复态仍须静默");
        assert!(
            group.note_reap_diag(0x100),
            "签名回到更早见过的值也应视为新一次状态变化"
        );
        // 初值语义: 从未打印过的组存的是 0, 写入 0 不算状态变化 (不得把
        // "尚未记录"误判成一次新签名而放行一行).
        let fresh = ThreadGroup::new_leader(9);
        assert!(!fresh.note_reap_diag(0), "初值 0 重复写入不得放行");
        assert!(fresh.note_reap_diag(0x300), "新组的首个真实签名应放行");
    }

    #[test]
    fn exec_de_thread_blocks_join_and_restores() {
        let group = ThreadGroup::new_leader(7);
        assert!(group.join(8));
        assert!(!group.exec_de_thread());
        let snapshot = group.begin_exec_de_thread_snapshot();
        assert_eq!(
            snapshot,
            alloc::vec![7, 8],
            "快照必须含 de_thread 前已入组者"
        );
        assert!(group.exec_de_thread());
        assert!(!group.join(9), "de_thread 期间不得再有线程入组");
        assert_eq!(group.members(), alloc::vec![7, 8]);
        group.end_exec_de_thread();
        assert!(!group.exec_de_thread());
        assert!(group.join(9), "de_thread 结束后可恢复入组");
    }
}
