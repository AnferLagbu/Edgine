#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! 内核能力与配置摘要类型 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原类型定义 (`ConfigSummary`/`KernelCapabilities`) 按统一判据"机制持有的数据
//! 结构/常量归 privileged"迁回 `privileged/config/caps.rs` (被 privileged
//! mm/vmm_x86_64 KPTI 决策与 config 机制直接消费)。privileged/config 的 caps 子
//! 模块为私有, 故经其顶层 re-export (`privileged::config::ConfigSummary` 等) 显式
//! 转发, 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::config::{ConfigSummary, KernelCapabilities};
