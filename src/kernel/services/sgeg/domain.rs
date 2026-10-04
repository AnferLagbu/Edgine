#![deny(unsafe_code)]
//! SGEG 域级行为门控 (`DomainFlags`) — services 层策略 (分册 9 批次 4)
//!
//! ## 框内核边界
//!
//! - 100% safe Rust
//! - 通过 `framework::proc` 公开机制 API 读写域标志, 通过 `framework::sgeg`
//!   公开 API 做权限判定
//! - 无 unsafe, 无裸指针
//!
//! ## 分工
//!
//! 门控裁决点在 `framework/proc/domain.rs::domain_gate_check` (syscall 咽喉点,
//! 在 seccomp 检查之后); 本文件仅提供用户态可见的查询/设置入口与权限策略。

use crate::framework::syscall::Errno;

/// `SYS_SGEG_GET_DOMAIN_FLAGS` — 查询当前进程域标志位掩码
///
/// 返回位掩码 (非负); 进程不存在返回 `ESRCH`.
pub fn domain_flags_get_syscall() -> i64 {
    let pid = crate::framework::proc::process_get_current_pid();
    match crate::framework::proc::domain_flags_get(pid) {
        Some(flags) => i64::from(flags.bits()),
        None => Errno::ESRCH.as_ret(),
    }
}

/// `SYS_SGEG_SET_DOMAIN_FLAGS` — 设置当前进程域标志位掩码
///
/// 安全约束 (仿 `pwm_set_syscall`, B07-05 先例):
/// 1. 仅作用于**当前进程** (不接受 pid 参数), 杜绝跨进程域限制篡改;
/// 2. 调用方必须持有 SYSTEM 域 `SYSTEM_CAP_SET_DOMAIN_FLAGS` 能力位
///    (bootstrap PWM 0 经 `engine::check` 恒全权) — 域标志可放宽既有
///    `SANDBOX`/`READONLY` 约束, 属身份安全关键操作;
/// 3. 未定义位由 framework 的 `from_bits_truncate` 丢弃 (fail-safe)。
///
/// # Errors
///
/// - 无权限 → `EPERM`
/// - 当前进程不存在 → `ESRCH`
pub fn domain_flags_set_syscall(flags: u64) -> i64 {
    use crate::services::sgeg::capability::{CAP_DOMAIN_SYSTEM, SYSTEM_CAP_SET_DOMAIN_FLAGS};
    let current = crate::framework::sgeg::pwm_get_current();
    if !crate::framework::sgeg::pwm_has_capability(
        current,
        CAP_DOMAIN_SYSTEM,
        SYSTEM_CAP_SET_DOMAIN_FLAGS,
    ) {
        return Errno::EPERM.as_ret();
    }
    let pid = crate::framework::proc::process_get_current_pid();
    let wanted = crate::framework::sgeg::DomainFlags::from_bits_truncate(flags as u32);
    if crate::framework::proc::domain_flags_set(pid, wanted) {
        0
    } else {
        Errno::ESRCH.as_ret()
    }
}
