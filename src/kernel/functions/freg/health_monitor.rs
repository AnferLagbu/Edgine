#![deny(unsafe_code)]
//! 健康监控器 — functions/freg/ 业务层
//!
//! ## 职责
//!
//! 周期 tick 推进, 调用 `privileged::freg::manager` 的安全 API 收集域健康
//! 状态, 主动隔离降级 (proactive quarantine).
//!
//! 与 `privileged::freg::manager::RecoveryManager::tick` 的区别:
//! - **privileged 层 tick**: 底层 health check + undo snapshot + BSR 升级
//! - **functions 层 monitor**: 业务层策略 — 跨多个域聚合判定
//!
//! ## @SAFE
//!
//! 本文件不含 `unsafe`. 通过 `privileged::freg::types::*` 与
//! `privileged::freg::recoverable::Snapshot` 与 TCB 交互.

use super::attribution::DomainFailureRecord;
use super::recovery_policy::{FaultSignal, RecoveryAction, RecoveryPolicy};

/// 域健康快照 (functions 层聚合视图)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainHealth {
    pub domain_id: u64,
    pub consecutive_failures: u32,
    pub current_tier: u32,
    pub is_healthy: bool,
    pub last_check_tick: u64,
}

impl DomainHealth {
    pub const fn unknown(domain_id: u64) -> Self {
        Self {
            domain_id,
            consecutive_failures: 0,
            current_tier: 0,
            is_healthy: true,
            last_check_tick: 0,
        }
    }

    /// 转为 `FaultSignal` (供 Policy 决策)
    pub const fn to_fault_signal(&self, heartbeat_gap: u64, dependents: u32) -> FaultSignal {
        FaultSignal {
            attribution: crate::functions::freg::attribution::FaultAttribution::Service {
                domain_id: self.domain_id,
                recoverable: true,
            },
            retry_count: self.consecutive_failures,
            heartbeat_gap,
            dependents,
            tick: self.last_check_tick,
        }
    }
}

/// 健康监控器
///
/// 业务层包装, 维护域健康快照数组, 周期执行健康检查 + 决策隔离/降级.
pub struct HealthMonitor<'a> {
    /// 被监控的域
    pub records: &'a mut [DomainFailureRecord],
    /// 健康快照 (与 records 平行, 缓存最近一次检查结果)
    pub snapshots: [DomainHealth; MAX_MONITOR_DOMAINS],
    /// 周期 tick 间隔
    pub check_interval_ticks: u64,
    /// 上次检查 tick
    pub last_check_tick: u64,
}

pub const MAX_MONITOR_DOMAINS: usize = 16;

impl<'a> HealthMonitor<'a> {
    pub const fn new(records: &'a mut [DomainFailureRecord]) -> Self {
        Self {
            records,
            snapshots: [DomainHealth {
                domain_id: 0,
                consecutive_failures: 0,
                current_tier: 0,
                is_healthy: true,
                last_check_tick: 0,
            }; MAX_MONITOR_DOMAINS],
            check_interval_ticks: 100,
            last_check_tick: 0,
        }
    }

    /// 周期 tick 入口
    ///
    /// 返回: 需要执行的动作 (Noop 表示无需操作)
    pub fn tick(&mut self, current_tick: u64) -> MonitorAction {
        if current_tick < self.last_check_tick + self.check_interval_ticks {
            return MonitorAction::Noop;
        }
        self.last_check_tick = current_tick;

        let mut quarantines: [u64; MAX_MONITOR_DOMAINS] = [0; MAX_MONITOR_DOMAINS];
        let mut quarantine_count = 0;
        let mut downgrades: [u64; MAX_MONITOR_DOMAINS] = [0; MAX_MONITOR_DOMAINS];
        let mut downgrade_count = 0;

        for (i, rec) in self.records.iter().enumerate() {
            if i >= MAX_MONITOR_DOMAINS {
                break;
            }
            let domain_id = rec.domain_id;
            let failures = rec
                .consecutive_failures
                .load(core::sync::atomic::Ordering::Acquire);
            let tier = rec.current_tier.load(core::sync::atomic::Ordering::Acquire);
            let last_fail = rec
                .last_failure_tick
                .load(core::sync::atomic::Ordering::Acquire);
            let heartbeat_gap = current_tick.saturating_sub(last_fail);

            self.snapshots[i] = DomainHealth {
                domain_id,
                consecutive_failures: failures,
                current_tier: tier,
                is_healthy: tier < 2,
                last_check_tick: current_tick,
            };

            // 决策
            let signal = self.snapshots[i].to_fault_signal(heartbeat_gap, 0);
            let action = RecoveryPolicy::decide(&signal);
            match action {
                RecoveryAction::Quarantine => {
                    if quarantine_count < MAX_MONITOR_DOMAINS {
                        quarantines[quarantine_count] = domain_id;
                        quarantine_count += 1;
                    }
                }
                RecoveryAction::FREGSoftReset | RecoveryAction::FREGBaseRecovery => {
                    if downgrade_count < MAX_MONITOR_DOMAINS {
                        downgrades[downgrade_count] = domain_id;
                        downgrade_count += 1;
                    }
                }
                _ => {}
            }
        }

        if quarantine_count > 0 {
            MonitorAction::QuarantineBatch {
                count: quarantine_count,
            }
        } else if downgrade_count > 0 {
            MonitorAction::RecoverBatch {
                count: downgrade_count,
            }
        } else {
            MonitorAction::Noop
        }
    }

    /// 标记域成功 (供 functions 业务层在请求成功后调用)
    pub fn report_success(&self, domain_id: u64) {
        for rec in self.records.iter() {
            if rec.domain_id == domain_id {
                rec.record_success();
                return;
            }
        }
    }

    /// 标记域失败 (供 functions 业务层在请求失败后调用)
    pub fn report_failure(&self, domain_id: u64, tick: u64) -> u32 {
        for rec in self.records.iter() {
            if rec.domain_id == domain_id {
                // B07-17: 距上次失败的时间间隔作为心跳因子 (失联检测),
                // 与 recovery_policy 的 heartbeat_gap 语义一致.
                let last = rec
                    .last_failure_tick
                    .load(core::sync::atomic::Ordering::Acquire);
                let gap = tick.saturating_sub(last);
                // dependents 当前无数据源 (快照内硬编码 0), 传 0.
                return rec.record_failure(tick, gap, 0);
            }
        }
        0
    }
}

/// 监控动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorAction {
    Noop,
    RecoverBatch { count: usize },
    QuarantineBatch { count: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::Ordering;

    #[test]
    fn health_default() {
        let h = DomainHealth::unknown(42);
        assert_eq!(h.domain_id, 42);
        assert!(h.is_healthy);
    }

    #[test]
    fn health_to_fault_signal() {
        let h = DomainHealth {
            domain_id: 5,
            consecutive_failures: 2,
            current_tier: 1,
            is_healthy: true,
            last_check_tick: 1000,
        };
        let s = h.to_fault_signal(50, 3);
        assert_eq!(s.retry_count, 2);
        assert_eq!(s.heartbeat_gap, 50);
        assert_eq!(s.dependents, 3);
        assert_eq!(s.tick, 1000);
    }

    #[test]
    fn monitor_tick_too_early_noop() {
        // DomainFailureRecord 含原子字段非 Copy, 不能用数组重复语法; 逐元素构造.
        let mut records = [(); MAX_MONITOR_DOMAINS].map(|()| DomainFailureRecord::new(1));
        let mut monitor = HealthMonitor::new(&mut records);
        monitor.last_check_tick = 100;
        let action = monitor.tick(150); // 间隔 50 < 100
        assert_eq!(action, MonitorAction::Noop);
    }

    #[test]
    fn monitor_tick_records_healthy() {
        // DomainFailureRecord 含原子字段非 Copy, 不能用数组重复语法; 逐元素构造.
        let mut records = [(); MAX_MONITOR_DOMAINS].map(|()| DomainFailureRecord::new(1));
        let mut monitor = HealthMonitor::new(&mut records);
        monitor.check_interval_ticks = 0;
        let action = monitor.tick(100);
        assert_eq!(action, MonitorAction::Noop);
    }

    #[test]
    fn monitor_tick_quarantine() {
        // DomainFailureRecord 含原子字段非 Copy, 不能用数组重复语法; 逐元素构造.
        let mut records = [(); MAX_MONITOR_DOMAINS].map(|()| DomainFailureRecord::new(1));
        // 制造 5 次连续失败 → 触发 quarantine
        for _ in 0..5 {
            records[0].record_failure(50, 0, 0);
        }
        let mut monitor = HealthMonitor::new(&mut records);
        monitor.check_interval_ticks = 0;
        let action = monitor.tick(100);
        assert!(matches!(action, MonitorAction::QuarantineBatch { .. }));
    }

    #[test]
    fn monitor_report_success_resets() {
        let mut records = [(); MAX_MONITOR_DOMAINS].map(|()| DomainFailureRecord::new(7));
        // record_failure 现签名为 (current_tick, heartbeat_gap, dependents).
        records[0].record_failure(50, 0, 0);
        records[0].record_failure(50, 0, 0);
        assert_eq!(records[0].consecutive_failures.load(Ordering::Acquire), 2);

        let monitor = HealthMonitor::new(&mut records);
        monitor.report_success(7);
        assert_eq!(records[0].consecutive_failures.load(Ordering::Acquire), 0);
    }

    #[test]
    fn monitor_report_failure_increments() {
        let mut records = [(); MAX_MONITOR_DOMAINS].map(|()| DomainFailureRecord::new(3));
        let monitor = HealthMonitor::new(&mut records);
        let new_tier = monitor.report_failure(3, 200);
        assert_eq!(new_tier, 0);
        assert_eq!(records[0].total_failures.load(Ordering::Acquire), 1);
    }
}
