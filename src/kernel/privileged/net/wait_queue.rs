// ============================================================================
// P2-I-41: Socket WaitQueue 基础设施 — privileged 机制实现
// ============================================================================
//
// ## DECISION-J 归属反转记录 (2026-09-12)
//
// 原策略代码于 T6-9 (2026-06-16) 迁至 `functions::net::wait_queue`, 本文件仅
// re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：
// `SOCKET_WAIT_QUEUES` 全局表被 privileged net/init `poll_network` 机制直接
// 消费 (host-test 注释亦声明"SOCKET_WAIT_QUEUES 是 privileged 内 static") —
// 属机制项, 迁回。
//
// functions 侧改 `pub use crate::privileged::net::wait_queue::*` 保持 API 兼容。
// 本文件 0 unsafe.
//
// ## 背景
//
// `sm_send` / `sm_recv` 持 NET_LOCK 自旋等待 socket 就绪的话, 会饿死 ISR 中
// `poll_network` 的 try_lock (NET_LOCK 不可重入), 导致数据包被静默丢弃.
//
// 当前实现是非阻塞 (Err 即返回 -E_AGAIN/-E_CONNRESET), 暂无"自旋等待"症状.
// 但结构上存在风险: 任何未来在 smoltcp 调用之间加 retry 的修改都会复发.
//
// ## 修复方案 (Phase 1)
//
// 本文件提供 **SocketWaitQueue**: 给每个 fd 关联一个轻量级等待队列.
// - smoltcp 状态机在 `poll_network` 末尾遍历所有 fd, 对刚刚可读/可写的 fd
//   调用 `SocketWaitQueue::wake`, 唤醒等待者.
// - `sm_send` / `sm_recv` 在 socket 未就绪时, **不**调用 wait (保持非阻塞
//   语义), 但基础设施已就位. 未来要切换为阻塞式, 只需在 Err 分支中调
//   `wait_queue.wait_with_timeout(NET_LOCK_*)`.
// - `wait_with_timeout` 释放 NET_LOCK → proc_sleep_ms(N) → 重抢 NET_LOCK,
//   持锁时间从"任意长"收敛为"无状态变化时 0 ms".
//
// ## 线程安全
//
// SocketWaitQueue 内部用 IrqSpinLock 保护 `pending` 状态. 在 ISR/poll 端用
// `try_lock` 避免阻塞; 在 syscall 端用 `lock`.
//
// ## 与 Framekernel 安全契约
//
// 不破坏任何既有边界; 只在 privileged/net/ 内部新增, 不跨层.
use crate::privileged::sync::IrqSpinLock as Mutex;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Socket 状态变化原因 (用于 wake 路径)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// Socket 变为可读 (有数据可 recv)
    Readable,
    /// Socket 变为可写 (有空间可 send)
    Writable,
    /// Socket 关闭 / 错误
    Closed,
}

/// 单 fd 的等待队列.
///
/// 持锁时间: `try_lock` 命中 → O(1) 修改 `pending` 标记 → 释放.
/// syscall 端 `wait_with_timeout`: 释放 `NET_LOCK` 后睡眠 10ms, 重抢 `NET_LOCK`.
pub struct SocketWaitQueue {
    /// 当前 fd 上是否有等待者 (简化: 1 个, 多于 1 个也只标记一次)
    pending: AtomicBool,
    /// 累计 wake 次数 (供测试 / 调试使用)
    wake_count: AtomicU32,
    /// 最近一次 wake 原因 (u8 repr of `WakeReason`)
    last_reason: AtomicU32,
    /// ISR 端抢锁 (`try_lock`) 用的 mutex
    lock: Mutex<()>,
}

impl SocketWaitQueue {
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            wake_count: AtomicU32::new(0),
            last_reason: AtomicU32::new(u32::MAX),
            lock: Mutex::new(()),
        }
    }

    /// 标记当前 fd 已被 wait. 由 `sm_send/sm_recv` 在 Err 分支调用 (未来).
    /// 返回 true 表示之前未标记 (首次 wait).
    pub fn mark_waiting(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// ISR / poll 端: 状态变化时调用 wake. 必须 `try_lock` 避免阻塞.
    /// 返回 true 表示成功唤醒了至少一个等待者.
    pub fn try_wake(&self, reason: WakeReason) -> bool {
        let _guard = match self.lock.try_lock() {
            Some(g) => g,
            None => return false,
        };
        let was_pending = self.pending.swap(false, Ordering::AcqRel);
        if was_pending {
            self.wake_count.fetch_add(1, Ordering::Relaxed);
            self.last_reason.store(reason as u32, Ordering::Relaxed);
        }
        was_pending
    }

    /// 是否有等待者
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// 累计 wake 次数 (测试用)
    pub fn wake_count(&self) -> u32 {
        self.wake_count.load(Ordering::Relaxed)
    }

    /// 最近一次 wake 原因
    pub fn last_reason(&self) -> Option<WakeReason> {
        match self.last_reason.load(Ordering::Relaxed) {
            0 => Some(WakeReason::Readable),
            1 => Some(WakeReason::Writable),
            2 => Some(WakeReason::Closed),
            _ => None,
        }
    }
}

/// Per-fd 等待队列表 (固定 16 项, 与 `MAX_SM_FD` 对齐).
pub struct SocketWaitQueueTable {
    queues: [SocketWaitQueue; 16],
}

impl SocketWaitQueueTable {
    pub const fn new() -> Self {
        Self {
            queues: [
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
                SocketWaitQueue::new(),
            ],
        }
    }

    /// 取 fd 对应队列 (fd 越界时返回 None)
    pub fn get(&self, fd: usize) -> Option<&SocketWaitQueue> {
        if fd < 16 {
            Some(&self.queues[fd])
        } else {
            None
        }
    }
}

// ============================================================================
// 全局表 (单例). 与 MAX_SM_FD = 16 对齐.
// ============================================================================

pub static SOCKET_WAIT_QUEUES: SocketWaitQueueTable = SocketWaitQueueTable::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_and_wake_roundtrip() {
        let q = SocketWaitQueue::new();
        assert!(!q.is_pending());
        assert!(q.mark_waiting());
        assert!(q.is_pending());
        assert!(!q.mark_waiting()); // 已 pending, 第二次不返回首次
        assert!(q.try_wake(WakeReason::Readable));
        assert!(!q.is_pending());
        assert_eq!(q.wake_count(), 1);
        assert_eq!(q.last_reason(), Some(WakeReason::Readable));
    }

    #[test]
    fn wake_without_waiter_is_noop() {
        let q = SocketWaitQueue::new();
        // 未 mark_waiting 直接 wake: 不应计数
        assert!(!q.try_wake(WakeReason::Writable));
        assert_eq!(q.wake_count(), 0);
        assert_eq!(q.last_reason(), None);
    }

    #[test]
    fn table_lookup_bounded() {
        let t = SocketWaitQueueTable::new();
        assert!(t.get(0).is_some());
        assert!(t.get(15).is_some());
        assert!(t.get(16).is_none());
        assert!(t.get(usize::MAX).is_none());
    }

    #[test]
    fn multiple_wake_increments_count() {
        let q = SocketWaitQueue::new();
        for _ in 0..3 {
            q.mark_waiting();
            assert!(q.try_wake(WakeReason::Closed));
        }
        assert_eq!(q.wake_count(), 3);
        assert_eq!(q.last_reason(), Some(WakeReason::Closed));
    }
}
