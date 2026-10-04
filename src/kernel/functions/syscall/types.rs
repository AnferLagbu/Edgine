#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! Syscall 类型定义和常量 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义（T5-4 于 2026-06-16 自 privileged 迁入）按统一判据"机制持有的
//! 数据结构/常量归 privileged"迁回 `privileged/syscall/types.rs` — syscall
//! 编号表是用户态 ABI 机制 (编号空间分配 DECISION-037 承载物), 被
//! privileged dispatch/dispatch_trait 消费, 依赖闭包为空。privileged/syscall
//! 的 types 为 `pub mod`, 直接 glob re-export, 保持 functions 侧 API 兼容
//! (functions→privileged 合法方向)。

pub use crate::privileged::syscall::types::*;
