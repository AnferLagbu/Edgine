#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! Seccomp — 系统调用过滤 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 策略代码 (过滤器 + 规则匹配 + syscall) 于 T1-5 (2026-06-16) 迁至
//! `functions::proc::seccomp`, 本文件仅 re-export。按统一判据反转：
//! SeccompState 挂在 Process 结构体 (privileged 进程机制状态), seccomp_check
//! 被 privileged syscall 分发路径消费 — 属机制项, 迁回。
//! 依赖闭包仅 privileged (sync/errno)。
//!
//! functions 侧改 `pub use crate::privileged::proc::seccomp::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。

#![deny(missing_docs)]

use crate::privileged::sync::IrqSpinLock;
use core::sync::atomic::{AtomicU8, Ordering};

use alloc::vec::Vec;

use crate::privileged::proc::PROCESS_TABLE;
use crate::privileged::proc::Pid;
use crate::privileged::proc::do_signal_send;
use crate::privileged::proc::process_get_current_pid;
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::types::{
    SYS_exit, SYS_exit_group, SYS_read, SYS_rt_sigreturn, SYS_write,
};

// ============================================================================
// 常量
// ============================================================================

/// 每进程最大 seccomp filter 数 (Linux 语义上限)
///
/// T2 批 2 (syscall-followup): 原 sys_seccomp 策略入口迁至 functions, 本常量
/// 属机制语义 (seccomp_check 与 functions 策略共用), 保持 privileged 单一权威.
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

/// seccomp 运行模式 — 禁用 / 严格白名单 / BPF 过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SeccompMode {
    /// 未启用 seccomp 过滤。
    Disabled = 0,
    /// 严格白名单模式: 仅放行白名单内 syscall。
    Strict = 1,
    /// 过滤器模式: 按规则匹配决定动作。
    Filter = 2,
}

// ============================================================================
// Seccomp 动作
// ============================================================================

/// seccomp 命中规则时的动作 (与 Linux SECCOMP_RET_* 语义对应)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeccompAction {
    /// 放行 syscall。
    Allow,
    /// 杀死当前线程。
    KillThread,
    /// 杀死整个进程。
    KillProcess,
    /// 陷入调试器 (SIGSYS)。
    Trap,
    /// 以指定 errno 返回失败。
    Errno(u32),
    /// 记录日志后放行。
    Log,
}

impl SeccompAction {
    /// 由 Linux `SECCOMP_RET_*` 编码解析动作。
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

    /// 编码为 Linux `SECCOMP_RET_*` 32 位返回值。
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
/// T2 批 2 (syscall-followup): functions 策略入口 (seccomp_syscall) 依赖本常量
/// 构造初始 filter, 与机制检查 seccomp_check 共用同一权威定义.
pub const DEFAULT_ACTION: SeccompAction = SeccompAction::Allow;

// ============================================================================
// 参数比较器
// ============================================================================

/// 规则中参数比较操作符 (与 Linux seccomp 比较类型对应)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CmpOp {
    /// 参数等于比较值。
    Equal = 0,
    /// 参数不等于比较值。
    NotEqual = 1,
    /// 参数大于比较值。
    GreaterThan = 2,
    /// 参数大于等于比较值。
    GreaterEqual = 3,
    /// 参数小于比较值。
    LessThan = 4,
    /// 参数小于等于比较值。
    LessEqual = 5,
    /// 掩码后等于比较值。
    MaskedEqual = 6,
}

/// 单条 syscall 参数比较器 — 指定参数下标、比较操作符、比较值与掩码。
#[derive(Debug, Clone, Copy)]
pub struct ArgComparator {
    /// 参数下标 (0..6)。
    pub index: u8,
    /// 比较操作符。
    pub op: CmpOp,
    /// 比较值。
    pub value: u64,
    /// 掩码 (仅 `MaskedEqual` 使用)。
    pub mask: u64,
}

impl ArgComparator {
    /// 按本比较器的操作符判定参数 `arg` 是否满足条件。
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

/// 一条 seccomp 规则 — 匹配某个 syscall 及其参数条件, 命中后执行指定动作。
#[derive(Debug, Clone)]
pub struct SeccompRule {
    /// 规则匹配的 syscall 编号。
    pub syscall_nr: u64,
    /// 参数比较器列表, 全部满足才算命中。
    pub arg_comparators: Vec<ArgComparator>,
    /// 命中该规则时执行的动作。
    pub action: SeccompAction,
}

impl SeccompRule {
    /// 判定本规则是否命中给定 syscall 及其参数。
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

/// seccomp 过滤器 — 一组规则加默认动作, 按序匹配首个命中的规则。
#[derive(Debug, Clone)]
pub struct SeccompFilter {
    /// 过滤器包含的规则 (按序匹配)。
    pub rules: Vec<SeccompRule>,
    /// 无规则命中时采用的默认动作。
    pub default_action: SeccompAction,
}

impl SeccompFilter {
    /// 由规则列表与默认动作构造过滤器。
    pub fn new(rules: Vec<SeccompRule>, default_action: SeccompAction) -> Self {
        Self {
            rules,
            default_action,
        }
    }

    /// 依次匹配规则, 返回首个命中规则的动作, 否则返回默认动作。
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

/// 每个进程的 seccomp 状态 — 当前模式、过滤器列表与 no_new_privs 标志。
pub struct SeccompState {
    /// 当前模式 (`SeccompMode` 的 u8 编码, 原子读写)。
    pub mode: AtomicU8,
    /// 已安装的过滤器列表 (中断安全自旋锁保护)。
    pub filters: IrqSpinLock<Vec<SeccompFilter>>,
    /// `no_new_privs` 标志 (原子读写)。
    pub no_new_privs: AtomicU8,
}

impl SeccompState {
    /// 构造默认 (禁用模式, 空过滤器) 的 seccomp 状态。
    pub fn new() -> Self {
        Self {
            mode: AtomicU8::new(SeccompMode::Disabled as u8),
            filters: IrqSpinLock::new(Vec::new()),
            no_new_privs: AtomicU8::new(0),
        }
    }

    /// 读取当前 seccomp 模式。
    pub fn get_mode(&self) -> SeccompMode {
        match self.mode.load(Ordering::Acquire) {
            1 => SeccompMode::Strict,
            2 => SeccompMode::Filter,
            _ => SeccompMode::Disabled,
        }
    }

    /// 置位 `no_new_privs` 标志。
    pub fn set_no_new_privs(&self) {
        self.no_new_privs.store(1, Ordering::Release);
    }

    /// 查询 `no_new_privs` 标志是否已置位。
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
/// seccomp 检查入口 — 按当前进程模式判定 `syscall_nr`, 返回拦截时的负错误码, 放行返回 `None`。
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
// syscall 策略入口 (sys_seccomp / sys_prctl_prctl) 已迁至 functions
// (functions::proc::seccomp::seccomp_syscall / prctl_syscall, T2 批 2,
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
