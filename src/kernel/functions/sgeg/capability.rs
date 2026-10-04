#![deny(unsafe_code)]
//! PWM v5 能力定义 — functions 层 re-export 壳
//!
//! ## 归属记录
//!
//! 纯常量定义 (16 域能力位 + viable floor) 的权威定义在
//! `privileged/sgeg/capability.rs` (第二十五批反转归位, DECISION-K 项 5
//! sgeg 判据). 本文件仅 re-export 保持 functions 内部消费者路径兼容.

pub use crate::privileged::sgeg::capability::*;
