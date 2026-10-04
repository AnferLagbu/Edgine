//! FREG降级策略 trait — 策略-机制分离接口
//!
//! 本模块落实「framekernel 范式落实」工程 §6.3: `freg/domain.rs`
//! 的 `apply_degradation` 降级矩阵属于策略, 应由 functions 实现,
//! privileged 仅保留 `RecoveryDomain` 状态机机制。
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackFREGDegradePolicy`), 与
//!   历史降级矩阵一致, 保证未注册时行为不变
//! - functions 在 `init()` 中通过 `register_freg_degrade_policy()` 注册
//!
//! ## 调用上下文
//!
//! 降级决策在回滚路径中调用 (可能处于 panic/中断上下文): 本 trait 方法
//! 必须是纯决策逻辑 (无阻塞 / 无分配), 只返回降级结果, 状态写入由
//! privileged 执行。

use super::types::{CAP_FS_WRITE, CAP_NET_SEND, CAP_PROC_CREATE};

/// 降级决策结果 — privileged 依据此结果写入域状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DegradeDecision {
    /// 降级后的能力掩码 (写入 `dom_cap_mask`)
    pub cap_mask: u64,
    /// 是否将域置为 `DomainState::Degraded`
    pub mark_degraded: bool,
    /// 是否将域置为 `DomainState::Quarantined`
    pub mark_quarantined: bool,
}

/// FREG降级策略接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait FREGDegradePolicy: Send + Sync {
    /// 依据连续失败次数计算降级决策
    ///
    /// # 参数
    /// - `failures`: 连续失败次数 (`consecutive_failures`)
    /// - `original_mask`: 原始能力掩码 (`original_cap_mask`)
    fn degrade(&self, failures: u32, original_mask: u64) -> DegradeDecision;
}

// ============================================================================
// 默认回退策略 (functions 尚未注册时使用, 与历史降级矩阵一致)
// ============================================================================

/// 框架内建回退策略 — 与 `domain.rs` 历史降级矩阵一致
///
/// 在 functions 注册策略之前, 降级路径使用此策略.
pub struct FallbackFREGDegradePolicy;

impl FREGDegradePolicy for FallbackFREGDegradePolicy {
    fn degrade(&self, failures: u32, original_mask: u64) -> DegradeDecision {
        match failures {
            // 1-2 次失败: 恢复原始能力, 不降级
            1..=2 => DegradeDecision {
                cap_mask: original_mask,
                mark_degraded: false,
                mark_quarantined: false,
            },
            // 3 次失败: 剥夺文件系统写能力, 置为降级态
            3 => DegradeDecision {
                cap_mask: original_mask & !CAP_FS_WRITE,
                mark_degraded: true,
                mark_quarantined: false,
            },
            // 4 次失败: 剥夺文件系统写/网络发送/进程创建能力, 置为降级态
            4 => DegradeDecision {
                cap_mask: original_mask & !(CAP_FS_WRITE | CAP_NET_SEND | CAP_PROC_CREATE),
                mark_degraded: true,
                mark_quarantined: false,
            },
            // 5 次及以上: 隔离 (不再回滚)
            _ => DegradeDecision {
                cap_mask: original_mask,
                mark_degraded: false,
                mark_quarantined: true,
            },
        }
    }
}

static FALLBACK_FREG_DEGRADE_POLICY: FallbackFREGDegradePolicy = FallbackFREGDegradePolicy;

/// 全局策略注册表 — functions 通过 `register_freg_degrade_policy` 注册
static FREG_DEGRADE_POLICY: crate::privileged::sync::OnceLock<&'static dyn FREGDegradePolicy> =
    crate::privileged::sync::OnceLock::new();

/// 注册FREG降级策略 (由 `functions::freg::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_freg_degrade_policy(
    policy: &'static dyn FREGDegradePolicy,
) -> Result<(), &'static dyn FREGDegradePolicy> {
    match FREG_DEGRADE_POLICY.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的FREG降级策略 (未注册时返回内建回退)
#[inline]
pub fn current_freg_degrade_policy() -> &'static dyn FREGDegradePolicy {
    match FREG_DEGRADE_POLICY.get() {
        Some(&p) => p,
        None => &FALLBACK_FREG_DEGRADE_POLICY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_degradation_matrix() {
        let p = FallbackFREGDegradePolicy;
        let original = u64::MAX;

        // 1-2 次失败: 恢复原始能力
        for failures in 1..=2 {
            let d = p.degrade(failures, original);
            assert_eq!(d.cap_mask, original);
            assert!(!d.mark_degraded);
            assert!(!d.mark_quarantined);
        }

        // 3 次失败: 剥夺 FS_WRITE
        let d = p.degrade(3, original);
        assert_eq!(d.cap_mask, original & !CAP_FS_WRITE);
        assert!(d.mark_degraded);
        assert!(!d.mark_quarantined);

        // 4 次失败: 剥夺 FS_WRITE | NET_SEND | PROC_CREATE
        let d = p.degrade(4, original);
        assert_eq!(
            d.cap_mask,
            original & !(CAP_FS_WRITE | CAP_NET_SEND | CAP_PROC_CREATE)
        );
        assert!(d.mark_degraded);
        assert!(!d.mark_quarantined);

        // 5 次及以上: 隔离
        for failures in 5..=8 {
            let d = p.degrade(failures, original);
            assert!(d.mark_quarantined);
        }
    }

    #[test]
    fn test_current_policy_falls_back() {
        // 未注册时返回内建回退, 不 panic
        let p = current_freg_degrade_policy();
        let d = p.degrade(3, u64::MAX);
        assert!(d.mark_degraded);
    }
}
