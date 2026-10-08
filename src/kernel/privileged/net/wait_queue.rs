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
// ## 机制 (D10 / DECISION-092: 真调度阻塞)
//
// 本文件提供 **SocketWaitQueue**: 给每个 fd 关联一个轻量级等待队列, 记录在该
// fd 上阻塞的等待者 pid.
// - `sm_recv` / `sm_send` 等在 socket 未就绪且 fd 阻塞时, 于持 `NET_STATE` 的
//   临界区内 `add_waiter(pid, want)`, 释放 `NET_STATE` 后 `scheduler_block`
//   + `scheduler_schedule` (与 `timer_sleep` / uffd / futex / epoll 既有范式同源).
// - smoltcp 状态机在 `poll_network` 末尾遍历所有 fd, 对就绪的 fd 经 `collect_waiters`
//   (非破坏) 采集方向匹配的待唤醒 pid; **释放 `NET_STATE` 后**逐个
//   `scheduler_unblock` (F8: 禁 `NET_STATE → SCHEDULER` 嵌套锁).
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
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

/// Smoltcp 段 FD 上限 (与 `FdPlan::SMOLTCP.capacity` 同源; 不引用 `net::init` 的
/// `MAX_SM_FD` 别名, 避免 `init ↔ wait_queue` 循环依赖). 权威定义见 `net/init.rs`.
const MAX_SM_FD: usize = crate::privileged::proc::FdPlan::SMOLTCP.capacity as usize;

/// per-fd 等待者槽上限 (定长, no_std 无堆分配, 可进自旋锁临界区).
///
/// SIMPLIFIED: 同一 fd 并发阻塞的等待者超过 `WAITER_SLOTS` 时, `add_waiter`
/// 返回 false → 消费者回退非阻塞 `-E_AGAIN`; 影响面为病态高并发同 fd 阻塞;
/// 若需无上限等待者, 待改为 per-fd 动态等待队列 (受 NET_STATE 保护的定长链).
pub const WAITER_SLOTS: usize = 16;
/// 等待者兴趣方向: 空槽.
pub const WAITER_NONE: u8 = 0;
/// 等待者兴趣方向: 等待可读 (recv).
pub const WAITER_READ: u8 = 1;
/// 等待者兴趣方向: 等待可写 (send).
pub const WAITER_WRITE: u8 = 2;

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

/// 单 fd 的等待队列 (多等待者, DECISION-094).
///
/// 等待者建模为定长 `waiter_pid`/`waiter_want` 槽数组, 每槽记一个在该 fd
/// 上阻塞的 pid 与其兴趣方向 (read/write). 所有 `add_waiter`/`remove_waiter`/
/// `collect_waiters` 均在 `NET_STATE` 临界区内调用 (消费者与 `poll_network` 互斥),
/// 数组内原子操作仅为防御.
///
/// syscall 端阻塞 (D10): 持 `NET_STATE` 内 `add_waiter(pid,want)` → **释放锁**
/// → `scheduler_block` + `scheduler_schedule`; 唤醒侧 `collect_waiters` 收集
/// 方向匹配 pid → **释放 `NET_STATE`** → 逐个 `scheduler_unblock` (F8: 禁
/// `NET_STATE → SCHEDULER` 嵌套锁).
///
/// 丢失唤醒自愈 (DECISION-094 事实②): `collect_waiters` **非破坏** (不移除
/// 等待者); 消费者操作成功后自行 `remove_waiter` 自摘. 若某次 unblock 落在
/// “释放锁→尚未 block()”窗口被吞 (unblock 对非 Blocked 目标 no-op), 则
/// `poll_network` 下一 tick 电平重扫会再次采集并唤醒, 自愈.
pub struct SocketWaitQueue {
    /// 当前 fd 上是否有等待者 (observability; 驱动不再依赖此位, 靠 waiter 槽 + 电平重扫)
    pending: AtomicBool,
    /// 累计 wake 次数 (供测试 / 调试使用)
    wake_count: AtomicU32,
    /// 最近一次 wake 原因 (u8 repr of `WakeReason`)
    last_reason: AtomicU32,
    /// 定长等待者 pid 数组 (0 = 空槽; pid 0 为 idle/内核线程, 从不阻塞于 socket).
    waiter_pid: [AtomicU32; WAITER_SLOTS],
    /// 与 `waiter_pid` 同索引的兴趣方向 (`WAITER_READ` / `WAITER_WRITE`).
    waiter_want: [AtomicU8; WAITER_SLOTS],
    /// ISR 端抢锁 (`try_lock`) 用的 mutex
    lock: Mutex<()>,
}

impl SocketWaitQueue {
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            wake_count: AtomicU32::new(0),
            last_reason: AtomicU32::new(u32::MAX),
            waiter_pid: [const { AtomicU32::new(0) }; WAITER_SLOTS],
            waiter_want: [const { AtomicU8::new(WAITER_NONE) }; WAITER_SLOTS],
            lock: Mutex::new(()),
        }
    }

    /// 标记当前 fd 已被 wait (observability). 返回 true 表示之前未标记.
    pub fn mark_waiting(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    /// 将 `pid` 登记为方向 `want` 的等待者. 由调用方在持 `NET_STATE` 临界区内
    /// 与 `mark_waiting` 一同调用. 同一 pid 已登记则更新方向 (幂等).
    /// 返 false 表示槽满 (消费者应回退非阻塞, 见 `WAITER_SLOTS` SIMPLIFIED).
    pub fn add_waiter(&self, pid: u32, want: u8) -> bool {
        // 已存在同 pid → 更新方向 (幂等, 避免重复占槽).
        if let Some(idx) = self
            .waiter_pid
            .iter()
            .position(|s| s.load(Ordering::Acquire) == pid)
        {
            self.waiter_want[idx].store(want, Ordering::Release);
            return true;
        }
        // 找空槽 CAS 入队.
        for i in 0..WAITER_SLOTS {
            if self.waiter_pid[i]
                .compare_exchange(0, pid, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.waiter_want[i].store(want, Ordering::Release);
                self.pending.store(true, Ordering::Release);
                return true;
            }
        }
        false // 满
    }

    /// 移除 `pid` 的等待登记 (清槽). 消费者操作成功返返前自摘, 或放弃阻塞时调用.
    pub fn remove_waiter(&self, pid: u32) {
        for i in 0..WAITER_SLOTS {
            if self.waiter_pid[i].swap(0, Ordering::AcqRel) == pid {
                self.waiter_want[i].store(WAITER_NONE, Ordering::Release);
                return;
            }
        }
    }

    /// 将方向匹配 `want` 的等待者 pid 采集到 `out`, 返回写入个数. **非破坏**
    /// (不移除等待者), 供 `poll_network` 在持 `NET_STATE` 时收集待唤醒 pid,
    /// 出临界区后逐个 `scheduler_unblock`. `out` 满则截断, 余下待下一 tick 自愈.
    pub fn collect_waiters(&self, want: u8, out: &mut [u32]) -> usize {
        let mut n = 0usize;
        for i in 0..WAITER_SLOTS {
            let pid = self.waiter_pid[i].load(Ordering::Acquire);
            if pid != 0 && self.waiter_want[i].load(Ordering::Acquire) == want {
                if n < out.len() {
                    out[n] = pid;
                    n += 1;
                } else {
                    break; // out 满, 余下待下一 tick 电平重扫自愈 (SIMPLIFIED)
                }
            }
        }
        n
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

/// Per-fd 等待队列表 (与 `MAX_SM_FD` 对齐, 当前 256).
pub struct SocketWaitQueueTable {
    queues: [SocketWaitQueue; MAX_SM_FD],
}

impl SocketWaitQueueTable {
    // 固定容量表 (MAX_SM_FD) 仅在全局 static `SOCKET_WAIT_QUEUES` 中物化, 常驻
    // .bss 而非栈; const 构造要求数组字面量在此函数体内展开, clippy 的
    // large_stack_arrays 仅在 host target (kernel_test/host-test 维) 按放大后的
    // 元素尺寸报出, x86_64-unknown-none 维不触发 — 故用 allow 而非 expect
    // (expect 会在 none target 产生 unfulfilled 警告, 破坏 lib 维 0 warning).
    // 非死代码抑制 (F9 不受影响), 是对静态表设计意图的正当豁免.
    #[allow(clippy::large_stack_arrays)]
    pub const fn new() -> Self {
        Self {
            queues: [const { SocketWaitQueue::new() }; MAX_SM_FD],
        }
    }

    /// 取 fd 对应队列 (fd 越界时返回 None)
    pub fn get(&self, fd: usize) -> Option<&SocketWaitQueue> {
        self.queues.get(fd)
    }
}

// ============================================================================
// 全局表 (单例). 与 MAX_SM_FD 对齐.
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
        assert!(t.get(MAX_SM_FD - 1).is_some());
        assert!(t.get(MAX_SM_FD).is_none());
        assert!(t.get(usize::MAX).is_none());
    }

    #[test]
    fn waiter_add_remove_roundtrip() {
        let q = SocketWaitQueue::new();
        let mut out = [0u32; WAITER_SLOTS];
        assert_eq!(q.collect_waiters(WAITER_READ, &mut out), 0); // 初始无等待者
        assert!(q.add_waiter(42, WAITER_READ));
        // 非破坏 collect: 采集到但不移除
        assert_eq!(q.collect_waiters(WAITER_READ, &mut out), 1);
        assert_eq!(out[0], 42);
        assert_eq!(q.collect_waiters(WAITER_READ, &mut out), 1); // 仍在 (非破坏)
        q.remove_waiter(42);
        assert_eq!(q.collect_waiters(WAITER_READ, &mut out), 0); // 自摘后清空
    }

    #[test]
    fn waiter_direction_filter() {
        let q = SocketWaitQueue::new();
        let mut out = [0u32; WAITER_SLOTS];
        assert!(q.add_waiter(7, WAITER_READ));
        assert!(q.add_waiter(8, WAITER_WRITE));
        // 只采集 read 方向
        assert_eq!(q.collect_waiters(WAITER_READ, &mut out), 1);
        assert_eq!(out[0], 7);
        assert_eq!(q.collect_waiters(WAITER_WRITE, &mut out), 1);
        assert_eq!(out[0], 8);
    }

    #[test]
    fn waiter_multi_idempotent_and_capacity() {
        let q = SocketWaitQueue::new();
        // 填满 WAITER_SLOTS 个不同 pid (均 read)
        for i in 0..WAITER_SLOTS as u32 {
            assert!(q.add_waiter(i + 1, WAITER_READ), "第 {i} 个等待者应入槽");
        }
        // 槽满: 新 pid 拒绝 (消费者回退非阻塞, 见 WAITER_SLOTS SIMPLIFIED)
        assert!(!q.add_waiter(1_000, WAITER_READ), "超 WAITER_SLOTS 应拒绝");
        // 同 pid 幂等更新方向 (不重复占槽): pid 1 → write
        assert!(q.add_waiter(1, WAITER_WRITE), "同 pid 应幂等更新方向");
        let mut out = [0u32; WAITER_SLOTS];
        assert_eq!(q.collect_waiters(WAITER_WRITE, &mut out), 1); // 仅 pid 1 转 write
        assert_eq!(out[0], 1);
        assert_eq!(q.collect_waiters(WAITER_READ, &mut out), WAITER_SLOTS - 1);
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
