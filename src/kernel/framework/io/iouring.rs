//! `io_uring` 异步 I/O 框架 — framework 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原策略代码 (数据结构 + 环形缓冲区 + 实例管理 + syscall 分发) 于 2026-06-18
//! 迁至 `services::io::iouring`, 本文件仅 re-export。按"机制持有的数据结构/常量
//! 归 framework"统一判据反转：`sys_io_uring_*` 被 framework syscall dispatch
//! 机制直接调用、`IoUring` 实例由内核持有 — 属机制项, 迁回。依赖闭包全在
//! framework 内 (sync::IrqSpinLock + errno::Errno)。
//!
//! services 侧改 `pub use crate::framework::io::iouring::*` 保持 API 兼容。

use core::sync::atomic::{AtomicU32, Ordering};

use alloc::vec::Vec;

use crate::framework::sync::IrqSpinLock;

use crate::framework::errno::Errno;

// ============================================================================
// 常量
// ============================================================================

/// 最大 `io_uring` 实例数
pub const MAX_URING_INSTANCES: usize = 16;

/// 默认队列深度 (2 的幂)
pub const DEFAULT_RING_SIZE: u32 = 256;

// ============================================================================
// 操作码
// ============================================================================

/// `io_uring` 操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IoOpCode {
    /// 无操作
    Nop = 0,
    /// 读
    Read = 1,
    /// 写
    Write = 2,
    /// fsync
    Fsync = 3,
    /// accept (网络)
    Accept = 4,
    /// connect (网络)
    Connect = 5,
    /// 发送
    Send = 6,
    /// 接收
    Recv = 7,
    /// 超时等待
    Timeout = 8,
}

impl IoOpCode {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Nop),
            1 => Some(Self::Read),
            2 => Some(Self::Write),
            3 => Some(Self::Fsync),
            4 => Some(Self::Accept),
            5 => Some(Self::Connect),
            6 => Some(Self::Send),
            7 => Some(Self::Recv),
            8 => Some(Self::Timeout),
            _ => None,
        }
    }
}

// ============================================================================
// SQE / CQE
// ============================================================================

/// 提交队列条目 (SQE)
///
/// 用户态写入, 内核消费.
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct Sqe {
    /// 操作码
    pub opcode: u8,
    /// 标志位
    pub flags: u8,
    /// 用户数据 (原样传递到 CQE)
    pub user_data: u64,
    /// 文件描述符
    pub fd: i32,
    /// 偏移量 (文件 I/O)
    pub offset: u64,
    /// 地址 (缓冲区指针)
    pub addr: u64,
    /// 长度
    pub len: u32,
}

/// 完成队列条目 (CQE)
///
/// 内核写入, 用户态消费.
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct Cqe {
    /// 用户数据 (从 SQE 复制)
    pub user_data: u64,
    /// 结果 (成功为正数/0, 失败为负 errno)
    pub result: i32,
    /// 标志位
    pub flags: u32,
}

// ============================================================================
// 环形缓冲区
// ============================================================================

/// 环形缓冲区 (SQ 或 CQ 共用结构)
pub struct RingBuffer<T> {
    /// 环形存储
    entries: Vec<T>,
    /// 容量 (2 的幂)
    capacity: u32,
    /// 掩码 (capacity - 1)
    mask: u32,
    /// 头索引 (消费位置)
    head: AtomicU32,
    /// 尾索引 (生产位置)
    tail: AtomicU32,
}

impl<T: Clone + Default> RingBuffer<T> {
    /// 创建指定容量的环形缓冲区
    pub fn new(capacity: u32) -> Self {
        let cap = capacity.next_power_of_two();
        let mut entries = Vec::with_capacity(cap as usize);
        for _ in 0..cap {
            entries.push(T::default());
        }
        Self {
            entries,
            capacity: cap,
            mask: cap - 1,
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
        }
    }

    /// 当前条目数
    pub fn len(&self) -> u32 {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 是否已满
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity
    }

    /// 生产: 写入一个条目 (成功返回 true)
    pub fn push(&mut self, entry: T) -> bool {
        if self.is_full() {
            return false;
        }
        let tail = self.tail.load(Ordering::Acquire);
        let idx = (tail & self.mask) as usize;
        self.entries[idx] = entry;
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// 消费: 读取一个条目 (成功返回 Some)
    pub fn pop(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let head = self.head.load(Ordering::Acquire);
        let idx = (head & self.mask) as usize;
        let entry = self.entries[idx].clone();
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(entry)
    }

    /// 窥视 (不移动 head)
    pub fn peek(&self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let head = self.head.load(Ordering::Acquire);
        let idx = (head & self.mask) as usize;
        Some(self.entries[idx].clone())
    }
}

// ============================================================================
// io_uring 实例
// ============================================================================

/// `io_uring` 实例
pub struct IoUring {
    /// 实例 ID
    pub id: u32,
    /// 队列深度
    pub ring_size: u32,
    /// 提交队列
    pub sq: IrqSpinLock<RingBuffer<Sqe>>,
    /// 完成队列
    pub cq: IrqSpinLock<RingBuffer<Cqe>>,
    /// 所属进程 PID
    pub owner_pid: u32,
    /// 标志位
    pub flags: AtomicU32,
}

impl IoUring {
    /// 创建新的 `io_uring` 实例
    pub fn new(id: u32, ring_size: u32, owner_pid: u32) -> Self {
        Self {
            id,
            ring_size,
            sq: IrqSpinLock::new(RingBuffer::new(ring_size)),
            cq: IrqSpinLock::new(RingBuffer::new(ring_size)),
            owner_pid,
            flags: AtomicU32::new(0),
        }
    }

    /// 提交 SQE
    ///
    /// # Errors
    /// 当 SQ 队列已满时返回 `EBUSY`.
    pub fn submit_sqe(&self, sqe: Sqe) -> Result<(), Errno> {
        let mut sq = self.sq.lock();
        if sq.push(sqe) {
            Ok(())
        } else {
            Err(Errno::EBUSY)
        }
    }

    /// 取出下一个 SQE (内核消费)
    pub fn consume_sqe(&self) -> Option<Sqe> {
        let mut sq = self.sq.lock();
        sq.pop()
    }

    /// 推送 CQE (内核生产)
    ///
    /// # Errors
    /// 当 CQ 队列已满时返回 `EBUSY`.
    pub fn push_cqe(&self, cqe: Cqe) -> Result<(), Errno> {
        let mut cq = self.cq.lock();
        if cq.push(cqe) {
            Ok(())
        } else {
            Err(Errno::EBUSY)
        }
    }

    /// 取出下一个 CQE (用户态消费)
    pub fn reap_cqe(&self) -> Option<Cqe> {
        let mut cq = self.cq.lock();
        cq.pop()
    }

    /// 处理所有待处理的 SQE
    ///
    /// 取出 SQE, 执行操作, 推送 CQE.
    pub fn process_pending(&self) -> u32 {
        let mut processed = 0u32;
        while let Some(sqe) = self.consume_sqe() {
            let result = self.execute_op(&sqe);
            let cqe = Cqe {
                user_data: sqe.user_data,
                result,
                flags: 0,
            };

            if self.push_cqe(cqe).is_err() {
                // CQ 满了, 放回 SQE
                let _ = self.submit_sqe(sqe);
                break;
            }
            processed += 1;
        }
        processed
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 执行单个操作
    fn execute_op(&self, sqe: &Sqe) -> i32 {
        let opcode = match IoOpCode::from_u8(sqe.opcode) {
            Some(op) => op,
            None => return -(Errno::EINVAL as i32),
        };

        match opcode {
            IoOpCode::Nop => 0,
            // 未实装占位 (登记分册 9 B09-10): VFS fd 表操作 (Read/Write/Fsync)、
            // 网络异步 (Accept/Connect/Send/Recv)、超时等待 (Timeout) — 统一 ENOSYS,
            // 分别实装后拆为独立 arm.
            IoOpCode::Read
            | IoOpCode::Write
            | IoOpCode::Fsync
            | IoOpCode::Accept
            | IoOpCode::Connect
            | IoOpCode::Send
            | IoOpCode::Recv
            | IoOpCode::Timeout => -(Errno::ENOSYS as i32),
        }
    }
}

// ============================================================================
// 全局 io_uring 管理
// ============================================================================

/// 全局 `io_uring` 实例表
static URING_TABLE: IrqSpinLock<Vec<Option<IoUring>>> = IrqSpinLock::new(Vec::new());
static NEXT_URING_ID: AtomicU32 = AtomicU32::new(0);

/// 创建 `io_uring` 实例
///
/// # Errors
/// 当实例表已满 (`MAX_URING_INSTANCES`) 时返回 `ENOMEM`.
pub fn io_uring_setup(entries: u32, owner_pid: u32) -> Result<u32, Errno> {
    let ring_size = if entries == 0 {
        DEFAULT_RING_SIZE
    } else {
        entries.next_power_of_two()
    };

    let mut table = URING_TABLE.lock();
    if table.len() >= MAX_URING_INSTANCES {
        return Err(Errno::ENOMEM);
    }

    let id = NEXT_URING_ID.fetch_add(1, Ordering::SeqCst);
    let uring = IoUring::new(id, ring_size, owner_pid);
    table.push(Some(uring));
    Ok(id)
}

/// 销毁 `io_uring` 实例
///
/// # Errors
/// 当指定 ID 的实例不存在时返回 `EBADF`.
pub fn io_uring_destroy(id: u32) -> Result<(), Errno> {
    let mut table = URING_TABLE.lock();
    let pos = table
        .iter()
        .position(|u| u.as_ref().map_or(false, |u| u.id == id));
    pos.map_or(Err(Errno::EBADF), |i| {
        table.remove(i);
        Ok(())
    })
}

/// 提交 SQE 到指定实例
///
/// # Errors
/// 当指定 ID 的实例不存在时返回 `EBADF`; 当 SQ 队列已满时返回 `EBUSY`.
pub fn io_uring_submit(id: u32, sqe: Sqe) -> Result<(), Errno> {
    let table = URING_TABLE.lock();
    let uring = table
        .iter()
        .find(|u| u.as_ref().map_or(false, |u| u.id == id))
        .and_then(|u| u.as_ref());
    uring.map_or(Err(Errno::EBADF), |u| u.submit_sqe(sqe))
}

/// 进入 `io_uring` (处理待处理请求 + 可选等待)
///
/// 返回处理的 CQE 数量
///
/// # Errors
/// 当指定 ID 的实例不存在时返回 `EBADF`.
pub fn io_uring_enter(id: u32, to_submit: u32, _min_complete: u32) -> Result<u32, Errno> {
    let table = URING_TABLE.lock();
    let uring = table
        .iter()
        .find(|u| u.as_ref().map_or(false, |u| u.id == id))
        .and_then(|u| u.as_ref());

    // 处理 to_submit 个 SQE
    let to_process = uring.map_or(0, |u| to_submit.min(u.sq.lock().len()));
    drop(table); // 释放表锁, process_pending 内部获取实例锁

    // 重新获取 (因为 drop 了 table)
    process_after_relock(id, to_process)
}

/// drop(table) 后重新加锁查找并处理 to_process 个 SQE
fn process_after_relock(id: u32, to_process: u32) -> Result<u32, Errno> {
    let table = URING_TABLE.lock();
    let uring = table
        .iter()
        .find(|u| u.as_ref().map_or(false, |u| u.id == id))
        .and_then(|u| u.as_ref());
    uring.map_or(Err(Errno::EBADF), |u| {
        let mut processed = 0u32;
        for _ in 0..to_process {
            // SAFETY/语义: None 时退出循环是合理 fallback, 等价于 break
            let Some(sqe) = u.consume_sqe() else { break };
            let result = u.execute_op(&sqe);
            let cqe = Cqe {
                user_data: sqe.user_data,
                result,
                flags: 0,
            };
            if u.push_cqe(cqe).is_err() {
                let _ = u.submit_sqe(sqe);
                break;
            }
            processed += 1;
        }
        Ok(processed)
    })
}

/// 收割 CQE
pub fn io_uring_reap(id: u32) -> Option<Cqe> {
    let table = URING_TABLE.lock();
    let uring = table
        .iter()
        .find(|u| u.as_ref().map_or(false, |u| u.id == id))
        .and_then(|u| u.as_ref());
    uring.and_then(IoUring::reap_cqe)
}

// ============================================================================
// Syscall 接口
// ============================================================================

// T2 批 3 (syscall-followup): sys_io_uring_setup / sys_io_uring_enter 策略
// 入口已迁至 services (services::io::iouring::io_uring_setup_syscall /
// io_uring_enter_syscall), 委托本文件机制函数 (io_uring_setup / io_uring_enter).
// 以下仅保留 EG_IO_URING_SUBMIT 机制独有入口 (EG_* 归 framework 回退层).

/// `sys_io_uring_submit_sqe` — 提交单个 SQE
pub fn sys_io_uring_submit_sqe(
    id: u64,
    opcode: u64,
    flags: u64,
    user_data: u64,
    fd: u64,
    offset_len: u64,
) -> i64 {
    let sqe = Sqe {
        opcode: opcode as u8,
        flags: flags as u8,
        user_data,
        fd: fd as i32,
        offset: offset_len >> 32,
        addr: 0, // 简化版不传地址
        len: (offset_len & 0xFFFF_FFFF) as u32,
    };

    match io_uring_submit(id as u32, sqe) {
        Ok(()) => 0,
        Err(e) => -(e as i64),
    }
}
