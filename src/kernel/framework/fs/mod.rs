//! 文件系统子系统
//!
//! ## 依赖声明
//!
//! framework 内部依赖: syscall, sync, proc, sgeg, driver
//! services 依赖: `services::fs` (安全代理)
//!
//! ## 阶段 4b 变更
//!
//! VFS 具象实现 (vfs/ramfs/devfs/initramfs) 已下沉 `services::fs`;
//! framework 仅保留契约机制面 (`vfs` 下的 `ops_trait` / `types_pod`)、
//! 轮询策略契约 (`vfs_poll_trait`) 与 unkfs 的 unsafe 机制适配层 (`unkfs`).

pub mod unkfs;
pub mod vfs;
pub mod vfs_poll_trait;

pub use vfs::*;
