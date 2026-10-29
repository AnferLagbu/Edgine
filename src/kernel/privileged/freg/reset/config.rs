//! 恢复配置与类型定义 — privileged 机制配置
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原配置/状态/统计于 T6-6 (2026-06-16) 迁至 `functions::freg::reset_config`,
//! 本文件仅 re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：
//! `RecoveryLayer`/`set_reset_in_progress` 等被 privileged proc/scheduler 机制
//! (FREG 恢复域降级路径) 直接消费 — 属机制项, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::freg::reset::config::*`
//! 保持 API 兼容。本文件 0 unsafe (纯配置 + 原子状态).

#![deny(missing_docs)]

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// 恢复成功的结果码
pub const RESET_SUCCESS: u32 = 0;
/// 恢复失败的结果码
pub const RESET_FAILED: u32 = 1;
/// 需升级到下一层恢复的结果码
pub const RESET_ESCALATE: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
/// FREG 恢复层级 (Layer1=BBR, Layer2=BSR, Layer3=BHR)。
pub enum RecoveryLayer {
    /// 第 1 层恢复 (BBR)
    Layer1 = 1,
    /// 第 2 层恢复 (BSR)
    Layer2 = 2,
    /// 第 3 层恢复 (BHR)
    Layer3 = 3,
}

impl RecoveryLayer {
    /// 由 `u32` 编码还原恢复层级 (未知值退化为 `Layer1`)
    #[must_use]
    pub fn from_u32(v: u32) -> Self {
        match v {
            2 => Self::Layer2,
            3 => Self::Layer3,
            _ => Self::Layer1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
/// 单层恢复的执行结果 (成功/失败/需升级到下一层)。
pub enum RecoveryResult {
    /// 恢复成功
    Success = RESET_SUCCESS,
    /// 恢复失败
    Failed = RESET_FAILED,
    /// 需升级到下一层
    Escalate = RESET_ESCALATE,
}

impl RecoveryResult {
    /// 是否为成功结果
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    /// 是否需要升级到下一层恢复
    #[must_use]
    pub fn should_escalate(self) -> bool {
        matches!(self, Self::Escalate)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
/// 回滚模式: 串行或并行执行多域回滚。
pub enum RollbackMode {
    /// 串行回滚
    Serial = 0,
    /// 并行回滚
    Parallel = 1,
}

/// 三层恢复开关
#[derive(Debug, Clone, Copy)]
pub struct LayerSwitches {
    /// 是否启用第 1 层 (BBR) 恢复
    pub layer1: bool,
    /// 是否启用第 2 层 (BSR) 恢复
    pub layer2: bool,
    /// 是否启用第 3 层 (BHR) 恢复
    pub layer3: bool,
}

#[derive(Debug)]
/// 恢复配置: 各层开关、失败阈值、设备超时与并行回滚参数。
pub struct RecoveryConfig {
    /// 三层恢复开关
    pub layers: LayerSwitches,
    /// 第 1 层连续失败阈值 (超过则升级到下一层)
    pub layer1_failure_threshold: u32,
    /// 第 2 层设备操作超时 (tick)
    pub layer2_device_timeout_ticks: u64,
    /// 第 2 层最大尝试次数
    pub layer2_max_attempts: u32,
    /// 是否启用审计记录
    pub audit_enabled: bool,
    /// 多域回滚模式
    pub rollback_mode: RollbackMode,
    /// 并行回滚最大工作线程数
    pub parallel_max_workers: u32,
}

impl RecoveryConfig {
    /// 返回默认配置 (三层全开、串行回滚)
    #[must_use]
    pub const fn default() -> Self {
        Self {
            layers: LayerSwitches {
                layer1: true,
                layer2: true,
                layer3: true,
            },
            layer1_failure_threshold: 5,
            layer2_device_timeout_ticks: 100,
            layer2_max_attempts: 3,
            audit_enabled: true,
            rollback_mode: RollbackMode::Serial,
            parallel_max_workers: 4,
        }
    }

    /// 是否启用并行回滚
    #[must_use]
    pub fn is_parallel(&self) -> bool {
        matches!(self.rollback_mode, RollbackMode::Parallel)
    }
}

/// 全局只读恢复配置
pub static RECOVERY_CONFIG: RecoveryConfig = RecoveryConfig::default();

/// 当前恢复层级编码 (0 = 未进行恢复)
pub static CURRENT_LAYER: AtomicU32 = AtomicU32::new(0);
/// 恢复流程进行中标志
pub static RESET_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
/// BBR (Layer1) 尝试计数
pub static BBR_ATTEMPT_COUNT: AtomicU32 = AtomicU32::new(0);
/// BSR (Layer2) 尝试计数
pub static BSR_ATTEMPT_COUNT: AtomicU32 = AtomicU32::new(0);
/// BHR (Layer3) 尝试计数
pub static BHR_ATTEMPT_COUNT: AtomicU32 = AtomicU32::new(0);
/// 上次恢复发起的 tick
pub static LAST_RESET_TICK: AtomicU64 = AtomicU64::new(0);
/// 并行回滚进行中标志
pub static PARALLEL_ROLLBACK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 返回当前是否已有恢复流程正在进行。
pub fn is_reset_in_progress() -> bool {
    RESET_IN_PROGRESS.load(Ordering::SeqCst)
}

/// 设置恢复进行中标志。
pub fn set_reset_in_progress(v: bool) {
    RESET_IN_PROGRESS.store(v, Ordering::SeqCst);
}

/// 返回当前正在执行的恢复层级 (未进行时为 Layer1)。
pub fn get_current_layer() -> RecoveryLayer {
    RecoveryLayer::from_u32(CURRENT_LAYER.load(Ordering::SeqCst))
}

/// 设置当前正在执行的恢复层级。
pub fn set_current_layer(layer: RecoveryLayer) {
    CURRENT_LAYER.store(layer as u32, Ordering::SeqCst);
}

/// 递增 BBR (Layer1) 尝试计数, 返回递增前的旧值。
pub fn increment_bbr_count() -> u32 {
    BBR_ATTEMPT_COUNT.fetch_add(1, Ordering::SeqCst)
}

/// 递增 BSR (Layer2) 尝试计数, 返回递增前的旧值。
pub fn increment_bsr_count() -> u32 {
    BSR_ATTEMPT_COUNT.fetch_add(1, Ordering::SeqCst)
}

/// 递增 BHR (Layer3) 尝试计数, 返回递增前的旧值。
pub fn increment_bhr_count() -> u32 {
    BHR_ATTEMPT_COUNT.fetch_add(1, Ordering::SeqCst)
}

/// 返回 (BSR 尝试次数, BHR 尝试次数, 上次重置 tick 低 32 位) 统计元组。
pub fn get_stats() -> (u32, u32, u32) {
    let bsr_count = BSR_ATTEMPT_COUNT.load(Ordering::SeqCst);
    let bhr_attempts = BHR_ATTEMPT_COUNT.load(Ordering::SeqCst);
    let last_tick =
        crate::privileged::glue::low_u32_from_u64(LAST_RESET_TICK.load(Ordering::SeqCst));
    (bsr_count, bhr_attempts, last_tick)
}

/// 清空全部恢复统计与状态标志。
pub fn reset_stats() {
    BSR_ATTEMPT_COUNT.store(0, Ordering::SeqCst);
    BHR_ATTEMPT_COUNT.store(0, Ordering::SeqCst);
    LAST_RESET_TICK.store(0, Ordering::SeqCst);
    CURRENT_LAYER.store(0, Ordering::SeqCst);
    RESET_IN_PROGRESS.store(false, Ordering::SeqCst);
    PARALLEL_ROLLBACK_ACTIVE.store(false, Ordering::SeqCst);
}

// E-03 (2026-09-06): feature 语义拆分 — 纯逻辑测试模块, host-test 下同样编译
/// 恢复配置与状态机的纯逻辑测试 (`kernel_test` / host-test 下编译)
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod tests {
    // J-01 (2026-09-08): wildcard_imports 清理 — 显式列出本模块使用的 super 符号
    use super::{RecoveryConfig, RecoveryLayer, RecoveryResult, get_stats, reset_stats};

    /// 验证恢复结果判定 (成功 / 需升级)
    #[must_use]
    pub fn test_recovery_result() -> bool {
        let success = RecoveryResult::Success;
        let failed = RecoveryResult::Failed;
        let escalate = RecoveryResult::Escalate;

        success.is_success()
            && !success.should_escalate()
            && !failed.is_success()
            && !failed.should_escalate()
            && !escalate.is_success()
            && escalate.should_escalate()
    }

    /// 验证恢复层级编码值
    #[must_use]
    pub fn test_recovery_layer() -> bool {
        let layer1 = RecoveryLayer::Layer1;
        let layer2 = RecoveryLayer::Layer2;
        let layer3 = RecoveryLayer::Layer3;

        layer1 as u32 == 1 && layer2 as u32 == 2 && layer3 as u32 == 3
    }

    /// 验证默认配置的层级开关与阈值
    #[must_use]
    pub fn test_config_default() -> bool {
        let config = RecoveryConfig::default();
        config.layers.layer1
            && config.layers.layer2
            && config.layers.layer3
            && config.layer1_failure_threshold == 5
    }

    /// 验证统计清零后各计数为 0
    #[must_use]
    pub fn test_stats() -> bool {
        reset_stats();
        let (bsr, bhr, tick) = get_stats();
        bsr == 0 && bhr == 0 && tick == 0
    }
}
