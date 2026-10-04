//! 恢复配置与类型定义 — framework 机制配置
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原配置/状态/统计于 T6-6 (2026-06-16) 迁至 `services::freg::reset_config`,
//! 本文件仅 re-export。按"机制持有的数据结构/常量归 framework"统一判据反转：
//! `RecoveryLayer`/`set_reset_in_progress` 等被 framework proc/scheduler 机制
//! (FREG 恢复域降级路径) 直接消费 — 属机制项, 迁回。
//!
//! services 侧改 `pub use crate::framework::freg::reset::config::*`
//! 保持 API 兼容。本文件 0 unsafe (纯配置 + 原子状态).

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const RESET_SUCCESS: u32 = 0;
pub const RESET_FAILED: u32 = 1;
pub const RESET_ESCALATE: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RecoveryLayer {
    Layer1 = 1,
    Layer2 = 2,
    Layer3 = 3,
}

impl RecoveryLayer {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Layer1,
            2 => Self::Layer2,
            3 => Self::Layer3,
            _ => Self::Layer1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RecoveryResult {
    Success = RESET_SUCCESS,
    Failed = RESET_FAILED,
    Escalate = RESET_ESCALATE,
}

impl RecoveryResult {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn should_escalate(&self) -> bool {
        matches!(self, Self::Escalate)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RollbackMode {
    Serial = 0,
    Parallel = 1,
}

#[derive(Debug)]
pub struct RecoveryConfig {
    pub enable_layer1: bool,
    pub enable_layer2: bool,
    pub enable_layer3: bool,
    pub layer1_failure_threshold: u32,
    pub layer2_device_timeout_ticks: u64,
    pub layer2_max_attempts: u32,
    pub audit_enabled: bool,
    pub rollback_mode: RollbackMode,
    pub parallel_max_workers: u32,
}

impl RecoveryConfig {
    pub const fn default() -> Self {
        Self {
            enable_layer1: true,
            enable_layer2: true,
            enable_layer3: true,
            layer1_failure_threshold: 5,
            layer2_device_timeout_ticks: 100,
            layer2_max_attempts: 3,
            audit_enabled: true,
            rollback_mode: RollbackMode::Serial,
            parallel_max_workers: 4,
        }
    }

    pub fn is_parallel(&self) -> bool {
        matches!(self.rollback_mode, RollbackMode::Parallel)
    }
}

pub static RECOVERY_CONFIG: RecoveryConfig = RecoveryConfig::default();

pub static CURRENT_LAYER: AtomicU32 = AtomicU32::new(0);
pub static RESET_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
pub static BBR_ATTEMPT_COUNT: AtomicU32 = AtomicU32::new(0);
pub static BSR_ATTEMPT_COUNT: AtomicU32 = AtomicU32::new(0);
pub static BHR_ATTEMPT_COUNT: AtomicU32 = AtomicU32::new(0);
pub static LAST_RESET_TICK: AtomicU64 = AtomicU64::new(0);
pub static PARALLEL_ROLLBACK_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn is_reset_in_progress() -> bool {
    RESET_IN_PROGRESS.load(Ordering::SeqCst)
}

pub fn set_reset_in_progress(v: bool) {
    RESET_IN_PROGRESS.store(v, Ordering::SeqCst);
}

pub fn get_current_layer() -> RecoveryLayer {
    RecoveryLayer::from_u32(CURRENT_LAYER.load(Ordering::SeqCst))
}

pub fn set_current_layer(layer: RecoveryLayer) {
    CURRENT_LAYER.store(layer as u32, Ordering::SeqCst);
}

pub fn increment_bbr_count() -> u32 {
    BBR_ATTEMPT_COUNT.fetch_add(1, Ordering::SeqCst)
}

pub fn increment_bsr_count() -> u32 {
    BSR_ATTEMPT_COUNT.fetch_add(1, Ordering::SeqCst)
}

pub fn increment_bhr_count() -> u32 {
    BHR_ATTEMPT_COUNT.fetch_add(1, Ordering::SeqCst)
}

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
pub fn get_stats() -> (u32, u32, u32) {
    let bsr_count = BSR_ATTEMPT_COUNT.load(Ordering::SeqCst);
    let bhr_count = BHR_ATTEMPT_COUNT.load(Ordering::SeqCst);
    let last_tick = LAST_RESET_TICK.load(Ordering::SeqCst) as u32;
    (bsr_count, bhr_count, last_tick)
}

pub fn reset_stats() {
    BSR_ATTEMPT_COUNT.store(0, Ordering::SeqCst);
    BHR_ATTEMPT_COUNT.store(0, Ordering::SeqCst);
    LAST_RESET_TICK.store(0, Ordering::SeqCst);
    CURRENT_LAYER.store(0, Ordering::SeqCst);
    RESET_IN_PROGRESS.store(false, Ordering::SeqCst);
    PARALLEL_ROLLBACK_ACTIVE.store(false, Ordering::SeqCst);
}

// E-03 (2026-09-06): feature 语义拆分 — 纯逻辑测试模块, host-test 下同样编译
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
pub mod tests {
    // J-01 (2026-09-08): wildcard_imports 清理 — 显式列出本模块使用的 super 符号
    use super::{RecoveryConfig, RecoveryLayer, RecoveryResult, get_stats, reset_stats};

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

    pub fn test_recovery_layer() -> bool {
        let layer1 = RecoveryLayer::Layer1;
        let layer2 = RecoveryLayer::Layer2;
        let layer3 = RecoveryLayer::Layer3;

        layer1 as u32 == 1 && layer2 as u32 == 2 && layer3 as u32 == 3
    }

    pub fn test_config_default() -> bool {
        let config = RecoveryConfig::default();
        config.enable_layer1
            && config.enable_layer2
            && config.enable_layer3
            && config.layer1_failure_threshold == 5
    }

    pub fn test_stats() -> bool {
        reset_stats();
        let (bsr, bhr, tick) = get_stats();
        bsr == 0 && bhr == 0 && tick == 0
    }
}
