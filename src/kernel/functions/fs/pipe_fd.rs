#![deny(unsafe_code)]
//! 匿名管道端点 fd — 管道纳入 VFS fd 对象模型
//!
//! ## 设计
//!
//! 每个管道端点 = 一个 [`PipeEndpointInode`] + 一个 `OpenFile`, 用户 fd 由
//! per-process `FdTable` 分配 (`local_fd` → `OpenFile` handle). 由此
//! fork 继承 / dup·dup2 / close-on-exec / close 全部复用既有 VFS fd 语义:
//!
//! - **dup**: 共享同一 `OpenFile`, `OpenFileTable` 引用计数 +1;
//! - **fork**: 子进程复制 fd 表, 逐 handle 增引用 (见 `proc_ops.rs`);
//! - **close**: 引用计数归零时 `OpenFile` 被丢弃, 其 `Arc<dyn Inode>` 归零,
//!   [`PipeEndpointInode`] 的 `Drop` 释放端点 key ⇒ 对端才能观测到 EOF/EPIPE.
//!
//! ## 与 `functions::ipc::pipe` 的关系
//!
//! `functions::ipc::pipe` 是**端点 key 级**的环形缓冲区策略 (本模块以全限定
//! 路径调用, 不建立 `fs → ipc` 的模块依赖边); 本模块是**用户 fd 级**的适配层.
//!
//! ## 阻塞语义
//!
//! 端点 Inode 自身的 `read`/`write` 是**非阻塞**的 (空/满返回 `EAGAIN`),
//! 因为它们运行在 `PIPE_LOCK` 临界区内, 持锁睡眠会死锁. 真正的阻塞等待由
//! syscall 层 (`functions::fs::io`) 在锁外完成: 收到 `EAGAIN` 且端点未置
//! `O_NONBLOCK` 时调用 [`wait_readable`] / [`wait_writable`] 睡眠, 唤醒后重试.

use alloc::sync::Arc;

use super::inode::Inode;
use super::open_file_table::OPEN_FILE_TABLE;
use super::vfs_types::{
    KernelError, KernelResult, OpenFile, VfsFileType, VfsOpenFlags, VfsSeekWhence, VfsStat,
};
use crate::privileged::proc::process_get_current_pid;

/// 管道端点合成的 `node_id` 基址 (与文件系统 inode 编号区隔)
const PIPE_NODE_ID_BASE: u32 = 0xF000_0000;

/// 管道端点权限位 (`0600`)
const PIPE_PERM: u16 = 0o600;

/// 管道端点 Inode — 读端或写端, 持有 `functions::ipc::pipe` 的端点 key.
pub struct PipeEndpointInode {
    /// 端点 key (`read_key` / `write_key`, 由管道 id 派生, **非**用户 fd)
    key: i32,
    /// 是否读端 (方向错误由本字段提前判定, 与 `pipe_*_safe` 的 key 校验互为兜底)
    is_read: bool,
}

impl PipeEndpointInode {
    /// 构造管道端点 Inode
    fn new(key: i32, is_read: bool) -> Self {
        Self { key, is_read }
    }
}

impl Inode for PipeEndpointInode {
    fn read(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        if !self.is_read {
            return Err(KernelError::BadFd);
        }
        let count = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        match crate::functions::ipc::pipe::pipe_read_safe(
            crate::privileged::ipc::IPC_NAMESPACE.get_mut(),
            self.key,
            buf,
            count,
        ) {
            Ok(n) => Ok(n as usize),
            // -4: 空管道且仍有写者 ⇒ 非阻塞结果; 阻塞等待由 syscall 层转 [`wait_readable`]
            Err(-4) => Err(KernelError::WouldBlock),
            // -1/-2: 端点已关闭或方向不符
            Err(_) => Err(KernelError::BadFd),
        }
    }

    fn write(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        if self.is_read {
            return Err(KernelError::BadFd);
        }
        let count = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        match crate::functions::ipc::pipe::pipe_write_safe(
            crate::privileged::ipc::IPC_NAMESPACE.get_mut(),
            self.key,
            buf,
            count,
        ) {
            Ok(n) => Ok(n as usize),
            // -3: 读端全部关闭 ⇒ EPIPE
            Err(-3) => Err(KernelError::BrokenPipe),
            // -4: 缓冲区已满 ⇒ 非阻塞结果; 阻塞等待由 syscall 层转 [`wait_writable`]
            Err(-4) => Err(KernelError::WouldBlock),
            Err(_) => Err(KernelError::BadFd),
        }
    }

    fn stat(&self, _pwm: u64) -> KernelResult<VfsStat> {
        Ok(VfsStat {
            node_id: self.node_id(),
            perm: PIPE_PERM,
            file_type: VfsFileType::File.as_u8(),
            ..VfsStat::default()
        })
    }

    fn truncate(&self, _size: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::InvalidArgument)
    }

    fn seek(
        &self,
        _offset: i64,
        _whence: VfsSeekWhence,
        _current_offset: u64,
    ) -> KernelResult<u64> {
        // POSIX: 管道不可定位 (ESPIPE)
        Err(KernelError::IllegalSeek)
    }

    fn is_dir(&self) -> bool {
        false
    }

    fn node_id(&self) -> u32 {
        PIPE_NODE_ID_BASE.wrapping_add(u32::try_from(self.key).unwrap_or(0))
    }

    fn mount_idx(&self) -> u32 {
        // 管道无挂载点 (与匿名文件同一哨兵)
        u32::MAX
    }
}

impl Drop for PipeEndpointInode {
    fn drop(&mut self) {
        // OpenFile 引用计数归零 ⇒ 端点真正关闭; 仅当全部 fd 副本 (dup/fork) 释放后
        // 才会执行, 故对端 EOF / EPIPE 语义由引用计数天然保证.
        let _ = crate::functions::ipc::pipe::pipe_close_safe(
            crate::privileged::ipc::IPC_NAMESPACE.get_mut(),
            self.key,
        );
    }
}

/// 创建一对管道端点并分配用户 fd (读端, 写端).
///
/// 两个 fd 共用当前进程的 fd 空间 (与 VFS fd 同源), 编号互不重叠.
/// `nonblock` 为 `pipe2(O_NONBLOCK)` 的初始状态, 之后可由 `fcntl(F_SETFL)` 切换.
///
/// # Errors
/// 管道槽位耗尽 / 全局 `OpenFile` 表满 / 进程 fd 表满 → `ProcessFileLimit` (EMFILE).
pub fn create_pair(cloexec: bool, nonblock: bool) -> KernelResult<(i32, i32)> {
    let (read_end, write_end) = crate::functions::ipc::IpcLock
        .pipe_create(process_get_current_pid())
        .map_err(|_| KernelError::ProcessFileLimit)?;
    let (read_key, write_key) = (read_end.fd, write_end.fd);

    let read_of = OpenFile::new_pipe(
        Arc::new(PipeEndpointInode::new(read_key, true)),
        VfsOpenFlags::RDONLY.bits(),
        0,
        read_key,
        true,
        nonblock,
    );
    let Some(read_handle) = OPEN_FILE_TABLE.alloc(read_of) else {
        // read_of 在 alloc 内随函数结束被丢弃 ⇒ 端点 Drop 自动释放 read_key
        return Err(KernelError::ProcessFileLimit);
    };

    let write_of = OpenFile::new_pipe(
        Arc::new(PipeEndpointInode::new(write_key, false)),
        VfsOpenFlags::WRONLY.bits(),
        0,
        write_key,
        false,
        nonblock,
    );
    let Some(write_handle) = OPEN_FILE_TABLE.alloc(write_of) else {
        OPEN_FILE_TABLE.close(read_handle);
        return Err(KernelError::ProcessFileLimit);
    };

    // 同一闭包内完成两次 fd 分配: 第二个失败时回收第一个, 避免少量泄漏
    let fds = crate::privileged::proc::with_current_fd_table(|t| {
        let r = t.alloc_fd(read_handle, cloexec)?;
        if let Some(w) = t.alloc_fd(write_handle, cloexec) {
            Some((r, w))
        } else {
            t.close_fd(r);
            None
        }
    })
    .flatten();

    if let Some((r, w)) = fds {
        Ok((
            crate::functions::intcast::low_i32_from_usize(r),
            crate::functions::intcast::low_i32_from_usize(w),
        ))
    } else {
        OPEN_FILE_TABLE.close(read_handle);
        OPEN_FILE_TABLE.close(write_handle);
        Err(KernelError::ProcessFileLimit)
    }
}

/// 判断用户 fd 是否为管道端点.
///
/// 与 `sendfile`/`splice` 的 VFS fd 判定互补 (管道端点同样位于 VFS fd 空间).
pub fn is_pipe_fd(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    let Some(handle_id) = super::handle::vfs_get_fd_handle(fd as usize) else {
        return false;
    };
    OPEN_FILE_TABLE
        .with_file(handle_id, OpenFile::is_pipe_endpoint)
        .unwrap_or(false)
}

/// 取管道端点元数据 `(key, 是否读端, 是否非阻塞)`; 非管道 fd 返回 `None`.
///
/// syscall 层据此决定是否需要阻塞等待, 以及等待哪个端点的队列.
pub fn pipe_endpoint_of(fd: i32) -> Option<(i32, bool, bool)> {
    if fd < 0 {
        return None;
    }
    let handle_id = super::handle::vfs_get_fd_handle(fd as usize)?;
    OPEN_FILE_TABLE
        .with_file(handle_id, |of| {
            of.pipe_endpoint()
                .map(|(key, is_read)| (key, is_read, of.is_nonblock()))
        })
        .flatten()
}

/// 阻塞等待管道读端可读 (空缓冲且仍有写者时睡眠).
///
/// 返回 `true` 表示调用方应**重试**读取; `false` 表示当前上下文无法阻塞
/// (中断上下文 / 无进程上下文 / 等待队列满), 调用方应回退 `EAGAIN`.
pub fn wait_readable(key: i32) -> bool {
    crate::functions::ipc::pipe::pipe_wait_readable(
        crate::privileged::ipc::IPC_NAMESPACE.get_mut(),
        key,
    )
}

/// 阻塞等待管道写端可写 (缓冲已满且仍有读者时睡眠).
///
/// 返回值语义同 [`wait_readable`].
pub fn wait_writable(key: i32) -> bool {
    crate::functions::ipc::pipe::pipe_wait_writable(
        crate::privileged::ipc::IPC_NAMESPACE.get_mut(),
        key,
    )
}
