#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码. 反向 re-export 仅为兼容旧调用路径.
//! UserContext — 用户态 CPU 寄存器快照
//!
//! ## DECISION-039 迁移记录 (2026-08-03)
//!
//! 类型定义于 2026-08-03 迁回 `privileged::userctx` (按 I3 不变式).
//! 本文件保留反向 re-export, 历史调用路径 (`functions::userctx::UserContext`)
//! 继续可用. 新代码应直接 `use crate::privileged::userctx::UserContext`.

pub use crate::privileged::userctx::*;
