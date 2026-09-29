//! Credo v1 Identity & Capability System
//!
//! 域身份 (DID) + 能力矩阵 + 会话管理 + 审计.
//! Credo: 密码决定身份 | 能力来自授予 | POSIX DAC + 能力双路径

#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {};
}

pub use serial_println;

pub mod api;
pub mod audit;
pub mod bootstrap;
pub mod capability;
pub mod csprng;
pub mod engine;
pub mod grant;
pub mod identity;
/// D6: 安全启动 + TPM 2.0
pub mod secure_boot;
pub mod session;
pub mod sha256;
pub mod types;

pub use audit::AuditLog;
pub use identity::IdentityTable;
// B08-22: 常数时间比较权威位 (framework TCB 安全原语), services::credo::crypto::ct_eq 委托
pub use identity::constant_time_eq;
pub use types::*;

// api 公共接口 re-export — 避免跨子系统直接访问 credo::api 内部
pub use api::*;

// session 公共接口 re-export — 避免跨子系统直接访问 credo::session 内部
pub use session::*;

// engine 公共接口 re-export — 避免跨子系统直接访问 credo::engine 内部
pub use engine::get_privilege_level;

// capability 公共接口 re-export — 避免跨子系统直接访问 credo::capability 内部
pub use capability::{
    CAP_DOMAIN_DEVICE, CAP_DOMAIN_FS, CAP_DOMAIN_IPC, CAP_DOMAIN_MEM, CAP_DOMAIN_NET,
    CAP_DOMAIN_PROC, CAP_DOMAIN_SYSTEM, CAP_DOMAIN_TIME, CAP_DOMAIN_USER_MGMT, DEVICE_CAP_BIND,
    DEVICE_CAP_DMA, DEVICE_CAP_IRQ, DEVICE_CAP_MMIO, SYSTEM_CAP_UTS_SETNAME,
};

// secure_boot 公共接口 re-export — 避免跨子系统直接访问 credo::secure_boot 内部
pub use secure_boot::*;

/// Credo 子系统初始化 — 安全启动 + TPM.
///
/// 从 `scheduler_init` 中分离, 消除 proc→credo 的初始化依赖.
/// 应在 `scheduler_init` 之后调用.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn credo_init() {
    use secure_boot::{Ed25519PubKey, secure_boot_init, tpm_init};
    // 默认平台密钥 (全零, 开发阶段)
    let default_pk = Ed25519PubKey::new([0u8; 32]);
    secure_boot_init(default_pk);
    tpm_init();
}
