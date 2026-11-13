#![deny(unsafe_code)]
#![deny(missing_docs)]
//! `DHCPv6` 重传 / 续约策略 (RFC 8415 §15, 纯函数).
//!
//! 与 [`crate::functions::net::dhcp_policy`] (`DHCPv4` 策略) 同构: 策略层只决定
//! **应该做什么** (继续 / 续约 / rebind / 重启 / 放弃) 与**重传退避时长**, 不碰
//! smoltcp 句柄, 不持状态. 状态机 (`super::state`) 按 [`Dhcpv6Action`] 推进,
//! 按 [`retransmit_timeout_ms`] 设置重传定时器.
//!
//! 全部 0 unsafe, 纯函数 (相同输入相同输出, 可离线单元测试).

use crate::functions::intcast::low_u64_from_u128;
use crate::privileged::net::iface_trait::Dhcpv6State;

// ============================================================================
// 策略决策
// ============================================================================

/// `DHCPv6` 策略的"行动"建议.
///
/// 状态机按 Action 推进; 策略层只决定**下一步**, 不直接操作句柄.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dhcpv6Action {
    /// 维持当前态 (等待应答 / 未到定时器).
    Continue,
    /// `T1` 到: 向原服务器单播 `Renew`.
    Renew,
    /// `T2` 到且无 Reply: 向任意服务器组播 `Rebind`.
    Rebind,
    /// 租约到期或重传耗尽: 释放地址并重启 SARR.
    Restart,
    /// 不可恢复错误: 转 `Failed`.
    GiveUp,
}

/// `DHCPv6` 客户端策略配置 (RFC 8415 §15 重传常数 + 续约阈值).
///
/// 重传常数单位毫秒; `T1`/`T2` 阈值以万分比表示 (0-10000), 与
/// [`crate::functions::net::dhcp_policy::DhcpPolicyConfig`] 同口径.
#[derive(Clone, Copy, Debug)]
pub struct Dhcpv6PolicyConfig {
    /// `Solicit` 初始重传间隔 (RFC `SOL_TIMEOUT` = 1s).
    pub sol_timeout_ms: u64,
    /// `Solicit` 重传间隔上限 (RFC `SOL_MAX_RT` = 120s).
    pub sol_max_rt_ms: u64,
    /// `Request` 初始重传间隔 (RFC `REQ_TIMEOUT` = 1s).
    pub req_timeout_ms: u64,
    /// `Request` 重传间隔上限 (RFC `REQ_MAX_RT` = 30s).
    pub req_max_rt_ms: u64,
    /// `Renew` 初始重传间隔 (RFC `REN_TIMEOUT` = 10s).
    pub ren_timeout_ms: u64,
    /// `Renew` 重传间隔上限 (RFC `REN_MAX_RT` = 600s).
    pub ren_max_rt_ms: u64,
    /// `Rebind` 初始重传间隔 (RFC `REB_TIMEOUT` = 10s).
    pub reb_timeout_ms: u64,
    /// `Rebind` 重传间隔上限 (RFC `REB_MAX_RT` = 600s).
    pub reb_max_rt_ms: u64,
    /// `Information-Request` 初始重传间隔 (RFC `INF_TIMEOUT` = 1s).
    pub inf_timeout_ms: u64,
    /// `Information-Request` 重传间隔上限 (RFC `INF_MAX_RT` = 120s).
    pub inf_max_rt_ms: u64,
    /// `Release` 初始重传间隔 (RFC `REL_TIMEOUT` = 1s).
    pub rel_timeout_ms: u64,
    /// 发现阶段 (`Solicit`/`Request`/`Information-Request`) 最大重传次数.
    ///
    /// RFC 8415 对 `Solicit` 无固定次数上限 (退避至 `MAX_RT` 持续重传);
    /// 内核客户端须有界以免永久挂起, 耗尽后转 [`Dhcpv6Action::Restart`].
    pub max_retries: u32,
    /// 续约 `T1` 阈值 (有效租期的万分比, 默认 5000 即 50%).
    pub renew_t1_ratio: u32,
    /// 续约 `T2` 阈值 (有效租期的万分比, 默认 8750 即 87.5%).
    pub renew_t2_ratio: u32,
}

impl Default for Dhcpv6PolicyConfig {
    /// RFC 8415 §15 默认重传常数 + `T1`=50% / `T2`=87.5%.
    fn default() -> Self {
        Self {
            sol_timeout_ms: 1_000,
            sol_max_rt_ms: 120_000,
            req_timeout_ms: 1_000,
            req_max_rt_ms: 30_000,
            ren_timeout_ms: 10_000,
            ren_max_rt_ms: 600_000,
            reb_timeout_ms: 10_000,
            reb_max_rt_ms: 600_000,
            inf_timeout_ms: 1_000,
            inf_max_rt_ms: 120_000,
            rel_timeout_ms: 1_000,
            max_retries: 8,
            renew_t1_ratio: 5_000,
            renew_t2_ratio: 8_750,
        }
    }
}

// ============================================================================
// 纯函数决策
// ============================================================================

/// 给定当前状态 + 上下文, 返回下一步行动 (仿 `dhcp_policy::decide`).
///
/// # 参数
///
/// - `state`: 当前 `DHCPv6` 状态 (观察类型).
/// - `retry_count`: 发现阶段已重传次数 (0 = 首次).
/// - `elapsed_ms`: 自 Bound 以来毫秒数 (0 = 未绑定).
/// - `lease_ms`: 有效租期总长 (ms, 0 = 未知 / 永不续约).
/// - `cfg`: 策略配置 (重传上限 + `T1`/`T2` 阈值).
///
/// # 行为
///
/// - `Idle` / `Releasing` / `InfoBound`: 维持.
/// - 发现态 (`Soliciting`/`Selecting`/`Requesting`/`InfoRequesting`):
///   重传未耗尽则维持, 耗尽则重启.
/// - `Bound`: 租约到期重启; 否则按 `T1`/`T2` 决定续约 / rebind.
/// - `Renewing`/`Rebinding`: 租约到期重启, 否则维持 (等 Reply).
/// - `Failed`: 放弃.
#[must_use]
pub fn decide(
    state: Dhcpv6State,
    retry_count: u32,
    elapsed_ms: u64,
    lease_ms: u64,
    cfg: Dhcpv6PolicyConfig,
) -> Dhcpv6Action {
    match state {
        Dhcpv6State::Idle | Dhcpv6State::Releasing | Dhcpv6State::InfoBound => {
            Dhcpv6Action::Continue
        }
        Dhcpv6State::Soliciting
        | Dhcpv6State::Selecting
        | Dhcpv6State::Requesting
        | Dhcpv6State::InfoRequesting => {
            if retry_count < cfg.max_retries {
                Dhcpv6Action::Continue
            } else {
                Dhcpv6Action::Restart
            }
        }
        Dhcpv6State::Bound { .. } => {
            if lease_ms == 0 {
                return Dhcpv6Action::Continue;
            }
            if elapsed_ms >= lease_ms {
                return Dhcpv6Action::Restart;
            }
            let t1 = t1_ms(lease_ms, cfg.renew_t1_ratio);
            let t2 = t2_ms(lease_ms, cfg.renew_t2_ratio);
            if elapsed_ms < t1 {
                Dhcpv6Action::Continue
            } else if elapsed_ms < t2 {
                Dhcpv6Action::Renew
            } else {
                Dhcpv6Action::Rebind
            }
        }
        Dhcpv6State::Renewing { .. } | Dhcpv6State::Rebinding { .. } => {
            if lease_ms > 0 && elapsed_ms >= lease_ms {
                Dhcpv6Action::Restart
            } else {
                Dhcpv6Action::Continue
            }
        }
        Dhcpv6State::Failed => Dhcpv6Action::GiveUp,
    }
}

/// RFC 8415 §15 指数退避: `RT = min(base * 2^retry, max_rt)`.
///
/// 确定性计算 (不含抖动); 抖动由 [`apply_jitter`] 另行叠加, 保持本函数纯且可测.
/// `retry` 超过 63 时直接取上限 (避免移位溢出).
#[inline]
#[must_use]
pub fn retransmit_timeout_ms(base_ms: u64, max_rt_ms: u64, retry_count: u32) -> u64 {
    if retry_count >= 64 {
        return max_rt_ms;
    }
    let factor = 1u64 << retry_count;
    base_ms.saturating_mul(factor).min(max_rt_ms)
}

/// RFC 8415 §15 随机抖动: 在 `rt` 上叠加 ±10% (由 `seed` 确定性派生).
///
/// `seed` 通常取事务标识或重传计数, 使离线测试可复现. 返回值落在
/// `[rt * 9 / 10, rt * 11 / 10]` 区间 (整数域, 不引入浮点).
#[inline]
#[must_use]
pub fn apply_jitter(rt_ms: u64, seed: u32) -> u64 {
    // 由 seed 派生 [-100, 100] 千分比偏移 (确定性, 无 RNG 依赖).
    let span = 201u64;
    let offset = u64::from(seed % 201);
    let permille = 900u64 + offset * 200 / span;
    low_u64_from_u128(u128::from(rt_ms) * u128::from(permille) / 1_000)
}

/// `T1` 时刻 (ms): 有效租期 × 万分比阈值 (默认 50%).
#[inline]
#[must_use]
pub fn t1_ms(lease_ms: u64, ratio: u32) -> u64 {
    low_u64_from_u128(u128::from(lease_ms) * u128::from(ratio) / 10_000)
}

/// `T2` 时刻 (ms): 有效租期 × 万分比阈值 (默认 87.5%).
#[inline]
#[must_use]
pub fn t2_ms(lease_ms: u64, ratio: u32) -> u64 {
    low_u64_from_u128(u128::from(lease_ms) * u128::from(ratio) / 10_000)
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认配置: RFC 8415 §15 常数 + `T1`/`T2` 阈值.
    #[test]
    fn test_default_config() {
        let cfg = Dhcpv6PolicyConfig::default();
        assert_eq!(cfg.sol_timeout_ms, 1_000);
        assert_eq!(cfg.sol_max_rt_ms, 120_000);
        assert_eq!(cfg.req_max_rt_ms, 30_000);
        assert_eq!(cfg.ren_timeout_ms, 10_000);
        assert_eq!(cfg.max_retries, 8);
        assert_eq!(cfg.renew_t1_ratio, 5_000);
        assert_eq!(cfg.renew_t2_ratio, 8_750);
    }

    /// `Idle`/`Releasing`/`InfoBound`: 维持.
    #[test]
    fn test_idle_releasing_infobound_continue() {
        let cfg = Dhcpv6PolicyConfig::default();
        for state in [
            Dhcpv6State::Idle,
            Dhcpv6State::Releasing,
            Dhcpv6State::InfoBound,
        ] {
            assert_eq!(decide(state, 0, 0, 0, cfg), Dhcpv6Action::Continue);
        }
    }

    /// 发现态: 重传未耗尽维持, 耗尽重启.
    #[test]
    fn test_discovery_retry_budget() {
        let cfg = Dhcpv6PolicyConfig::default();
        let state = Dhcpv6State::Soliciting;
        assert_eq!(decide(state, 7, 0, 0, cfg), Dhcpv6Action::Continue);
        assert_eq!(decide(state, 8, 0, 0, cfg), Dhcpv6Action::Restart);
        assert_eq!(
            decide(Dhcpv6State::InfoRequesting, 8, 0, 0, cfg),
            Dhcpv6Action::Restart
        );
    }

    /// `Bound`: `T1` 前维持, `T1`..`T2` 续约, `T2` 后 rebind.
    #[test]
    fn test_bound_t1_t2_thresholds() {
        let cfg = Dhcpv6PolicyConfig::default();
        let state = Dhcpv6State::Bound {
            ipv6: [0u8; 16],
            lease_expires_at: 0,
        };
        // 租期 1000s: T1=500s, T2=875s.
        let lease = 1_000_000;
        assert_eq!(
            decide(state, 0, 100_000, lease, cfg),
            Dhcpv6Action::Continue
        );
        assert_eq!(decide(state, 0, 600_000, lease, cfg), Dhcpv6Action::Renew);
        assert_eq!(decide(state, 0, 900_000, lease, cfg), Dhcpv6Action::Rebind);
    }

    /// `Bound`: 租约到期重启; 租期 0 永不续约.
    #[test]
    fn test_bound_lease_expiry_and_zero() {
        let cfg = Dhcpv6PolicyConfig::default();
        let state = Dhcpv6State::Bound {
            ipv6: [0u8; 16],
            lease_expires_at: 0,
        };
        assert_eq!(
            decide(state, 0, 1_000_000, 1_000_000, cfg),
            Dhcpv6Action::Restart
        );
        assert_eq!(decide(state, 0, 9_999_999, 0, cfg), Dhcpv6Action::Continue);
    }

    /// `Renewing`/`Rebinding`: 租约到期重启, 否则维持.
    #[test]
    fn test_renewing_rebinding_lease() {
        let cfg = Dhcpv6PolicyConfig::default();
        let renewing = Dhcpv6State::Renewing { ipv6: [0u8; 16] };
        let rebinding = Dhcpv6State::Rebinding { ipv6: [0u8; 16] };
        assert_eq!(
            decide(renewing, 0, 500_000, 1_000_000, cfg),
            Dhcpv6Action::Continue
        );
        assert_eq!(
            decide(renewing, 0, 1_000_000, 1_000_000, cfg),
            Dhcpv6Action::Restart
        );
        assert_eq!(
            decide(rebinding, 0, 1_000_000, 1_000_000, cfg),
            Dhcpv6Action::Restart
        );
    }

    /// `Failed`: 放弃.
    #[test]
    fn test_failed_giveup() {
        let cfg = Dhcpv6PolicyConfig::default();
        assert_eq!(
            decide(Dhcpv6State::Failed, 0, 0, 0, cfg),
            Dhcpv6Action::GiveUp
        );
    }

    /// 指数退避: 倍增 + 上限封顶 + 大 retry 不溢出.
    #[test]
    fn test_retransmit_backoff() {
        assert_eq!(retransmit_timeout_ms(1_000, 120_000, 0), 1_000);
        assert_eq!(retransmit_timeout_ms(1_000, 120_000, 1), 2_000);
        assert_eq!(retransmit_timeout_ms(1_000, 120_000, 3), 8_000);
        assert_eq!(retransmit_timeout_ms(1_000, 120_000, 7), 120_000);
        assert_eq!(retransmit_timeout_ms(1_000, 120_000, 100), 120_000);
        // 上限小于退避值时取上限.
        assert_eq!(retransmit_timeout_ms(10_000, 30_000, 5), 30_000);
    }

    /// 抖动: 落在 ±10% 区间, 确定性可复现.
    #[test]
    fn test_jitter_bounds_and_determinism() {
        let rt = 10_000;
        for seed in 0..201u32 {
            let j = apply_jitter(rt, seed);
            assert!((9_000..=11_000).contains(&j), "seed {seed} -> {j}");
        }
        assert_eq!(apply_jitter(rt, 42), apply_jitter(rt, 42));
    }

    /// `T1`/`T2` 万分比计算.
    #[test]
    fn test_t1_t2_ratios() {
        assert_eq!(t1_ms(1_000_000, 5_000), 500_000);
        assert_eq!(t2_ms(1_000_000, 8_750), 875_000);
        assert_eq!(t1_ms(0, 5_000), 0);
    }

    /// `Dhcpv6Action` 等值.
    #[test]
    fn test_action_eq() {
        assert_eq!(Dhcpv6Action::Continue, Dhcpv6Action::Continue);
        assert_ne!(Dhcpv6Action::Renew, Dhcpv6Action::Rebind);
        assert_ne!(Dhcpv6Action::Restart, Dhcpv6Action::GiveUp);
    }
}
