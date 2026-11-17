#![deny(unsafe_code)]
//! SHA-256 哈希实现 — functions 层 re-export 壳
//!
//! ## 归属记录
//!
//! 权威定义在 `privileged/crypto/sha256.rs` (S-2, 2026-11-17 子系统解耦工程:
//! 由 `privileged/sgeg/sha256.rs` 迁至中立位置 —— 纯算法不带安全策略语义).
//! 本文件仅 re-export, 保持 functions 内部消费者的既有路径兼容.

pub use crate::privileged::crypto::sha256::*;
