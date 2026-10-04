//! # FREG Stack — `Edgine` 宏内核故障恢复子系统
//!
//! FREG是 `Edgine` 在宏内核架构下实现模块级"重生"的核心基础设施。
//!
//! ## 架构
//!
//! ```text
//! Rust panic!() → panic_handler → PANIC_FLAG → int 0x82
//!   → isr0x82 → exception_handler → recovery_try_recover_from_idt()
//!     → RecoveryManager::locate_domain() → cascade_rollback_bfs()
//!       → DomainState::RollingBack → undo.rollback_to(gen)
//!         → mark_recovered() → audit_log → PANIC_FLAG clear → IDT return
//! ```
//!
//! ## 分层恢复策略
//!
//! ```text
//! Layer 1: FREG Recovery (模块级回滚)  ~1μs   >95%成功率
//! Layer 2: BSR (FREG Soft Reset)      ~50ms  >80%成功率
//! Layer 3: BHR (FREG Hard Reset)      ~120ms ~100%成功率
//! ```
//!
//! ## 模块组织
//!
//! ```text
//! freg/
//! ├── mod.rs           入口：pub mod + 重新导出 + 全局静态变量
//! ├── types.rs         数据类型：DomainState, UndoEntry, FREGSnapshot, RollbackEvent
//! ├── undo_log.rs      撤销日志：UndoLog + fnv1a_32
//! ├── domain.rs        恢复域：RecoveryDomain
//! ├── manager.rs       管理器：RecoveryManager + 审计日志
//! ├── recoverable.rs   可恢复原语：Snapshot trait + RecoverableMutex
//! ├── fault_inject.rs  故障注入：maybe_inject_fault (feature gate)
//! ├── snapshot.rs      设备快照：DeviceSnapshot + DeviceSnapshotRegistry
//! ├── reset/           BSR/BHR 模块化实现
//! │   ├── config.rs    配置与类型定义
//! │   ├── audit.rs     审计日志
//! │   ├── bsr.rs       FREG Soft Reset
//! │   ├── bhr.rs       FREG Hard Reset
//! │   ├── parallel.rs  并发回滚机制
//! │   ├── layered.rs   分层恢复入口
//! │   └── mod.rs       模块导出
//! └── ffi.rs           C FFI 桥接层
//! ```

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::framework::sync::IrqSpinLock;
pub mod api;
pub mod degrade_policy;
pub mod domain;
pub mod fault_inject;
pub mod manager;
pub mod recoverable;
pub mod recovery;
pub mod reset;
pub mod snapshot;
pub mod types;
pub mod undo_log;

pub use domain::RecoveryDomain;
pub use fault_inject::maybe_inject_fault;
pub use manager::{ROLLBACK_LOG, RecoveryManager};
pub use recoverable::{Recoverable, RecoverableMutex, Snapshot};
pub use reset::{
    BBR_ATTEMPT_COUNT, BHR_ATTEMPT_COUNT, BSR_ATTEMPT_COUNT, CURRENT_LAYER, DependencyLayer,
    DependencyLayers, RECOVERY_CONFIG, RESET_AUDIT_LOG, RESET_IN_PROGRESS, RecoveryConfig,
    RecoveryLayer, RecoveryResult, RecoveryStatus, ResetAuditEntry, ResetAuditLog, RollbackMode,
    bbr_execute, bhr_execute, bhr_execute_fallback, bsr_execute, compute_dependency_layers,
    execute_from_panic, get_current_layer, get_parallel_stats, get_recovery_status, get_stats,
    recovery_execute_layered, reset_stats, rollback_all, rollback_all_parallel,
};
pub use snapshot::{
    DEVICE_SNAPSHOTS, DeviceSnapshot, DeviceSnapshotRegistry, DeviceType, snapshot_capture_init,
    snapshot_is_init_captured, snapshot_register_device, snapshot_restore_all,
    snapshot_unregister_device,
};
pub use types::*;
pub use undo_log::UndoLog;

// 降级策略 trait — 机制/策略分离 (§6.3 domain.rs 拆分接口)
pub use degrade_policy::{
    DegradeDecision, FREGDegradePolicy, FallbackFREGDegradePolicy, current_freg_degrade_policy,
    register_freg_degrade_policy,
};

pub use api::{
    recovery_domain_add_addr_range, recovery_domain_add_dep, recovery_domain_dep_count,
    recovery_domain_get_failures, recovery_domain_get_state, recovery_domain_register,
    recovery_domain_set_cbs, recovery_domain_unregister, recovery_freg_maintenance,
    recovery_panic_flag_clear, recovery_panic_flag_is_set, recovery_rollback_log_count,
    recovery_trigger_panic, recovery_try_recover_from_idt, recovery_undo_count,
    recovery_undo_record, recovery_was_attempted,
};

#[cfg(feature = "kernel_test")]
pub use api::recovery_test_rollback;

#[cfg(feature = "fault_injection")]
pub use api::{recovery_get_fault_rate, recovery_set_fault_rate};

pub static PANIC_FLAG: AtomicBool = AtomicBool::new(false);
pub static PANIC_MSG: IrqSpinLock<[u8; 128]> = IrqSpinLock::new([0u8; 128]);
pub static CRASH_RIP: AtomicU64 = AtomicU64::new(0);

pub static RECOVERY_MANAGER: IrqSpinLock<RecoveryManager> =
    IrqSpinLock::new(RecoveryManager::new());
pub static NEED_BSR_ESCALATION: AtomicBool = AtomicBool::new(false);

pub fn check_and_clear_bsr_escalation() -> bool {
    NEED_BSR_ESCALATION.swap(false, Ordering::SeqCst)
}
