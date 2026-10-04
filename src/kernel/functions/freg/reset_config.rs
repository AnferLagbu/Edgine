#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! 恢复配置与类型定义 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原配置/状态/统计按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/freg/reset/config.rs` (被 privileged proc/scheduler 机制直接
//! 消费)。privileged/freg/reset 的 config 为公开子模块 (`pub mod config`),
//! 可直接 glob re-export, 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::freg::reset::config::*;
