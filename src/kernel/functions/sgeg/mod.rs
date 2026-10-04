#![deny(unsafe_code)]
//! 身份与权限 — PWM/能力矩阵/会话 (functions 层)
//!
//! ## 框内核中的 PWID 表达
//!
//! ```text
//! privileged/sgeg/                  ← TCB (unsafe 允许)
//!   ├─ identity.rs                   ← 身份表 (全原子字段)
//!   ├─ capability.rs                 ← 能力矩阵
//!   └─ types.rs                      ← PWM 类型定义
//!
//! functions/sgeg/ (本模块)         ← 100% safe Rust
//!   ├─ policy.rs                     ← 能力检查策略 ✅
//!   ├─ grants.rs                     ← 委托规则 ✅
//!   ├─ sessions.rs                   ← 会话生命周期 ✅
//!   ├─ persist.rs                    ← 磁盘序列化编排 ✅
//!   └─ audit.rs                      ← 审计日志生成 ✅
//! ```
//!
//! ## @SAFE
//! 本文件不含 `unsafe`. 所有硬件交互通过 `privileged::sgeg` 的安全 API.

pub mod audit;
/// SGEG 认证策略 — PWM 登录/登出/创建/删除/验证/授权
pub mod auth;
/// T6-8: PWM 能力常量定义 (原 privileged/sgeg/capability.rs)
pub mod capability;
pub mod crypto;
/// 分册 9 批次 4: 域级行为门控 (`DomainFlags`) 查询/设置 syscall 策略
pub mod domain;
pub mod grants;
pub mod identity;
/// 持久化存储编排 — PWM 数据库 save/load/remove (原 privileged/sgeg/storage.rs 下沉)
pub mod persist;
pub mod policy;
/// D6: 安全启动 + TPM 安全封装
pub mod secure_boot;
pub mod sessions;
/// T6-8: SHA-256 哈希实现 (原 privileged/sgeg/sha256.rs)
pub mod sha256;
/// SGEG 私有存储子系统 — 块设备/格式化/分区 safe 代理
pub mod storage;
/// T6-7: SGEG 类型定义 (原 privileged/sgeg/types.rs)
pub mod types;
pub mod uid;

pub use audit::{AUDIT_BUFFER_SIZE, AuditEvent, AuditEventKind, AuditLog, HashChainNode};
pub use capability::{
    CAP_DOMAIN_DEVICE, CAP_DOMAIN_FS, CAP_DOMAIN_IPC, CAP_DOMAIN_MEM, CAP_DOMAIN_NET,
    CAP_DOMAIN_PROC, CAP_DOMAIN_SYSTEM, CAP_DOMAIN_TIME, CAP_DOMAIN_USER_MGMT, DEVICE_CAP_BIND,
    DEVICE_CAP_DMA, DEVICE_CAP_IRQ, DEVICE_CAP_MMIO,
};
pub use grants::{
    DelegationDeny, DelegationEngine, DelegationResult, GRANT_TABLE_CAPACITY, GrantFlags,
    GrantRecord, GrantTable,
};
pub use identity::{PwmError, PwmId, PwmResult};
pub use policy::{
    CAP_DOMAINS, CapBits, CapDomain, CapabilityMatrix, DenyReason, GrantResult, InMemoryMatrix,
    PolicyEngine, PolicyResult, RevokeResult, VIABLE_FLOOR,
};
pub use sessions::{
    LoginDeny, LoginResult, SGEG_MAX_SESSIONS, Session, SessionError, SessionId, SessionManager,
    SessionState, SessionTable,
};
