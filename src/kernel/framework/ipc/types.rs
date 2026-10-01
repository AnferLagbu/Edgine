//! IPC 数据类型定义 — framework 机制类型
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原属 services/ipc/types.rs (T6-3 于 2026-06-16 从 framework 迁出)。
//! 按"机制持有的数据结构归 framework、功能实现归 services"统一判据反转：
//! `IpcNamespace` 被 framework `IPC_NAMESPACE` 全局机制持有、`Pipe`/`MsgQueue`
//! 等是其数组元素类型、`IPC_MAX_*`/`PIPE_BUFFER_SIZE` 等为机制常量 — 均为
//! framework 机制直接持有的数据结构/常量, 故迁回 framework。
//!
//! services 侧改 `pub use crate::framework::ipc::types::*` 保持 API 兼容。
//! 本文件 0 unsafe (纯类型定义 + 常量 + 等待队列机制)。

use alloc::vec::Vec;
use core::sync::atomic::AtomicPtr;

/// IPC 资源 ID 类型 (全局唯一标识符)
pub type IpcId = u32;

// ============================================================================
// 常量定义
// ============================================================================

/// 最大管道数量
///
/// 测试模式下缩减至 2 以节省内存
// E-03 (2026-09-06): feature 语义拆分 — 以下 4 组 IPC_MAX_* 缩减常量属纯逻辑测试模式
// (kernel_test + host-test 同语义), 统一改 any 门控
#[cfg(not(any(feature = "kernel_test", feature = "host-test")))]
pub const IPC_MAX_PIPES: usize = 64;
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub const IPC_MAX_PIPES: usize = 2;
/// 最大信号数量
pub const IPC_MAX_SIGNALS: usize = 32;
/// 最大共享内存段数量
///
/// 测试模式下缩减至 2 以节省内存
#[cfg(not(any(feature = "kernel_test", feature = "host-test")))]
pub const IPC_MAX_SHM_SEGS: usize = 16;
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub const IPC_MAX_SHM_SEGS: usize = 2;
/// 最大消息队列数量
///
/// 测试模式下缩减至 2 以节省内存
#[cfg(not(any(feature = "kernel_test", feature = "host-test")))]
pub const IPC_MAX_MSG_QUEUES: usize = 32;
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub const IPC_MAX_MSG_QUEUES: usize = 2;
/// 最大信号量数量
///
/// 测试模式下缩减至 2 以节省内存
#[cfg(not(any(feature = "kernel_test", feature = "host-test")))]
pub const IPC_MAX_SEMAPHORES: usize = 64;
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub const IPC_MAX_SEMAPHORES: usize = 2;

/// 管道缓冲区大小 (4KB)
pub const PIPE_BUFFER_SIZE: usize = 4096;
/// 最大共享内存大小 (16MB)
pub const SHM_MAX_SIZE: u64 = 16 * 1024 * 1024;
/// 最大单条消息大小 (4KB)
pub const MSG_MAX_SIZE: usize = 4096;
/// 单个队列最大消息数
pub const MSG_QUEUE_MAX_MSGS: u32 = 64;

// ============================================================================
// 枚举定义
// ============================================================================

/// IPC 资源类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IpcType {
    /// 管道
    Pipe = 1,
    /// 信号
    Signal,
    /// 共享内存
    Shm,
    /// 消息队列
    MsgQ,
    /// 信号量
    Sem,
}

/// 信号编号 (POSIX 兼容)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalNum {
    /// 无效/空信号
    None = 0,
    /// 中断 (Ctrl+C)
    Int = 1,
    /// 非法指令
    Ill = 2,
    /// 浮点异常
    Fpe = 3,
    /// 段错误
    Segv = 4,
    /// 终止请求
    Term = 5,
    /// 强制终止
    Kill = 6,
    /// 停止进程
    Stop = 7,
    /// 继续执行
    Cont = 8,
    /// 子进程状态改变
    Chld = 9,
    /// 用户自定义信号 1
    Usr1 = 10,
    /// 用户自定义信号 2
    Usr2 = 11,
    /// 定时器到期
    Alarm = 12,
    /// 管道断裂
    Pipe = 13,
}

impl From<u8> for SignalNum {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    fn from(val: u8) -> Self {
        match val {
            0 => Self::None,
            1 => Self::Int,
            2 => Self::Ill,
            3 => Self::Fpe,
            4 => Self::Segv,
            5 => Self::Term,
            6 => Self::Kill,
            7 => Self::Stop,
            8 => Self::Cont,
            9 => Self::Chld,
            10 => Self::Usr1,
            11 => Self::Usr2,
            12 => Self::Alarm,
            13 => Self::Pipe,
            _ => Self::None,
        }
    }
}

/// 信号处理动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalAction {
    /// 默认处理 (终止/忽略)
    Default = 0,
    /// 忽略信号
    Ignore,
    /// 用户自定义处理器
    Handler,
    /// 阻塞信号
    Block,
}

// ============================================================================
// 核心数据结构
// ============================================================================

/// 等待队列项 (简化版，用于阻塞等待)
///
/// 在完整实现中应使用内核的 `wait_queue` 机制
#[derive(Debug, Clone, Copy)]
pub struct WaitQueueItem {
    /// 被阻塞进程的 PID (B07-15 修复: 以 pid 入队/唤醒, 与调度器
    /// `scheduler_unblock(pid)` 语义一致 — 原实现误存线程 tid, 唤醒错位)
    pub pid: u32,
}

/// 简化版等待队列 (环形缓冲区) — B07-15: 中断安全化
///
/// 由 `IrqSpinLock` 保护队列结构。中断上下文只能登记唤醒请求
/// ([`WaitQueue::request_wake`]), 不触碰队列/调度器; 实际出队 + 调度唤醒
/// 统一由进程上下文路径执行 ([`WaitQueue::drain_pending`] / `wake_one`),
/// 避免在 ISR/softirq 中持锁调用调度器导致死锁。
#[derive(Debug)]
pub struct WaitQueue {
    items: [Option<WaitQueueItem>; 4],
    count: u32,
    /// 中断上下文登记的唤醒请求, 由进程上下文 `drain_pending` 补唤醒
    wake_pending: bool,
    /// 队列锁 (中断安全)
    lock: crate::framework::sync::IrqSpinLock<()>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            items: [const { None }; 4],
            count: 0,
            wake_pending: false,
            lock: crate::framework::sync::IrqSpinLock::new(()),
        }
    }

    pub fn init(&mut self) {
        self.items = [None; 4];
        self.count = 0;
        self.wake_pending = false;
    }

    /// 队列中等待者数量 (进程上下文; 中断上下文禁止调用, 应改用
    /// [`WaitQueue::request_wake`]).
    pub fn count(&self) -> u32 {
        let _g = self.lock.lock();
        self.count
    }

    /// 入队 (进程上下文). 队列已满时返回 `false` — 调用方必须避免阻塞
    /// (否则等待项丢失, 进程永久睡眠, B07-15 修复).
    pub fn add(&mut self, item: WaitQueueItem) -> bool {
        let _g = self.lock.lock();
        if self.count >= 4 {
            return false;
        }
        for i in 0..4 {
            if self.items[i].is_none() {
                self.items[i] = Some(item);
                self.count += 1;
                return true;
            }
        }
        false
    }

    /// 中断上下文: 登记一次唤醒请求, 不触碰队列/调度器.
    /// 实际唤醒由后续进程上下文 `drain_pending` 完成 (B07-15 修复:
    /// 原实现仅清标志不唤醒, 中断上下文唤醒被静默丢弃).
    pub fn request_wake(&mut self) {
        self.wake_pending = true;
    }

    /// 进程上下文: 补唤醒 — 取出全部剩余等待项并清除 pending 标志.
    /// 返回的项由调用方逐个 `scheduler_unblock(pid)` (B07-15 修复:
    /// 原 `drain_pending` 只清标志不做实际唤醒, 文档与实现不符).
    pub fn drain_pending(&mut self) -> alloc::vec::Vec<WaitQueueItem> {
        if !self.wake_pending {
            return alloc::vec::Vec::new();
        }
        let _g = self.lock.lock();
        self.wake_pending = false;
        let mut out = alloc::vec::Vec::new();
        for slot in &mut self.items {
            if let Some(item) = slot.take() {
                self.count -= 1;
                out.push(item);
            }
        }
        out
    }

    /// 进程上下文: 唤醒一个等待项 (出队, 不调用调度器).
    /// 调度器唤醒由调用方执行 (持锁期间不得调用调度器).
    pub fn wake_one(&mut self) -> Option<WaitQueueItem> {
        let _g = self.lock.lock();
        for i in 0..4 {
            if let Some(item) = self.items[i].take() {
                self.count -= 1;
                return Some(item);
            }
        }
        None
    }
}

/// 管道结构体
///
/// 实现字节流通信，支持阻塞式读写
#[derive(Debug)]
pub struct Pipe {
    /// 全局唯一 ID
    pub id: IpcId,
    /// 环形缓冲区
    pub buffer: [u8; PIPE_BUFFER_SIZE],
    /// 读位置
    pub read_pos: u32,
    /// 写位置
    pub write_pos: u32,
    /// 当前缓冲区中的字节数
    pub count: u32,

    /// 创建者 PID (读端)
    pub read_pid: u32,
    /// 写端 PID
    pub write_pid: u32,

    /// 文件描述符 (读端)
    pub read_fd: i32,
    /// 文件描述符 (写端)
    pub write_fd: i32,

    /// 当前读者数量
    pub readers: i32,
    /// 当前写者数量
    pub writers: i32,

    /// 读等待队列
    pub read_wait: WaitQueue,
    /// 写等待队列
    pub write_wait: WaitQueue,

    /// 标志位 (`O_NONBLOCK` 等)
    pub flags: i32,
}

impl Pipe {
    pub const fn new() -> Self {
        Self {
            id: 0,
            buffer: [0u8; PIPE_BUFFER_SIZE],
            read_pos: 0,
            write_pos: 0,
            count: 0,
            read_pid: 0,
            write_pid: 0,
            read_fd: 0,
            write_fd: 0,
            readers: 0,
            writers: 0,
            read_wait: WaitQueue::new(),
            write_wait: WaitQueue::new(),
            flags: 0,
        }
    }
}

/// 信号处理函数签名
pub type SignalHandlerFn = extern "C" fn(i32);

/// 信号处理结构
#[derive(Debug, Clone, Copy)]
pub struct SignalHandler {
    /// 处理函数指针 (C ABI)
    pub handler: Option<SignalHandlerFn>,
    /// 处理函数地址 (用户空间)
    pub handler_addr: u64,
    /// 栈地址 (用户空间)
    pub stack_addr: u64,
    /// 标志位
    pub flags: u32,
    /// 信号掩码
    pub mask: u32,
}

/// 待处理信号状态
#[derive(Debug)]
pub struct SignalPending {
    /// 待处理信号位图
    pub pending: u32,
    /// 已屏蔽信号位图
    pub blocked: u32,
    /// 已注册的处理函数表
    pub handlers: [SignalHandler; IPC_MAX_SIGNALS],
}

impl SignalPending {
    pub const fn new() -> Self {
        Self {
            pending: 0,
            blocked: 0,
            handlers: [SignalHandler {
                handler: None,
                handler_addr: 0,
                stack_addr: 0,
                flags: 0,
                mask: 0,
            }; IPC_MAX_SIGNALS],
        }
    }
}

/// 共享内存段
#[derive(Debug)]
pub struct ShmSegment {
    /// 全局唯一 ID
    pub id: IpcId,
    /// 物理起始地址
    pub phys_addr: u64,
    /// 段大小 (字节)
    pub size: u64,

    /// 创建者 PID
    pub creator: u32,
    /// 引用计数
    pub ref_count: u32,

    /// 已附加的 PID 列表
    pub attached_pids: [u32; 16],
    /// 当前附加进程数
    pub attach_count: u32,

    /// 标志位
    pub flags: i32,
    /// 权限 (如 0666)
    pub perm: i32,
}

impl ShmSegment {
    pub const fn new() -> Self {
        Self {
            id: 0,
            phys_addr: 0,
            size: 0,
            creator: 0,
            ref_count: 0,
            attached_pids: [0u32; 16],
            attach_count: 0,
            flags: 0,
            perm: 0,
        }
    }
}

/// 消息结构
#[derive(Debug)]
pub struct Message {
    /// 消息类型 (用户自定义)
    pub type_: u64,
    /// 发送者 PID
    pub sender: u64,
    /// 数据长度
    pub size: u64,
    /// 消息数据
    pub data: [u8; MSG_MAX_SIZE],
    /// 下一条消息 (侵入式链表, AtomicPtr 保证 Send/Sync; 队列操作在持锁下执行, Relaxed 足够)
    pub next: AtomicPtr<Message>,
}

impl Message {
    pub const fn new() -> Self {
        Self {
            type_: 0,
            sender: 0,
            size: 0,
            data: [0u8; MSG_MAX_SIZE],
            next: AtomicPtr::new(core::ptr::null_mut()),
        }
    }
}

/// 消息队列
#[derive(Debug)]
pub struct MsgQueue {
    /// 全局唯一 ID
    pub id: IpcId,
    /// 所有者 PID
    pub owner: u32,

    /// 队列头指针 (AtomicPtr: B03-24 根治, 使 IpcNamespace 满足 Send)
    pub head: AtomicPtr<Message>,
    /// 队列尾指针
    pub tail: AtomicPtr<Message>,
    /// 当前消息数
    pub count: u32,
    /// 最大消息数
    pub max_msgs: u32,
    /// 单条最大字节数
    pub max_size: u32,

    /// 发送等待队列
    pub send_wait: WaitQueue,
    /// 接收等待队列
    pub recv_wait: WaitQueue,

    /// 标志位
    pub flags: i32,
    /// 权限
    pub perm: i32,
}

impl MsgQueue {
    pub const fn new() -> Self {
        Self {
            id: 0,
            owner: 0,
            head: AtomicPtr::new(core::ptr::null_mut()),
            tail: AtomicPtr::new(core::ptr::null_mut()),
            count: 0,
            max_msgs: MSG_QUEUE_MAX_MSGS,
            max_size: MSG_MAX_SIZE as u32,
            send_wait: WaitQueue::new(),
            recv_wait: WaitQueue::new(),
            flags: 0,
            perm: 0,
        }
    }
}

/// 信号量
#[derive(Debug)]
pub struct Semaphore {
    /// 全局唯一 ID
    pub id: IpcId,
    /// 所有者 PID
    pub owner: u32,

    /// 当前计数 (可为负表示等待线程数)
    pub count: i32,
    /// 最大计数值
    pub max_count: u32,

    /// 等待队列
    pub wait: WaitQueue,

    /// 标志位
    pub flags: i32,
    /// 权限
    pub perm: i32,
}

impl Semaphore {
    pub const fn new() -> Self {
        Self {
            id: 0,
            owner: 0,
            count: 0,
            max_count: 0,
            wait: WaitQueue::new(),
            flags: 0,
            perm: 0,
        }
    }
}

/// IPC 命名空间 (全局资源容器)
///
/// 存储所有 IPC 资源的静态数组。
/// 内部 NonNull 链表指针仅在持锁时访问 (Mutex 保护每个数组元素),
/// 故 IPC 命名空间整体可安全跨线程传递。
///
/// 资源数组改为堆上 `Vec`, 静态实例经 [`IpcNamespace::empty`] 在 BSS
/// 仅占 4 个 `Vec` 头部 (约 96 字节); 槽位由 [`IpcNamespace::allocate`]
/// 在 kmalloc 堆上惰性分配, 避免约 275 KB 常驻 BSS。
#[derive(Debug)]
pub struct IpcNamespace {
    /// 管道数组
    pub pipes: Vec<Pipe>,
    /// 共享内存段数组
    pub shm_segs: Vec<ShmSegment>,
    /// 消息队列数组
    pub msg_queues: Vec<MsgQueue>,
    /// 信号量数组
    pub semaphores: Vec<Semaphore>,
}

impl IpcNamespace {
    /// 创建空命名空间 (所有资源数组为空 `Vec`)。
    ///
    /// 供静态实例 `const` 构造使用; 资源槽位需经 [`IpcNamespace::allocate`] 填充。
    pub const fn empty() -> Self {
        Self {
            pipes: Vec::new(),
            shm_segs: Vec::new(),
            msg_queues: Vec::new(),
            semaphores: Vec::new(),
        }
    }

    /// 按 `IPC_MAX_*` 容量逐元素在 kmalloc 堆上分配全部资源槽位。
    pub fn allocate(&mut self) {
        self.pipes = (0..IPC_MAX_PIPES).map(|_| Pipe::new()).collect();
        self.shm_segs = (0..IPC_MAX_SHM_SEGS).map(|_| ShmSegment::new()).collect();
        self.msg_queues = (0..IPC_MAX_MSG_QUEUES).map(|_| MsgQueue::new()).collect();
        self.semaphores = (0..IPC_MAX_SEMAPHORES).map(|_| Semaphore::new()).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    // B07-15 修复④: 队列满 4 个后 add 返回 false.
    // 原实现静默丢弃, 在真实阻塞语义下第 5 个等待者会永久睡眠 (不在任何队列中).
    #[test]
    fn wait_queue_full_rejected() {
        let mut wq = WaitQueue::new();
        for i in 0..4 {
            assert!(wq.add(WaitQueueItem { pid: 10 + i }));
        }
        assert!(!wq.add(WaitQueueItem { pid: 99 }));
        assert_eq!(wq.count(), 4);
    }

    // B07-15 修复①: 入队/唤醒以 pid 为准, 出队项 pid 不变.
    // 原实现误存线程 tid, 唤醒时 `scheduler_unblock(tid)` 按 pid 查找错位.
    #[test]
    fn wait_queue_wake_one_pid_roundtrip() {
        let mut wq = WaitQueue::new();
        wq.add(WaitQueueItem { pid: 42 });
        let item = wq.wake_one().expect("应取出等待项");
        assert_eq!(item.pid, 42);
        assert_eq!(wq.count(), 0);
        assert!(wq.wake_one().is_none(), "空队列 wake_one 应返回 None");
    }

    // B07-15 修复③: request_wake (中断上下文登记) 后,
    // 进程上下文 drain_pending 补唤醒全部等待项并清除 pending 标志.
    #[test]
    fn wait_queue_pending_drain() {
        let mut wq = WaitQueue::new();
        wq.add(WaitQueueItem { pid: 1 });
        wq.add(WaitQueueItem { pid: 2 });
        wq.request_wake();
        let drained = wq.drain_pending();
        let mut pids: Vec<u32> = drained.iter().map(|i| i.pid).collect();
        pids.sort_unstable();
        assert_eq!(pids.as_slice(), [1u32, 2].as_slice());
        assert_eq!(wq.count(), 0);
        // pending 已清, 再次 drain 返回空
        assert!(wq.drain_pending().is_empty());
    }

    // B07-15 修复③: 无 pending 请求时 drain 返回空.
    #[test]
    fn wait_queue_drain_empty_when_no_pending() {
        let mut wq = WaitQueue::new();
        assert!(wq.drain_pending().is_empty());
    }
}
