#![deny(unsafe_code)]
//! 调度策略与属性系统调用 — `sched_setscheduler` / `sched_getscheduler` /
//! `sched_setattr` / `sched_getattr`.
//!
//! ## 当前支持面 (D9③ + D8③)
//!
//! - 策略：只接受 `SCHED_OTHER`；其余策略一律返回 `EINVAL`。内核内部的
//!   `SchedPolicy::{Fifo,Rr,Idle,Deadline}` 没有用户态入口，直接改写策略会
//!   让任务留在 RT/DL 队列里却按 CFS 语义被挑选，故不提供半截迁移.
//! - 修改（`setscheduler` / `setattr`）只允许作用于当前进程，其它进程返回
//!   `EPERM`；查询（`getscheduler` / `getattr`）不设此限制.
//! - `sched_setattr` 的 `sched_nice` 经 `Scheduler::set_nice` 生效，
//!   `sched_runtime`（纳秒）换算为 tick 后交给 `Scheduler::set_requested_slice`
//!   按 D8③ 夹取；`sched_flags` / `sched_priority` / `sched_deadline` /
//!   `sched_period` 必须为 0，否则 `EINVAL`（不静默忽略）.
//! - `sched_getscheduler` / `sched_getattr` 把内部策略映射回 Linux ABI 值：
//!   `Normal→0` / `Fifo→1` / `Rr→2` / `Idle→5` / `Deadline→6`.
//!
//! ## 框内核边界
//! - 100% safe Rust；只调用 privileged 安全 API
//! - ABI 常量与结构布局来自 `edgine-abi::sched` 单一权威源
//! - 不在此层复制权重、切片或虚拟时间公式

use crate::functions::syscall::types::{
    MAX_NICE, MIN_NICE, SCHED_ATTR_SIZE_VER0, SCHED_DEADLINE, SCHED_FIFO, SCHED_IDLE, SCHED_OTHER,
    SCHED_RR, SchedAttr, SchedParam,
};

use crate::privileged::proc::process::Process;
use crate::privileged::proc::{SchedPolicy, Scheduler, process_get_current_pid, process_with};
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::api::{read_struct_from_user, write_struct_to_user};
use crate::privileged::timer::tick::{ns_to_ticks, ticks_to_ns};

use super::priority::setpriority_for_pid;

/// 内核内部调度策略 → Linux ABI 策略值.
///
/// 内部枚举以 `Normal=0 / Fifo=1 / Rr=2 / Idle=3 / Deadline=4` 编码，
/// 而 Linux ABI 中 `SCHED_IDLE=5` / `SCHED_DEADLINE=6`，两个值域**不可混用**.
fn policy_to_abi(policy: SchedPolicy) -> u32 {
    match policy {
        SchedPolicy::Normal => SCHED_OTHER,
        SchedPolicy::Fifo => SCHED_FIFO,
        SchedPolicy::Rr => SCHED_RR,
        SchedPolicy::Idle => SCHED_IDLE,
        SchedPolicy::Deadline => SCHED_DEADLINE,
    }
}

/// 解析查询型 `pid`（0 = 当前进程；负数 `EINVAL`）.
fn resolve_query_pid(pid: i32) -> Result<u32, Errno> {
    if pid < 0 {
        return Err(Errno::Einval);
    }
    if pid == 0 {
        let current = process_get_current_pid();
        return if current == 0 {
            Err(Errno::Esrch)
        } else {
            Ok(current)
        };
    }
    Ok(pid.cast_unsigned())
}

/// 解析修改型 `pid`：目标必须存在且为当前进程.
///
/// 返回码顺序固定为 `EINVAL`（负 pid）→ `ESRCH`（目标不存在）→ `EPERM`
/// （目标不是当前进程），避免"不存在"与"无权限"因检查顺序互相掩盖.
fn resolve_self_pid(pid: i32) -> Result<u32, Errno> {
    let target = resolve_query_pid(pid)?;
    if process_with(target, |_| ()).is_none() {
        return Err(Errno::Esrch);
    }
    let current = process_get_current_pid();
    if current != 0 && target == current {
        Ok(target)
    } else {
        Err(Errno::Eperm)
    }
}

/// `sched_setscheduler(pid, policy, param_ptr)` 策略.
///
/// 只接受 `SCHED_OTHER` + `sched_priority == 0`；成功返回 0。当前策略已经
/// 是 `SCHED_NORMAL` 时是幂等 no-op，任务处于内部 RT/DL/Idle 策略时返回
/// `EINVAL`（无安全的用户态策略迁移路径）.
#[must_use]
pub fn sched_setscheduler_syscall(pid: i32, policy: u32, param_ptr: u64) -> i64 {
    if policy != SCHED_OTHER {
        return Errno::Einval.as_ret();
    }
    let target = match resolve_self_pid(pid) {
        Ok(target) => target,
        Err(errno) => return errno.as_ret(),
    };

    let mut param = SchedParam::default();
    if !read_struct_from_user(param_ptr, &mut param) {
        return Errno::Efault.as_ret();
    }
    if param.sched_priority != 0 {
        return Errno::Einval.as_ret();
    }

    match process_with(target, Process::get_sched_policy) {
        Some(SchedPolicy::Normal) => 0,
        Some(_) => Errno::Einval.as_ret(),
        None => Errno::Esrch.as_ret(),
    }
}

/// `sched_getscheduler(pid)` 策略：返回 Linux ABI 策略值.
#[must_use]
pub fn sched_getscheduler_syscall(pid: i32) -> i64 {
    let target = match resolve_query_pid(pid) {
        Ok(target) => target,
        Err(errno) => return errno.as_ret(),
    };
    match process_with(target, |proc| policy_to_abi(proc.get_sched_policy())) {
        Some(policy) => i64::from(policy),
        None => Errno::Esrch.as_ret(),
    }
}

/// `sched_setattr(pid, attr_ptr, flags)` 策略.
///
/// `size` 必须恰为 48（VER0）；`sched_nice` 必须落在 `[-20, 19]`；
/// `sched_runtime` 为请求切片纳秒值（0 = 清除请求）。所有未支持的字段
/// 显式返回 `EINVAL`.
#[must_use]
pub fn sched_setattr_syscall(pid: i32, attr_ptr: u64, flags: u32) -> i64 {
    if flags != 0 {
        return Errno::Einval.as_ret();
    }

    let mut attr = SchedAttr::default();
    if !read_struct_from_user(attr_ptr, &mut attr) {
        return Errno::Efault.as_ret();
    }
    if attr.size != SCHED_ATTR_SIZE_VER0
        || attr.sched_policy != SCHED_OTHER
        || attr.sched_flags != 0
        || attr.sched_priority != 0
        || attr.sched_deadline != 0
        || attr.sched_period != 0
        || !(MIN_NICE..=MAX_NICE).contains(&attr.sched_nice)
    {
        return Errno::Einval.as_ret();
    }

    let target = match resolve_self_pid(pid) {
        Ok(target) => target,
        Err(errno) => return errno.as_ret(),
    };
    match process_with(target, Process::get_sched_policy) {
        Some(SchedPolicy::Normal) => {}
        Some(_) => return Errno::Einval.as_ret(),
        None => return Errno::Esrch.as_ret(),
    }

    let ret = setpriority_for_pid(target, attr.sched_nice);
    if ret < 0 {
        return ret;
    }
    let requested_ticks = ns_to_ticks(attr.sched_runtime);
    if !Scheduler::set_requested_slice(target, requested_ticks) {
        return Errno::Esrch.as_ret();
    }
    0
}

/// `sched_getattr(pid, attr_ptr, size, flags)` 策略.
///
/// `size` 是用户提供的结构容量：小于 48 返回 `EINVAL`，大于 48 时只回写
/// 48 字节的 VER0 结构（Linux 兼容语义）。未支持的字段回写 0.
#[must_use]
pub fn sched_getattr_syscall(pid: i32, attr_ptr: u64, size: u32, flags: u32) -> i64 {
    if flags != 0 || size < SCHED_ATTR_SIZE_VER0 {
        return Errno::Einval.as_ret();
    }
    let target = match resolve_query_pid(pid) {
        Ok(target) => target,
        Err(errno) => return errno.as_ret(),
    };

    let Some((policy, nice, rt_priority, requested_ticks)) = process_with(target, |proc| {
        (
            proc.get_sched_policy(),
            proc.get_nice(),
            proc.get_rt_priority(),
            proc.eevdf_requested_slice
                .load(core::sync::atomic::Ordering::Acquire),
        )
    }) else {
        return Errno::Esrch.as_ret();
    };

    let attr = SchedAttr {
        size: SCHED_ATTR_SIZE_VER0,
        sched_policy: policy_to_abi(policy),
        sched_flags: 0,
        sched_nice: i32::from(nice),
        sched_priority: u32::from(rt_priority),
        sched_runtime: ticks_to_ns(requested_ticks),
        sched_deadline: 0,
        sched_period: 0,
    };
    if !write_struct_to_user(attr_ptr, &attr) {
        return Errno::Efault.as_ret();
    }
    0
}
