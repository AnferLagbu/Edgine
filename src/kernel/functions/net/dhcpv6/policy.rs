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

/// `DHCPv6` 客户端策略配置 (RFC 8415 §7.6 重传常数 + 续约阈值).
///
/// 重传常数单位毫秒; `T1`/`T2` 阈值以万分比表示 (0-10000), 与
/// [`crate::functions::net::dhcp_policy::DhcpPolicyConfig`] 同口径.
#[derive(Clone, Copy, Debug)]
pub struct Dhcpv6PolicyConfig {
    /// `Solicit` 初始重传间隔 (RFC `SOL_TIMEOUT` = 1s).
    pub sol_timeout_ms: u64,
    /// `Solicit` 重传间隔上限 (RFC 8415 §7.6 `SOL_MAX_RT` = 3600s, RFC 7083 自 120s 更新).
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
    /// `Information-Request` 重传间隔上限 (RFC 8415 §7.6 `INF_MAX_RT` = 3600s, RFC 7083 自 120s 更新).
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
    /// RFC 8415 §7.6 默认重传常数 (`SOL/INF_MAX_RT` = 3600s, RFC 7083) + `T1`=50% / `T2`=87.5%.
    fn default() -> Self {
        Self {
            sol_timeout_ms: 1_000,
            sol_max_rt_ms: 3_600_000,
            req_timeout_ms: 1_000,
            req_max_rt_ms: 30_000,
            ren_timeout_ms: 10_000,
            ren_max_rt_ms: 600_000,
            reb_timeout_ms: 10_000,
            reb_max_rt_ms: 600_000,
            inf_timeout_ms: 1_000,
            inf_max_rt_ms: 3_600_000,
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
// 运行时配置校验
// ============================================================================

/// 重传间隔合法上限 (ms, 24 小时): 防止退避定时器溢出与荒谬配置.
const MAX_TIMEOUT_MS: u64 = 86_400_000;
/// 发现阶段最大重传次数上限 (与 [`retransmit_timeout_ms`] 的移位封顶对齐).
const MAX_RETRIES_CAP: u32 = 64;
/// 续约阈值万分比上限 (100%).
const RATIO_MAX: u32 = 10_000;

/// 运行时策略配置校验错误 (逐字段定位,  syscall 统一映射 `EINVAL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// 基值重传间隔为 0 或超上限 (`field` = sol/req/ren/reb/inf/rel).
    TimeoutOutOfRange {
        /// 出错字段名.
        field: &'static str,
    },
    /// 重传上限小于基值或超上限 (`field` = sol/req/ren/reb/inf).
    MaxRtInvalid {
        /// 出错字段名.
        field: &'static str,
    },
    /// 最大重传次数为 0 或超 64.
    MaxRetriesOutOfRange,
    /// 续约阈值为万分比超 10000 (`field` = t1/t2).
    RatioOutOfRange {
        /// 出错字段名.
        field: &'static str,
    },
    /// `T1` 阈值大于 `T2` (违反续约时序).
    RatioOrder,
}

/// 校验运行时策略配置 (纯函数, 相同输入相同输出, 可离线单元测试).
///
/// 规则: 各基值重传间隔 `(0, 24h]`; 各上限 `>=` 对应基值且 `<=` 24h;
/// `max_retries` 落在 `[1, 64]`; `T1`/`T2` 万分比 `<= 10000` 且 `T1 <= T2`.
/// 默认配置 ([`Dhcpv6PolicyConfig::default`]) 必过校验.
///
/// # Errors
/// 任一字段越界返对应 [`ConfigError`]: 基值/上限间隔越界 (`TimeoutOutOfRange` /
/// `MaxRtInvalid`), 重试次数越界 (`MaxRetriesOutOfRange`), 万分比越界
/// (`RatioOutOfRange`), 或 `T1 > T2` (`RatioOrder`).
pub fn validate_config(cfg: Dhcpv6PolicyConfig) -> Result<(), ConfigError> {
    let pairs = [
        (cfg.sol_timeout_ms, cfg.sol_max_rt_ms, "sol"),
        (cfg.req_timeout_ms, cfg.req_max_rt_ms, "req"),
        (cfg.ren_timeout_ms, cfg.ren_max_rt_ms, "ren"),
        (cfg.reb_timeout_ms, cfg.reb_max_rt_ms, "reb"),
        (cfg.inf_timeout_ms, cfg.inf_max_rt_ms, "inf"),
    ];
    for (base, max_rt, name) in pairs {
        if base == 0 || base > MAX_TIMEOUT_MS {
            return Err(ConfigError::TimeoutOutOfRange { field: name });
        }
        if max_rt < base || max_rt > MAX_TIMEOUT_MS {
            return Err(ConfigError::MaxRtInvalid { field: name });
        }
    }
    if cfg.rel_timeout_ms == 0 || cfg.rel_timeout_ms > MAX_TIMEOUT_MS {
        return Err(ConfigError::TimeoutOutOfRange { field: "rel" });
    }
    if cfg.max_retries == 0 || cfg.max_retries > MAX_RETRIES_CAP {
        return Err(ConfigError::MaxRetriesOutOfRange);
    }
    if cfg.renew_t1_ratio > RATIO_MAX {
        return Err(ConfigError::RatioOutOfRange { field: "t1" });
    }
    if cfg.renew_t2_ratio > RATIO_MAX {
        return Err(ConfigError::RatioOutOfRange { field: "t2" });
    }
    if cfg.renew_t1_ratio > cfg.renew_t2_ratio {
        return Err(ConfigError::RatioOrder);
    }
    Ok(())
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认配置: RFC 8415 §7.6 常数 + `T1`/`T2` 阈值.
    #[test]
    fn test_default_config() {
        let cfg = Dhcpv6PolicyConfig::default();
        assert_eq!(cfg.sol_timeout_ms, 1_000);
        assert_eq!(cfg.sol_max_rt_ms, 3_600_000);
        assert_eq!(cfg.inf_max_rt_ms, 3_600_000);
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

    /// 默认配置必过校验.
    #[test]
    fn test_validate_default_ok() {
        assert_eq!(validate_config(Dhcpv6PolicyConfig::default()), Ok(()));
    }

    /// 基值重传间隔: 0 与超 24h 均拒.
    #[test]
    fn test_validate_timeout_out_of_range() {
        let mut cfg = Dhcpv6PolicyConfig::default();
        cfg.sol_timeout_ms = 0;
        assert_eq!(
            validate_config(cfg),
            Err(ConfigError::TimeoutOutOfRange { field: "sol" })
        );
        cfg = Dhcpv6PolicyConfig::default();
        cfg.rel_timeout_ms = 86_400_001;
        assert_eq!(
            validate_config(cfg),
            Err(ConfigError::TimeoutOutOfRange { field: "rel" })
        );
        // 边界: 恰 24h 合法.
        cfg = Dhcpv6PolicyConfig::default();
        cfg.sol_timeout_ms = 86_400_000;
        cfg.sol_max_rt_ms = 86_400_000;
        assert_eq!(validate_config(cfg), Ok(()));
    }

    /// 重传上限: 小于基值或超 24h 均拒; 等于基值合法.
    #[test]
    fn test_validate_max_rt_invalid() {
        let mut cfg = Dhcpv6PolicyConfig::default();
        cfg.req_max_rt_ms = cfg.req_timeout_ms - 1;
        assert_eq!(
            validate_config(cfg),
            Err(ConfigError::MaxRtInvalid { field: "req" })
        );
        cfg = Dhcpv6PolicyConfig::default();
        cfg.inf_max_rt_ms = 86_400_001;
        assert_eq!(
            validate_config(cfg),
            Err(ConfigError::MaxRtInvalid { field: "inf" })
        );
        cfg = Dhcpv6PolicyConfig::default();
        cfg.ren_max_rt_ms = cfg.ren_timeout_ms;
        assert_eq!(validate_config(cfg), Ok(()));
    }

    /// 最大重传次数: 0 与超 64 拒; 1 与 64 合法.
    #[test]
    fn test_validate_max_retries() {
        let mut cfg = Dhcpv6PolicyConfig::default();
        cfg.max_retries = 0;
        assert_eq!(validate_config(cfg), Err(ConfigError::MaxRetriesOutOfRange));
        cfg.max_retries = 65;
        assert_eq!(validate_config(cfg), Err(ConfigError::MaxRetriesOutOfRange));
        cfg.max_retries = 1;
        assert_eq!(validate_config(cfg), Ok(()));
        cfg.max_retries = 64;
        assert_eq!(validate_config(cfg), Ok(()));
    }

    /// 续约阈值: 超 10000 拒; `T1` > `T2` 拒; 相等合法.
    #[test]
    fn test_validate_ratios() {
        let mut cfg = Dhcpv6PolicyConfig::default();
        cfg.renew_t1_ratio = 10_001;
        assert_eq!(
            validate_config(cfg),
            Err(ConfigError::RatioOutOfRange { field: "t1" })
        );
        cfg = Dhcpv6PolicyConfig::default();
        cfg.renew_t2_ratio = 10_001;
        assert_eq!(
            validate_config(cfg),
            Err(ConfigError::RatioOutOfRange { field: "t2" })
        );
        cfg = Dhcpv6PolicyConfig::default();
        cfg.renew_t1_ratio = 9_000;
        cfg.renew_t2_ratio = 8_000;
        assert_eq!(validate_config(cfg), Err(ConfigError::RatioOrder));
        cfg = Dhcpv6PolicyConfig::default();
        cfg.renew_t1_ratio = 8_750;
        cfg.renew_t2_ratio = 8_750;
        assert_eq!(validate_config(cfg), Ok(()));
    }
}
