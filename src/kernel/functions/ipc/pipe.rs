#![deny(unsafe_code)]
#![deny(missing_docs)]
//! 管道策略 — T6-1 从 privileged/ipc/pipe.rs 提取
//!
//! 纯策略逻辑: 槽位查找、环形缓冲区读写、端点计数.
//! 自旋锁操作通过 `privileged::sync::IrqSpinLock` 机制完成.
//!
//! ## 端点 key 与用户 fd
//!
//! 本模块以**端点 key** (`read_key` / `write_key`, 由管道 id 派生) 标识端点,
//! 不再承载用户可见 fd. 用户 fd 由 `functions::fs::pipe_fd` 的 `OpenFile`
//! 对象承载 (per-process `FdTable` 分配), 因此 fork 继承 / dup / CLOEXEC /
//! close 语义全部复用 VFS fd 对象模型.

use crate::privileged::ipc::types::{
    IPC_MAX_PIPES, IpcId, IpcNamespace, PIPE_BUFFER_SIZE, WaitQueueItem,
};
use crate::privileged::proc::{
    BlockReason, process_get_current_pid, scheduler_block, scheduler_yield_ex,
};
use crate::privileged::sync::{IrqSpinLock, in_irq_context};

/// 管道全局自旋锁 (privileged 机制, 短临界区)
static PIPE_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

/// 在命名空间中查找首个空闲管道槽位.
///
/// 空闲判据为槽位 `id == 0` (未分配); 全部占满时返回 `None`.
#[must_use]
pub fn pipe_find_free_index(namespace: &IpcNamespace) -> Option<usize> {
    namespace
        .pipes
        .iter()
        .take(IPC_MAX_PIPES)
        .position(|pipe| pipe.id == 0)
}

/// 按端点 key 查找管道槽位.
///
/// key 为 0 表示"端点已关闭"哨兵, 不匹配任何槽位.
#[must_use]
pub fn pipe_find_by_key_index(namespace: &IpcNamespace, key: i32) -> Option<usize> {
    if key == 0 {
        return None;
    }
    namespace
        .pipes
        .iter()
        .take(IPC_MAX_PIPES)
        .position(|pipe| pipe.id != 0 && (pipe.read_key == key || pipe.write_key == key))
}

/// 创建管道, 返回 `(读端点 key, 写端点 key)`.
///
/// 端点 key 仅用于本模块内部寻址, 不等于用户 fd; 用户 fd 由调用方
/// (`functions::fs::pipe_fd::create_pair`) 经 VFS fd 对象模型分配.
///
/// # Errors
/// 当管道表已满、无空闲槽位时返回 `Err(-1)`.
pub fn pipe_create_safe(
    namespace: &mut IpcNamespace,
    next_id: &mut IpcId,
    current_pid: u32,
) -> Result<(i32, i32), i32> {
    let _lock = PIPE_LOCK.lock();

    let Some(idx) = pipe_find_free_index(namespace) else {
        return Err(-1);
    };

    let pipe = &mut namespace.pipes[idx];

    pipe.id = *next_id;
    *next_id += 1;

    pipe.buffer = [0u8; PIPE_BUFFER_SIZE];
    pipe.read_pos = 0;
    pipe.write_pos = 0;
    pipe.count = 0;

    pipe.read_pid = current_pid;
    pipe.write_pid = current_pid;

    pipe.read_key = (pipe.id * 2).cast_signed();
    pipe.write_key = (pipe.id * 2 + 1).cast_signed();

    pipe.readers = 1;
    pipe.writers = 1;
    pipe.flags = 0;

    pipe.read_wait.init();
    pipe.write_wait.init();

    Ok((pipe.read_key, pipe.write_key))
}

/// 从管道读取数据到 `buf`.
///
/// # Errors
/// 当 key 不是管道端点时返回 `Err(-1)`; 当 key 不是读端时返回 `Err(-2)`;
/// 当读端已空且无写者阻塞等待时返回 `Err(-4)`.
pub fn pipe_read_safe(
    namespace: &mut IpcNamespace,
    key: i32,
    buf: &mut [u8],
    count: u32,
) -> Result<u32, i32> {
    let _lock = PIPE_LOCK.lock();

    let Some(idx) = pipe_find_by_key_index(namespace, key) else {
        return Err(-1);
    };

    let pipe = &mut namespace.pipes[idx];

    if key != pipe.read_key {
        return Err(-2);
    }

    // key 校验先于 count==0 早退: POSIX 要求 read/write 先校验 fd (EBADF),
    // 原实现 count==0 直接 Ok(0) 会掩盖无效 fd (2026-09-24 UT-06 修复).
    if count == 0 {
        return Ok(0);
    }

    let mut read_count: u32 = 0;

    while read_count < count {
        if pipe.count == 0 {
            if pipe.writers == 0 {
                break;
            }
            if read_count > 0 {
                break;
            }
            return Err(-4);
        }

        buf[read_count as usize] = pipe.buffer[pipe.read_pos as usize];
        pipe.read_pos =
            (pipe.read_pos + 1) % crate::functions::intcast::low_u32_from_usize(PIPE_BUFFER_SIZE);
        pipe.count -= 1;
        read_count += 1;

        if pipe.write_wait.count() > 0 {
            super::scheduler_integration::wake_one_thread(&mut pipe.write_wait);
        }
    }

    Ok(read_count)
}

/// 将 `buf` 中的数据写入管道.
///
/// # Errors
/// 当 key 不是管道端点时返回 `Err(-1)`; 当 key 不是写端时返回 `Err(-2)`;
/// 当没有读者时返回 `Err(-3)`; 当缓冲区已满且一个字节都未写入时返回 `Err(-4)`;
/// 缓冲区中途写满时返回已写入的字节数 (部分写, POSIX 语义).
pub fn pipe_write_safe(
    namespace: &mut IpcNamespace,
    key: i32,
    buf: &[u8],
    count: u32,
) -> Result<u32, i32> {
    let _lock = PIPE_LOCK.lock();

    let Some(idx) = pipe_find_by_key_index(namespace, key) else {
        return Err(-1);
    };

    let pipe = &mut namespace.pipes[idx];

    if key != pipe.write_key {
        return Err(-2);
    }

    // key 校验先于 count==0 早退 (与 pipe_read_safe 同一处置; 2026-09-24 UT-06 修复).
    if count == 0 {
        return Ok(0);
    }

    if pipe.readers == 0 {
        return Err(-3);
    }

    let mut written: u32 = 0;

    while written < count {
        if pipe.count >= crate::functions::intcast::low_u32_from_usize(PIPE_BUFFER_SIZE) {
            // 部分写: 已写入 >0 时返回已写字节数 (调用方据此推进缓冲),
            // 一个字节都写不下才报"缓冲区满".
            if written > 0 {
                break;
            }
            return Err(-4);
        }

        pipe.buffer[pipe.write_pos as usize] = buf[written as usize];
        pipe.write_pos =
            (pipe.write_pos + 1) % crate::functions::intcast::low_u32_from_usize(PIPE_BUFFER_SIZE);
        pipe.count += 1;
        written += 1;

        if pipe.read_wait.count() > 0 {
            super::scheduler_integration::wake_one_thread(&mut pipe.read_wait);
        }
    }

    Ok(written)
}

/// 关闭管道的一个端点 key.
///
/// 端点关闭以 `OpenFile` 引用计数归零为准 (最后一份 fd 释放时调用),
/// 因此 fork 共享的端点只在全部副本关闭后才会触发 EOF / 槽位回收.
///
/// # Errors
/// 当 key 不是管道端点时返回 `Err(-1)`.
pub fn pipe_close_safe(namespace: &mut IpcNamespace, key: i32) -> Result<(), i32> {
    let _lock = PIPE_LOCK.lock();

    let Some(idx) = pipe_find_by_key_index(namespace, key) else {
        return Err(-1);
    };

    let pipe = &mut namespace.pipes[idx];

    if key == pipe.read_key {
        pipe.readers -= 1;
        pipe.read_key = 0;
        if pipe.readers == 0 {
            super::scheduler_integration::wake_all_threads(&mut pipe.write_wait);
        }
    } else if key == pipe.write_key {
        pipe.writers -= 1;
        pipe.write_key = 0;
        if pipe.writers == 0 {
            super::scheduler_integration::wake_all_threads(&mut pipe.read_wait);
        }
    }

    if pipe.readers == 0 && pipe.writers == 0 {
        pipe.id = 0;
        pipe.read_key = 0;
        pipe.write_key = 0;
    }

    Ok(())
}

/// 阻塞等待端点可读 (空缓冲且仍有写者时睡眠).
///
/// 返回 `true` 表示调用方应**重试**读取 (条件可能已满足, 或端点已失效 —
/// 重试会得到 EOF/EBADF 等确定结果); 返回 `false` 表示当前上下文无法阻塞
/// (中断上下文 / 无进程上下文 / 等待队列满), 调用方应回退 `EAGAIN`.
pub fn pipe_wait_readable(namespace: &mut IpcNamespace, key: i32) -> bool {
    pipe_block_on(namespace, key, true)
}

/// 阻塞等待端点可写 (缓冲已满且仍有读者时睡眠).
///
/// 返回值语义同 [`pipe_wait_readable`]. 读端全部关闭时本函数立即返回 `true`,
/// 由调用方重试写, 从而得到确定的 `EPIPE`.
pub fn pipe_wait_writable(namespace: &mut IpcNamespace, key: i32) -> bool {
    pipe_block_on(namespace, key, false)
}

/// 在 `PIPE_LOCK` 内置 `Blocked` 后入睡 — 关闭"唤醒先于入睡"的丢唤醒窗口.
///
/// 关键次序 (不可调整):
/// 1. 持 `PIPE_LOCK` 复查条件并**入队**;
/// 2. 仍持锁调用 `scheduler_block` 把当前进程标记为 `Blocked`;
/// 3. 释放 `PIPE_LOCK` 后再 `yield`.
///
/// 唤醒方 (`pipe_read/write/close_safe`) 也持同一把 `PIPE_LOCK` 执行
/// `scheduler_unblock`, 因此"入队 + 置 Blocked"与"出队 + 置 Ready"天然互斥;
/// 若先释放锁再标记, 唤醒可能在两步之间发生并被丢弃 (永久睡眠).
fn pipe_block_on(namespace: &mut IpcNamespace, key: i32, want_read: bool) -> bool {
    if in_irq_context() {
        // 中断上下文禁止睡眠 (持锁入睡会死锁).
        return false;
    }
    let pid = process_get_current_pid();
    if pid == 0 {
        // idle / 无进程上下文: 无法真实阻塞.
        return false;
    }

    let lock = PIPE_LOCK.lock();
    let Some(idx) = pipe_find_by_key_index(namespace, key) else {
        return false;
    };
    let pipe = &mut namespace.pipes[idx];

    // 条件已满足 (或有意义的终态: 对端全关) ⇒ 直接重试, 无需睡眠.
    let ready = if want_read {
        key != pipe.read_key || pipe.count > 0 || pipe.writers == 0
    } else {
        key != pipe.write_key
            || pipe.count < crate::functions::intcast::low_u32_from_usize(PIPE_BUFFER_SIZE)
            || pipe.readers == 0
    };
    if ready {
        return true;
    }

    let queue = if want_read {
        &mut pipe.read_wait
    } else {
        &mut pipe.write_wait
    };
    // 队列满 ⇒ 入队失败, 此时入睡将无人唤醒 (永久睡眠), 回退 EAGAIN.
    if !queue.add(WaitQueueItem { pid }) {
        return false;
    }
    // 持锁标记 Blocked (见函数文档的次序契约).
    scheduler_block(BlockReason::WaitingForIo);
    drop(lock);
    scheduler_yield_ex();
    true
}
