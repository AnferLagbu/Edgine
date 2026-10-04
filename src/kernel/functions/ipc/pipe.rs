#![deny(unsafe_code)]
//! 管道策略 — T6-1 从 privileged/ipc/pipe.rs 提取
//!
//! 纯策略逻辑: 槽位查找、环形缓冲区读写、fd 管理、读者/写者计数.
//! 自旋锁操作通过 `privileged::sync::IrqSpinLock` 机制完成.

use crate::privileged::ipc::types::{IPC_MAX_PIPES, IpcId, IpcNamespace, PIPE_BUFFER_SIZE};
use crate::privileged::sync::IrqSpinLock;

/// 管道全局自旋锁 (privileged 机制, 短临界区)
static PIPE_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

pub fn pipe_find_free_index(namespace: &IpcNamespace) -> Option<usize> {
    for i in 0..IPC_MAX_PIPES {
        if namespace.pipes[i].id == 0 {
            return Some(i);
        }
    }
    None
}

pub fn pipe_find_by_fd_index(namespace: &IpcNamespace, fd: i32) -> Option<usize> {
    for i in 0..IPC_MAX_PIPES {
        let pipe = &namespace.pipes[i];
        if pipe.id != 0 && (pipe.read_fd == fd || pipe.write_fd == fd) {
            return Some(i);
        }
    }
    None
}

/// 判断 fd 是否为 pipe fd (公开接口, 供 sendfile/splice 使用)
pub fn is_pipe_fd(fd: i32) -> bool {
    let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
    pipe_find_by_fd_index(ns, fd).is_some()
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 创建管道, 返回 (读 fd, 写 fd) 对.
///
/// # Errors
/// 当管道表已满、无空闲槽位时返回 `Err(-1)`.
pub fn pipe_create_safe(
    namespace: &mut IpcNamespace,
    next_id: &mut IpcId,
    current_pid: u32,
) -> Result<(i32, i32), i32> {
    let _lock = PIPE_LOCK.lock();

    let idx = match pipe_find_free_index(namespace) {
        Some(i) => i,
        None => return Err(-1),
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

    pipe.read_fd = (pipe.id * 2) as i32;
    pipe.write_fd = (pipe.id * 2 + 1) as i32;

    pipe.readers = 1;
    pipe.writers = 1;
    pipe.flags = 0;

    pipe.read_wait.init();
    pipe.write_wait.init();

    Ok((pipe.read_fd, pipe.write_fd))
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 从管道读取数据到 `buf`.
///
/// # Errors
/// 当 fd 不是管道 fd 时返回 `Err(-1)`; 当 fd 不是读端时返回 `Err(-2)`;
/// 当读端已空且无写者阻塞等待时返回 `Err(-4)`.
pub fn pipe_read_safe(
    namespace: &mut IpcNamespace,
    fd: i32,
    buf: &mut [u8],
    count: u32,
) -> Result<u32, i32> {
    let _lock = PIPE_LOCK.lock();

    let idx = match pipe_find_by_fd_index(namespace, fd) {
        Some(i) => i,
        None => return Err(-1),
    };

    let pipe = &mut namespace.pipes[idx];

    if fd != pipe.read_fd {
        return Err(-2);
    }

    // fd 校验先于 count==0 早退: POSIX 要求 write/read 先校验 fd (EBADF),
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
        pipe.read_pos = (pipe.read_pos + 1) % PIPE_BUFFER_SIZE as u32;
        pipe.count -= 1;
        read_count += 1;

        if pipe.write_wait.count() > 0 {
            super::scheduler_integration::wake_one_thread(&mut pipe.write_wait);
        }
    }

    Ok(read_count)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 将 `buf` 中的数据写入管道.
///
/// # Errors
/// 当 fd 不是管道 fd 时返回 `Err(-1)`; 当 fd 不是写端时返回 `Err(-2)`;
/// 当没有读者时返回 `Err(-3)`; 当管道缓冲区已满时返回 `Err(-4)`.
pub fn pipe_write_safe(
    namespace: &mut IpcNamespace,
    fd: i32,
    buf: &[u8],
    count: u32,
) -> Result<u32, i32> {
    let _lock = PIPE_LOCK.lock();

    let idx = match pipe_find_by_fd_index(namespace, fd) {
        Some(i) => i,
        None => return Err(-1),
    };

    let pipe = &mut namespace.pipes[idx];

    if fd != pipe.write_fd {
        return Err(-2);
    }

    // fd 校验先于 count==0 早退 (与 pipe_read_safe 同一处置; 2026-09-24 UT-06 修复).
    if count == 0 {
        return Ok(0);
    }

    if pipe.readers == 0 {
        return Err(-3);
    }

    let mut written: u32 = 0;

    while written < count {
        if pipe.count >= PIPE_BUFFER_SIZE as u32 {
            return Err(-4);
        }

        pipe.buffer[pipe.write_pos as usize] = buf[written as usize];
        pipe.write_pos = (pipe.write_pos + 1) % PIPE_BUFFER_SIZE as u32;
        pipe.count += 1;
        written += 1;

        if pipe.read_wait.count() > 0 {
            super::scheduler_integration::wake_one_thread(&mut pipe.read_wait);
        }
    }

    Ok(written)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 关闭管道的一个端点 fd.
///
/// # Errors
/// 当 fd 不是管道 fd 时返回 `Err(-1)`.
pub fn pipe_close_safe(namespace: &mut IpcNamespace, fd: i32) -> Result<(), i32> {
    let _lock = PIPE_LOCK.lock();

    let idx = match pipe_find_by_fd_index(namespace, fd) {
        Some(i) => i,
        None => return Err(-1),
    };

    let pipe = &mut namespace.pipes[idx];

    if fd == pipe.read_fd {
        pipe.readers -= 1;
        pipe.read_fd = 0;
        if pipe.readers == 0 {
            super::scheduler_integration::wake_all_threads(&mut pipe.write_wait);
        }
    } else if fd == pipe.write_fd {
        pipe.writers -= 1;
        pipe.write_fd = 0;
        if pipe.writers == 0 {
            super::scheduler_integration::wake_all_threads(&mut pipe.read_wait);
        }
    }

    if pipe.readers == 0 && pipe.writers == 0 {
        pipe.id = 0;
        pipe.read_fd = 0;
        pipe.write_fd = 0;
    }

    Ok(())
}
