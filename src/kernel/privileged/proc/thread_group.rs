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
//! - 共享资源句柄（fd 表 / sigaction / FS 状态）归 K-03；本阶段不建占位字段，
//!   避免"先有字段、后无读者"的死字段。
//!
//! ## 锁域
//!
//! 成员表是进程属性级锁（`docs/explain/ref-lock-order.md` 层级 9），与本域的
//! `children` 同层。持进程表锁（层级 7）时可以获取它；反向（持成员表锁时取
//! 进程表锁或任何 rq 锁）禁止。

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::privileged::proc::Pid;
use crate::privileged::sync::IrqSpinLock as Mutex;

/// 线程组 — 组身份（tgid）、成员账与退出状态。
pub struct ThreadGroup {
    /// 组 ID（= 组长 TID），生命周期内不变。
    tgid: Pid,
    /// 组内存活线程的 TID 集合（受锁保护）。
    members: Mutex<Vec<Pid>>,
    /// 组是否已退出（全部线程已离开）；仅首次置位生效。
    exited: AtomicBool,
    /// 组退出码；仅在 `exited == true` 时有意义。
    exit_code: AtomicU32,
}

impl ThreadGroup {
    /// 以首个线程为组长创建单线程组（`tgid == leader`）。
    #[must_use]
    pub fn new_leader(leader: Pid) -> Self {
        Self {
            tgid: leader,
            members: Mutex::new(alloc::vec![leader]),
            exited: AtomicBool::new(false),
            exit_code: AtomicU32::new(0),
        }
    }

    /// 返回组 ID（tgid）。
    #[must_use]
    pub fn tgid(&self) -> Pid {
        self.tgid
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

    /// 新线程入组时登记其 TID（K-02）。
    ///
    /// 调用方保证 `tid` 是新分配且未登记的 TID；重复登记会使成员账失真。
    pub fn join(&self, tid: Pid) {
        self.members.lock().push(tid);
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
        group.join(8);
        group.join(9);
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
        let group = ThreadGroup {
            tgid: 7,
            members: Mutex::new(alloc::vec![7, 8]),
            exited: AtomicBool::new(false),
            exit_code: AtomicU32::new(0),
        };
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
}
