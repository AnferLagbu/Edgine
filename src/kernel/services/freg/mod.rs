#![deny(unsafe_code)]
//! 故障恢复 — FREG恢复 (services 层)
//!
//! ## 模块拓扑
//!
//! ```text
//! services::freg/
//! ├── attribution.rs    故障归属: panic_rip → 域 / TCB 判定
//! ├── recovery_policy.rs 策略决策: 故障信号 → BBR/BSR/BHR/Quarantine
//! ├── health_monitor.rs  健康监控: 周期 tick + 主动降级/隔离
//! ├── cascade.rs        拓扑感知级联: parent/child 关系编排
//! └── audit_export.rs   审计导出: ROLLBACK_LOG → dmesg 友好格式
//! ```
//!
//! ## 框内核边界
//!
//! - 本层 **100% safe Rust** (`#![deny(unsafe_code)]`)
//! - 通过 `framework::freg::*` 安全公开 API 访问 TCB
//! - 不直接接触 `spin::Mutex` / `AtomicU64` (仅在内部聚合统计)
//!
//! ## @SAFE
//!
//! 所有子模块经 CI 的 `audit_services_boundary.py` 检查, 无 unsafe/裸指针泄漏.

pub mod attribution;
pub mod audit_export;
pub mod cascade;
pub mod health_monitor;
pub mod recovery_policy;
/// T6-6: 恢复配置与类型定义 (原 framework/freg/reset/config.rs)
pub mod reset_config;

pub use attribution::{
    AddrRange, CrossLayerHandler, DomainFailureRecord, FaultAttribution, FaultAttributor,
    MAX_SERVICE_DOMAINS, SERVICE_RANGES, TCB_RANGES, TcbModule,
};
pub use audit_export::{AuditExporter, RollbackSummary};
pub use cascade::{
    CascadeDirection, CascadePlan, CascadePolicy, CascadeQueue, DomainNode, DomainTopology,
    MAX_TOPOLOGY_DOMAINS,
};
pub use health_monitor::{DomainHealth, HealthMonitor, MAX_MONITOR_DOMAINS, MonitorAction};
pub use recovery_policy::{FaultSignal, RecoveryAction, RecoveryPolicy};
