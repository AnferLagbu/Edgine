#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! Slab 分配器配置常量 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/config/slab.rs` (被 privileged mm/slab 机制直接消费)。
//! privileged/config 的 slab 子模块为私有, 故经其顶层 re-export
//! (`privileged::config::SLAB_*`) 显式转发, 保持 functions 侧 API 兼容
//! (functions→privileged 合法方向)。

pub use crate::privileged::config::{
    SLAB_DEFAULT_SIZE, SLAB_GENERAL_CACHE_NUM, SLAB_MAX_OBJECT_SIZE, SLAB_MIN_OBJECT_SIZE,
};
