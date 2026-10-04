#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! 同步原语数据类型定义 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原类型定义 (锁状态/守卫/统计) 按统一判据"机制持有的数据结构/常量归
//! privileged"迁回 `privileged/sync/types.rs` (被 privileged sync 机制
//! spinlock/rwlock/mutex FFI 层直接消费, 与 C 版本布局兼容)。
//! privileged/sync 的 types 为公开子模块 (`pub mod types`), 可直接 glob
//! re-export, 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::sync::types::*;
