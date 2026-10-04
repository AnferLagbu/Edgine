#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export。
//! 系统容量常量 — functions 侧 re-export 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量定义 (MAX_CPUS/MAX_IRQS/MAX_PROCESSES 等) 按统一判据"机制持有的数据
//! 结构/常量归 privileged"迁回 `privileged/config/capacity.rs` (被 privileged
//! smp/cpu_local/rcu/irq 机制直接消费)。privileged/config 子模块为私有, 故经
//! 其顶层 re-export (`privileged::config::MAX_CPUS` 等) 显式转发, 保持 functions
//! 侧 API 兼容 (functions→privileged 合法方向)。

pub use crate::privileged::config::{
    MAX_CPUS, MAX_IRQS, MAX_OPEN_FILES, MAX_PROCESSES, MAX_SESSIONS, MAX_THREADS,
    MAX_THREADS_PER_PROCESS,
};
