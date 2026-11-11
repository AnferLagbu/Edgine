#![deny(unsafe_code)]
#![deny(missing_docs)]
//! timerfd — 定时器 fd 对象的 functions 层完整实现
//!
//! ## fd 对象模型 (阶段 3 第 6 个迁移子系统)
//!
//! timerfd 不再是"全局 fd 编号区间 [1160, 1176)"的位图模型, 而是
//! [`TimerFdFile`] 对象挂在全局 `OpenFileTable`, 用户 fd 由 per-process
//! `FdTable` 分配 (与 VFS / socket / UDS / inotify fd 同源). 对象只持有
//! privileged 槽位表 (`privileged::syscall::timerfd::TFD_TABLE`) 的**槽位
//! 下标**; 槽位生灭由对象 `release()` 驱动: 最后一个 fd 引用关闭 →
//! `release()` → 槽位复位. 由此与其它 fd 对象同构地获得:
//!
//! - **close / dup / fork**: 引用计数语义与 VFS fd 一致;
//! - **CLOEXEC**: fd 级标志落 per-process fd 表;
//! - **epoll / poll**: 对象实现 [`File::poll`] (到期计数非空 → `IN`), 经通用
//!   对象分发求值;
//! - **阻塞状态**: 由 `OpenFile::nonblock` 单点持有.
//!
//! 对象**不实现** `Inode` (`as_inode() == None`), 不参与 pcache / inotify
//! 命名空间, 也不可作 mmap 后端.
//!
//! ## 槽位归属
//!
//! `HrTimer` 与到期回调留在 privileged 静态槽位表 (见该模块文档的 UAF
//! 论证): `hrtimer_cancel` 异步生效, `HrTimer` 不能随堆上对象释放. 本模块
//! 只持槽位下标, 经 safe 槽位 API (分配 / 设时 / 取到期 / 轮询 / 释放)
//! 驱动机制面.
//!
//! ## 语义 (Linux timerfd)
//!
//! - create: 校验 `clockid` 与 flags, 分配槽位并挂表
//! - settime: `it_value` 全零 = disarm; `TFD_TIMER_ABSTIME` 按绝对时刻解释;
//!   周期定时器由回调按上次到期时刻推进
//! - gettime: 返回 `(interval, remaining)`
//! - read: 返回到期次数 (u64) 并清零; 无到期事件 → `EAGAIN`
//! - poll: 到期计数非空 → `IN`

use alloc::sync::Arc;

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::api::{read_struct_from_user, write_struct_to_user};
use crate::privileged::syscall::{
    TakeExpiry, alloc_slot, free_slot, get_time, poll_ready, set_time, set_wake_fd, take_expiry,
};

// ============================================================================
// 常量
// ============================================================================

/// `TFD_CLOEXEC` (`O_CLOEXEC`)
pub const TFD_CLOEXEC: i32 = 0o200_0000;
/// `TFD_NONBLOCK` (`O_NONBLOCK`)
pub const TFD_NONBLOCK: i32 = 0o4000;
/// `TFD_TIMER_ABSTIME` (`it_value` 按绝对时刻解释)
pub const TFD_TIMER_ABSTIME: i32 = 1;

/// `CLOCK_REALTIME`
pub const CLOCK_REALTIME: i32 = 0;
/// `CLOCK_MONOTONIC`
pub const CLOCK_MONOTONIC: i32 = 1;

/// read 返回的到期计数字节数 (`uint64_t`)
pub const TFD_EXPIRY_SIZE: usize = 8;

/// 每秒纳秒数
const NSEC_PER_SEC: i64 = 1_000_000_000;

// ============================================================================
// itimerspec (与 Linux ABI 兼容)
// ============================================================================

/// `itimerspec` — 定时器设置 / 读数结构
///
/// 字段名对应 C `itimerspec` 的 `it_interval` / `it_value` 两个 `timespec`
/// 的 `tv_sec` / `tv_nsec` (按序仅去前缀, 顺序与类型不变).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Itimerspec {
    /// 间隔时间秒部分
    pub interval_sec: i64,
    /// 间隔时间纳秒部分
    pub interval_nsec: i64,
    /// 首次到期时间秒部分
    pub value_sec: i64,
    /// 首次到期时间纳秒部分
    pub value_nsec: i64,
}

impl Itimerspec {
    /// 构造全零的 `itimerspec` (无间隔、无到期时间, 即 disarm).
    #[must_use]
    pub const fn zeroed() -> Self {
        Self {
            interval_sec: 0,
            interval_nsec: 0,
            value_sec: 0,
            value_nsec: 0,
        }
    }

    /// 由纳秒读数构造 `(interval, remaining)` 形态的 `itimerspec`.
    fn from_ns(interval_ns: u64, remaining_ns: u64) -> Self {
        let per_sec = NSEC_PER_SEC.cast_unsigned();
        Self {
            interval_sec: (interval_ns / per_sec).cast_signed(),
            interval_nsec: (interval_ns % per_sec).cast_signed(),
            value_sec: (remaining_ns / per_sec).cast_signed(),
            value_nsec: (remaining_ns % per_sec).cast_signed(),
        }
    }
}

/// 校验并折算 `(sec, nsec)` 为纳秒.
///
/// Linux 语义: 负值或 `nsec >= 1e9` → `EINVAL`.
fn spec_ns(sec: i64, nsec: i64) -> Result<u64, Errno> {
    if sec < 0 || !(0..NSEC_PER_SEC).contains(&nsec) {
        return Err(Errno::Einval);
    }
    Ok(sec
        .cast_unsigned()
        .saturating_mul(NSEC_PER_SEC.cast_unsigned())
        .saturating_add(nsec.cast_unsigned()))
}

// ============================================================================
// 对象结构
// ============================================================================

/// timerfd fd 对象 — 仅持 privileged 槽位表下标
pub struct TimerFdFile {
    /// `TFD_TABLE` 槽位下标
    idx: usize,
}

impl TimerFdFile {
    /// 以槽位下标构造对象 (槽位所有权转移给本对象)
    #[must_use]
    pub fn new(idx: usize) -> Self {
        Self { idx }
    }

    /// 槽位下标 — 系统调用层委托槽位操作时使用
    #[must_use]
    pub fn idx(&self) -> usize {
        self.idx
    }

    /// 归还槽位 (最后一个 fd 引用关闭时调用)
    fn free_slot(idx: usize) {
        free_slot(idx);
    }

    /// 对象 → `OpenFileTable` → per-process fd 表; 任一步失败归还槽位
    fn attach(idx: usize, nonblock: bool, cloexec: bool) -> Result<usize, Errno> {
        let pwm = crate::privileged::sgeg::pwm_get_current();
        let open_file = OpenFile::from_file(Arc::new(Self::new(idx)), 0, pwm);
        open_file.set_nonblock(nonblock);
        let Some(handle_id) = OPEN_FILE_TABLE.alloc(open_file) else {
            // 对象未挂表 → release() 不会被调用, 槽位须在此显式归还
            Self::free_slot(idx);
            return Err(Errno::Emfile);
        };
        let Some(fd_idx) =
            crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, cloexec))
                .flatten()
        else {
            // 挂表成功但 fd 表耗尽: close 触发最后一个引用的 release() → 归还槽位
            OPEN_FILE_TABLE.close(handle_id);
            return Err(Errno::Emfile);
        };
        // SIMPLIFIED: epoll 唤醒注册表仍以数值 fd 为键 (跨子系统统一改造归
        // epoll 子系统); 影响面 = 仅登记创建时的首个 fd 号, dup 别名的就绪
        // 唤醒需按该 fd 注册兴趣 (与 socket/UDS/inotify 迁移同口径); 何时需
        // 扩展: 唤醒注册表改按 OpenFile handle 索引时.
        let wake_fd = crate::functions::intcast::low_i32_from_usize(fd_idx);
        set_wake_fd(idx, wake_fd);
        Ok(fd_idx)
    }
}

impl File for TimerFdFile {
    // SIMPLIFIED: 阻塞读未实装 — 无到期事件时恒返回 EAGAIN (与原 privileged
    // 实现一致, 无等待队列); 影响面 = 未置 TFD_NONBLOCK 的读也不睡眠;
    // 何时需扩展: timerfd 等待队列接入 epoll 唤醒路径时.
    fn read_at(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        // Linux: 缓冲区小于 sizeof(uint64_t) → EINVAL
        if buf.len() < TFD_EXPIRY_SIZE {
            return Err(KernelError::InvalidArgument);
        }
        match take_expiry(self.idx) {
            TakeExpiry::Count(count) => {
                buf[..TFD_EXPIRY_SIZE].copy_from_slice(&count.to_ne_bytes());
                Ok(TFD_EXPIRY_SIZE)
            }
            TakeExpiry::Empty => Err(KernelError::WouldBlock),
            TakeExpiry::Invalid => Err(KernelError::BadFd),
        }
    }

    fn write_at(&self, _offset: u64, _buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        // timerfd 只读 (Linux: 向其写入返回 EINVAL)
        Err(KernelError::InvalidArgument)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        if matches!(poll_ready(self.idx), Some(true)) {
            events & PollEvents::IN
        } else {
            PollEvents::empty()
        }
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn release(&self) {
        Self::free_slot(self.idx);
    }

    // SIMPLIFIED: `VfsFileType` 无独立的 timerfd 变体 — 以 File 呈现 (与
    // eventfd / signalfd / inotify / socket 同构); 影响面 = fstat 的 st_mode
    // 报普通文件; 何时需扩展: VfsFileType 增加匿名 inode 类变体并贯通 stat
    // 编码时.
    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn stream(&self) -> bool {
        // POSIX: timerfd 不可定位 (lseek → ESPIPE)
        true
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// 由用户 fd 解析 timerfd 槽位下标, 并在解析期间钉住 `OpenFile`.
///
/// 返回的 `handle_id` 供调用方用毕后 `dec_ref`; 解析与 `inc_ref` 在同一
/// `OpenFileTable` 临界区内完成, 避免 close 竞争导致的槽位 ABA.
fn resolve_timerfd_fd(fd: i64) -> Option<(u32, usize)> {
    let fd_idx = usize::try_from(fd).ok()?;
    let handle_id = crate::functions::fs::vfs_get_fd_handle(fd_idx)?;
    let idx = OPEN_FILE_TABLE
        .with_file(handle_id, |of| {
            let idx = of.file().as_any().downcast_ref::<TimerFdFile>()?.idx();
            of.inc_ref();
            Some(idx)
        })
        .flatten()?;
    Some((handle_id, idx))
}

// ============================================================================
// 系统调用
// ============================================================================

/// `timerfd_create` — 创建 timerfd 对象
///
/// # Errors
///
/// - `clockid` 非 `CLOCK_MONOTONIC` / `CLOCK_REALTIME` → `EINVAL`
/// - flags 含未知位 → `EINVAL`
/// - 槽位表 / `OpenFileTable` / per-process fd 表耗尽 → `EMFILE`
pub fn timerfd_create_syscall(clockid: i32, flags: i32) -> Result<usize, Errno> {
    if clockid != CLOCK_MONOTONIC && clockid != CLOCK_REALTIME {
        return Err(Errno::Einval);
    }
    if flags & !(TFD_CLOEXEC | TFD_NONBLOCK) != 0 {
        return Err(Errno::Einval);
    }
    // SIMPLIFIED: clockid 仅校验不参与计时基 — hrtimer 只有单调时基, 原实现
    // 亦只保存未使用; 影响面 = CLOCK_REALTIME 定时器按单调时基计时; 何时需
    // 扩展: hrtimer 支持实时时钟基时.
    let Some(idx) = alloc_slot() else {
        return Err(Errno::Emfile);
    };
    TimerFdFile::attach(idx, flags & TFD_NONBLOCK != 0, flags & TFD_CLOEXEC != 0)
}

/// `timerfd_settime` 的策略入口 — `itimerspec` 已从用户态读出
///
/// 与 [`timerfd_settime_syscall`] 分离: 本函数不触碰用户内存, host 侧可直接
/// 驱动设时全链. 返回设置前的 `(interval, remaining)`.
///
/// # Errors
///
/// - `fd` 不是 timerfd 对象 → `EBADF`
/// - flags 含 `TFD_TIMER_ABSTIME` 之外的位 → `EINVAL`
/// - `sec` 为负 / `nsec` 为负或 ≥ 1e9 → `EINVAL`
pub fn timerfd_settime_apply(fd: i64, flags: i32, spec: &Itimerspec) -> Result<Itimerspec, Errno> {
    let Some((handle_id, idx)) = resolve_timerfd_fd(fd) else {
        return Err(Errno::Ebadf);
    };
    let result = settime_on_slot(idx, flags, spec);
    OPEN_FILE_TABLE.dec_ref(handle_id);
    result
}

/// 在已解析的槽位上下发设时请求
fn settime_on_slot(idx: usize, flags: i32, spec: &Itimerspec) -> Result<Itimerspec, Errno> {
    if flags & !TFD_TIMER_ABSTIME != 0 {
        return Err(Errno::Einval);
    }
    let value_ns = spec_ns(spec.value_sec, spec.value_nsec)?;
    let interval_ns = spec_ns(spec.interval_sec, spec.interval_nsec)?;
    let Some((old_interval_ns, old_remaining_ns)) =
        set_time(idx, value_ns, interval_ns, flags & TFD_TIMER_ABSTIME != 0)
    else {
        return Err(Errno::Ebadf);
    };
    Ok(Itimerspec::from_ns(old_interval_ns, old_remaining_ns))
}

/// `timerfd_settime` 系统调用
///
/// # Errors
///
/// - `new_value_ptr` 为空或不可读 → `EFAULT`
/// - 其余错误同 [`timerfd_settime_apply`]; `old_value_ptr` 不可写 → `EFAULT`
pub fn timerfd_settime_syscall(
    fd: i64,
    flags: i32,
    new_value_ptr: u64,
    old_value_ptr: u64,
) -> Result<usize, Errno> {
    // Linux 顺序: 先读入参结构 (EFAULT), 再解析 fd (EBADF)
    let mut new_value = Itimerspec::zeroed();
    if !read_struct_from_user(new_value_ptr, &mut new_value) {
        return Err(Errno::Efault);
    }
    let old = timerfd_settime_apply(fd, flags, &new_value)?;
    // 旧值在槽位表锁外拷回用户空间
    if old_value_ptr != 0 && !write_struct_to_user(old_value_ptr, &old) {
        return Err(Errno::Efault);
    }
    Ok(0)
}

/// `timerfd_gettime` 的策略入口 — 返回当前 `(interval, remaining)`
///
/// # Errors
///
/// - `fd` 不是 timerfd 对象 → `EBADF`
pub fn timerfd_gettime_apply(fd: i64) -> Result<Itimerspec, Errno> {
    let Some((handle_id, idx)) = resolve_timerfd_fd(fd) else {
        return Err(Errno::Ebadf);
    };
    let time = get_time(idx);
    OPEN_FILE_TABLE.dec_ref(handle_id);
    let Some((interval_ns, remaining_ns)) = time else {
        return Err(Errno::Ebadf);
    };
    Ok(Itimerspec::from_ns(interval_ns, remaining_ns))
}

/// `timerfd_gettime` 系统调用
///
/// # Errors
///
/// - `fd` 不是 timerfd 对象 → `EBADF`
/// - `curr_value_ptr` 为空或不可写 → `EFAULT`
pub fn timerfd_gettime_syscall(fd: i64, curr_value_ptr: u64) -> Result<usize, Errno> {
    let value = timerfd_gettime_apply(fd)?;
    if curr_value_ptr == 0 || !write_struct_to_user(curr_value_ptr, &value) {
        return Err(Errno::Efault);
    }
    Ok(0)
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

    /// 关闭 fd (u32 形态)
    fn close_fd(fd: usize) {
        assert_eq!(
            crate::functions::fs::api::vfs_close_safe(
                crate::functions::intcast::low_u32_from_usize(fd)
            ),
            0,
            "close 应成功"
        );
    }

    #[test]
    fn object_contract_sentinels() {
        let file = TimerFdFile::new(0);
        assert!(file.stream(), "timerfd 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "timerfd 非 inode 承载");
        assert_eq!(file.file_type(), VfsFileType::File);
        assert_eq!(
            file.write_at(0, &[0u8; TFD_EXPIRY_SIZE], 0),
            Err(KernelError::InvalidArgument),
            "timerfd 只读, 写入返回 EINVAL"
        );
        assert_eq!(
            file.read_at(0, &mut [0u8; TFD_EXPIRY_SIZE - 1], 0),
            Err(KernelError::InvalidArgument),
            "短缓冲读返回 EINVAL"
        );
        assert_eq!(
            file.io_control(0, 0),
            Err(KernelError::NotSupported),
            "timerfd 不处理 ioctl 命令"
        );
    }

    #[test]
    fn syscall_create_flags_and_fd_origin() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("timerfd-create", || {
            let fd = timerfd_create_syscall(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK)
                .expect("创建 timerfd");
            assert!(fd >= 3, "timerfd fd 由 per-process fd 表分配");
            let handle = crate::functions::fs::vfs_get_fd_handle(fd).expect("fd 应有 OpenFile");
            assert_eq!(
                crate::privileged::proc::with_current_fd_table(|t| t.is_cloexec(fd)),
                Some(true),
                "TFD_CLOEXEC 应落 per-process fd 表"
            );
            assert!(
                OPEN_FILE_TABLE
                    .with_file(handle, |of| of.is_nonblock())
                    .unwrap_or(false),
                "TFD_NONBLOCK 应落 OpenFile"
            );
            assert!(
                OPEN_FILE_TABLE
                    .with_file(handle, |of| of
                        .file()
                        .as_any()
                        .downcast_ref::<TimerFdFile>()
                        .is_some())
                    .unwrap_or(false),
                "fd 应指向 TimerFdFile 对象"
            );
            close_fd(fd);
        });
    }

    #[test]
    fn syscall_settime_gettime_disarm() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("timerfd-time", || {
            let fd = timerfd_create_syscall(CLOCK_MONOTONIC, 0).expect("创建 timerfd");
            let fd_i64 = crate::functions::intcast::i64_from_usize(fd);

            // 未启动: 读数全零, 读事件 EAGAIN
            let idle = timerfd_gettime_apply(fd_i64).expect("gettime");
            assert_eq!((idle.interval_sec, idle.interval_nsec), (0, 0));
            assert_eq!((idle.value_sec, idle.value_nsec), (0, 0));
            let mut buf = [0u8; TFD_EXPIRY_SIZE];
            assert_eq!(
                OPEN_FILE_TABLE
                    .with_file(
                        crate::functions::fs::vfs_get_fd_handle(fd).expect("handle"),
                        |of| { of.file().read_at(0, &mut buf, 0) }
                    )
                    .expect("read"),
                Err(KernelError::WouldBlock),
                "无到期事件读返回 EAGAIN"
            );

            // 设置 10ms 后首次到期、5ms 周期
            let spec = Itimerspec {
                interval_sec: 0,
                interval_nsec: 5_000_000,
                value_sec: 0,
                value_nsec: 10_000_000,
            };
            let old = timerfd_settime_apply(fd_i64, 0, &spec).expect("settime");
            assert_eq!(
                (
                    old.interval_sec,
                    old.interval_nsec,
                    old.value_sec,
                    old.value_nsec
                ),
                (0, 0, 0, 0),
                "首次设置旧值全零"
            );

            let armed = timerfd_gettime_apply(fd_i64).expect("gettime armed");
            assert_eq!(armed.interval_nsec, 5_000_000, "周期值应可读回");
            // host 测试无时基 (hrtimer 未 init, start 为空操作, expiry 保持初值):
            // 剩余时间只校验上界, 到期推进由 kernel_test / QEMU 腿覆盖.
            assert!(
                armed.value_sec == 0 && armed.value_nsec <= 10_000_000,
                "剩余时间应在 [0, 10ms] 内"
            );

            // disarm: it_value 全零, 返回旧 (interval, remaining)
            let disarm = Itimerspec::zeroed();
            let old2 = timerfd_settime_apply(fd_i64, 0, &disarm).expect("disarm");
            assert_eq!(old2.interval_nsec, 5_000_000, "disarm 应返回旧周期");
            assert!(
                old2.value_sec == 0 && old2.value_nsec <= 10_000_000,
                "disarm 应返回旧剩余时间"
            );
            let stopped = timerfd_gettime_apply(fd_i64).expect("gettime disarmed");
            assert_eq!(
                (
                    stopped.interval_sec,
                    stopped.interval_nsec,
                    stopped.value_sec,
                    stopped.value_nsec
                ),
                (0, 0, 0, 0),
                "disarm 后读数全零"
            );

            close_fd(fd);
        });
    }

    #[test]
    fn syscall_slot_exhaustion_and_reclaim() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("timerfd-slots", || {
            let mut fds = [0usize; crate::privileged::syscall::TFD_MAX_SLOTS];
            for fd in &mut fds {
                *fd = timerfd_create_syscall(CLOCK_MONOTONIC, 0).expect("创建 timerfd");
            }
            assert_eq!(
                timerfd_create_syscall(CLOCK_MONOTONIC, 0),
                Err(Errno::Emfile),
                "槽位耗尽返回 EMFILE"
            );
            close_fd(fds[0]);
            let again = timerfd_create_syscall(CLOCK_MONOTONIC, 0).expect("槽位释放后可重建");
            for &fd in &fds[1..] {
                close_fd(fd);
            }
            close_fd(again);
        });
    }

    #[test]
    fn syscall_validation_errors() {
        assert_eq!(
            timerfd_create_syscall(99, 0),
            Err(Errno::Einval),
            "非法 clockid"
        );
        assert_eq!(
            timerfd_create_syscall(CLOCK_MONOTONIC, 0x10),
            Err(Errno::Einval),
            "未知 flags"
        );
        assert_eq!(
            timerfd_settime_syscall(3, 0, 0, 0),
            Err(Errno::Efault),
            "空新值指针"
        );

        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("timerfd-bad", || {
            // 非 timerfd fd / 未分配 fd → EBADF
            assert_eq!(
                timerfd_settime_apply(3, 0, &Itimerspec::zeroed()),
                Err(Errno::Ebadf),
                "非 timerfd fd → EBADF"
            );
            assert_eq!(
                timerfd_gettime_apply(3),
                Err(Errno::Ebadf),
                "非 timerfd fd → EBADF"
            );

            let fd = timerfd_create_syscall(CLOCK_MONOTONIC, 0).expect("创建 timerfd");
            let fd_i64 = crate::functions::intcast::i64_from_usize(fd);

            assert_eq!(
                timerfd_settime_apply(fd_i64, 0x10, &Itimerspec::zeroed()),
                Err(Errno::Einval),
                "未知 settime flags"
            );
            let negative = Itimerspec {
                interval_sec: 0,
                interval_nsec: 0,
                value_sec: -1,
                value_nsec: 0,
            };
            assert_eq!(
                timerfd_settime_apply(fd_i64, 0, &negative),
                Err(Errno::Einval),
                "负秒值拒绝"
            );
            let overflow_nsec = Itimerspec {
                interval_sec: 0,
                interval_nsec: 0,
                value_sec: 0,
                value_nsec: NSEC_PER_SEC,
            };
            assert_eq!(
                timerfd_settime_apply(fd_i64, 0, &overflow_nsec),
                Err(Errno::Einval),
                "纳秒 >= 1e9 拒绝"
            );

            close_fd(fd);
        });
    }

    #[test]
    fn syscall_dup_shares_slot_and_close_frees() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("timerfd-dup", || {
            let fd = timerfd_create_syscall(CLOCK_MONOTONIC, 0).expect("创建 timerfd");
            let fd_i64 = crate::functions::intcast::i64_from_usize(fd);
            let handle = crate::functions::fs::vfs_get_fd_handle(fd).expect("fd 应有 OpenFile");

            let dup =
                crate::functions::fs::vfs_dup(crate::functions::intcast::low_u32_from_usize(fd));
            assert!(dup >= 0, "dup 应成功");
            let dup_idx = crate::functions::intcast::usize_from_i32(dup);
            assert_eq!(
                crate::functions::fs::vfs_get_fd_handle(dup_idx),
                Some(handle),
                "dup 共享同一 OpenFile (同一槽位)"
            );

            // 经 dup 设时, 经原 fd 读周期 (共享同一槽位).
            // host 测试无时基 (hrtimer 未 init), 剩余时间恒 0, 故以周期读回作共享判据.
            let spec = Itimerspec {
                interval_sec: 0,
                interval_nsec: 5_000_000,
                value_sec: 1,
                value_nsec: 0,
            };
            timerfd_settime_apply(crate::functions::intcast::i64_from_usize(dup_idx), 0, &spec)
                .expect("经 dup 设时");
            let armed = timerfd_gettime_apply(fd_i64).expect("经原 fd 读数");
            assert_eq!(armed.interval_nsec, 5_000_000, "同一槽位的周期应共享");

            // 关闭原 fd 后槽位仍由 dup 持有
            close_fd(fd);
            assert_eq!(
                timerfd_gettime_apply(crate::functions::intcast::i64_from_usize(dup_idx))
                    .expect("dup 仍有效")
                    .interval_nsec,
                5_000_000
            );

            // 最后一个引用关闭 → 槽位归还, fd 失效
            close_fd(dup_idx);
            assert_eq!(
                timerfd_gettime_apply(crate::functions::intcast::i64_from_usize(dup_idx)),
                Err(Errno::Ebadf),
                "close 后 fd 失效"
            );
            let reclaimed = timerfd_create_syscall(CLOCK_MONOTONIC, 0).expect("槽位已归还");
            close_fd(reclaimed);
        });
    }
}
