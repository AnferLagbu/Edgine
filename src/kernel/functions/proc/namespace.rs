#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有机制状态访问经 privileged 安全 API.
//! Linux 兼容 Namespace 框架 (D1) — functions 侧 re-export + syscall 策略入口
//!
//! ## 分层 (T2 批 4, syscall-followup)
//!
//! 机制留在 privileged (`privileged/proc/namespace.rs`):
//!   - `NamespaceSet` (Process 机制字段) + 各 ns 实例 (Uts/Ipc/Pid/Mount/User/Net/Cgroup)
//!   - `NsType` / `CLONE_NEW_*` 常量 / `NsRegistry` / `ns_register`
//!   - `NamespaceSet::unshare` / `setns_by_type` (机制语义)
//!
//! 本文件实现 syscall 策略: 参数解析 + 特权判定 + 委托机制状态变更.
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义（T1-3 于 2026-06-16 自 privileged 迁入）按统一判据迁回 privileged —
//! NamespaceSet 是 Process 机制字段, 被 privileged clone 消费。T2 批 4 再将
//! unshare/setns syscall 策略入口迁回 functions, 机制状态仍归 privileged。

pub use crate::privileged::proc::namespace::*;

use crate::privileged::proc::process_get_current_pid;
use crate::privileged::proc::process_with_mut;
use crate::privileged::syscall::Errno;

/// unshare(flags) 策略 — 取消共享指定 namespace
///
/// T2 批 4 自 privileged 回退层迁移. 委托 `NamespaceSet::unshare` (机制:
/// 按 flags 为各 ns 创建新实例), functions 仅 errno 映射.
pub fn unshare_syscall(flags: u64) -> i64 {
    let pid = process_get_current_pid();

    match process_with_mut(pid, |p| p.namespaces.lock().unshare(flags)) {
        Some(Ok(())) => 0,
        Some(Err(e)) => e.as_ret(),
        None => Errno::ESRCH.as_ret(),
    }
}

/// setns(ns_type, target_ns_id) 策略 — 切换到目标 namespace
///
/// T2 批 4 自 privileged 回退层迁移. 参数解析 (CLONE_NEW_* 标志位与简化枚举
/// 双语义) + CAP_SYS_ADMIN 特权判定 + 委托 `NamespaceSet::setns_by_type`.
pub fn setns_syscall(ns_type: u64, target_ns_id: u64) -> i64 {
    // B06-18: 修正原 `1 << (ns_type + 8)` 位运算公式错误 (恒不匹配 CLONE_NEW* 导致
    // from_clone_flag 恒 None)。现直接用 ns_type 匹配: 兼容 CLONE_NEW* 标志位
    // (0x00020000 等) 与 Edgine 简化枚举值 (0-6) 两种语义。
    let ns_t = match NsType::from_clone_flag(ns_type) {
        Some(t) => t,
        None => match ns_type {
            0 => NsType::Mount,
            1 => NsType::Uts,
            2 => NsType::Ipc,
            3 => NsType::User,
            4 => NsType::Pid,
            5 => NsType::Net,
            6 => NsType::Cgroup,
            _ => return Errno::EINVAL.as_ret(),
        },
    };

    // B06-20: setns 切换 namespace 需 CAP_SYS_ADMIN (SYSTEM 域 0x01), 与 mount/umount2 先例一致
    let pwm = crate::privileged::sgeg::pwm_get_current();
    if !crate::privileged::sgeg::pwm_has_capability(pwm, 0, 0x01) {
        return Errno::EPERM.as_ret();
    }

    let pid = process_get_current_pid();

    match process_with_mut(pid, |p| {
        p.namespaces.lock().setns_by_type(ns_t, target_ns_id)
    }) {
        Some(Ok(())) => 0,
        Some(Err(e)) => e.as_ret(),
        None => Errno::ESRCH.as_ret(),
    }
}
