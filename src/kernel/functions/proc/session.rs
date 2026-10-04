#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有机制状态访问经 privileged 安全 API.
//! 会话 / 进程组 / 控制终端 — functions 侧 syscall 策略入口
//!
//! ## 分层 (T2 批 2, syscall-followup)
//!
//! 机制留在 privileged (`privileged/proc/session.rs`):
//!   - `SessionManager` / `SESSION_MANAGER` (会话/进程组/控制终端机制状态)
//!   - `proc_setsid/getsid/setpgid/getpgid` (进程组策略, 早于本批迁入)
//!   - `get_foreground_pgid` (前台进程组查询辅助)
//!
//! 本文件新增 syscall 策略: `tcgetpgrp_syscall` / `tcsetpgrp_syscall`.
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义 (T1-6 于 2026-06-16 自 privileged 迁入) 按统一判据迁回 privileged —
//! 会话/进程组是进程关系机制状态。T2 批 2 再将 tcgetpgrp/tcsetpgrp syscall
//! 策略入口迁回 functions, 机制状态仍归 privileged。

pub use crate::privileged::proc::session::*;

use crate::privileged::proc::process_for_each;
use crate::privileged::proc::process_get_current_pid;
use crate::privileged::proc::process_with;
use crate::privileged::syscall::Errno;
use core::sync::atomic::Ordering;

/// tcgetpgrp(fd) 策略 — 获取当前会话的前台进程组
///
/// T2 批 2 自 privileged 回退层迁移. POSIX 简化语义: `fd` 忽略 (无控制终端
/// 设备模型), 直接返回当前进程所属会话的前台进程组.
pub fn tcgetpgrp_syscall(_fd: i32) -> i64 {
    i64::from(crate::privileged::proc::get_foreground_pgid())
}

/// tcsetpgrp(fd, pgid) 策略 — 设置前台进程组
///
/// T2 批 2 自 privileged 回退层迁移. 校验 pgid 属于当前会话的进程组后,
/// 委托 `SESSION_MANAGER.set_foreground_pgid`. POSIX 简化语义: `fd` 忽略.
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pid/pgid); 重命名会破坏 POSIX 语义对应, 仅在确实混淆时才人工拆分"
)]
pub fn tcsetpgrp_syscall(_fd: i32, pgid: i32) -> i64 {
    if pgid <= 0 {
        return Errno::EINVAL.as_ret();
    }

    let pid = process_get_current_pid();
    if pid == 0 {
        return -1;
    }

    let sid = process_with(pid, |p| p.session_id.load(Ordering::SeqCst)).unwrap_or(0);
    if sid == 0 {
        return -1;
    }

    let mut found = false;
    process_for_each(|proc| {
        let pg = proc.pgid.load(Ordering::SeqCst);
        let proc_sid = proc.session_id.load(Ordering::SeqCst);
        if pg == pgid as u32 && proc_sid == sid {
            found = true;
        }
        true
    });

    if !found {
        return Errno::EINVAL.as_ret();
    }

    if crate::privileged::proc::SESSION_MANAGER.set_foreground_pgid(sid, pgid as u32) {
        0
    } else {
        -1
    }
}
