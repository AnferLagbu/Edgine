//! 配置自检策略 trait — 策略-机制分离接口 (DECISION-K 项 2)
//!
//! framework config 机制的启动自检 (validate_system_config / validate_drivers /
//! validate_pci_subsystem / validate_network_subsystem) 原为 framework→services
//! 直接引用 (经 validate.rs 壳)。按 DECISION-K 统一"机制 init 后立即注册策略"
//! 启动契约: framework 定义 `ConfigValidateHook` 契约, services 实现并注册,
//! framework 经 `current_config_validate_hook()` 调用。
//!
//! ## 语义 (DECISION-K, 与 IpcStrategy 统一"逻辑错误降级原则")
//!
//! - **Option 可空**: `current_config_validate_hook()` 返回 `Option`, 未注册时
//!   config::init()/pci/net 自检**跳过校验 + 打日志** (validate 是启动增强, 不 panic,
//!   不进 freg 恢复流程)。
//! - **注册时序**: 注册在 framework `config::init()` 之前 (kernel_init 早期, lib.rs
//!   编排), services validate 依赖闭包轻可极早注册。
//!
//! 本文件 0 unsafe (纯 trait + OnceLock).

use super::error::ConfigError;

/// 配置自检策略接口 — services 实现, framework 启动校验调用
///
/// 所有方法为纯校验逻辑 (启动自检), 不涉及硬件操作或 unsafe.
pub trait ConfigValidateHook: Send + Sync {
    /// 全系统配置校验 (返回错误数)
    fn validate_system_config(&self) -> u32;

    /// 驱动子系统配置校验 (返回错误数)
    fn validate_drivers(&self) -> u32;

    /// PCI 子系统自检
    ///
    /// # Errors
    /// 当 PCI 子系统未初始化时返回 `Err(ConfigError::DriverConfigInvalid("pci"))`.
    fn validate_pci_subsystem(&self) -> Result<(), ConfigError>;

    /// 网络子系统自检 (当前为空校验)
    ///
    /// # Errors
    /// 当前实现总是返回 `Ok(())`.
    fn validate_network_subsystem(&self) -> Result<(), ConfigError>;
}

// ============================================================================
// 全局注册表
// ============================================================================

/// 全局配置自检策略注册表 — services 通过 `register_config_validate_hook` 注册
static CONFIG_VALIDATE_HOOK: crate::framework::sync::OnceLock<&'static dyn ConfigValidateHook> =
    crate::framework::sync::OnceLock::new();

/// 注册配置自检策略 (由 `services::config::validate_hook` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_config_validate_hook(
    hook: &'static dyn ConfigValidateHook,
) -> Result<(), &'static dyn ConfigValidateHook> {
    match CONFIG_VALIDATE_HOOK.set(hook) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的配置自检策略.
///
/// 未注册时返回 `None` — **DECISION-K**: validate 是启动增强, 未注册跳过校验 + 日志,
/// 不 panic (逻辑错误降级原则, 不进 freg 恢复流程).
pub fn current_config_validate_hook() -> Option<&'static dyn ConfigValidateHook> {
    CONFIG_VALIDATE_HOOK.get().copied()
}
