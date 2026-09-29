#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! wait4 / waitid — services 层实现 (从 framework/syscall/wait4.rs 下沉, §6.2)
//!
//! POSIX `pid_t wait4(pid_t pid, int *wstatus, int options, struct rusage *rusage)`
//! 与 `int waitid(idtype_t idtype, id_t id, siginfo_t *infop, int options)`.
//!
//! ## 安全边界
//!
//! - services 层: 0 unsafe; 参数 (pid 范围 / options 标志) 在此校验
//! - 进程表只读/收割经 framework `proc::api` (`process_with` /
//!   `process_remove_and_free` / `scheduler_yield`)
//! - 用户态缓冲写经 framework `syscall::api::write_struct_to_user`
//!   (内部先 `check_user_buf` 校验再写入)
//!
//! ## pid 参数
//!
//! - pid > 0: 等待特定 PID 的子进程
//! - pid == 0: 等待同进程组任意子进程
//! - pid == -1: 等待任意子进程 (POSIX `wait()`)
//! - pid < -1: 等待进程组 |pid| 内的任意子进程

use core::sync::atomic::Ordering;

use crate::framework::proc::ProcessState;
use crate::framework::proc::api;
use crate::framework::syscall::Errno;

// ============================================================================
// wait4 — options 与等待/收割机制
// ============================================================================

/// wait4 options: 非阻塞, 无子进程退出立即返回 0
pub const WNOHANG: i32 = 0x1;
/// wait4 options: 报告已停止的子进程
pub const WUNTRACED: i32 = 0x2;
/// wait4 options: 报告已恢复的子进程
pub const WCONTINUED: i32 = 0x8;

/// 等待收集结果 (wait4 与 waitid 共用)
#[derive(Debug)]
pub struct WaitInfo {
    /// 被收割 (或观察) 的子进程 PID
    pub pid: u32,
    /// 子进程退出码 (原始值)
    pub exit_code: u32,
}

/// 等待结果
#[derive(Debug)]
pub enum WaitOutcome {
    /// 收割成功 (keep_zombie 时不释放子进程 PCB)
    Reaped(WaitInfo),
    /// 有匹配子进程但尚未退出 (仅非阻塞模式返回)
    Running,
    /// 无匹配子进程
    NoChild,
}

/// 统一的子进程等待/收割机制 (wait4 与 waitid 共用)
///
/// - `target_pid > 0`: 等待特定 PID 的子进程
/// - `target_pid == 0`: 等待同进程组任意子进程
/// - `target_pid == -1`: 等待任意子进程
/// - `target_pid < -1`: 等待进程组 |target_pid| 内的任意子进程
///
/// `non_blocking`: true 时子进程未退出立即返回 `Running`.
/// `keep_zombie`: true 时读取退出码后保留子进程 Zombie 状态 (WNOWAIT 语义).
pub fn wait_reap(target_pid: i32, non_blocking: bool, keep_zombie: bool) -> WaitOutcome {
    let current_pid = api::process_get_current_pid();
    if current_pid == 0 {
        return WaitOutcome::NoChild;
    }

    let Some(child_pid) = find_waitable_child(current_pid, target_pid) else {
        return WaitOutcome::NoChild;
    };

    let state = api::process_with(
        child_pid,
        crate::framework::proc::process::Process::get_state,
    )
    .unwrap_or(ProcessState::Terminated);

    if state != ProcessState::Zombie {
        if non_blocking {
            return WaitOutcome::Running;
        }
        // SIMPLIFIED: 阻塞等待用轮询 + scheduler_yield, 无专用等待队列/唤醒；
        // 影响面: 忙轮询带来额外调度开销, 无法精确唤醒; 何时需扩展: 引入进程级
        // wait_queue 后改为事件驱动唤醒.
        loop {
            let state = api::process_with(
                child_pid,
                crate::framework::proc::process::Process::get_state,
            )
            .unwrap_or(ProcessState::Terminated);
            if state == ProcessState::Zombie {
                return reap_zombie(child_pid, keep_zombie);
            }
            crate::framework::proc::scheduler_yield();
        }
    }

    reap_zombie(child_pid, keep_zombie)
}

/// 收割 Zombie 子进程: 读取退出码, 按 `keep_zombie` 决定是否释放 PCB
fn reap_zombie(child_pid: u32, keep_zombie: bool) -> WaitOutcome {
    let exit_code =
        api::process_with(child_pid, |p| p.exit_code.load(Ordering::SeqCst)).unwrap_or(0);
    if !keep_zombie {
        api::process_remove_and_free(child_pid);
    }
    WaitOutcome::Reaped(WaitInfo {
        pid: child_pid,
        exit_code,
    })
}

/// 查找可等待的子进程
///
/// 根据 pid 参数匹配, 返回 PID 或 None.
fn find_waitable_child(parent_pid: u32, target_pid: i32) -> Option<u32> {
    let children = api::process_with(parent_pid, |p| p.children.lock().clone()).unwrap_or_default();

    for &child in &children {
        let child_pid = child.0;
        let state = api::process_with(
            child_pid,
            crate::framework::proc::process::Process::get_state,
        )
        .unwrap_or(ProcessState::Terminated);

        // 只匹配未结束的子进程 (或 Zombie 用于收割)
        if state == ProcessState::Terminated {
            continue;
        }

        // pid 匹配规则
        if target_pid == -1 {
            return Some(child_pid);
        } else if target_pid > 0 {
            if child_pid == target_pid as u32 {
                return Some(child_pid);
            }
        } else if target_pid == 0 {
            // 同进程组 (SIMPLIFIED: 简化为总是匹配; 影响面: 不同进程组子进程也会被
            // 纳入; 何时需扩展: 引入 pgid 相等判定)
            return Some(child_pid);
        } else {
            // target_pid < -1: 进程组 ID = |target_pid|
            let want_pgid = target_pid.unsigned_abs();
            let pgid = api::process_with(child_pid, |p| p.pgid.load(Ordering::SeqCst)).unwrap_or(0);
            if pgid == want_pgid {
                return Some(child_pid);
            }
        }
    }
    None
}

/// wait4 系统调用实现
///
/// 验证: pid 范围合法, options 仅含合法标志
///
/// # Errors
///
/// - `pid` 超出合法范围或 `options` 含非法标志 → `EINVAL`
/// - `wstatus` 指向非法用户空间或写回失败 → `EFAULT`
/// - 无子进程且非 WNOHANG → `ECHILD`
pub fn wait4_syscall(pid: i32, wstatus_ptr: u64, options: i32) -> Result<usize, Errno> {
    // pid 范围: -PID_MAX_LIMIT .. PID_MAX_LIMIT (简化: -32768..=32767)
    const PID_MAX: i32 = 0x7FFF;
    const PID_MIN: i32 = -0x8000;
    if !(PID_MIN..=PID_MAX).contains(&pid) {
        return Err(Errno::EINVAL);
    }

    let valid_opts = WNOHANG | WUNTRACED | WCONTINUED;
    if options & !valid_opts != 0 {
        return Err(Errno::EINVAL);
    }

    let current_pid = api::process_get_current_pid();
    if current_pid == 0 {
        return Err(Errno::ECHILD);
    }
    // wstatus 指针如果为 0, 允许 (调用方不需要状态)
    if wstatus_ptr != 0 && !crate::framework::syscall::api::validate_user_ptr(wstatus_ptr) {
        return Err(Errno::EFAULT);
    }

    let non_blocking = options & WNOHANG != 0;

    match wait_reap(pid, non_blocking, false) {
        WaitOutcome::Reaped(info) => {
            if wstatus_ptr != 0 {
                // 写 wstatus (WIFEXITED | exit_code << 8)
                let status: i32 = (info.exit_code as i32) << 8;
                if !crate::framework::syscall::api::write_struct_to_user(wstatus_ptr, &status) {
                    return Err(Errno::EFAULT);
                }
            }
            Ok(info.pid as usize)
        }
        // WNOHANG: 子进程尚未退出
        WaitOutcome::Running => Ok(0),
        WaitOutcome::NoChild => {
            if non_blocking {
                Ok(0) // WNOHANG: 无可等待子进程
            } else {
                Err(Errno::ECHILD)
            }
        }
    }
}

// ============================================================================
// waitid
// ============================================================================

/// `idtype_t`: 等待全部子进程
const P_ALL: i32 = 0;
/// `idtype_t`: 等待特定 PID
const P_PID: i32 = 1;
/// `idtype_t`: 等待特定进程组
const P_PGID: i32 = 2;

/// waitid options: 报告已退出子进程
const WEXITED: i32 = 0x0000_0004;
/// waitid options: 报告已停止子进程 (与 wait4 `WUNTRACED` 同位)
const WSTOPPED: i32 = 0x0000_0002;
/// waitid options: 观察但不收割 (子进程保持 Zombie)
const WNOWAIT: i32 = 0x0100_0000;

/// `si_code`: 正常退出
const CLD_EXITED: i32 = 1;
/// SIGCHLD 信号编号 (x86_64)
const SIGCHLD: i32 = 17;

/// `struct siginfo_t` 的 SIGCHLD 子布局 (Linux x86_64 用户 ABI)
///
/// 写回字段: `si_signo/si_errno/si_code/si_pid/si_uid/si_status/si_utime/si_stime`.
#[repr(C)]
#[derive(Copy, Clone)]
struct SiginfoChld {
    /// 信号编号 (SIGCHLD)
    si_signo: i32,
    /// errno (恒 0)
    si_errno: i32,
    /// 来源码 (CLD_EXITED)
    si_code: i32,
    /// 对齐填充
    __pad0: i32,
    /// 子进程 PID
    si_pid: i32,
    /// 子进程 UID (无 uid 模型, 恒 0)
    si_uid: u32,
    /// 退出码
    si_status: i32,
    /// 用户态耗时 (未跟踪, 恒 0)
    si_utime: i32,
    /// 内核态耗时 (未跟踪, 恒 0)
    si_stime: i32,
    /// 剩余填充 (保持 128 字节 ABI 尺寸)
    __pad: [u8; 96],
}

/// `waitid(idtype, id, infop, options)` 策略 — 等待子进程状态变化
///
/// SIMPLIFIED: 1) `si_code` 恒为 CLD_EXITED — 信号投递路径未记录致死信号
/// (termsig), 无法区分 CLD_KILLED/CLD_DUMPED; 影响面: 被信号终止的子进程
/// siginfo 归类为正常退出; 何时需扩展: 信号投递记录 termsig 至 Process 后映射.
/// 2) `si_uid` 恒 0 — 无 uid 模型 (Credo PWM). 3) 拒绝 WSTOPPED/WCONTINUED
/// (EINVAL) — 无进程 stop/continue 状态跟踪, 作业控制等待不可用.
///
/// # Errors
///
/// - `options` 含非法位, 或未含 `WEXITED/WSTOPPED/WCONTINUED` 之一 → `EINVAL`
/// - `options` 含 `WSTOPPED/WCONTINUED` (无状态跟踪) → `EINVAL`
/// - `idtype` 非法, 或 `P_PID/P_PGID` 下 `id == 0` 或超出 PID 空间 → `EINVAL`
/// - 无匹配子进程 → `ECHILD`
/// - siginfo 写回失败 → `EFAULT`
pub fn waitid_syscall(idtype: i32, id: u64, infop: u64, options: i32) -> Result<usize, Errno> {
    // PID 空间上界 (与 wait4 校验一致)
    const PID_MAX: u64 = 0x7FFF;
    const VALID_OPTS: i32 = WEXITED | WSTOPPED | WCONTINUED | WNOHANG | WNOWAIT;
    if options & !VALID_OPTS != 0 {
        return Err(Errno::EINVAL);
    }
    // 必须指定等待类别之一 (Linux 语义)
    if options & (WEXITED | WSTOPPED | WCONTINUED) == 0 {
        return Err(Errno::EINVAL);
    }
    // 无 stopped/continued 状态跟踪 (见函数级 SIMPLIFIED 注释)
    if options & (WSTOPPED | WCONTINUED) != 0 {
        return Err(Errno::EINVAL);
    }

    let target: i32 = match idtype {
        P_ALL => -1,
        P_PID => {
            if id == 0 || id > PID_MAX {
                return Err(Errno::EINVAL);
            }
            id as i32
        }
        P_PGID => {
            if id == 0 || id > PID_MAX {
                return Err(Errno::EINVAL);
            }
            // wait_reap 进程组语义: target < -1 → 匹配 pgid == |target|
            -(id as i32)
        }
        _ => return Err(Errno::EINVAL),
    };

    let non_blocking = options & WNOHANG != 0;
    let keep_zombie = options & WNOWAIT != 0;

    match wait_reap(target, non_blocking, keep_zombie) {
        WaitOutcome::Reaped(info) => {
            if infop != 0 {
                let si = SiginfoChld {
                    si_signo: SIGCHLD,
                    si_errno: 0,
                    si_code: CLD_EXITED,
                    __pad0: 0,
                    si_pid: info.pid as i32,
                    si_uid: 0,
                    si_status: info.exit_code as i32,
                    si_utime: 0,
                    si_stime: 0,
                    __pad: [0; 96],
                };
                if !crate::framework::syscall::api::write_struct_to_user(infop, &si) {
                    return Err(Errno::EFAULT);
                }
            }
            Ok(0)
        }
        // WNOHANG: 子进程尚未退出
        WaitOutcome::Running => Ok(0),
        WaitOutcome::NoChild => Err(Errno::ECHILD),
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait4_option_bits_match_linux() {
        // Linux x86_64: WNOHANG=1, WUNTRACED=2, WCONTINUED=8
        assert_eq!(WNOHANG | WUNTRACED | WCONTINUED, 0xB);
    }

    #[test]
    fn waitid_option_bits_match_linux() {
        // Linux: WEXITED=4, WSTOPPED=2, WCONTINUED=8, WNOHANG=1, WNOWAIT=0x0100_0000
        assert_eq!(WEXITED, 4);
        assert_eq!(WSTOPPED, 2);
        assert_eq!(WNOHANG, 1);
        assert_eq!(WNOWAIT, 0x0100_0000);
    }

    #[test]
    fn wait4_rejects_invalid_pid_and_options() {
        // pid 超出 [-32768, 32767] → EINVAL
        assert!(matches!(wait4_syscall(0x8000, 0, 0), Err(Errno::EINVAL)));
        assert!(matches!(wait4_syscall(-0x8001, 0, 0), Err(Errno::EINVAL)));
        // options 含非法位 → EINVAL
        assert!(matches!(wait4_syscall(-1, 0, 0x10), Err(Errno::EINVAL)));
    }

    #[test]
    fn waitid_rejects_invalid_options_and_idtype() {
        // 未指定等待类别 → EINVAL
        assert!(matches!(
            waitid_syscall(P_ALL, 0, 0, WNOHANG),
            Err(Errno::EINVAL)
        ));
        // 含 WSTOPPED (无状态跟踪) → EINVAL
        assert!(matches!(
            waitid_syscall(P_ALL, 0, 0, WSTOPPED),
            Err(Errno::EINVAL)
        ));
        // idtype 非法 → EINVAL
        assert!(matches!(
            waitid_syscall(99, 0, 0, WEXITED),
            Err(Errno::EINVAL)
        ));
        // P_PID 下 id == 0 → EINVAL
        assert!(matches!(
            waitid_syscall(P_PID, 0, 0, WEXITED),
            Err(Errno::EINVAL)
        ));
    }
}
