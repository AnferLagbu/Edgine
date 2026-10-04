#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! POSIX Timer 系统调用 — functions 层实现 (从 privileged/syscall/posix_timer.rs 下沉, §6.1)
//!
//! 纯策略包装: 实际 per-process 定时器机制由 privileged `proc::posix_timer` 提供。
//!
//! ## 编号 (740-745: POSIX Timer)
//!
//! - `EG_TIMER_CREATE`     (740): 创建 per-process 定时器
//! - `EG_TIMER_SETTIME`    (741): 启动 / 调整 / 停止定时器
//! - `EG_TIMER_GETTIME`    (742): 查询剩余时间和间隔
//! - `EG_TIMER_DELETE`     (743): 释放定时器
//! - `EG_TIMER_GETOVERRUN` (744): 返回上次 read 之后补打的次数
//! - `EG_CLOCK_GETRES`     (745): 时钟分辨率

use crate::privileged::proc as ptimer;

// ============================================================================
// sys_timer_create
// ============================================================================

/// `sys_timer_create(clockid, sigev_ptr, timer_id_ptr) -> 0/-errno`
pub fn sys_timer_create(a0: u64, a1: u64, a2: u64) -> i64 {
    ptimer::sys_timer_create(a0 as i32, a1, a2)
}

// ============================================================================
// sys_timer_settime
// ============================================================================

/// `sys_timer_settime(timer_id, flags, new_value_ptr, old_value_ptr) -> 0/-errno`
pub fn sys_timer_settime(a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    ptimer::sys_timer_settime(a0 as i32, a1 as i32, a2, a3)
}

// ============================================================================
// sys_timer_gettime
// ============================================================================

/// `sys_timer_gettime(timer_id, curr_value_ptr) -> 0/-errno`
pub fn sys_timer_gettime(a0: u64, a1: u64) -> i64 {
    ptimer::sys_timer_gettime(a0 as i32, a1)
}

// ============================================================================
// sys_timer_delete
// ============================================================================

/// `sys_timer_delete(timer_id) -> 0/-errno`
pub fn sys_timer_delete(a0: u64) -> i64 {
    ptimer::sys_timer_delete(a0 as i32)
}

// ============================================================================
// sys_timer_getoverrun
// ============================================================================

/// `sys_timer_getoverrun(timer_id) -> overrun / -errno`
pub fn sys_timer_getoverrun(a0: u64) -> i64 {
    ptimer::sys_timer_getoverrun(a0 as i32)
}

// ============================================================================
// sys_clock_getres
// ============================================================================

/// `sys_clock_getres(clockid, res_ptr) -> 0/-errno`
///
/// `res_ptr` 可为 NULL (仅做时钟存在性检查)。
pub fn sys_clock_getres(a0: u64, a1: u64) -> i64 {
    ptimer::sys_clock_getres(a0 as i32, a1)
}
