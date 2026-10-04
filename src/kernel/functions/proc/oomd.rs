#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! OOMD — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义（2026-06-17 自 privileged 迁入）按统一判据迁回
//! `privileged/proc/oomd.rs` — OOMD 是 privileged scheduler tick 直接驱动的
//! 机制组件。glob re-export 保持 functions 侧 API 兼容。

pub use crate::privileged::proc::oomd::*;
