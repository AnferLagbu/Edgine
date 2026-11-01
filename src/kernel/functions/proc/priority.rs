#![deny(unsafe_code)]
//! 进程优先级策略 — nice / getpriority / setpriority
//!
//! - nice_to_priority: nice 值 → ProcessPriority 映射 (历史粗粒度字段)
//! - nice_syscall: nice() 系统调用策略
//! - getpriority_syscall: getpriority() 系统调用策略 (返回真实 nice)
//! - setpriority_syscall: setpriority() 系统调用策略 (nice 与权重同步)
//!
//! nice 的权威值是 `Process::nice` 与其派生 `Process::cfs_weight`;
//! `ProcessPriority` 只作粗粒度兼容字段, 每次写 nice 时同步维护.
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 经 privileged 安全 API (`Scheduler::set_nice` / `process_with`) 访问进程表
//! - 无 unsafe, 无裸指针

use crate::privileged::proc::process::Process;
use crate::privileged::proc::{
    ProcessPriority, Scheduler, process_get_current_pid, process_with, process_with_mut,
};
use crate::privileged::syscall::Errno;

const PRIO_PROCESS: i32 = 0;

/// nice 值 → `ProcessPriority` 映射
#[must_use]
pub fn nice_to_priority(nice: i32) -> ProcessPriority {
    let clamped = nice.clamp(-20, 19);
    if clamped < -10 {
        ProcessPriority::RealTime
    } else if clamped < 0 {
        ProcessPriority::High
    } else if clamped < 10 {
        ProcessPriority::Normal
    } else if clamped < 19 {
        ProcessPriority::Low
    } else {
        ProcessPriority::Idle
    }
}

/// nice(inc) 系统调用策略
#[must_use]
pub fn nice_syscall(inc: i32) -> i64 {
    let pid = process_get_current_pid();
    if pid == 0 {
        return Errno::Esrch.as_ret();
    }
    apply_nice(pid, inc)
}

/// 对 `pid` 应用 nice 增量, 返回新 nice (负值 = errno).
///
/// `nice_syscall` 的显式 pid 形式: 负数 nice 是合法值, 不得与错误返回码混淆.
#[must_use]
pub fn apply_nice(pid: u32, inc: i32) -> i64 {
    let Some(current_nice) = process_with(pid, Process::get_nice) else {
        return Errno::Esrch.as_ret();
    };
    let new_nice = i32::from(current_nice).saturating_add(inc).clamp(-20, 19);
    let ret = setpriority_for_pid(pid, new_nice);
    if ret < 0 { ret } else { i64::from(new_nice) }
}

/// getpriority(which, who) 系统调用策略
#[must_use]
pub fn getpriority_syscall(which: i32, who: u32) -> i64 {
    if which != PRIO_PROCESS {
        return Errno::Einval.as_ret();
    }
    let Some(pid) = resolve_pid(who) else {
        return Errno::Esrch.as_ret();
    };
    let Some(nice) = process_with(pid, Process::get_nice) else {
        return Errno::Esrch.as_ret();
    };
    i64::from(nice)
}

/// setpriority(which, who, prio) 系统调用策略
#[must_use]
pub fn setpriority_syscall(which: i32, who: u32, prio: i32) -> i64 {
    if which != PRIO_PROCESS {
        return Errno::Einval.as_ret();
    }
    let Some(pid) = resolve_pid(who) else {
        return Errno::Esrch.as_ret();
    };
    setpriority_for_pid(pid, prio)
}

/// `who == 0` 解析为当前进程; 无法解析时返回 `None`.
fn resolve_pid(who: u32) -> Option<u32> {
    if who == 0 {
        let pid = process_get_current_pid();
        (pid != 0).then_some(pid)
    } else {
        Some(who)
    }
}

/// 夹取 nice 后经调度器写入 nice/权重, 并同步粗粒度优先级.
///
/// `sched_setattr` 的 `sched_nice` 复用本入口，保证 nice 只有一个写路径.
pub(crate) fn setpriority_for_pid(pid: u32, prio: i32) -> i64 {
    let clamped = prio.clamp(-20, 19);
    if !Scheduler::set_nice(pid, crate::functions::intcast::low_i8_from_i32(clamped)) {
        return Errno::Esrch.as_ret();
    }
    let coarse = nice_to_priority(clamped);
    process_with_mut(pid, |proc| proc.set_priority(coarse));
    0
}
