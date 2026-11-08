#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! wait4 / waitid — functions 层实现 (从 privileged/syscall/wait4.rs 下沉, §6.2)
//!
//! POSIX `pid_t wait4(pid_t pid, int *wstatus, int options, struct rusage *rusage)`
//! 与 `int waitid(idtype_t idtype, id_t id, siginfo_t *infop, int options)`.
//!
//! ## 安全边界
//!
//! - functions 层: 0 unsafe; 参数 (pid 范围 / options 标志) 在此校验
//! - 进程表只读/收割经 privileged `proc::api` (`process_with` /
//!   `process_reap_group`); 阻塞等待走 `scheduler_block` / `scheduler_unblock`,
//!   由子组退出侧 (`Scheduler::exit` → `wake_child_waiters`) 唤醒
//! - 用户态缓冲写经 privileged `syscall::api::write_struct_to_user`
//!   (内部先 `check_user_buf` 校验再写入)
//!
//! ## pid 参数
//!
//! - pid > 0: 等待特定 PID 的子进程
//! - pid == 0: 等待同进程组任意子进程
//! - pid == -1: 等待任意子进程 (POSIX `wait()`)
//! - pid < -1: 等待进程组 |pid| 内的任意子进程

use crate::privileged::proc::api;
use crate::privileged::proc::{
    BlockReason, scheduler_block, scheduler_unblock, scheduler_yield_ex,
};
use crate::privileged::syscall::Errno;

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
    /// 收割成功 (`keep_zombie` 时不释放子进程 PCB)
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
/// - `target_pid < -1`: 等待进程组 |`target_pid`| 内的任意子进程
///
/// `non_blocking`: true 时子进程未退出立即返回 `Running`.
/// `keep_zombie`: true 时读取退出码后保留子进程 Zombie 状态 (WNOWAIT 语义).
#[must_use]
pub fn wait_reap(target_pid: i32, non_blocking: bool, keep_zombie: bool) -> WaitOutcome {
    let current_pid = api::process_get_current_pid();
    if current_pid == 0 {
        return WaitOutcome::NoChild;
    }

    let Some(child_tgid) = find_waitable_child(current_pid, target_pid) else {
        return WaitOutcome::NoChild;
    };

    // K-06: 收割单位是线程组 — 就绪条件 = 组已整体退出且全部 TCB 落 Zombie.
    // 组长 TCB 在整组被收割前始终留在表中, 其存在性即"子组仍可等待".
    loop {
        let exit_code = api::process_group_exit_code(child_tgid);
        if exit_code >= 0 {
            return reap_group(
                child_tgid,
                crate::functions::intcast::low_u32_from_i64(exit_code),
                keep_zombie,
            );
        }
        if api::process_with(child_tgid, |p| p.group().tgid()).is_none() {
            // 组已被并发等待者抢先回收: 对本调用而言已无匹配子进程.
            return WaitOutcome::NoChild;
        }
        if non_blocking {
            return WaitOutcome::Running;
        }
        // P-78: 真阻塞等待 — 置 Blocked 并撤出可运行集, 由子组退出侧
        // (`Scheduler::exit` → `wake_child_waiters`) 精确唤醒. 旧实现用
        // `scheduler_yield` 忙轮询: 等待循环期间本核 IF=0, 收不到 tick,
        // EEVDF 合格集随 vruntime 冻结而冻结, 被等待的子进程可能永不被选中
        // (x86_64 满载启动静默挂死的直接机制).
        scheduler_block(BlockReason::WaitingForChild);
        // 丢唤醒闭窗: "读退出码 → 置 Blocked" 之间子组可能已完成退出并广播
        // 唤醒, 彼时本线程仍是 Running, `unblock` 对 Running 不投送, 唤醒被
        // 丢弃. 故置 Blocked **之后**在本轮内复核一次: 条件已成立就撤销阻塞
        // 并回到循环顶部收割; 否则才真正让出 CPU.
        let child_ready = api::process_group_exit_code(child_tgid) >= 0;
        let child_gone = api::process_with(child_tgid, |p| p.group().tgid()).is_none();
        if child_ready || child_gone {
            scheduler_unblock(current_pid);
            continue;
        }
        // 唤醒返回处收口致命信号 (SIGKILL + 组退出 / de_thread): 等待循环不是
        // 信号投递点, 不在此处退出会让整组永久收不齐.
        scheduler_yield_ex();
    }
}

/// 收割已就绪的线程组: 按 `keep_zombie` 决定是否释放组内全部僵尸 TCB.
fn reap_group(child_tgid: u32, exit_code: u32, keep_zombie: bool) -> WaitOutcome {
    if !keep_zombie && !api::process_reap_group(child_tgid) {
        // 认领 CAS 失败 = 另一等待者已完成回收 (多线程 wait4 同一子组).
        return WaitOutcome::NoChild;
    }
    WaitOutcome::Reaped(WaitInfo {
        pid: child_tgid,
        exit_code,
    })
}

/// 查找可等待的子线程组
///
/// 根据 pid 参数匹配子组 tgid, 返回 tgid 或 None。子进程账挂在线程组上,
/// 组内任一线程调用 wait4 看到同一份账; 非组长 TID 不是可等待对象.
fn find_waitable_child(parent_pid: u32, target_pid: i32) -> Option<u32> {
    let (children, caller_pgid) =
        api::process_with(parent_pid, |p| (p.group().children(), p.group().pgid()))
            .unwrap_or_default();

    for &child_tgid in &children {
        // 已被并发等待者回收的组不再可等待.
        if api::process_with(child_tgid, |p| p.group().tgid()).is_none() {
            continue;
        }

        // pid 匹配规则
        if target_pid == -1 {
            return Some(child_tgid);
        } else if target_pid > 0 {
            if child_tgid == target_pid.cast_unsigned() {
                return Some(child_tgid);
            }
        } else if target_pid == 0 {
            // 同进程组: 与调用线程组 pgid 相等
            let pgid = api::process_with(child_tgid, |p| p.group().pgid()).unwrap_or(0);
            if pgid == caller_pgid {
                return Some(child_tgid);
            }
        } else {
            // target_pid < -1: 进程组 ID = |target_pid|
            let want_pgid = target_pid.unsigned_abs();
            let pgid = api::process_with(child_tgid, |p| p.group().pgid()).unwrap_or(0);
            if pgid == want_pgid {
                return Some(child_tgid);
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
        return Err(Errno::Einval);
    }

    let valid_opts = WNOHANG | WUNTRACED | WCONTINUED;
    if options & !valid_opts != 0 {
        return Err(Errno::Einval);
    }

    let current_pid = api::process_get_current_pid();
    if current_pid == 0 {
        return Err(Errno::Echild);
    }
    // wstatus 指针如果为 0, 允许 (调用方不需要状态)
    if wstatus_ptr != 0 && !crate::privileged::syscall::api::validate_user_ptr(wstatus_ptr) {
        return Err(Errno::Efault);
    }

    let non_blocking = options & WNOHANG != 0;

    match wait_reap(pid, non_blocking, false) {
        WaitOutcome::Reaped(info) => {
            if wstatus_ptr != 0 {
                // 写 wstatus (WIFEXITED | exit_code << 8)
                let status: i32 = info.exit_code.cast_signed() << 8;
                if !crate::privileged::syscall::api::write_struct_to_user(wstatus_ptr, &status) {
                    return Err(Errno::Efault);
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
                Err(Errno::Echild)
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
    /// 来源码 (`CLD_EXITED`)
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
/// SIMPLIFIED: 1) `si_code` 恒为 `CLD_EXITED` — 信号投递路径未记录致死信号
/// (termsig), 无法区分 `CLD_KILLED/CLD_DUMPED`; 影响面: 被信号终止的子进程
/// siginfo 归类为正常退出; 何时需扩展: 信号投递记录 termsig 至 Process 后映射.
/// 2) `si_uid` 恒 0 — 无 uid 模型 (SGEG PWM). 3) 拒绝 WSTOPPED/WCONTINUED
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
        return Err(Errno::Einval);
    }
    // 必须指定等待类别之一 (Linux 语义)
    if options & (WEXITED | WSTOPPED | WCONTINUED) == 0 {
        return Err(Errno::Einval);
    }
    // 无 stopped/continued 状态跟踪 (见函数级 SIMPLIFIED 注释)
    if options & (WSTOPPED | WCONTINUED) != 0 {
        return Err(Errno::Einval);
    }

    let target: i32 = match idtype {
        P_ALL => -1,
        P_PID => {
            if id == 0 || id > PID_MAX {
                return Err(Errno::Einval);
            }
            crate::functions::intcast::low_i32_from_u64(id)
        }
        P_PGID => {
            if id == 0 || id > PID_MAX {
                return Err(Errno::Einval);
            }
            // wait_reap 进程组语义: target < -1 → 匹配 pgid == |target|
            -crate::functions::intcast::low_i32_from_u64(id)
        }
        _ => return Err(Errno::Einval),
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
                    si_pid: info.pid.cast_signed(),
                    si_uid: 0,
                    si_status: info.exit_code.cast_signed(),
                    si_utime: 0,
                    si_stime: 0,
                    __pad: [0; 96],
                };
                if !crate::privileged::syscall::api::write_struct_to_user(infop, &si) {
                    return Err(Errno::Efault);
                }
            }
            Ok(0)
        }
        // WNOHANG: 子进程尚未退出
        WaitOutcome::Running => Ok(0),
        WaitOutcome::NoChild => Err(Errno::Echild),
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
        assert!(matches!(wait4_syscall(0x8000, 0, 0), Err(Errno::Einval)));
        assert!(matches!(wait4_syscall(-0x8001, 0, 0), Err(Errno::Einval)));
        // options 含非法位 → EINVAL
        assert!(matches!(wait4_syscall(-1, 0, 0x10), Err(Errno::Einval)));
    }

    #[test]
    fn waitid_rejects_invalid_options_and_idtype() {
        // 未指定等待类别 → EINVAL
        assert!(matches!(
            waitid_syscall(P_ALL, 0, 0, WNOHANG),
            Err(Errno::Einval)
        ));
        // 含 WSTOPPED (无状态跟踪) → EINVAL
        assert!(matches!(
            waitid_syscall(P_ALL, 0, 0, WSTOPPED),
            Err(Errno::Einval)
        ));
        // idtype 非法 → EINVAL
        assert!(matches!(
            waitid_syscall(99, 0, 0, WEXITED),
            Err(Errno::Einval)
        ));
        // P_PID 下 id == 0 → EINVAL
        assert!(matches!(
            waitid_syscall(P_PID, 0, 0, WEXITED),
            Err(Errno::Einval)
        ));
    }
}
