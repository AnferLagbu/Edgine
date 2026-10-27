//! SGEG v1 Identity & Capability System
//!
//! 域身份 (DID) + 能力矩阵 + 会话管理 + 审计.
//! SGEG: 密码决定身份 | 能力来自授予 | POSIX DAC + 能力双路径

/// 内核态串口打印宏: 在 SGEG 上下文提供 `println!` 形态输出 (`no_std` 下为空实现).
#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {};
}

pub use serial_println;

pub mod api;
/// 审计日志: 记录身份与能力相关安全事件
pub mod audit;
/// 启动引导: SGEG 子系统初始化编排
pub mod bootstrap;
pub mod capability;
/// 密码学安全随机数发生器
pub mod csprng;
/// 权限决策引擎: 能力矩阵求值
pub mod engine;
/// 能力授予与撤销
pub mod grant;
/// 全局身份表与 PWM 能力授予 / 撤销
pub mod identity;
/// D6: 安全启动 + TPM 2.0
pub mod secure_boot;
/// 会话管理
pub mod session;
pub mod sha256;
pub mod types;

pub use audit::AuditLog;
pub use identity::IdentityTable;
// B08-22: 常数时间比较权威位 (privileged TCB 安全原语), functions::sgeg::crypto::ct_eq 委托
pub use identity::constant_time_eq;
pub use types::*;

// api 公共接口 re-export — 避免跨子系统直接访问 sgeg::api 内部
pub use api::*;

// session 公共接口 re-export — 避免跨子系统直接访问 sgeg::session 内部
pub use session::*;

// engine 公共接口 re-export — 避免跨子系统直接访问 sgeg::engine 内部
pub use engine::get_privilege_level;

// capability 公共接口 re-export — 避免跨子系统直接访问 sgeg::capability 内部
pub use capability::{
    CAP_DOMAIN_DEVICE, CAP_DOMAIN_FS, CAP_DOMAIN_IPC, CAP_DOMAIN_MEM, CAP_DOMAIN_NET,
    CAP_DOMAIN_PROC, CAP_DOMAIN_SYSTEM, CAP_DOMAIN_TIME, CAP_DOMAIN_USER_MGMT, DEVICE_CAP_BIND,
    DEVICE_CAP_DMA, DEVICE_CAP_IRQ, DEVICE_CAP_MMIO, SYSTEM_CAP_UTS_SETNAME,
};

// secure_boot 公共接口 re-export — 避免跨子系统直接访问 sgeg::secure_boot 内部
pub use secure_boot::*;

/// SGEG 子系统初始化 — 安全启动 + TPM.
///
/// 机制层普通 Rust 函数: 唯一调用者是 crate root 编排入口
/// (`lib.rs::kernel_init` 的「延迟子系统编排」块, P-8 init 编排收口).
/// 原 `#[unsafe(no_mangle)] pub extern "C"` 形态是为已删除的 FFI 孤儿入口
/// `sched_ops::scheduler_init` 服务的, 保留 C ABI 只会让它看起来还有外部调用者.
pub fn sgeg_init() {
    use secure_boot::{Ed25519PubKey, secure_boot_init, tpm_init};
    // 默认平台密钥 (全零, 开发阶段)
    let default_pk = Ed25519PubKey::new([0u8; 32]);
    secure_boot_init(default_pk);
    tpm_init();
}
