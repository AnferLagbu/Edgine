#![deny(unsafe_code)]
//! SGEG v1 类型定义 — services 层 re-export 壳
//!
//! ## 归属记录
//!
//! 纯数据定义 (PWM 类型/能力矩阵/身份条目/审计类型) 的权威定义在
//! `framework/sgeg/types.rs` (第二十五批反转归位, DECISION-K 项 5 sgeg
//! 判据 — framework/proc/process.rs 进程表机制持有 PwmContext). 本文件仅
//! re-export 保持 services 内部消费者路径兼容.

pub use crate::framework::sgeg::types::*;
