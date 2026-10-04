//! 信号决策 trait — 策略-机制分离接口
//!
//! T-06: 信号默认动作判定、不可捕获判定、优先级选择策略由 functions 实现,
//! privileged 仅保留信号发送/投递/栈帧构建等机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型 `SignalDefaultAction`)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackSignalPolicy`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_signal_decision()` 注册自己的策略实现

pub use super::signal::SignalDefaultAction;
use crate::privileged::sync::OnceLock;

/// 信号决策接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait SignalDecision: Send + Sync {
    /// 获取标准信号的默认动作
    ///
    /// `sig` 为信号编号 (1..=31).
    fn default_action(&self, sig: u8) -> SignalDefaultAction;

    /// 信号是否不可捕获/屏蔽 (如 SIGKILL/SIGSTOP)
    fn is_uncatchable(&self, sig: u8) -> bool;

    /// 从可投递信号集中选择下一个信号
    ///
    /// `deliverable` 为 pending & ~blocked 位图.
    /// 返回 `None` 表示无待投递信号.
    fn pick_next_signal(&self, deliverable: u64) -> Option<u8>;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建 POSIX 信号回退策略
///
/// 在 functions 注册策略之前, 信号子系统使用此策略.
/// 遵循 POSIX 标准信号语义.
pub struct FallbackSignalPolicy;

impl SignalDecision for FallbackSignalPolicy {
    fn default_action(&self, sig: u8) -> SignalDefaultAction {
        match sig {
            // 忽略: CHLD(17), URG(23)
            17 | 23 => SignalDefaultAction::Ign,
            // 停止: STOP(19), TSTP(20), TTIN(21), TTOU(22)
            19 | 20 | 21 | 22 => SignalDefaultAction::Stop,
            // 继续: CONT(18)
            18 => SignalDefaultAction::Cont,
            // 核心转储: QUIT(3), ILL(4), ABRT(6), BUS(7), FPE(8), SEGV(11), SYS(31), XCPU(24), XFSZ(25)
            3 | 4 | 6 | 7 | 8 | 11 | 31 | 24 | 25 => SignalDefaultAction::Core,
            // 终止: 其余所有信号
            _ => SignalDefaultAction::Term,
        }
    }

    fn is_uncatchable(&self, sig: u8) -> bool {
        sig == 9 || sig == 19 // SIGKILL, SIGSTOP
    }

    fn pick_next_signal(&self, deliverable: u64) -> Option<u8> {
        if deliverable == 0 {
            return None;
        }
        let sig_bit = deliverable.trailing_zeros() as u8;
        if sig_bit == 0 || sig_bit > 31 {
            return None;
        }
        Some(sig_bit)
    }
}

// ============================================================================
// 全局策略注册表
// ============================================================================

static SIGNAL_DECISION: OnceLock<&'static dyn SignalDecision> = OnceLock::new();

/// 注册信号决策策略
///
/// functions 在 `init()` 中调用, 替换默认回退策略.
/// 仅允许注册一次, 重复注册返回 `Err(旧策略)`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_signal_decision(
    policy: &'static dyn SignalDecision,
) -> Result<(), &'static dyn SignalDecision> {
    SIGNAL_DECISION.set(policy)
}

/// 获取当前信号决策策略
///
/// 若 functions 尚未注册, 返回默认回退策略.
pub fn current_signal_decision() -> &'static dyn SignalDecision {
    SIGNAL_DECISION
        .get()
        .copied()
        .unwrap_or(&FallbackSignalPolicy)
}
