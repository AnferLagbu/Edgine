#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! # 全局统一 FD 分配器 (TD-02) — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原代码按统一判据"机制持有的数据结构/常量归 privileged"迁回
//! `privileged/proc/fd_alloc.rs` (全局 FD 编号为用户态可见内核机制, 被 privileged
//! 4 处 + functions inotify/pidfd 消费, 依赖闭包为空)。privileged/proc 的 fd_alloc
//! 为公开子模块 (`pub mod fd_alloc`), 可直接 glob re-export, 保持 functions 侧
//! API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::proc::fd_alloc::*;
