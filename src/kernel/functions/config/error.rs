#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! 配置校验结果类型 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! `ConfigError` 为 `ConfigValidateHook` trait 返回类型 (DECISION-K 项 2), 被
//! privileged config 机制直接消费, 迁回 `privileged/config/error.rs`。
//! privileged/config 的 error 子模块为私有, 故经其顶层 re-export
//! (`privileged::config::ConfigError`) 显式转发, 保持 functions 侧 API 兼容
//! (functions→privileged 合法方向)。

pub use crate::privileged::config::ConfigError;
