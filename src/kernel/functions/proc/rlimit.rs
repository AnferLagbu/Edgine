#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! Per-process 资源限制 (rlimit) — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义（T1-8 于 2026-06-16 自 privileged 迁入）按统一判据迁回
//! `privileged/proc/rlimit.rs`（RlimitTable 是 Process 机制字段, syscall
//! 入口同时合并回该文件）。glob re-export 保持 functions 侧 API 兼容。

pub use crate::privileged::proc::rlimit::*;
