#![deny(unsafe_code)]
//! pidfd 系统调用实现
//!
//! pidfd_open: 为进程打开一个 pidfd (分配真实 fd, 维护 fd→pid 映射)
//! pidfd_send_signal: 通过 pidfd 发送信号 (按映射表反查 pid)
//! pidfd_getfd: 通过 pidfd 获取文件描述符 (需要 OpenFile 系统, 暂存根)
//!
//! ## 设计 (B05-32 P0-10 修复)
//!
//! 旧实现直接把 pid 当作 fd 返回, 导致:
//! - pid=1 的进程 pidfd 恒为 1, 与 stdin 冲突
//! - 同一进程多次 pidfd_open 返回相同 fd (无独立句柄语义)
//! - 任意进程可对任意 pid 伪造"fd"并注入信号
//!
//! 新实现通过 `fd_alloc::alloc_fd(FdSubsystem::PidFd)` 分配独立 fd 编号,
//! 并维护 `fd → pid` 映射表. 映射表容量与 `FdPlan::PID_FD` 容量一致 (16).
//!
//! ## 并发安全
//!
//! 映射表用 `Mutex<[(i32, u32); CAP]>` 保护; 槽位复用 (`slot_id` 单调递增)
//! 防止 ABA 问题.

use crate::privileged::proc::fd_alloc::{FdSubsystem, alloc_fd, free_fd, idx_of};
use crate::privileged::sync::{Mutex, OnceLock};
use crate::privileged::syscall::Errno;

/// pidfd 标志位
const PIDFD_NONBLOCK: u32 = 1;

/// 映射表容量 (与 `FdPlan::PID_FD` 容量一致)
const PIDFD_CAP: usize = 16;

/// `(fd, pid, slot_id)` 映射条目. `slot_id` 单调递增, 用于区分 fd 复用.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PidFdEntry {
    fd: i32,
    pid: u32,
    slot_id: u64,
}

/// 全局 pidfd → pid 映射表 (OnceLock 延迟初始化, 首个 lock 时创建)
static PIDFD_MAP: OnceLock<Mutex<[Option<PidFdEntry>; PIDFD_CAP]>> = OnceLock::new();

/// 单调递增槽位号 (用于 fd 复用后区分新旧映射)
static NEXT_SLOT_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

/// 获取映射表锁 (首次调用时惰性初始化)
///
/// 使用 `OnceLock::set` 而非 `get_or_init` (后者闭包需 unsafe, functions 层禁止).
/// `set` 内部 `call_once` 保证并发安全: 首个线程写入, 其余线程的 `set` 返回
/// `Err(value)` (Mutex 被 drop, 无泄漏). `call_once` 返回后 `get` 必返回 `Some`.
fn pidfd_map() -> &'static Mutex<[Option<PidFdEntry>; PIDFD_CAP]> {
    // set 失败 (Err) 表示已初始化, 退回的 Mutex 被 drop; 此时 get 必 Some.
    if PIDFD_MAP.set(Mutex::new([None; PIDFD_CAP])).is_err() {
        // 并发场景: 另一个线程已初始化, 继续走 get
    }
    PIDFD_MAP
        .get()
        .expect("pidfd map: OnceLock set 后 get 必 Some (call_once 互斥保证)")
}

/// 分配一个 pidfd 映射槽位
///
/// # Errors
///
/// - 进程不存在 → `ESRCH`
/// - 映射表已满 → `EMFILE`
fn alloc_entry(pid: u32) -> Result<i32, Errno> {
    // 先验证进程存在
    if crate::privileged::proc::PROCESS_TABLE.get(pid).is_none() {
        return Err(Errno::ESRCH);
    }

    let fd = alloc_fd(FdSubsystem::PidFd).ok_or(Errno::EMFILE)?;
    let slot_id = NEXT_SLOT_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    let mut map = pidfd_map().lock();
    for entry in map.iter_mut() {
        if entry.is_none() {
            *entry = Some(PidFdEntry { fd, pid, slot_id });
            return Ok(fd);
        }
    }
    // 映射表满 (理论不可达: fd 位图与映射表容量一致, 但防御性处理)
    free_fd(FdSubsystem::PidFd, fd);
    Err(Errno::EMFILE)
}

/// 释放 pidfd 映射 (由 close 路径调用)
pub fn free_entry(fd: i32) -> bool {
    let mut map = pidfd_map().lock();
    for entry in map.iter_mut() {
        if let Some(e) = entry {
            if e.fd == fd {
                *entry = None;
                let _ = free_fd(FdSubsystem::PidFd, fd);
                return true;
            }
        }
    }
    false
}

/// 通过 pidfd 反查目标 pid
fn pid_of_fd(fd: i32) -> Option<u32> {
    let map = pidfd_map().lock();
    for entry in map.iter() {
        if let Some(e) = entry {
            if e.fd == fd {
                return Some(e.pid);
            }
        }
    }
    None
}

/// 检查 fd 是否属于 pidfd 空间
pub fn is_pidfd_fd(fd: i32) -> bool {
    matches!(idx_of(fd), Some((FdSubsystem::PidFd, _)))
}

/// `pidfd_open` — 为进程打开一个 pidfd
///
/// # Errors
///
/// - flags 含非法位 → `EINVAL`
/// - 进程不存在 → `ESRCH`
/// - 映射表已满 → `EMFILE`
pub fn pidfd_open(pid: u32, flags: u32) -> Result<usize, Errno> {
    if flags & !PIDFD_NONBLOCK != 0 {
        return Err(Errno::EINVAL);
    }

    let fd = alloc_entry(pid)?;
    Ok(fd as usize)
}

/// `pidfd_send_signal` — 通过 pidfd 发送信号
///
/// # Errors
///
/// - 信号编号不在 1..=63 范围 → `EINVAL`
/// - pidfd 无效 → `EBADF`
/// - 目标进程不存在 → `ESRCH`
pub fn pidfd_send_signal(pidfd: u32, sig: i32, _siginfo: u64, _flags: u32) -> Result<usize, Errno> {
    if !(1..=63).contains(&sig) {
        return Err(Errno::EINVAL);
    }

    let pid = pid_of_fd(pidfd as i32).ok_or(Errno::EBADF)?;

    // 复用 kill_syscall 的 pid>0 语义 (privileged 内部 4 路径分发)
    let ret = crate::privileged::syscall::api::sys_kill(pid as i32, sig);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(0)
    }
}

/// `pidfd_getfd` — 通过 pidfd 获取文件描述符
///
/// 需要完整的 `OpenFile` 系统支持, 暂存根.
///
/// # Errors
///
/// 该接口尚未实现, 始终返回 `ENOSYS`.
pub fn pidfd_getfd(_pidfd: u32, _targetfd: u32, _flags: u32) -> Result<usize, Errno> {
    // 依赖 Task 4 (OpenFile 系统) 完成后实现 (登记分册 9 B09-10)
    Err(Errno::ENOSYS)
}
