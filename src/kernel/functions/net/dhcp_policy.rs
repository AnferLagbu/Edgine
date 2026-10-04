#![deny(unsafe_code)]
//! DHCP 策略 trait 抽象 (W6 阶段实装)
//!
//! ## 定位
//!
//! 把 DHCP 客户端的**策略** (何时重试, 何时 fallback 到静态 IP, 何时认为
//! 失败) 从 smoltcp 协议栈的**机制**中解耦. 协议栈负责:
//! - 状态机: `DhcpState { Idle, Discovering, Requesting, Bound, Renewing, Failed }`
//! - 数据包收发: 与 smoltcp `dhcpv4::Socket` 交互
//! - 句柄分配: 复用 NetStack 句柄池
//!
//! 策略层负责:
//! - 重试策略: 多少次重试后才转 `Failed`
//! - Fallback 策略: 失败后是否回退到 `NetConfig::static_ipv4`
//! - 续约策略: 提前多久续约 (T1/T2 阈值)
//!
//! ## 与 Framekernel 规则一致
//!
//! - 0 unsafe (functions 层铁律)
//! - 不依赖 smoltcp 任何具体类型 (类型擦除已由 `DhcpState` 完成)
//! - 调用方只通过 trait 调用, 实现方可热替换 (e.g. 测试时用 `MockPolicy`)
//!
//! ## 默认实现
//!
//! `DefaultDhcpPolicy` 提供工业界标准策略:
//! - 重试 4 次后转 Failed
//! - 失败后回退到静态 IP
//! - 续约阈值 T1=50%, T2=87.5% (RFC 2131 §4.4.5)
//!
//! 单元测试验证: 状态转移、fallback 决策、续约时机.
//!
//! ## 子任务归属
//!
//! REVAL-W 第 6 组 (W6), 2026-06-25 实装.

use crate::privileged::net::iface_trait::{DhcpState, Ipv4Addr, NetConfig};

// ============================================================================
// DHCP 策略决策
// ============================================================================

/// DHCP 策略的"行动"建议.
///
/// 协议栈 (smoltcp) 按 Action 推进状态机; 策略层只决定**应该做什么**,
/// 不直接操作 smoltcp 句柄.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DhcpAction {
    /// 继续当前 Discover/Request 流程
    Continue,
    /// 启动续约 (RENEWING)
    Renew,
    /// 已失败, 走 fallback (回退到静态 IP 或停机)
    FallbackToStatic(Ipv4Addr),
    /// 错误无法恢复
    GiveUp,
}

/// DHCP 客户端配置 (策略可调字段).
///
/// 复制自 `NetConfig` 中与 DHCP 相关的字段, 避免策略层依赖整个 `NetConfig`.
#[derive(Clone, Copy, Debug)]
pub struct DhcpPolicyConfig {
    /// Discover/Request 最大重试次数 (默认 4)
    pub max_retries: u32,
    /// 续约 T1 阈值 (租期的 0-1 之间, 默认 0.5 即 50%)
    pub renew_t1_ratio: u32, // 以万分比表示, 0-10000
    /// 续约 T2 阈值 (租期的 0-1 之间, 默认 0.875 即 87.5%)
    pub renew_t2_ratio: u32,
    /// 失败后是否回退到静态 IP
    pub fallback_to_static: bool,
}

impl Default for DhcpPolicyConfig {
    /// 工业界默认: 4 次重试 + RFC 2131 T1/T2 阈值.
    fn default() -> Self {
        Self {
            max_retries: 4,
            renew_t1_ratio: 5000, // 50.00%
            renew_t2_ratio: 8750, // 87.50%
            fallback_to_static: true,
        }
    }
}

// ============================================================================
// DHCP 策略 trait (W6 核心抽象)
// ============================================================================

/// DHCP 策略 — 决定状态机如何推进.
///
/// ## 实现方契约
///
/// 0 unsafe; 纯函数 (无副作用); 相同输入产生相同输出 (可单元测试).
///
/// ## 调用方契约
///
/// 由 `SmoltcpNetStack::poll()` 调用, 传入当前状态 + 已重试次数 +
/// 租期参数, 返回 Action. 协议栈按 Action 推进, 不再保留策略逻辑.
///
/// ## 与 `NetStack::dhcp_state` 的区别
///
/// - `dhcp_state`: 报告**当前**状态 (观察)
/// - `dhcp_policy::decide`: 决定**下一步**动作 (策略)
pub trait DhcpPolicy {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 给定当前状态 + 上下文, 返回下一步 Action.
    ///
    /// ## 参数
    ///
    /// - `state`: 当前 DHCP 状态 (来自 `NetStack::dhcp_state()`)
    /// - `cfg`: 完整网络配置 (含静态 IP, fallback 备用)
    /// - `policy_cfg`: 策略可调字段 (重试次数, 续约阈值)
    /// - `retry_count`: 当前 Discover/Request 已重试次数 (0 = 第一次)
    /// - `elapsed_ms`: 自 Bound 以来的毫秒数 (0 = 未绑定)
    /// - `lease_duration_ms`: 租期总长 (ms)
    fn decide(
        &self,
        state: &DhcpState,
        cfg: &NetConfig,
        policy_cfg: &DhcpPolicyConfig,
        retry_count: u32,
        elapsed_ms: u64,
        lease_duration_ms: u64,
    ) -> DhcpAction;
}

// ============================================================================
// 默认策略实现: RFC 2131 兼容
// ============================================================================

/// RFC 2131 §4.4.5 标准策略.
///
/// 行为:
/// - Idle: 启动 Discover (返回 Continue, 协议栈进入 Discovering)
/// - Discovering / Requesting 状态:
///   - `retry_count` < `max_retries`: 继续 (Continue)
///   - `retry_count` >= `max_retries`: 走 fallback (按 `fallback_to_static` 决定)
/// - Bound { ipv4, `lease_expires_at` } 状态:
///   - elapsed < lease * t1: 继续 (无需续约)
///   - elapsed < lease * t2: 触发续约 (Renew)
///   - elapsed >= lease * t2: 给 0 容忍, 立刻触发 Rebind
/// - Renewing: 续约中, 返回 Continue (等待 ACK)
/// - Failed: 走 fallback
pub struct DefaultDhcpPolicy;

impl DhcpPolicy for DefaultDhcpPolicy {
    fn decide(
        &self,
        state: &DhcpState,
        cfg: &NetConfig,
        policy_cfg: &DhcpPolicyConfig,
        retry_count: u32,
        elapsed_ms: u64,
        lease_duration_ms: u64,
    ) -> DhcpAction {
        match state {
            DhcpState::Idle => {
                // 协议栈刚启动, 应当发起 Discover
                DhcpAction::Continue
            }
            DhcpState::Discovering | DhcpState::Requesting => {
                if retry_count < policy_cfg.max_retries {
                    DhcpAction::Continue
                } else if policy_cfg.fallback_to_static {
                    cfg.static_ipv4.map_or(DhcpAction::GiveUp, |octets| {
                        DhcpAction::FallbackToStatic(Ipv4Addr::from_octets(octets))
                    })
                } else {
                    DhcpAction::GiveUp
                }
            }
            DhcpState::Bound { .. } => {
                // 续约时机: T1 (50%) → Unicast Renew, T2 (87.5%) → Server Rebind
                if lease_duration_ms == 0 {
                    // 0 表示租期未知, 永不续约
                    return DhcpAction::Continue;
                }
                let t1_ms = (u128::from(lease_duration_ms) * u128::from(policy_cfg.renew_t1_ratio)
                    / 10_000) as u64;
                let t2_ms = (u128::from(lease_duration_ms) * u128::from(policy_cfg.renew_t2_ratio)
                    / 10_000) as u64;
                if elapsed_ms < t1_ms {
                    DhcpAction::Continue
                } else if elapsed_ms < t2_ms {
                    DhcpAction::Renew
                } else {
                    // T2 已过, 协议栈应转 Rebinding, 这里也返回 Renew
                    // (smoltcp 内部 Rebind 流程由协议栈驱动, 策略不直接切换)
                    DhcpAction::Renew
                }
            }
            DhcpState::Renewing { .. } => {
                // 续约中, 等待 ACK
                DhcpAction::Continue
            }
            DhcpState::Failed => {
                if policy_cfg.fallback_to_static {
                    cfg.static_ipv4.map_or(DhcpAction::GiveUp, |octets| {
                        DhcpAction::FallbackToStatic(Ipv4Addr::from_octets(octets))
                    })
                } else {
                    DhcpAction::GiveUp
                }
            }
        }
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::net::iface_trait::NetConfig;

    /// 验证 Idle 状态: 启动 Discover.
    #[test]
    fn test_idle_action_continue() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty();
        let pc = DhcpPolicyConfig::default();
        let action = policy.decide(&DhcpState::Idle, &cfg, &pc, 0, 0, 0);
        assert_eq!(action, DhcpAction::Continue);
    }

    /// 验证 Discovering 状态: 重试 < max 时继续.
    #[test]
    fn test_discovering_retry_under_limit_continue() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty();
        let pc = DhcpPolicyConfig::default(); // max_retries = 4
        // retry_count=3 < 4: 继续
        assert_eq!(
            policy.decide(&DhcpState::Discovering, &cfg, &pc, 3, 0, 0),
            DhcpAction::Continue
        );
    }

    /// 验证 Discovering 状态: 重试 >= max 时回退 (静态 IP 存在).
    #[test]
    fn test_discovering_retry_over_limit_fallback_to_static() {
        let policy = DefaultDhcpPolicy;
        let mut cfg = NetConfig::empty();
        cfg.static_ipv4 = Some([192, 168, 1, 100]);
        let pc = DhcpPolicyConfig::default(); // 默认配置: max_retries=4, fallback=true
        // retry_count=4 >= 4: 回退
        assert_eq!(
            policy.decide(&DhcpState::Discovering, &cfg, &pc, 4, 0, 0),
            DhcpAction::FallbackToStatic(Ipv4Addr::from_octets([192, 168, 1, 100]))
        );
    }

    /// 验证 Discovering 状态: 重试 >= max 但无静态 IP 时 GiveUp.
    #[test]
    fn test_discovering_retry_over_limit_giveup_without_static() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty(); // static_ipv4 = None
        let pc = DhcpPolicyConfig::default();
        assert_eq!(
            policy.decide(&DhcpState::Discovering, &cfg, &pc, 4, 0, 0),
            DhcpAction::GiveUp
        );
    }

    /// 验证 Bound 状态: 续约阈值 T1=50% 之前 Continue.
    #[test]
    fn test_bound_before_t1_continue() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty();
        let pc = DhcpPolicyConfig::default(); // t1=50%, t2=87.5%
        let state = DhcpState::Bound {
            ipv4: [10, 0, 2, 15],
            lease_expires_at: 3600_000,
        };
        // 租期 3600s, t1=1800s, elapsed=1000s < 1800s
        assert_eq!(
            policy.decide(&state, &cfg, &pc, 0, 1_000_000, 3_600_000),
            DhcpAction::Continue
        );
    }

    /// 验证 Bound 状态: T1..T2 之间 Renew.
    #[test]
    fn test_bound_between_t1_t2_renew() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty();
        let pc = DhcpPolicyConfig::default();
        let state = DhcpState::Bound {
            ipv4: [10, 0, 2, 15],
            lease_expires_at: 0,
        };
        // 租期 1000s, t1=500s, t2=875s, elapsed=600s (T1..T2)
        assert_eq!(
            policy.decide(&state, &cfg, &pc, 0, 600_000, 1_000_000),
            DhcpAction::Renew
        );
    }

    /// 验证 Bound 状态: 超过 T2 也 Renew (协议栈转 Rebinding).
    #[test]
    fn test_bound_after_t2_renew() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty();
        let pc = DhcpPolicyConfig::default();
        let state = DhcpState::Bound {
            ipv4: [10, 0, 2, 15],
            lease_expires_at: 0,
        };
        // 租期 1000s, t1=500s, t2=875s, elapsed=900s (>T2)
        assert_eq!(
            policy.decide(&state, &cfg, &pc, 0, 900_000, 1_000_000),
            DhcpAction::Renew
        );
    }

    /// 验证 Failed 状态: 走 fallback (有静态 IP).
    #[test]
    fn test_failed_with_static_fallback() {
        let policy = DefaultDhcpPolicy;
        let mut cfg = NetConfig::empty();
        cfg.static_ipv4 = Some([10, 0, 0, 5]);
        let pc = DhcpPolicyConfig::default();
        assert_eq!(
            policy.decide(&DhcpState::Failed, &cfg, &pc, 0, 0, 0),
            DhcpAction::FallbackToStatic(Ipv4Addr::from_octets([10, 0, 0, 5]))
        );
    }

    /// 验证 Failed 状态: fallback 关闭 → GiveUp.
    #[test]
    fn test_failed_fallback_disabled_giveup() {
        let policy = DefaultDhcpPolicy;
        let cfg = NetConfig::empty();
        let mut pc = DhcpPolicyConfig::default();
        pc.fallback_to_static = false;
        assert_eq!(
            policy.decide(&DhcpState::Failed, &cfg, &pc, 0, 0, 0),
            DhcpAction::GiveUp
        );
    }

    /// 验证默认配置: 4 次重试 + RFC 阈值.
    #[test]
    fn test_default_policy_config() {
        let pc = DhcpPolicyConfig::default();
        assert_eq!(pc.max_retries, 4);
        assert_eq!(pc.renew_t1_ratio, 5000);
        assert_eq!(pc.renew_t2_ratio, 8750);
        assert!(pc.fallback_to_static);
    }

    /// 验证 DhcpAction 等值.
    #[test]
    fn test_dhcp_action_eq() {
        assert_eq!(DhcpAction::Continue, DhcpAction::Continue);
        assert_eq!(DhcpAction::Renew, DhcpAction::Renew);
        assert_eq!(DhcpAction::GiveUp, DhcpAction::GiveUp);
        assert_eq!(
            DhcpAction::FallbackToStatic(Ipv4Addr::from_octets([1, 2, 3, 4])),
            DhcpAction::FallbackToStatic(Ipv4Addr::from_octets([1, 2, 3, 4]))
        );
        assert_ne!(DhcpAction::Continue, DhcpAction::Renew);
    }
}
