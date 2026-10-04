#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。re-export + 纯策略校验。
//! KASLR 配置 — functions 侧 re-export 兼容层 + 自检策略
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量与全局状态 (`KASLR_BASE_OFFSET`) 按统一判据"机制持有的数据结构/常量归
//! privileged"迁回 `privileged/config/kaslr.rs` (机制持有的全局状态, 被 privileged
//! config/caps 机制消费)。`validate_kaslr_offset` 为启动自检策略 (返回 functions
//! `KernelError`), 留在此处。privileged/config 的 kaslr 子模块为私有, 故经其顶层
//! re-export (`privileged::config::KASLR_*` 等) 显式转发, 保持 functions 侧 API 兼容
//! (functions→privileged 合法方向)。

use crate::functions::error::KernelError;

// privileged 顶层以 `is_kaslr_aligned` 别名暴露 `is_aligned`, 此处还原为 `is_aligned`
// 保持 functions 侧 API 兼容
pub use crate::privileged::config::{
    KASLR_ALIGN, KASLR_BASE_OFFSET, KASLR_DEFAULT_OFFSET, KASLR_ENABLED, KASLR_MAX_OFFSET,
    get_kaslr_offset, is_kaslr_aligned as is_aligned, set_kaslr_offset,
};

/// 校验运行时 KASLR 状态与配置的一致性.
///
/// 启动期自检:
/// - 当 `KASLR_ENABLED = true` 时, 实际偏移必须**非零**
/// - 偏移必须对齐到 `KASLR_ALIGN`
/// - 偏移必须不超过 `KASLR_MAX_OFFSET`
///
/// 返回错误细节 (供 validate 框架统一报告).
///
/// # Errors
/// 当偏移未对齐到 `KASLR_ALIGN` 或超过 `KASLR_MAX_OFFSET` 时返回
/// `Err(KernelError::InvalidArgument)`; 当 KASLR 已启用但实际偏移为 0 时返回
/// `Err(KernelError::NotInitialized)`.
pub fn validate_kaslr_offset() -> Result<(), KernelError> {
    let off = get_kaslr_offset();

    if !is_aligned(off) {
        return Err(KernelError::InvalidArgument);
    }
    if off > KASLR_MAX_OFFSET {
        return Err(KernelError::InvalidArgument);
    }
    if KASLR_ENABLED && off == 0 {
        return Err(KernelError::NotInitialized);
    }
    Ok(())
}
