#![deny(unsafe_code)]
//! SHA-256 哈希实现 — functions 层 re-export 壳
//!
//! ## 归属记录
//!
//! 纯算法实现 (SHA-256 哈希, 安全原语) 的权威定义在
//! `privileged/sgeg/sha256.rs` (第二十五批反转归位, DECISION-K 项 5 sgeg
//! 判据 — privileged/sgeg/secure_boot 直接消费). 本文件仅 re-export 保持
//! functions 内部消费者路径兼容.

pub use crate::privileged::sgeg::sha256::*;
