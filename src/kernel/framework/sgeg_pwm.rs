//! SGEG PWM FFI 安全代理 — framework TCB
//!
//! ## 职责
//!
//! 这是 services 层与 `kernel::sgeg::api::pwm_*` 之间的**唯一** unsafe 边界。
//! 所有 `unsafe { ... }` 块都集中在本模块处理, services 层 0 unsafe。
//!
//! ## 设计原则
//!
//! 1. 每个 `unsafe { ... }` 块都带 SAFETY 注释
//! 2. 切片 API (`&[u8]`) 替代 `*const u8` C 字符串
//! 3. 强类型 `i32` → `PwmError` 翻译在 services 层做, 本模块只透传 i32
//!
//! 评估日期: 2026-06-04

use crate::framework::sgeg;

// ============================================================================
// PWM 生命周期
// ============================================================================

/// 创世 (工厂: 第一个管理员)
///
/// # Safety
///
/// `password` 必须为有效非空切片, 调用期间不释放; 内核读取其内容到 NUL 终止的 C 字符串。
pub fn pwm_try_genesis(password: *const u8) -> i64 {
    sgeg::api::pwm_try_genesis(password)
}

/// 创世 + 创建 root 身份
///
/// # Safety
///
/// 同 `pwm_try_genesis`。
pub fn pwm_create_first_identity(password: *const u8) -> i64 {
    sgeg::api::pwm_create_first_identity(password)
}

/// 创建新身份
///
/// # Safety
///
/// `password` / `note` 必须为有效切片, 调用期间不释放; 内核会读取 NUL 终止的字节序列。
pub fn pwm_create(password: *const u8, note: *const u8, creator: u64) -> i64 {
    sgeg::api::pwm_create(password, note, creator)
}

/// 验证密码
///
/// # Safety
///
/// `password` 必须为有效非空切片, 调用期间不释放。
pub fn pwm_verify_password(pwm: u64, password: *const u8) -> bool {
    sgeg::api::pwm_verify_password(pwm, password)
}

/// 改密
///
/// # Safety
///
/// `old` / `new` 必须为有效非空切片, 调用期间不释放。
pub fn pwm_change_password(pwm: u64, old: *const u8, new: *const u8) -> i32 {
    sgeg::api::pwm_change_password(pwm, old, new)
}
