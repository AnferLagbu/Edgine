#![deny(unsafe_code)]
//! eventfd — 事件通知 fd 对象的 functions 层完整实现
//!
//! ## fd 对象模型 (阶段 3 首个迁移子系统)
//!
//! eventfd 不再是"全局位图 + 数值区间 [1100, 1116)"的槽位, 而是
//! [`EventFdFile`] 对象挂在全局 `OpenFileTable`, 用户 fd 由 per-process
//! `FdTable` 分配 (与 VFS fd 同源). 由此天然获得:
//!
//! - **close**: 引用计数归零时 `OpenFile` 槽位回收, 不再泄漏;
//! - **dup / fork**: 共享同一 `OpenFile`, 语义与 VFS fd 一致;
//! - **CLOEXEC**: fd 级标志落 per-process fd 表, exec 时统一关闭;
//! - **epoll**: 对象实现 [`File::poll`], 经 `VfsOps::fd_poll_special` 求值.
//!
//! 对象**不实现** `Inode` (`as_inode() == None`), 该判据同时用于:
//! sendfile/splice 排除特殊对象、写循环对特殊对象短写即停、`fstat` 合成 stat.
//!
//! ## 语义 (Linux eventfd)
//!
//! - read: 信号量模式返回 1 并减 1; 否则返回计数并清零; 计为 0 → `EAGAIN`
//! - write: 计 += value; `value == 0` → `EINVAL`; 溢出 (`> U64_MAX - 1`) → `EAGAIN`
//! - poll: 计数 > 0 → `IN`; 计数 < `U64_MAX - 1` → `OUT`

use alloc::sync::Arc;

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::syscall::Errno;

/// `EFD_SEMAPHORE` 标志 (与 Linux 一致)
pub const EFD_SEMAPHORE: i32 = 0o1;
/// `EFD_NONBLOCK` 标志 (与 Linux 一致)
pub const EFD_NONBLOCK: i32 = 0o4000;
/// `EFD_CLOEXEC` 标志 (与 Linux 一致)
pub const EFD_CLOEXEC: i32 = 0o200_0000;

/// eventfd 内部状态 — 计数器与模式, 由自旋锁保护
struct EventFdState {
    /// 内核计数器 (不变量: `<= u64::MAX - 1`)
    counter: u64,
    /// 是否信号量模式
    semaphore: bool,
}

/// eventfd fd 对象 — 计数器 + 信号量模式
pub struct EventFdFile {
    /// 计数器状态
    state: IrqSpinLock<EventFdState>,
}

impl EventFdFile {
    /// 构造 eventfd 对象
    ///
    /// 调用方保证 `initval <= u64::MAX - 1` (syscall 入口拒绝 `u64::MAX`).
    #[must_use]
    pub fn new(initval: u64, semaphore: bool) -> Self {
        Self {
            state: IrqSpinLock::new(EventFdState {
                counter: initval,
                semaphore,
            }),
        }
    }
}

impl File for EventFdFile {
    // SIMPLIFIED: 阻塞读等待未实装 — 计为 0 时恒返回 EAGAIN (与原 privileged
    // 实现一致, 无等待队列); 影响面 = 未置 EFD_NONBLOCK 的读也不睡眠; 何时需
    // 扩展: eventfd 等待队列接入 epoll 唤醒路径时.
    fn read_at(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        if buf.len() < 8 {
            return Err(KernelError::InvalidArgument);
        }
        let value = {
            let mut state = self.state.lock();
            if state.counter == 0 {
                return Err(KernelError::WouldBlock);
            }
            if state.semaphore {
                state.counter -= 1;
                1u64
            } else {
                core::mem::take(&mut state.counter)
            }
        };
        buf[..8].copy_from_slice(&value.to_ne_bytes());
        Ok(8)
    }

    fn write_at(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        if buf.len() < 8 {
            return Err(KernelError::InvalidArgument);
        }
        let value = u64::from_ne_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]);
        if value == 0 {
            return Err(KernelError::InvalidArgument);
        }
        let mut state = self.state.lock();
        // 溢出判据与 Linux 一致: 计数上限 U64_MAX - 1 保留给唤醒值
        let Some(room) = (u64::MAX - 1).checked_sub(state.counter) else {
            return Err(KernelError::WouldBlock);
        };
        if value > room {
            return Err(KernelError::WouldBlock);
        }
        state.counter += value;
        Ok(8)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let state = self.state.lock();
        let mut ready = PollEvents::empty();
        if state.counter > 0 {
            ready |= PollEvents::IN;
        }
        if state.counter < u64::MAX - 1 {
            ready |= PollEvents::OUT;
        }
        ready & events
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn stream(&self) -> bool {
        // POSIX: eventfd 不可定位 (lseek → ESPIPE)
        true
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// `eventfd` / `eventfd2` 系统调用 — 创建 eventfd 对象并挂 per-process fd 表
///
/// # Errors
///
/// - flags 含未知位, 或 `initval == u64::MAX` → `EINVAL`
/// - `OpenFileTable` 或 per-process fd 表耗尽 → `EMFILE`
pub fn eventfd_syscall(initval: u64, flags: i32) -> Result<usize, Errno> {
    let valid_flags = EFD_CLOEXEC | EFD_NONBLOCK | EFD_SEMAPHORE;
    if flags & !valid_flags != 0 || initval == u64::MAX {
        return Err(Errno::Einval);
    }
    let semaphore = (flags & EFD_SEMAPHORE) != 0;
    let nonblock = (flags & EFD_NONBLOCK) != 0;
    let cloexec = (flags & EFD_CLOEXEC) != 0;

    let pwm = crate::privileged::sgeg::pwm_get_current();
    let open_file = OpenFile::from_file(Arc::new(EventFdFile::new(initval, semaphore)), 0, pwm);
    open_file.set_nonblock(nonblock);

    let Some(handle_id) = OPEN_FILE_TABLE.alloc(open_file) else {
        return Err(Errno::Emfile);
    };
    // 无进程上下文 / fd 表耗尽: 回滚 OpenFile 槽位后报 EMFILE
    let Some(fd_idx) =
        crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, cloexec))
            .flatten()
    else {
        OPEN_FILE_TABLE.close(handle_id);
        return Err(Errno::Emfile);
    };
    Ok(fd_idx)
}

// ============================================================================
// host 侧单元测试 (cargo test; 含 syscall 级 fd 分配/dup/close 闭环)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时安装当前进程 (含独立 FdTable), 执行闭包后无条件拆除现场
    ///
    /// per-process fd 分配依赖当前进程上下文; 与 `fs` 模块的单测共用
    /// `FS_GLOBAL_TEST_LOCK` 串行化全局进程表改动.
    fn with_temp_process<F: FnOnce()>(name: &str, f: F) {
        use crate::privileged::proc::PROCESS_TABLE;
        use crate::privileged::proc::raw;
        use crate::privileged::proc::scheduler::Scheduler;

        let Some(pid) = PROCESS_TABLE.allocate_pid() else {
            panic!("临时进程: 无空闲 pid");
        };
        let proc_ptr = raw::alloc_process(pid, name, None);
        if !PROCESS_TABLE.insert(proc_ptr) {
            raw::drop_boxed_process(proc_ptr);
            PROCESS_TABLE.free_pid(pid);
            panic!("临时进程: 插入进程表失败");
        }

        let prev_current = Scheduler::current();
        Scheduler::set_current(pid);

        f();

        Scheduler::set_current(prev_current.unwrap_or(0));
        let _ = PROCESS_TABLE.remove(pid);
        raw::drop_boxed_process(proc_ptr);
        PROCESS_TABLE.free_pid(pid);
    }

    #[test]
    fn object_read_write_and_errors() {
        let file = EventFdFile::new(5, false);
        let mut buf = [0u8; 8];
        assert_eq!(file.read_at(0, &mut buf, 0), Ok(8));
        assert_eq!(u64::from_ne_bytes(buf), 5);
        assert_eq!(
            file.read_at(0, &mut buf, 0),
            Err(KernelError::WouldBlock),
            "计数清零后再读 EAGAIN"
        );

        let val = 10u64.to_ne_bytes();
        assert_eq!(file.write_at(0, &val, 0), Ok(8));
        assert_eq!(file.read_at(0, &mut buf, 0), Ok(8));
        assert_eq!(u64::from_ne_bytes(buf), 10);

        assert_eq!(
            file.write_at(0, &[0u8; 8], 0),
            Err(KernelError::InvalidArgument),
            "write(0) → EINVAL"
        );
        assert_eq!(
            file.read_at(0, &mut [0u8; 4], 0),
            Err(KernelError::InvalidArgument),
            "短缓冲读 → EINVAL"
        );
        assert_eq!(
            file.write_at(0, &[1u8; 4], 0),
            Err(KernelError::InvalidArgument),
            "短缓冲写 → EINVAL"
        );
    }

    #[test]
    fn object_semaphore_counts_one() {
        let file = EventFdFile::new(3, true);
        let mut buf = [0u8; 8];
        for _ in 0..3 {
            assert_eq!(file.read_at(0, &mut buf, 0), Ok(8));
            assert_eq!(u64::from_ne_bytes(buf), 1);
        }
        assert_eq!(
            file.read_at(0, &mut buf, 0),
            Err(KernelError::WouldBlock),
            "信号量计数耗尽 EAGAIN"
        );
    }

    #[test]
    fn object_write_overflow_is_eagain() {
        let file = EventFdFile::new(u64::MAX - 2, false);
        let val = 1u64.to_ne_bytes();
        assert_eq!(file.write_at(0, &val, 0), Ok(8));
        assert_eq!(
            file.write_at(0, &val, 0),
            Err(KernelError::WouldBlock),
            "计数达 U64_MAX - 1 后再写 EAGAIN"
        );
    }

    #[test]
    fn object_poll_and_stream_contract() {
        let file = EventFdFile::new(0, false);
        assert!(file.poll(PollEvents::IN).is_empty(), "计数 0 不可读");
        assert_eq!(file.poll(PollEvents::OUT), PollEvents::OUT, "计数 0 可写");
        let val = 1u64.to_ne_bytes();
        assert_eq!(file.write_at(0, &val, 0), Ok(8));
        assert_eq!(
            file.poll(PollEvents::IN | PollEvents::OUT),
            PollEvents::IN | PollEvents::OUT
        );
        assert!(file.stream(), "eventfd 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "eventfd 非 inode 承载");
        assert_eq!(file.file_type(), VfsFileType::File);
    }

    #[test]
    fn syscall_create_dup_read_close() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("eventfd-t", || {
            let fd = eventfd_syscall(5, EFD_CLOEXEC | EFD_SEMAPHORE).expect("创建 eventfd");
            assert!(
                fd >= 3,
                "eventfd fd 由 per-process 表分配 (>= FIRST_USER_FD)"
            );
            let handle = crate::functions::fs::vfs_get_fd_handle(fd).expect("fd 应有 OpenFile");
            assert_eq!(
                crate::privileged::proc::with_current_fd_table(|t| t.is_cloexec(fd)),
                Some(true),
                "EFD_CLOEXEC 应落 per-process fd 表"
            );

            let mut buf = [0u8; 8];
            assert_eq!(
                crate::functions::fs::api::vfs_read_safe(
                    crate::functions::intcast::low_u32_from_usize(fd),
                    &mut buf
                ),
                8
            );
            assert_eq!(u64::from_ne_bytes(buf), 1, "信号量模式读返回 1");

            let dup_fd =
                crate::functions::fs::vfs_dup(crate::functions::intcast::low_u32_from_usize(fd));
            assert!(dup_fd >= 0, "dup 应成功");
            let dup_idx = crate::functions::intcast::usize_from_i32(dup_fd);
            assert_ne!(dup_idx, fd, "dup 分配新 fd");
            assert_eq!(
                crate::functions::fs::vfs_get_fd_handle(dup_idx),
                Some(handle),
                "dup 共享同一 OpenFile"
            );

            let val = 7u64.to_ne_bytes();
            assert_eq!(
                crate::functions::fs::api::vfs_write_safe(
                    crate::functions::intcast::low_u32_from_usize(fd),
                    &val
                ),
                8
            );
            assert_eq!(
                crate::functions::fs::api::vfs_read_safe(
                    crate::functions::intcast::low_u32_from_usize(dup_idx),
                    &mut buf
                ),
                8,
                "dup 共享状态与偏移"
            );
            assert_eq!(u64::from_ne_bytes(buf), 1);

            assert_eq!(
                crate::functions::fs::api::vfs_close_safe(
                    crate::functions::intcast::low_u32_from_usize(fd)
                ),
                0
            );
            assert!(
                crate::functions::fs::vfs_get_fd_handle(fd).is_none(),
                "close 后 fd 槽位释放"
            );
            assert_eq!(
                crate::functions::fs::api::vfs_close_safe(dup_fd.cast_unsigned()),
                0
            );
        });
    }

    #[test]
    fn syscall_rejects_invalid_flags_and_initval() {
        assert_eq!(eventfd_syscall(0, 0o10), Err(Errno::Einval), "未知标志");
        assert_eq!(
            eventfd_syscall(u64::MAX, 0),
            Err(Errno::Einval),
            "initval == U64_MAX"
        );
    }
}
