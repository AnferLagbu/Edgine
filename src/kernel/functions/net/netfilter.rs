#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! Netfilter 包过滤框架 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原策略代码按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/net/netfilter.rs` (`sys_nf_*` 被 privileged syscall dispatch
//! 机制直接调用, 依赖闭包全在 privileged 内)。privileged/net 的 netfilter 为
//! 公开子模块 (`pub mod netfilter`), 可直接 glob re-export, 保持 functions 侧
//! API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::net::netfilter::*;
