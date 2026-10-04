#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! 进程类型定义 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义（T6-2 于 2026-06-16 自 privileged 迁入）按统一判据"机制持有的
//! 数据结构/常量归 privileged"迁回 `privileged/proc/types.rs` — 进程核心
//! 类型被 privileged user_proc/process/scheduler 消费。privileged/proc 的
//! types 为 `pub mod`, 直接 glob re-export, 保持 functions 侧 API 兼容
//! (functions→privileged 合法方向)。

pub use crate::privileged::proc::types::*;
