#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! POSIX Timer — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 纯类型安全
//! - 封装 `privileged::proc::posix_timer` 的 per-process 定时器
//!
//! ## API 形态
//!
//! ```ignore
//! use crate::functions::timer::posix_timer;
//!
//! // 启动
//! let new_value = Itimerspec { it_interval_sec: 1, it_interval_nsec: 0,
//!                              it_value_sec: 1, it_value_nsec: 0 };
//! posix_timer::timer_settime(id, 0, Some(&new_value), None);
//!
//! // 查询
//! let mut curr = Itimerspec::zeroed();
//! posix_timer::timer_gettime(id, &mut curr);
//!
//! // 删除
//! posix_timer::timer_delete(id);
//! ```
//!
//! ## 注意事项
//!
//! `timer_create` 需要向内核传入 `sigevent` 用户态布局指针. 该结构体
//! 必须是 `repr(C)` POD, 且其首字段 (`sigev_value`) 在用户态栈上构造.
//! 本 functions 模块仅暴露 *high-level* helper (不直接调用 `timer_create`),
//! 而要求调用方在用户态构造 sigevent 后传入. 这是为了避免将 user-space
//! 结构体布局泄漏到内核.
//
//! ## 与 timerfd 的差异
//!
//! - **POSIX Timer**: 通过 `timer_t` 句柄 + 信号通知
//! - **timerfd**: 通过文件描述符 + read/epoll 通知
//!
//! 两者底层共用 hrtimer, 但用户态交互方式不同。

// Re-export privileged 层的类型和常量
pub use crate::privileged::proc::{
    CLOCK_MONOTONIC, CLOCK_REALTIME, Itimerspec, MAX_POSIX_TIMERS, SIGEV_NONE, SIGEV_SIGNAL,
    Sigevent, TFD_TIMER_ABSTIME, posix_timer_active_count,
};

// §6.1 下沉: 系统调用包装迁至 functions/syscall/posix_timer (纯策略)
use crate::functions::syscall::posix_timer as syscall_ptimer;

// ============================================================================
// 系统调用安全包装
// ============================================================================

/// `timer_settime` — 启动 / 调整 / 停止定时器
///
/// `new_value` 为 None 时按 disarm 处理。`old_value` 为 None 时不读取旧值。
///
/// ## 用户态使用
///
/// ```c
/// struct itimerspec new_val = { .it_interval = {1, 0}, .it_value = {1, 0} };
/// syscall(EG_TIMER_SETTIME, id, 0, &new_val, NULL);
/// ```
pub fn timer_settime(timer_id: i32, flags: i32, new_value_ptr: u64, old_value_ptr: u64) -> i64 {
    syscall_ptimer::sys_timer_settime(timer_id as u64, flags as u64, new_value_ptr, old_value_ptr)
}

/// `timer_gettime` — 查询定时器剩余时间和间隔
///
/// `curr_value_ptr` 必须指向 32 字节有效的 itimerspec 缓冲。
pub fn timer_gettime(timer_id: i32, curr_value_ptr: u64) -> i64 {
    syscall_ptimer::sys_timer_gettime(timer_id as u64, curr_value_ptr)
}

/// `timer_delete` — 释放定时器
pub fn timer_delete(timer_id: i32) -> i64 {
    syscall_ptimer::sys_timer_delete(timer_id as u64)
}

/// `timer_getoverrun` — 返回上次 read 之后补打的次数
pub fn timer_getoverrun(timer_id: i32) -> i64 {
    syscall_ptimer::sys_timer_getoverrun(timer_id as u64)
}

/// `clock_getres` — 时钟分辨率
///
/// `res_ptr` 可为 0 (仅做时钟存在性检查)。
pub fn clock_getres(clockid: i32, res_ptr: u64) -> i64 {
    syscall_ptimer::sys_clock_getres(clockid as u64, res_ptr)
}
