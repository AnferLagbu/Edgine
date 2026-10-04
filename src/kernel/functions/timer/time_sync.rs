#![deny(unsafe_code)]
//! NTP/PTP 时钟同步安全代理 — functions 层 (0 unsafe)
//!
//! 封装 `privileged::timer::time_sync` 的安全 API.

// 重导出强类型
pub use crate::privileged::timer::{
    ADJ_RATE_NS, ClockAdjState, MAX_FREQ_ADJUST_PPM, MAX_OFFSET_NS, NTP_EPOCH_OFFSET, NTP_PORT,
    NtpPacket, NtpResult, NtpTimestamp, PTP_EVENT_PORT, PTP_GENERAL_PORT, PtpMessageType,
    TimeSyncSubsystem,
};

use crate::privileged::timer::{
    sys_timesync, timesync_init, timesync_is_initialized, timesync_subsystem,
};

/// 初始化时间同步
pub fn init() {
    timesync_init();
}

/// 时间同步是否已初始化
pub fn is_initialized() -> bool {
    timesync_is_initialized()
}

/// 获取全局时间同步子系统
pub fn subsystem() -> &'static TimeSyncSubsystem {
    timesync_subsystem()
}

/// 时间同步系统调用 (安全封装)
pub fn timesync_syscall(cmd: u64, a1: u64, a2: u64) -> i64 {
    sys_timesync(cmd, a1, a2)
}
