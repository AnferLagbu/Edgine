#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! ftrace / KGDB 系统调用 — services 层实现 (从 framework/syscall/ftrace_kgdb.rs 下沉, §6.2)
//!
//! ## 编号 (800-809: 内核调试 / 跟踪)
//!
//! - `QX_FTRACE_ENABLE`  (800): 启用 ftrace 全局开关
//! - `QX_FTRACE_DISABLE` (801): 禁用 ftrace 全局开关
//! - `QX_FTRACE_READ`    (802): 弹出一条事件, 按紧凑布局写入用户态缓冲
//! - `QX_FTRACE_STAT`    (803): 查询 (`event_count`, `overflow_count`) 到用户态
//! - `QX_KGDB_ENTER`     (804): 主动进入 KGDB 主循环 (等待外部 gdb)
//!
//! ## 用户态布局
//!
//! ```c
//! struct user_trace_event {
//!     uint64_t timestamp;
//!     uint64_t name_hash;
//!     uint64_t arg0;
//!     uint64_t arg1;
//!     uint64_t arg2;
//!     uint64_t arg3;
//! }; // 48 字节, 逐字段 native-endian 序列化, 不要求用户缓冲对齐
//! ```
//!
//! ## 安全
//!
//! - 用户态指针经 services `check_user_buf` 校验后, 交由 framework `mm::copy_user`
//!   (SMAP + 异常表兜底) 完成拷贝, 不假设用户内存对齐
//! - `kgdb_enter` 要求串口已注册 (`kgdb_serial_ready`), 否则返回 `ENODEV`

use crate::framework::debug::{
    KgdbRegs, TraceEvent, ftrace_disable, ftrace_enable, ftrace_event_count, ftrace_overflow_count,
    ftrace_pop_event, kgdb_breakpoint, kgdb_serial_ready,
};
use crate::framework::mm::copy_user::copy_to_user;

const EFAULT: i64 = -14;
const ENODEV: i64 = -19;

const TRACE_EVENT_SIZE: usize = core::mem::size_of::<TraceEvent>();
const FTRACE_STAT_SIZE: usize = 16;

/// 向用户态写入字节切片 (经 framework `copy_to_user`)
fn write_user_bytes(ptr: u64, data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if !super::check_user_buf(ptr, data.len() as u64) {
        return false;
    }
    copy_to_user(ptr, data, data.len()).is_ok()
}

/// 将 `TraceEvent` 序列化为 native-endian 字节数组
///
/// 不依赖结构体内存对齐, 逐字段 `to_ne_bytes` 拼接 (6 × u64 = 48 字节)。
fn trace_event_bytes(ev: &TraceEvent) -> [u8; TRACE_EVENT_SIZE] {
    let mut b = [0u8; TRACE_EVENT_SIZE];
    b[0..8].copy_from_slice(&ev.timestamp.to_ne_bytes());
    b[8..16].copy_from_slice(&ev.name_hash.to_ne_bytes());
    b[16..24].copy_from_slice(&ev.arg0.to_ne_bytes());
    b[24..32].copy_from_slice(&ev.arg1.to_ne_bytes());
    b[32..40].copy_from_slice(&ev.arg2.to_ne_bytes());
    b[40..48].copy_from_slice(&ev.arg3.to_ne_bytes());
    b
}

/// 将 (`event_count`, `overflow_count`) 序列化为 native-endian 字节数组
fn ftrace_stat_bytes(event_count: u64, overflow_count: u64) -> [u8; FTRACE_STAT_SIZE] {
    let mut b = [0u8; FTRACE_STAT_SIZE];
    b[0..8].copy_from_slice(&event_count.to_ne_bytes());
    b[8..16].copy_from_slice(&overflow_count.to_ne_bytes());
    b
}

/// `sys_ftrace_enable`: 启用 ftrace 全局开关
pub fn sys_ftrace_enable() -> i64 {
    ftrace_enable();
    0
}

/// `sys_ftrace_disable`: 禁用 ftrace 全局开关
pub fn sys_ftrace_disable() -> i64 {
    ftrace_disable();
    0
}

/// `sys_ftrace_read`: 弹出一条事件, 写入用户态缓冲
///
/// - `a0`: 用户态指针 (`UserTraceEvent` 布局 48 字节)
/// - 返回 0 = 成功, 1 = 缓冲区空 (无事件), 负数 = errno
pub fn sys_ftrace_read(a0: u64) -> i64 {
    if !super::check_user_buf(a0, TRACE_EVENT_SIZE as u64) {
        return EFAULT;
    }
    ftrace_pop_event().map_or(1, |ev| {
        let bytes = trace_event_bytes(&ev);
        if write_user_bytes(a0, &bytes) {
            0
        } else {
            EFAULT
        }
    })
}

/// `sys_ftrace_stat`: 拷贝 (`event_count`, `overflow_count`) 到用户态
///
/// - `a0`: 用户态指针 (16 字节, `[u64; 2]` 布局)
pub fn sys_ftrace_stat(a0: u64) -> i64 {
    if !super::check_user_buf(a0, FTRACE_STAT_SIZE as u64) {
        return EFAULT;
    }
    let bytes = ftrace_stat_bytes(ftrace_event_count(), ftrace_overflow_count());
    if write_user_bytes(a0, &bytes) {
        0
    } else {
        EFAULT
    }
}

/// `sys_kgdb_enter`: 主动进入 KGDB 主循环
///
/// - 串口未注册时返回 `ENODEV`
/// - 串口已注册时: 阻塞与外部 gdb 通信, 收到 c/s/k 后返回 0
pub fn sys_kgdb_enter() -> i64 {
    if !kgdb_serial_ready() {
        return ENODEV;
    }
    let mut regs = KgdbRegs::default();
    kgdb_breakpoint(&mut regs);
    0
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_event_size_matches_layout() {
        // 用户态 ABI 布局: 6 × u64 = 48 字节
        assert_eq!(TRACE_EVENT_SIZE, 48);
    }

    #[test]
    fn trace_event_bytes_layout() {
        let ev = TraceEvent {
            timestamp: 1,
            name_hash: 2,
            arg0: 3,
            arg1: 4,
            arg2: 5,
            arg3: 6,
        };
        let b = trace_event_bytes(&ev);
        assert_eq!(&b[0..8], &1u64.to_ne_bytes());
        assert_eq!(&b[8..16], &2u64.to_ne_bytes());
        assert_eq!(&b[16..24], &3u64.to_ne_bytes());
        assert_eq!(&b[24..32], &4u64.to_ne_bytes());
        assert_eq!(&b[32..40], &5u64.to_ne_bytes());
        assert_eq!(&b[40..48], &6u64.to_ne_bytes());
    }

    #[test]
    fn ftrace_stat_bytes_layout() {
        let b = ftrace_stat_bytes(7, 8);
        assert_eq!(&b[0..8], &7u64.to_ne_bytes());
        assert_eq!(&b[8..16], &8u64.to_ne_bytes());
    }
}
