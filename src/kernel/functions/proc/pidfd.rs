#![deny(unsafe_code)]
//! pidfd — 进程句柄 fd 对象的 functions 层完整实现
//!
//! ## fd 对象模型 (阶段 3 第 7 个迁移子系统)
//!
//! pidfd 不再是"全局 fd→pid 映射表 + 数值区间 `[1180, 1196)`"的槽位, 而是
//! [`PidFdFile`] 对象挂在全局 `OpenFileTable`, 用户 fd 由 per-process
//! `FdTable` 分配 (与 VFS fd 同源). 由此天然获得:
//!
//! - **close**: 引用计数归零时 `OpenFile` 槽位回收;
//! - **dup / fork**: 共享同一 `OpenFile`, 语义与 VFS fd 一致;
//! - **epoll**: 对象实现 [`File::poll`], 经 `VfsOps::fd_poll_special` 求值.
//!
//! 对象**不实现** `Inode` (`as_inode() == None`), 该判据同时用于
//! sendfile/splice 排除特殊对象与 `fstat` 合成 stat.
//!
//! ## 对象持有目标进程引用 (PID 复用防护)
//!
//! `pidfd_open` 经 `proc::table::try_inc_ref` 持有目标进程引用, 最后一个
//! fd 关闭 (`File::release`) 时归还. 目标退出后 PCB/PID 保留到 pidfd 关闭
//! (与 Linux `struct pid` 的句柄钉住语义一致), 旧 pidfd 不会因 PID 复用而
//! 把信号误发到无关进程.
//!
//! ## 语义 (Linux pidfd)
//!
//! - read / write: `EINVAL` (pidfd 无数据面)
//! - poll: 目标已退出 (`Zombie`/`Terminated`, 或 PCB 已回收) → `POLLIN`
//! - `pidfd_send_signal`: 经 pidfd 解析对象后转 `sys_kill`
//! - `pidfd_getfd`: 未实装, `ENOSYS`

use alloc::sync::Arc;

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::functions::proc::table;
use crate::privileged::proc::ProcessState;
use crate::privileged::syscall::Errno;

/// `PIDFD_NONBLOCK` 标志 (与 Linux `O_NONBLOCK` = 0o4000 一致)
const PIDFD_NONBLOCK: u32 = 0o4000;

/// pidfd fd 对象 — 目标进程 pid
///
/// 对象构造时即持有目标进程引用 (见模块文档), 生命周期与 `OpenFile`
/// 引用计数一致.
pub struct PidFdFile {
    /// 目标进程 pid
    pid: u32,
}

impl PidFdFile {
    /// 构造 pidfd 对象
    ///
    /// 调用方保证 `pid` 在进程表中存在且已经 `try_inc_ref` 持有引用
    /// (创建路径 [`pidfd_open`] 负责).
    #[must_use]
    pub fn new(pid: u32) -> Self {
        Self { pid }
    }

    /// 目标进程 pid
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl File for PidFdFile {
    fn read_at(&self, _offset: u64, _buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        // Linux: pidfd 无数据面, read 返回 EINVAL
        Err(KernelError::InvalidArgument)
    }

    fn write_at(&self, _offset: u64, _buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        Err(KernelError::InvalidArgument)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        // 目标已退出 ⇒ 可读 (Linux pidfd 用 EPOLLIN 报告进程退出);
        // PCB 已被回收 (防御性路径) 同样视为已退出.
        let exited = match table::get_state(self.pid) {
            Some(state) => matches!(state, ProcessState::Zombie | ProcessState::Terminated),
            None => true,
        };
        if exited {
            events & PollEvents::IN
        } else {
            PollEvents::empty()
        }
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn release(&self) {
        // 归还 `pidfd_open` 时持有的进程引用; 若目标已退出且 wait4 已摘除,
        // 这里完成 PCB/PID 的实际回收.
        table::dec_ref_and_maybe_free(self.pid);
    }

    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn stream(&self) -> bool {
        // POSIX: pidfd 不可定位 (lseek → ESPIPE)
        true
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// 经 fd 解析 pidfd 对象, 返回目标 pid.
///
/// 非 pidfd 对象 (VFS 文件 / 管道 / 其他特殊对象) → `None`.
fn resolve_pid(fd: i32) -> Option<u32> {
    let handle_id =
        crate::functions::fs::vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))?;
    OPEN_FILE_TABLE
        .with_file(handle_id, |of| {
            of.file()
                .as_any()
                .downcast_ref::<PidFdFile>()
                .map(PidFdFile::pid)
        })
        .flatten()
}

/// `pidfd_open` — 为进程打开一个 pidfd
///
/// # Errors
///
/// - flags 含非法位 → `EINVAL`
/// - 进程不存在 → `ESRCH`
/// - `OpenFileTable` 或 per-process fd 表耗尽 → `EMFILE`
pub fn pidfd_open(pid: u32, flags: u32) -> Result<usize, Errno> {
    if flags & !PIDFD_NONBLOCK != 0 {
        return Err(Errno::Einval);
    }
    let nonblock = flags & PIDFD_NONBLOCK != 0;

    // 进程存在性 + 引用持有 (两者之间被回收 → try_inc_ref 失败, 同样 ESRCH)
    if table::get_state(pid).is_none() {
        return Err(Errno::Esrch);
    }
    table::try_inc_ref(pid).map_err(|_| Errno::Esrch)?;

    let pwm = crate::privileged::sgeg::pwm_get_current();
    let open_file = OpenFile::from_file(Arc::new(PidFdFile::new(pid)), 0, pwm);
    open_file.set_nonblock(nonblock);

    let Some(handle_id) = OPEN_FILE_TABLE.alloc(open_file) else {
        // OpenFile 槽位耗尽: 对象未被挂表 (release 不会触发), 显式归还引用
        table::dec_ref_and_maybe_free(pid);
        return Err(Errno::Emfile);
    };
    // 无进程上下文 / fd 表耗尽: 关闭 handle 触发 release (内部归还进程引用)
    let Some(fd_idx) =
        crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, false)).flatten()
    else {
        OPEN_FILE_TABLE.close(handle_id);
        return Err(Errno::Emfile);
    };
    Ok(fd_idx)
}

/// `pidfd_send_signal` — 通过 pidfd 发送信号
///
/// # Errors
///
/// - 信号编号不在 1..=63 范围 → `EINVAL`
/// - pidfd 无效 (fd 不存在 / 非 pidfd 对象) → `EBADF`
/// - 目标进程不存在 → `ESRCH`
pub fn pidfd_send_signal(pidfd: i32, sig: i32, _siginfo: u64, _flags: u32) -> Result<usize, Errno> {
    if !(1..=63).contains(&sig) {
        return Err(Errno::Einval);
    }

    let pid = resolve_pid(pidfd).ok_or(Errno::Ebadf)?;

    // 复用 kill_syscall 的 pid>0 语义 (privileged 内部 4 路径分发)
    let ret = crate::privileged::syscall::api::sys_kill(pid.cast_signed(), sig);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(0)
    }
}

/// `pidfd_getfd` — 通过 pidfd 获取文件描述符
///
/// 需要完整的跨进程 `OpenFile` 复制语义, 暂存根.
///
/// # Errors
///
/// 该接口尚未实现, 始终返回 `ENOSYS`.
pub fn pidfd_getfd(_pidfd: i32, _targetfd: u32, _flags: u32) -> Result<usize, Errno> {
    // 依赖跨进程 fd 复制能力, 登记分册 9 B09-10
    Err(Errno::Enosys)
}

// ============================================================================
// host 侧单元测试 (cargo test; 含 syscall 级 fd 分配/dup/close 与引用钉住闭环)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::intcast::{low_i32_from_usize, low_u32_from_usize, usize_from_i32};
    use crate::privileged::proc::PROCESS_TABLE;
    use crate::privileged::proc::raw;
    use crate::privileged::proc::scheduler::Scheduler;
    use core::sync::atomic::Ordering;

    /// SIGCHLD (默认动作忽略, 用于验证 send_signal 通路)
    const SIGCHLD: i32 = 17;

    /// 临时安装当前进程 (含独立 FdTable), 执行闭包后无条件拆除现场
    fn with_temp_process<F: FnOnce()>(name: &str, f: F) {
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

    /// 分配一个非当前的目标进程 (不切换调度器), 返回 `(pid, PCB 指针)`.
    fn alloc_target(name: &str) -> (u32, *mut crate::privileged::proc::Process) {
        let Some(pid) = PROCESS_TABLE.allocate_pid() else {
            panic!("目标进程: 无空闲 pid");
        };
        let proc_ptr = raw::alloc_process(pid, name, None);
        if !PROCESS_TABLE.insert(proc_ptr) {
            raw::drop_boxed_process(proc_ptr);
            PROCESS_TABLE.free_pid(pid);
            panic!("目标进程: 插入进程表失败");
        }
        (pid, proc_ptr)
    }

    fn target_cleanup(pid: u32, proc_ptr: *mut crate::privileged::proc::Process) {
        let _ = PROCESS_TABLE.remove(pid);
        raw::drop_boxed_process(proc_ptr);
        PROCESS_TABLE.free_pid(pid);
    }

    fn ref_count(pid: u32) -> u32 {
        table::with(pid, |p| p.ref_count.load(Ordering::Acquire)).expect("进程表项存在")
    }

    fn close_fd(fd: usize) -> i32 {
        crate::functions::fs::api::vfs_close_safe(low_u32_from_usize(fd))
    }

    #[test]
    fn object_file_contract() {
        let file = PidFdFile::new(1234);
        assert_eq!(file.pid(), 1234);
        assert!(file.stream(), "pidfd 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "pidfd 非 inode 承载");
        assert!(file.mmap_backing().is_none(), "pidfd 不可作 mmap 后端");
        assert!(
            file.inode_namespace_id().is_none(),
            "pidfd 不属于 inode 命名空间"
        );
        assert_eq!(file.file_type(), VfsFileType::File);
        assert_eq!(
            file.read_at(0, &mut [0u8; 8], 0),
            Err(KernelError::InvalidArgument),
            "pidfd 无数据面 (read → EINVAL)"
        );
        assert_eq!(
            file.write_at(0, &[0u8; 8], 0),
            Err(KernelError::InvalidArgument),
            "pidfd 无数据面 (write → EINVAL)"
        );
        assert_eq!(
            file.io_control(0x1234_5678, 0),
            Err(KernelError::NotSupported),
            "pidfd 对象不处理通用 ioctl"
        );
    }

    #[test]
    fn object_downcast() {
        let file = PidFdFile::new(7);
        let erased: &dyn File = &file;
        assert_eq!(
            erased
                .as_any()
                .downcast_ref::<PidFdFile>()
                .map(PidFdFile::pid),
            Some(7),
            "PidFdFile 必须可下转型"
        );
    }

    #[test]
    fn syscall_create_dup_close_and_pin_process() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("pidfd-t", || {
            let self_pid = Scheduler::current().expect("当前临时进程");
            let before = ref_count(self_pid);

            let fd = pidfd_open(self_pid, 0).expect("pidfd_open");
            assert!(fd >= 3, "pidfd 由 per-process fd 表分配 (>= FIRST_USER_FD)");
            assert_eq!(
                ref_count(self_pid),
                before + 1,
                "pidfd 必须钉住目标进程引用"
            );

            let handle = crate::functions::fs::vfs_get_fd_handle(fd).expect("fd 应有 OpenFile");
            assert_eq!(
                OPEN_FILE_TABLE.with_file(handle, |of| of.is_nonblock()),
                Some(false),
                "未带 PIDFD_NONBLOCK 时 OpenFile 非阻塞位应为 false"
            );
            assert_eq!(
                crate::privileged::proc::with_current_fd_table(|t| t.is_cloexec(fd)),
                Some(false),
                "pidfd_open 无 CLOEXEC 语义"
            );

            let dup_fd = crate::functions::fs::vfs_dup(low_u32_from_usize(fd));
            assert!(dup_fd >= 0, "dup 应成功");
            let dup_idx = usize_from_i32(dup_fd);
            assert_ne!(dup_idx, fd, "dup 分配新 fd");
            assert_eq!(
                crate::functions::fs::vfs_get_fd_handle(dup_idx),
                Some(handle),
                "dup 共享同一 OpenFile"
            );

            // 存活目标: poll 不就绪; send_signal 经对象解析成功
            assert_eq!(
                OPEN_FILE_TABLE.with_file(handle, |of| of.file().poll(PollEvents::IN)),
                Some(PollEvents::empty()),
                "目标存活时 pidfd 不可读"
            );
            assert_eq!(
                pidfd_send_signal(low_i32_from_usize(fd), SIGCHLD, 0, 0),
                Ok(0)
            );
            assert_eq!(
                pidfd_send_signal(dup_fd, SIGCHLD, 0, 0),
                Ok(0),
                "dup 可正常解析"
            );

            // 关闭原 fd: dup 仍持引用, 对象仍然可用
            assert_eq!(close_fd(fd), 0);
            assert_eq!(pidfd_send_signal(dup_fd, SIGCHLD, 0, 0), Ok(0));
            assert_eq!(ref_count(self_pid), before + 1, "dup 未关闭前引用不归还");

            // 关闭 dup → release → 引用归还
            assert_eq!(close_fd(dup_idx), 0);
            assert_eq!(ref_count(self_pid), before, "最后关闭必须归还进程引用");
        });
    }

    #[test]
    fn syscall_esrch_einval_ebadf_and_exit_readiness() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("pidfd-t2", || {
            let self_pid = Scheduler::current().expect("当前临时进程");

            // 未插入进程表的 pid → ESRCH
            let free_pid = PROCESS_TABLE.allocate_pid().expect("空 pid");
            assert_eq!(pidfd_open(free_pid, 0), Err(Errno::Esrch));
            PROCESS_TABLE.free_pid(free_pid);

            // 非法 flags → EINVAL (bit 0 不在 PIDFD_NONBLOCK)
            assert_eq!(pidfd_open(self_pid, 1), Err(Errno::Einval));
            // PIDFD_NONBLOCK 合法
            let nbfd = pidfd_open(self_pid, PIDFD_NONBLOCK).expect("非阻塞 pidfd");
            let nb_handle = crate::functions::fs::vfs_get_fd_handle(nbfd).expect("OpenFile");
            assert_eq!(
                OPEN_FILE_TABLE.with_file(nb_handle, |of| of.is_nonblock()),
                Some(true),
                "PIDFD_NONBLOCK 应落 OpenFile 非阻塞位"
            );
            assert_eq!(close_fd(nbfd), 0);

            // 非 pidfd fd → EBADF
            assert_eq!(pidfd_send_signal(0, SIGCHLD, 0, 0), Err(Errno::Ebadf));
            assert_eq!(
                pidfd_send_signal(99, SIGCHLD, 0, 0),
                Err(Errno::Ebadf),
                "未分配 fd → EBADF"
            );
            // 非法信号 → EINVAL
            assert_eq!(pidfd_send_signal(0, 0, 0, 0), Err(Errno::Einval));
            assert_eq!(pidfd_send_signal(0, 64, 0, 0), Err(Errno::Einval));

            // 目标退出 ⇒ poll 可读
            let (target_pid, target_ptr) = alloc_target("pidfd-target");
            let fd = pidfd_open(target_pid, 0).expect("打开目标 pidfd");
            let handle = crate::functions::fs::vfs_get_fd_handle(fd).expect("OpenFile");
            assert_eq!(
                OPEN_FILE_TABLE.with_file(handle, |of| of.file().poll(PollEvents::IN)),
                Some(PollEvents::empty())
            );
            table::with_mut(target_pid, |p| p.mark_zombie()).expect("目标进程存在");
            assert_eq!(
                OPEN_FILE_TABLE.with_file(handle, |of| of.file().poll(PollEvents::IN)),
                Some(PollEvents::IN),
                "目标退出后 pidfd 应报告可读"
            );
            assert_eq!(close_fd(fd), 0);
            target_cleanup(target_pid, target_ptr);
        });
    }
}
