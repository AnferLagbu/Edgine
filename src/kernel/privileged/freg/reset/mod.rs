//! # FREG Reset 模块
//!
//! 分层恢复策略：BBR → BSR → BHR
//!
//! ```text
//! Layer 1: BBR (FREG Base Recovery)  ~1μs   >95%成功率
//! Layer 2: BSR (FREG Soft Reset)     ~50ms  >80%成功率
//! Layer 3: BHR (FREG Hard Reset)     ~120ms ~100%成功率
//! ```

pub mod audit;
pub mod bbr;
pub mod bhr;
pub mod bsr;
pub mod config;
pub mod layered;
pub mod parallel;

pub use config::{
    BBR_ATTEMPT_COUNT, BHR_ATTEMPT_COUNT, BSR_ATTEMPT_COUNT, CURRENT_LAYER, RECOVERY_CONFIG,
    RESET_IN_PROGRESS, RecoveryConfig, RecoveryLayer, RecoveryResult, RollbackMode,
    get_current_layer, get_stats, is_reset_in_progress, reset_stats, set_current_layer,
    set_reset_in_progress,
};

pub use audit::{
    RESET_AUDIT_LOG, ResetAuditEntry, ResetAuditLog, audit_clear, audit_get_last, audit_record,
    audit_record_domain,
};

pub use bbr::{
    cascade_rollback, compute_fingerprint, execute as bbr_execute, locate_domain_from_panic,
    mark_recovered, should_attempt_recovery, try_rollback_single,
};

pub use bsr::{
    clear_panic_state as bsr_clear_panic_state, execute as bsr_execute,
    freeze_all_domains as bsr_freeze_all_domains, reset_devices as bsr_reset_devices,
    reset_interrupts as bsr_reset_interrupts, rollback_to_init as bsr_rollback_to_init,
    unfreeze_all_domains as bsr_unfreeze_all_domains,
};

pub use bhr::{
    disable_interrupts as bhr_disable_interrupts, execute as bhr_execute,
    execute_fallback as bhr_execute_fallback, keyboard_reset as bhr_keyboard_reset,
    mask_all_irqs as bhr_mask_all_irqs, save_crash_info as bhr_save_crash_info,
    shutdown_devices as bhr_shutdown_devices, triple_fault as bhr_triple_fault,
};

pub use parallel::{
    DependencyLayer, DependencyLayers, compute_dependency_layers, get_parallel_stats, rollback_all,
    rollback_all_parallel, rollback_layer_parallel, rollback_layer_serial,
};

pub use layered::{
    RecoveryStatus, execute_from_panic, execute_layered as recovery_execute_layered,
    get_recovery_status, try_bbr_first,
};
