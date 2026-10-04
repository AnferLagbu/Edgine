//! Timer 子系统 — functions 层安全代理
//!
//! 为定时器相关系统调用提供参数验证和类型安全封装。
//! 本模块整合了分散在 proc/driver/sync 中的 timer 策略代码。
//!
//! ## 子模块
//!
//! - `clock` — clock_gettime / gettimeofday 时钟查询
//! - `sleep` — nanosleep 安全代理
//! - `posix_timer` — POSIX Timer 安全代理
//! - `timerfd` — timerfd 安全代理
//! - `tickless` — Tickless (NO_HZ) 安全代理
//! - `time_sync` — NTP/PTP 时钟同步安全代理
//!
//! ## 安全边界
//!
//! - functions 层验证标量参数（时钟 ID、标志位、时间值等）
//! - 原始指针解引用委托给 privileged 层（指针合法性由 syscall 入口保证）
//! - 本模块 0 unsafe，所有 unsafe 操作在 privileged 层完成

#![deny(unsafe_code)]

pub mod clock;
pub mod posix_timer;
pub mod sleep;
pub mod tickless;
pub mod time_sync;
pub mod timerfd;
