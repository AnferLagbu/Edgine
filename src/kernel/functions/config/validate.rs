#![deny(unsafe_code)]
//! 启动自检: 验证常量自洽性、内存布局、中断控制器 — functions 层策略实现
//!
//! ## 迁移记录
//!
//! 策略代码于 2026-06-17 从 privileged::config::validate 迁移至此。
//! privileged 层经 ConfigValidateHook trait 注入 (DECISION-K) 调用本模块策略。
//!
//! ## DECISION-J (2026-09-12)
//!
//! `ConfigError` 迁回 `privileged/config/error.rs` (ConfigValidateHook 返回类型);
//! `validate_*` 函数经 `ConfigValidateHook` 实现 (`DefaultConfigValidateHook`) 注册,
//! privileged config::init()/pci/net 自检经 trait 调用。

use crate::privileged::config::ConfigError;
use crate::privileged::config::{ConfigValidateHook, register_config_validate_hook};
use crate::privileged::config::{
    HUGE_PAGE_2M_SIZE, KERNEL_STACK_SIZE, MAX_CPUS, PAGE_SIZE, SLAB_DEFAULT_SIZE, USER_CODE_BASE,
    USER_STACK_GUARD, USER_STACK_SIZE, USER_STACK_TOP,
};
use crate::slog_err;

/// 校验 CPU 配置.
///
/// # Errors
/// 当实际 CPU 数量超过 `MAX_CPUS` 时返回 `Err(ConfigError::CpuCountExceedsMax { actual, max })`.
pub fn validate_cpu_config() -> Result<(), ConfigError> {
    let cpu_count = crate::privileged::smp::get_cpu_count();

    if cpu_count as usize > MAX_CPUS {
        return Err(ConfigError::CpuCountExceedsMax {
            actual: cpu_count,
            max: MAX_CPUS,
        });
    }

    Ok(())
}

/// 校验内存布局一致性.
///
/// # Errors
/// 当任一内存布局约束不满足 (页大小非 2 的幂、`SLAB_DEFAULT_SIZE` 非页大小整数倍、
/// 栈大小小于一页、`USER_STACK_TOP` 与代码基址重叠过近、`KERNEL_STACK_SIZE` 过小等) 时,
/// 返回 `Err(ConfigError::MemoryLayoutInvalid)`.
pub fn validate_memory_config() -> Result<(), ConfigError> {
    if PAGE_SIZE == 0 || (PAGE_SIZE & (PAGE_SIZE - 1)) != 0 {
        return Err(ConfigError::MemoryLayoutInvalid);
    }

    if !(SLAB_DEFAULT_SIZE as u64).is_multiple_of(PAGE_SIZE) {
        return Err(ConfigError::MemoryLayoutInvalid);
    }

    if USER_STACK_SIZE < PAGE_SIZE {
        return Err(ConfigError::MemoryLayoutInvalid);
    }
    if USER_STACK_GUARD > 16 * 1024 * 1024 {
        return Err(ConfigError::MemoryLayoutInvalid);
    }

    if USER_STACK_TOP <= USER_CODE_BASE + HUGE_PAGE_2M_SIZE {
        return Err(ConfigError::MemoryLayoutInvalid);
    }

    if (KERNEL_STACK_SIZE as u64) < PAGE_SIZE {
        return Err(ConfigError::MemoryLayoutInvalid);
    }

    Ok(())
}

/// 校验中断配置.
///
/// # Errors
#[cfg_attr(
    target_arch = "aarch64",
    expect(
        clippy::unnecessary_wraps,
        reason = "aarch64 上 APIC/IOAPIC 检查已禁用, 返回 Ok; x86_64 才真正检查"
    )
)]
/// 在 `x86_64` 上, 当 APIC 与 IOAPIC 均未初始化时返回
/// `Err(ConfigError::IrqControllerUnavailable)`.
pub fn validate_interrupt_config() -> Result<(), ConfigError> {
    #[cfg(target_arch = "x86_64")]
    {
        let apic_ok = crate::privileged::arch::apic::is_initialized();
        let ioapic_ok = crate::privileged::arch::ioapic::is_initialized();
        if !apic_ok && !ioapic_ok {
            return Err(ConfigError::IrqControllerUnavailable);
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // GIC 初始化失败已经在 arch 层 panic, 此处只需做软校验
    }

    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 跨模块一致性校验.
///
/// # Errors
/// 当前实现为空校验, 总是返回 `Ok(())`, 不会返回错误.
pub fn validate_cross_module_consistency() -> Result<(), ConfigError> {
    Ok(())
}

/// 验证 PCI 子系统已初始化.
///
/// # Errors
/// 当 PCI 子系统未初始化时返回 `Err(ConfigError::DriverConfigInvalid("pci"))`.
pub fn validate_pci_subsystem() -> Result<(), ConfigError> {
    if !crate::privileged::pci::is_initialized() {
        return Err(ConfigError::DriverConfigInvalid("pci"));
    }
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 验证网络子系统配置一致性.
///
/// # Errors
/// 当前实现为空校验, 总是返回 `Ok(())`, 不会返回错误.
pub fn validate_network_subsystem() -> Result<(), ConfigError> {
    Ok(())
}

/// 校验所有驱动配置.
pub fn validate_drivers() -> u32 {
    let mut errors = 0u32;

    match validate_pci_subsystem() {
        Ok(()) => {}
        Err(e) => {
            errors += 1;
            log_config_error(&e);
        }
    }

    match validate_network_subsystem() {
        Ok(()) => {}
        Err(e) => {
            errors += 1;
            log_config_error(&e);
        }
    }

    errors
}

/// 校验所有系统配置.
///
/// # Panics
/// 在 `debug_assertions` 构建下, 当 CPU 数量超过 `MAX_CPUS` 时触发 `panic!`,
/// 错误信息为 "CONFIG: CPU count {actual} exceeds `MAX_CPUS` {max}".
pub fn validate_system_config() -> u32 {
    let mut errors = 0u32;

    match validate_cpu_config() {
        Ok(()) => {}
        Err(e) => {
            errors += 1;
            log_config_error(&e);
            #[cfg(debug_assertions)]
            if let ConfigError::CpuCountExceedsMax { actual, max } = e {
                // 不可恢复: CPU 数量超过 MAX_CPUS 是配置错误, release 模式下仅 log,
                // debug 模式下必须停机以强制修正容量参数
                panic!(
                    "CONFIG: CPU count {actual} exceeds MAX_CPUS {max}. \
                     Increase MAX_CPUS in kernel/config/capacity.rs"
                );
            }
        }
    }

    match validate_memory_config() {
        Ok(()) => {}
        Err(e) => {
            errors += 1;
            log_config_error(&e);
        }
    }

    match validate_interrupt_config() {
        Ok(()) => {}
        Err(e) => {
            errors += 1;
            log_config_error(&e);
        }
    }

    match validate_cross_module_consistency() {
        Ok(()) => {}
        Err(e) => {
            errors += 1;
            log_config_error(&e);
        }
    }

    // KASLR 偏移自检
    if let Err(msg) = crate::functions::config::kaslr::validate_kaslr_offset() {
        errors += 1;
        slog_err!(Boot, "CONFIG: KASLR: {}", msg);
    }

    errors
}

#[inline]
fn log_config_error(e: &ConfigError) {
    slog_err!(Boot, "CONFIG: {}", e);
}

// ============================================================================
// ConfigValidateHook 实现 (DECISION-K 项 2: privileged 经 trait 调用启动自检)
// ============================================================================

/// 默认配置自检策略 — 包装本模块 `validate_*` 函数
pub struct DefaultConfigValidateHook;

impl ConfigValidateHook for DefaultConfigValidateHook {
    fn validate_system_config(&self) -> u32 {
        validate_system_config()
    }

    fn validate_drivers(&self) -> u32 {
        validate_drivers()
    }

    fn validate_pci_subsystem(&self) -> Result<(), ConfigError> {
        validate_pci_subsystem()
    }

    fn validate_network_subsystem(&self) -> Result<(), ConfigError> {
        validate_network_subsystem()
    }
}

/// 全局默认配置自检策略实例
static CONFIG_VALIDATE_HOOK: DefaultConfigValidateHook = DefaultConfigValidateHook;

/// 注册默认配置自检策略 (由 lib.rs 编排, privileged config::init() 之前注册)
///
/// 幂等性: 已注册时返回 `Err(())` (与 pmm/slab/swap policy 注册模式一致).
///
/// # Errors
/// 当自检策略已注册时返回 `Err(())`.
pub fn register_default_config_validate_hook() -> Result<(), ()> {
    register_config_validate_hook(&CONFIG_VALIDATE_HOOK).map_err(|_| ())
}
