#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! exFAT 文件系统实现

pub mod alloc;
pub mod dir;
pub mod fat;
pub mod mount;
pub mod read;
pub mod super_block;
