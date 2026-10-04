#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! madvise / mlock / mincore 系统调用实现 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原策略代码按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/proc/madvise_mlock.rs` (依赖闭包全在 privileged 内)。
//! privileged/proc 的 madvise_mlock 为公开子模块, 可直接 glob re-export,
//! 保持 functions 侧 API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::proc::madvise_mlock::*;
