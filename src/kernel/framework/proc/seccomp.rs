#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! Seccomp — 系统调用过滤 — framework 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 策略代码 (过滤器 + 规则匹配 + syscall) 于 T1-5 (2026-06-16) 迁至
//! `services::proc::seccomp`, 本文件仅 re-export。按统一判据反转：
//! SeccompState 挂在 Process 结构体 (framework 进程机制状态), seccomp_check
//! 被 framework syscall 分发路径消费 — 属机制项, 迁回。
//! 依赖闭包仅 framework (sync/errno)。
//!
//! services 侧改 `pub use crate::framework::proc::seccomp::*`
//! 保持 API 兼容 (services→framework 合法方向)。

use crate::framework::sync::IrqSpinLock;
use core::sync::atomic::{AtomicU8, Ordering};

use alloc::vec::Vec;

use crate::framework::proc::PROCESS_TABLE;
use crate::framework::proc::Pid;
use crate::framework::proc::do_signal_send;
use crate::framework::proc::process_get_current_pid;
use crate::framework::syscall::Errno;
use crate::framework::syscall::types::{
    SYS_exit, SYS_exit_group, SYS_read, SYS_rt_sigreturn, SYS_write,
};

// ============================================================================
// 常量
// ============================================================================

/// 每进程最大 seccomp filter 数 (Linux 语义上限)
///
/// T2 批 2 (syscall-followup): 原 sys_seccomp 策略入口迁至 services, 本常量
/// 属机制语义 (seccomp_check 与 services 策略共用), 保持 framework 单一权威.
pub const MAX_FILTERS: usize = 4;

// B09-17 (2026-09-14): 白名单数值从 EG_* 私有区 (501+) 归位 Linux 编号 (SYS_*).
// 原 EG_* 值 (502/503/...) 与用户态实际 syscall 编号 (SYS_read=0 等) 不一致 → 白名单永不命中 (bug).
const STRICT_ALLOWED: &[u64] = &[
    SYS_read,         // read
    SYS_write,        // write
    SYS_exit,         // exit
    SYS_exit_group,   // exit_group
    SYS_rt_sigreturn, // rt_sigreturn
];

// ============================================================================
// Seccomp 模式
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SeccompMode {
    Disabled = 0,
    Strict = 1,
    Filter = 2,
}

// ============================================================================
// Seccomp 动作
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeccompAction {
    Allow,
    KillThread,
    KillProcess,
    Trap,
    Errno(u32),
    Log,
}

impl SeccompAction {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_linux(ret: u32) -> Self {
        match ret & 0xFFFF_0000 {
            0x7FFF_0000 => Self::Allow,
            0x0000_0000 => Self::KillThread,
            0x8000_0000 => Self::KillProcess,
            0x0003_0000 => Self::Trap,
            0x0005_0000 => Self::Errno(ret & 0xFFFF),
            0x7FFC_0000 => Self::Log,
            _ => Self::Allow,
        }
    }

    pub fn to_linux(self) -> u32 {
        match self {
            Self::Allow => 0x7FFF_0000,
            Self::KillThread => 0x0000_0000,
            Self::KillProcess => 0x8000_0000,
            Self::Trap => 0x0003_0000,
            Self::Errno(e) => 0x0005_0000 | (e & 0xFFFF),
            Self::Log => 0x7FFC_0000,
        }
    }
}

/// 默认 seccomp 动作 (Linux SECCOMP_RET_ALLOW)
///
/// T2 批 2 (syscall-followup): services 策略入口 (seccomp_syscall) 依赖本常量
/// 构造初始 filter, 与机制检查 seccomp_check 共用同一权威定义.
pub const DEFAULT_ACTION: SeccompAction = SeccompAction::Allow;

// ============================================================================
// 参数比较器
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CmpOp {
    Equal = 0,
    NotEqual = 1,
    GreaterThan = 2,
    GreaterEqual = 3,
    LessThan = 4,
    LessEqual = 5,
    MaskedEqual = 6,
}

#[derive(Debug, Clone, Copy)]
pub struct ArgComparator {
    pub index: u8,
    pub op: CmpOp,
    pub value: u64,
    pub mask: u64,
}

impl ArgComparator {
    pub fn evaluate(&self, arg: u64) -> bool {
        match self.op {
            CmpOp::Equal => arg == self.value,
            CmpOp::NotEqual => arg != self.value,
            CmpOp::GreaterThan => arg > self.value,
            CmpOp::GreaterEqual => arg >= self.value,
            CmpOp::LessThan => arg < self.value,
            CmpOp::LessEqual => arg <= self.value,
            CmpOp::MaskedEqual => (arg & self.mask) == self.value,
        }
    }
}

// ============================================================================
// Seccomp 规则
// ============================================================================

#[derive(Debug, Clone)]
pub struct SeccompRule {
    pub syscall_nr: u64,
    pub arg_comparators: Vec<ArgComparator>,
    pub action: SeccompAction,
}

impl SeccompRule {
    pub fn matches(&self, syscall_nr: u64, args: &[u64; 6]) -> bool {
        if self.syscall_nr != syscall_nr {
            return false;
        }
        self.arg_comparators.iter().all(|cmp| {
            let idx = cmp.index as usize;
            if idx >= 6 {
                return false;
            }
            cmp.evaluate(args[idx])
        })
    }
}

// ============================================================================
// Seccomp 过滤器
// ============================================================================

#[derive(Debug, Clone)]
pub struct SeccompFilter {
    pub rules: Vec<SeccompRule>,
    pub default_action: SeccompAction,
}

impl SeccompFilter {
    pub fn new(rules: Vec<SeccompRule>, default_action: SeccompAction) -> Self {
        Self {
            rules,
            default_action,
        }
    }

    pub fn check(&self, syscall_nr: u64, args: &[u64; 6]) -> SeccompAction {
        for rule in &self.rules {
            if rule.matches(syscall_nr, args) {
                return rule.action;
            }
        }
        self.default_action
    }
}

// ============================================================================
// Per-process Seccomp 状态
// ============================================================================

pub struct SeccompState {
    pub mode: AtomicU8,
    pub filters: IrqSpinLock<Vec<SeccompFilter>>,
    pub no_new_privs: AtomicU8,
}

impl SeccompState {
    pub fn new() -> Self {
        Self {
            mode: AtomicU8::new(SeccompMode::Disabled as u8),
            filters: IrqSpinLock::new(Vec::new()),
            no_new_privs: AtomicU8::new(0),
        }
    }

    pub fn get_mode(&self) -> SeccompMode {
        match self.mode.load(Ordering::Acquire) {
            1 => SeccompMode::Strict,
            2 => SeccompMode::Filter,
            _ => SeccompMode::Disabled,
        }
    }

    pub fn set_no_new_privs(&self) {
        self.no_new_privs.store(1, Ordering::Release);
    }

    pub fn is_no_new_privs(&self) -> bool {
        self.no_new_privs.load(Ordering::Acquire) != 0
    }
}

// ============================================================================
// Seccomp 检查入口
// ============================================================================

#[inline(never)]
#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
pub fn seccomp_check(syscall_nr: u64, args: &[u64; 6]) -> Option<i64> {
    let pid = process_get_current_pid();
    let mode = PROCESS_TABLE
        .with_process(pid, |p| p.seccomp.get_mode())
        .unwrap_or(SeccompMode::Disabled);

    match mode {
        SeccompMode::Disabled => None,
        SeccompMode::Strict => {
            if STRICT_ALLOWED.contains(&syscall_nr) {
                None
            } else {
                let _ = do_signal_send(pid as Pid, 9);
                Some(-(Errno::EPERM as i64))
            }
        }
        SeccompMode::Filter => {
            let action = PROCESS_TABLE
                .with_process(pid, |p| {
                    let filters = p.seccomp.filters.lock();
                    let mut result = DEFAULT_ACTION;
                    for filter in filters.iter() {
                        result = filter.check(syscall_nr, args);
                        if result != SeccompAction::Allow {
                            break;
                        }
                    }
                    result
                })
                .unwrap_or(SeccompAction::Allow);

            match action {
                SeccompAction::Allow => None,
                SeccompAction::Log => None,
                SeccompAction::KillThread => {
                    let _ = do_signal_send(pid as Pid, 31);
                    Some(-(Errno::EPERM as i64))
                }
                SeccompAction::KillProcess => {
                    let _ = do_signal_send(pid as Pid, 31);
                    Some(-(Errno::EPERM as i64))
                }
                SeccompAction::Trap => {
                    let _ = do_signal_send(pid as Pid, 31);
                    Some(-(Errno::EPERM as i64))
                }
                SeccompAction::Errno(e) => Some(-i64::from(e)),
            }
        }
    }
}

// ============================================================================
// syscall 策略入口 (sys_seccomp / sys_prctl_prctl) 已迁至 services
// (services::proc::seccomp::seccomp_syscall / prctl_syscall, T2 批 2,
// syscall-followup) — 参数校验 + 特权判定 + 委托机制状态变更.
// ============================================================================

/// 为指定进程添加 seccomp 过滤规则.
///
/// 若进程当前为禁用状态, 会先切换到 Filter 模式.
///
/// # Errors
///
/// 当 `pid` 对应的进程不存在时返回 `ESRCH`.
pub fn add_rule(pid: u64, rule: SeccompRule) -> Result<(), Errno> {
    PROCESS_TABLE
        .with_process(pid as u32, |p| {
            let mode = p.seccomp.get_mode();
            if mode == SeccompMode::Disabled {
                p.seccomp
                    .mode
                    .store(SeccompMode::Filter as u8, Ordering::Release);
            }
            let mut filters = p.seccomp.filters.lock();
            if filters.is_empty() {
                filters.push(SeccompFilter::new(alloc::vec![rule], DEFAULT_ACTION));
            } else {
                filters[0].rules.push(rule);
            }
            Ok(())
        })
        .unwrap_or(Err(Errno::ESRCH))
}
