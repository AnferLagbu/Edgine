#![deny(unsafe_code)]
//! I/O 子系统安全代理 — functions 层 (0 unsafe)
//!
//! 封装 `privileged::io` 的安全 API.

pub mod iouring;
