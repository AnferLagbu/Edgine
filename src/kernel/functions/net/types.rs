#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! 网络子系统公共类型 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量与全局状态按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/net/types.rs` (被 privileged net init/dns 机制直接消费)。
//! privileged/net 的 types 为公开子模块 (`pub mod types`), 可直接 glob
//! re-export, 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::net::types::*;
