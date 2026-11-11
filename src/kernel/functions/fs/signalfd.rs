#![deny(unsafe_code)]
//! signalfd — 信号接收 fd 对象的 functions 层完整实现
//!
//! ## fd 对象模型 (阶段 3 第 2 个迁移子系统)
//!
//! signalfd 不再是"全局槽位表 + 数值区间 [1120, 1136)"的位图模型, 而是
//! [`SignalFdFile`] 对象挂在全局 `OpenFileTable`, 用户 fd 由 per-process
//! `FdTable` 分配 (与 VFS fd 同源). 由此天然获得 close/dup/fork/CLOEXEC
//! 与 epoll 的统一语义 (与 eventfd 同构).
//!
//! 对象**不实现** `Inode` (`as_inode() == None`), 该判据同时用于:
//! sendfile/splice 排除特殊对象、写循环对特殊对象短写即停、`fstat` 合成 stat.
//!
//! ## 语义 (Linux signalfd)
//!
//! - create/modify: `signalfd(fd, mask, flags)`; `fd == -1` 创建, `fd >= 0`
//!   修改既有实例掩码 (经 [`File::io_control`] 内部命令 [`SFD_IOC_SETMASK`])
//! - read: 取 `pending & sigmask` 中最低编号信号, 消费该信号, 写 128 字节
//!   `signalfd_siginfo`; 无待处理信号 → `EAGAIN`
//! - poll: `pending & sigmask != 0` → `IN`
//!
//! **PID 绑定**: 对象记录创建进程 PID, read/poll 仅对创建进程生效 (与原实现
//! 一致; fork 继承的 fd 在子进程读取按无效 fd 返回 `EBADF`).
//!
//! **掩码宽度**: 内部掩码收窄为 `u64` —— 进程 pending 位图本就是 `u64`
//! (信号 1-64), 用户 `u128` 掩码的高 64 位永无对应 pending 信号.

use alloc::sync::Arc;

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::syscall::Errno;

/// `SFD_CLOEXEC` (`O_CLOEXEC`)
pub const SFD_CLOEXEC: i32 = 0o200_0000;
/// `SFD_NONBLOCK` (`O_NONBLOCK`)
pub const SFD_NONBLOCK: i32 = 0o4000;

/// `signalfd_siginfo` 大小 (128 字节, 与 Linux 一致)
pub const SIGNALFD_SIGINFO_SIZE: usize = 128;

/// 修改既有 signalfd 掩码的内部命令 (`File::io_control` 用).
///
/// Linux ABI 没有对应的 ioctl 命令号 (掩码修改走 `signalfd(2)` 自身);
/// 该命令仅供本模块的创建/修改路径经 `dyn File` 回访对象私有状态.
/// 经通用 `ioctl(2)` 携带此命令同样生效, 入参掩码在 `io_control` 内重复过滤.
pub const SFD_IOC_SETMASK: u32 = 0x5346_0001;

/// `SIGKILL`(9) / `SIGSTOP`(19) 不可屏蔽 — 掩码中 bit 8 / bit 18 恒被过滤
const UNMASKABLE_SIGNALS: u64 = (1u64 << 8) | (1u64 << 18);

// ============================================================================
// signalfd_siginfo 布局 (与 Linux 兼容)
// ============================================================================

/// `signalfd_siginfo` — 传递给用户空间的信号信息
///
/// 布局与 Linux `struct signalfd_siginfo` 兼容 (128 字节). 当前仅填充
/// `ssi_signo`, 其余字段置零 (发送者 PID / 来源码等暂不追踪).
#[repr(C)]
#[derive(Copy, Clone)]
pub struct SignalFdSigInfo {
    /// 0: 信号编号
    pub ssi_signo: u32,
    /// 4: 错误码 (通常 0)
    pub ssi_errno: i32,
    /// 8: 信号来源码
    pub ssi_code: i32,
    /// 12: 发送者 PID
    pub ssi_pid: u32,
    /// 16: 发送者 UID
    pub ssi_uid: u32,
    /// 20: 文件描述符 (SIGIO)
    pub ssi_fd: i32,
    /// 24: 带宽事件 (SIGIO)
    pub ssi_band: u32,
    /// 28: 定时器 ID (`SIGEV_THREAD_ID`)
    pub ssi_tid: u32,
    /// 32: 定时器溢出计数
    pub ssi_overrun: u32,
    /// 36: 陷阱号
    pub ssi_trapno: u32,
    /// 40: 退出状态 / 信号码
    pub ssi_status: i32,
    /// 44: POSIX.1b 信号值 (int)
    pub ssi_int: i32,
    /// 48: POSIX.1b 信号值 (ptr)
    pub ssi_ptr: u64,
    /// 56: 用户 CPU 时间
    pub ssi_utime: u64,
    /// 64: 系统 CPU 时间
    pub ssi_stime: u64,
    /// 72: 触发地址
    pub ssi_addr: u64,
    /// 80-127: 填充到 128 字节
    pub pad: [u8; 48],
}

impl SignalFdSigInfo {
    /// 以信号编号构造, 其余字段置零
    #[must_use]
    pub const fn with_signo(signo: u32) -> Self {
        Self {
            ssi_signo: signo,
            ssi_errno: 0,
            ssi_code: 0,
            ssi_pid: 0,
            ssi_uid: 0,
            ssi_fd: 0,
            ssi_band: 0,
            ssi_tid: 0,
            ssi_overrun: 0,
            ssi_trapno: 0,
            ssi_status: 0,
            ssi_int: 0,
            ssi_ptr: 0,
            ssi_utime: 0,
            ssi_stime: 0,
            ssi_addr: 0,
            pad: [0u8; 48],
        }
    }
}

// ============================================================================
// 对象结构
// ============================================================================

/// signalfd 内部状态 — 掩码与绑定 PID, 由自旋锁保护
struct SignalFdState {
    /// 信号掩码 (bit N = 信号 N+1; 已过滤 SIGKILL/SIGSTOP)
    sigmask: u64,
    /// 绑定进程 PID
    pid: u32,
}

/// signalfd fd 对象 — 掩码 + 绑定 PID
pub struct SignalFdFile {
    /// 掩码与绑定 PID
    state: IrqSpinLock<SignalFdState>,
}

impl SignalFdFile {
    /// 构造 signalfd 对象
    ///
    /// 调用方保证 `sigmask` 已过滤 SIGKILL/SIGSTOP.
    #[must_use]
    pub fn new(sigmask: u64, pid: u32) -> Self {
        Self {
            state: IrqSpinLock::new(SignalFdState { sigmask, pid }),
        }
    }
}

impl File for SignalFdFile {
    // SIMPLIFIED: 阻塞读等待未实装 — 无待处理信号时恒返回 EAGAIN (与原
    // privileged 实现一致, 无等待队列); 影响面 = 未置 SFD_NONBLOCK 的读也不
    // 睡眠; 何时需扩展: signalfd 等待队列接入 epoll 唤醒路径时.
    fn read_at(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        // Linux: 缓冲区小于 sizeof(signalfd_siginfo) → EINVAL
        if buf.len() < SIGNALFD_SIGINFO_SIZE {
            return Err(KernelError::InvalidArgument);
        }
        let current_pid = crate::privileged::proc::process_get_current_pid();
        if current_pid == 0 {
            return Err(KernelError::InvalidArgument);
        }
        // 先复制私有状态再查询 pending, 避免持对象锁嵌套进程表锁
        let (sigmask, bound_pid) = {
            let state = self.state.lock();
            (state.sigmask, state.pid)
        };
        if bound_pid != current_pid {
            return Err(KernelError::BadFd);
        }
        let ready = process_pending(current_pid) & sigmask;
        if ready == 0 {
            return Err(KernelError::WouldBlock);
        }
        // 取最低编号信号 (trailing zeros + 1) 并消费
        let signo = ready.trailing_zeros() + 1;
        clear_pending(current_pid, signo);

        let info = SignalFdSigInfo::with_signo(signo);
        // I4: POD 字节视图经 privileged 安全辅助构造 (无对齐/别名假设)
        let bytes = crate::privileged::mm::pod_as_bytes(&info);
        buf[..SIGNALFD_SIGINFO_SIZE].copy_from_slice(bytes);
        Ok(SIGNALFD_SIGINFO_SIZE)
    }

    fn write_at(&self, _offset: u64, _buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        // signalfd 只读 (Linux: 向 signalfd 写入返回 EINVAL)
        Err(KernelError::InvalidArgument)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let current_pid = crate::privileged::proc::process_get_current_pid();
        let (sigmask, bound_pid) = {
            let state = self.state.lock();
            (state.sigmask, state.pid)
        };
        if bound_pid != current_pid {
            // 绑定进程之外: fd 对当前进程无效, 按 poll 契约无条件附加 ERR
            return PollEvents::ERR;
        }
        let mut ready = PollEvents::empty();
        if process_pending(current_pid) & sigmask != 0 {
            ready |= PollEvents::IN;
        }
        ready & events
    }

    fn io_control(&self, cmd: u32, arg: usize) -> KernelResult<usize> {
        if cmd == SFD_IOC_SETMASK {
            // 通用 ioctl(2) 也可携带本命令, 故在此边界重复过滤不可屏蔽信号
            let sigmask = crate::functions::intcast::u64_from_usize(arg) & !UNMASKABLE_SIGNALS;
            let mut state = self.state.lock();
            state.sigmask = sigmask;
            Ok(0)
        } else {
            Err(KernelError::NotSupported)
        }
    }

    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn stream(&self) -> bool {
        // POSIX: signalfd 不可定位 (lseek → ESPIPE)
        true
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// ============================================================================
// 系统调用
// ============================================================================

/// `signalfd` / `signalfd4` 系统调用 — 创建对象或修改既有实例掩码
///
/// `fd == -1` 创建新 signalfd 并挂 per-process fd 表; `fd >= 0` 经
/// `File::io_control` 修改既有 signalfd 对象的掩码并原样返回该 fd.
///
/// # Errors
///
/// - flags 含未知位 → `EINVAL`
/// - 掩码指针为空或不可读 → `EFAULT`
/// - 无进程上下文 → `EINVAL`; `fd >= 0` 但不是 signalfd → `EBADF`
/// - `OpenFileTable` 或 per-process fd 表耗尽 → `EMFILE`
pub fn signalfd_syscall(fd: i32, mask_ptr: u64, flags: i32) -> Result<usize, Errno> {
    let valid_flags = SFD_CLOEXEC | SFD_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Err(Errno::Einval);
    }
    if mask_ptr == 0 {
        return Err(Errno::Efault);
    }
    // I4: 用户掩码经 privileged 安全代理读取 (校验后再 read_unaligned);
    // 原实现直接裸指针 read, 越界/未对齐指针即 fault.
    let mut mask: u128 = 0;
    if !crate::privileged::syscall::api::read_struct_from_user(mask_ptr, &mut mask) {
        return Err(Errno::Efault);
    }
    // SIMPLIFIED: 掩码内部收窄为 u64 — 进程 pending 位图为 u64 (信号 1-64),
    // u128 掩码的 bit 64-127 永无对应 pending 信号; 影响面 = 注册 >64 号信号
    // 无效 (本就无对应 pending); 何时需扩展: 支持 >64 个信号时恢复 u128 存储.
    let sigmask = crate::functions::intcast::low_u64_from_u128(mask) & !UNMASKABLE_SIGNALS;
    signalfd_apply(fd, sigmask, flags)
}

/// 创建/修改 signalfd 的策略入口 — 掩码已从用户态读出并完成不可屏蔽信号过滤
///
/// 与 [`signalfd_syscall`] 分离: 本函数不触碰用户内存, host 侧可直接驱动
/// 对象创建/改掩码全链 (同构先例: `clock_nanosleep_wait_ns`).
///
/// `flags` 须已由 [`signalfd_syscall`] 校验 (未知位拒绝).
///
/// # Errors
///
/// - 无进程上下文 → `EINVAL`; `fd >= 0` 但不是 signalfd → `EBADF`
/// - `OpenFileTable` 或 per-process fd 表耗尽 → `EMFILE`
pub fn signalfd_apply(fd: i32, sigmask: u64, flags: i32) -> Result<usize, Errno> {
    let current_pid = crate::privileged::proc::process_get_current_pid();
    if current_pid == 0 {
        return Err(Errno::Einval);
    }

    // 修改既有实例: 经 dyn File 的 io_control 回访对象私有掩码
    if fd != -1 {
        let Some(handle_id) =
            crate::functions::fs::vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))
        else {
            return Err(Errno::Ebadf);
        };
        let outcome = OPEN_FILE_TABLE.with_file(handle_id, |of| {
            of.file().io_control(
                SFD_IOC_SETMASK,
                crate::functions::intcast::usize_from_u64(sigmask),
            )
        });
        return match outcome {
            Some(Ok(_)) => Ok(crate::functions::intcast::usize_from_i32(fd)),
            // 非 signalfd 对象 (io_control 未实现该命令) / fd 槽位失效 → EBADF
            Some(Err(KernelError::NotSupported)) | None => Err(Errno::Ebadf),
            Some(Err(e)) => Err(e.as_errno()),
        };
    }

    // 创建: 构造对象 → OpenFileTable → per-process fd 表
    let pwm = crate::privileged::sgeg::pwm_get_current();
    let open_file = OpenFile::from_file(Arc::new(SignalFdFile::new(sigmask, current_pid)), 0, pwm);
    open_file.set_nonblock((flags & SFD_NONBLOCK) != 0);
    let cloexec = (flags & SFD_CLOEXEC) != 0;

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
// 进程信号状态访问 (桥接 proc::signal)
// ============================================================================

/// 获取进程 pending 信号位图
fn process_pending(pid: u32) -> u64 {
    crate::privileged::proc::process_with(pid, crate::privileged::proc::Process::signal_pending_get)
        .unwrap_or(0)
}

/// 清除进程指定信号的 pending 位
fn clear_pending(pid: u32, signo: u32) {
    let bit = 1u64 << (signo - 1);
    let _ = crate::privileged::proc::process_with(pid, |proc| proc.signal_pending_clear(bit));
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
    fn object_mask_and_unmaskable_filter() {
        let file = SignalFdFile::new(0, 0);
        // 置 SIGUSR1 (bit 9) + SIGKILL (bit 8) + SIGSTOP (bit 18), 过滤后仅剩 bit 9
        let raw = (1u64 << 9) | (1u64 << 8) | (1u64 << 18);
        assert_eq!(
            file.io_control(
                SFD_IOC_SETMASK,
                crate::functions::intcast::usize_from_u64(raw)
            ),
            Ok(0)
        );
        let state = file.state.lock();
        assert_eq!(state.sigmask, 1u64 << 9, "SIGKILL/SIGSTOP 必须被过滤");
    }

    #[test]
    fn object_io_control_rejects_unknown_command() {
        let file = SignalFdFile::new(0, 0);
        assert_eq!(
            file.io_control(0x1234_5678, 0),
            Err(KernelError::NotSupported),
            "未知控制命令返回 EOPNOTSUPP (ioctl 回落默认策略)"
        );
    }

    #[test]
    fn object_contract_sentinels() {
        let file = SignalFdFile::new(0, 0);
        assert!(file.stream(), "signalfd 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "signalfd 非 inode 承载");
        assert_eq!(file.file_type(), VfsFileType::File);
        assert_eq!(
            file.write_at(0, &[0u8; SIGNALFD_SIGINFO_SIZE], 0),
            Err(KernelError::InvalidArgument),
            "signalfd 只读, 写入返回 EINVAL"
        );
        assert_eq!(
            file.read_at(0, &mut [0u8; 64], 0),
            Err(KernelError::InvalidArgument),
            "短缓冲读返回 EINVAL"
        );
    }

    #[test]
    fn siginfo_layout_matches_linux() {
        assert_eq!(
            core::mem::size_of::<SignalFdSigInfo>(),
            SIGNALFD_SIGINFO_SIZE,
            "signalfd_siginfo 必须为 128 字节"
        );
        let info = SignalFdSigInfo::with_signo(9);
        let bytes = crate::privileged::mm::pod_as_bytes(&info);
        assert_eq!(&bytes[..4], &9u32.to_ne_bytes(), "ssi_signo 位于偏移 0");
        assert!(bytes[4..].iter().all(|&b| b == 0), "其余字段置零");
    }

    #[test]
    fn syscall_create_modify_close() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("signalfd-t", || {
            // 掩码已从用户态读出后的策略入口 (host 侧不触碰用户内存)
            let fd =
                signalfd_apply(-1, (1u64 << 9) | (1u64 << 8), SFD_CLOEXEC).expect("创建 signalfd");
            assert!(fd >= 3, "signalfd fd 由 per-process 表分配");
            let handle = crate::functions::fs::vfs_get_fd_handle(fd).expect("fd 应有 OpenFile");
            assert_eq!(
                crate::privileged::proc::with_current_fd_table(|t| t.is_cloexec(fd)),
                Some(true),
                "SFD_CLOEXEC 应落 per-process fd 表"
            );

            // 修改掩码: 返回同一 fd
            let again = signalfd_apply(
                crate::functions::intcast::low_i32_from_usize(fd),
                1u64 << 30,
                0,
            )
            .expect("修改 signalfd 掩码");
            assert_eq!(again, fd, "修改路径返回原 fd");

            let dup_fd =
                crate::functions::fs::vfs_dup(crate::functions::intcast::low_u32_from_usize(fd));
            assert!(dup_fd >= 0, "dup 应成功");
            let dup_idx = crate::functions::intcast::usize_from_i32(dup_fd);
            assert_eq!(
                crate::functions::fs::vfs_get_fd_handle(dup_idx),
                Some(handle),
                "dup 共享同一 OpenFile"
            );

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
    fn syscall_rejects_invalid_flags_and_modify_target() {
        // flags 校验先于用户内存读取 ⇒ 非法 flags 不触碰指针
        assert_eq!(
            signalfd_syscall(-1, 1, 0o10),
            Err(Errno::Einval),
            "未知标志"
        );
        assert_eq!(signalfd_syscall(-1, 0, 0), Err(Errno::Efault), "空掩码指针");
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("signalfd-bad", || {
            assert_eq!(
                signalfd_apply(3, 0, 0),
                Err(Errno::Ebadf),
                "fd >= 0 但不是 signalfd → EBADF"
            );
        });
    }
}
