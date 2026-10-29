//! # 恢复审计日志

#![deny(missing_docs)]

use super::config::RecoveryLayer;
use super::config::RecoveryResult;

use crate::privileged::sync::IrqSpinLock;
/// 恢复审计日志的环形条目容量
pub const MAX_AUDIT_ENTRIES: usize = 16;

/// 循环复用的恢复审计日志 (定长环形条目数组 + 有效计数)。
#[derive(Debug)]
pub struct ResetAuditLog {
    /// 环形条目数组
    pub entries: [ResetAuditEntry; MAX_AUDIT_ENTRIES],
    /// 当前有效条目数
    pub count: usize,
}

/// 单条恢复审计记录: 时间刻度/层级/结果/原因码/域标识/回滚条目数。
#[derive(Debug, Clone, Copy)]
pub struct ResetAuditEntry {
    /// 记录时刻 (tick)
    pub tick: u64,
    /// 恢复层级
    pub layer: RecoveryLayer,
    /// 恢复结果
    pub result: RecoveryResult,
    /// 原因码
    pub reason: u32,
    /// 故障域标识
    pub domain_id: u64,
    /// 已回滚条目数
    pub entries_rolled: usize,
}

impl ResetAuditEntry {
    /// 构造空审计条目 (const, 供静态初始化)
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            tick: 0,
            layer: RecoveryLayer::Layer1,
            result: RecoveryResult::Success,
            reason: 0,
            domain_id: 0,
            entries_rolled: 0,
        }
    }
}

impl Default for ResetAuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ResetAuditLog {
    /// 构造空审计日志 (const, 供静态初始化)
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [ResetAuditEntry::empty(); MAX_AUDIT_ENTRIES],
            count: 0,
        }
    }

    /// 记录一条完整审计 (满则丢弃最旧条目)
    pub fn record(
        &mut self,
        tick: u64,
        layer: RecoveryLayer,
        result: RecoveryResult,
        reason: u32,
        domain_id: u64,
        entries_rolled: usize,
    ) {
        if self.count >= MAX_AUDIT_ENTRIES {
            for i in 0..MAX_AUDIT_ENTRIES - 1 {
                self.entries[i] = self.entries[i + 1];
            }
            self.count = MAX_AUDIT_ENTRIES - 1;
        }
        self.entries[self.count] = ResetAuditEntry {
            tick,
            layer,
            result,
            reason,
            domain_id,
            entries_rolled,
        };
        self.count += 1;
    }

    /// 记录一条不含域信息的审计 (域标识/回滚数置 0)
    pub fn record_simple(
        &mut self,
        tick: u64,
        layer: RecoveryLayer,
        result: RecoveryResult,
        reason: u32,
    ) {
        self.record(tick, layer, result, reason, 0, 0);
    }

    /// 最近一条审计记录 (无记录返回 `None`)
    #[must_use]
    pub fn last(&self) -> Option<&ResetAuditEntry> {
        if self.count > 0 {
            Some(&self.entries[self.count - 1])
        } else {
            None
        }
    }

    /// 清空日志 (有效计数归零)
    pub fn clear(&mut self) {
        self.count = 0;
    }

    /// 统计指定恢复层级的记录数
    #[must_use]
    pub fn count_by_layer(&self, layer: RecoveryLayer) -> usize {
        self.entries[..self.count]
            .iter()
            .filter(|e| e.layer == layer)
            .count()
    }

    /// 统计指定恢复结果的记录数
    #[must_use]
    pub fn count_by_result(&self, result: RecoveryResult) -> usize {
        self.entries[..self.count]
            .iter()
            .filter(|e| e.result == result)
            .count()
    }
}

/// 全局恢复审计日志 (自旋锁保护)
pub static RESET_AUDIT_LOG: IrqSpinLock<ResetAuditLog> = IrqSpinLock::new(ResetAuditLog::new());

/// 记录一条不含域信息的恢复审计 (审计关闭时直接返回)。
pub fn audit_record(layer: RecoveryLayer, result: RecoveryResult, reason: u32) {
    use super::config::RECOVERY_CONFIG;

    if !RECOVERY_CONFIG.audit_enabled {
        return;
    }
    let tick = crate::privileged::timer::get_ticks();
    let mut log = RESET_AUDIT_LOG.lock();
    log.record_simple(tick, layer, result, reason);
}

/// 记录一条含域标识与回滚条目数的恢复审计 (审计关闭时直接返回)。
pub fn audit_record_domain(
    layer: RecoveryLayer,
    result: RecoveryResult,
    reason: u32,
    domain_id: u64,
    entries_rolled: usize,
) {
    use super::config::RECOVERY_CONFIG;

    if !RECOVERY_CONFIG.audit_enabled {
        return;
    }
    let tick = crate::privileged::timer::get_ticks();
    let mut log = RESET_AUDIT_LOG.lock();
    log.record(tick, layer, result, reason, domain_id, entries_rolled);
}

/// 读取最近一条恢复审计记录的副本, 无记录时返回 None。
pub fn audit_get_last() -> Option<ResetAuditEntry> {
    let log = RESET_AUDIT_LOG.lock();
    log.last().copied()
}

/// 清空恢复审计日志 (重置有效计数)。
pub fn audit_clear() {
    let mut log = RESET_AUDIT_LOG.lock();
    log.clear();
}

// UT-07 (2026-09-26): 原 `#[cfg(feature = "kernel_test")] pub mod tests { pub fn .. -> bool }`
// 形态 (host 不编译, 且非 cargo 可发现的 `#[test]`) 改写为源侧 `#[cfg(test)] #[test]` —
// 注册侧 `privileged/tests/reset.rs::freg::audit` 的薄包装 `check!(tests::test_*())` 随之删除,
// 断言以本模块为唯一归属.
#[cfg(test)]
mod tests {
    // J-01 (2026-09-08): wildcard_imports 清理 — 显式列出本模块使用的 super 符号
    use super::{RecoveryLayer, RecoveryResult, ResetAuditLog};

    #[test]
    fn audit_log() {
        let mut log = ResetAuditLog::new();
        log.record_simple(100, RecoveryLayer::Layer1, RecoveryResult::Success, 0);
        log.record_simple(200, RecoveryLayer::Layer2, RecoveryResult::Escalate, 1);
        assert_eq!(log.count, 2, "两条记录");
    }

    #[test]
    fn audit_count_by_layer() {
        let mut log = ResetAuditLog::new();
        log.record_simple(100, RecoveryLayer::Layer1, RecoveryResult::Success, 0);
        log.record_simple(200, RecoveryLayer::Layer2, RecoveryResult::Success, 0);
        log.record_simple(300, RecoveryLayer::Layer1, RecoveryResult::Failed, 1);

        assert_eq!(log.count_by_layer(RecoveryLayer::Layer1), 2, "Layer1 计数");
        assert_eq!(log.count_by_layer(RecoveryLayer::Layer2), 1, "Layer2 计数");
    }
}
