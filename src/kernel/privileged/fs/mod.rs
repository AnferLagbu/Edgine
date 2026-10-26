//! 文件系统子系统
//!
//! ## 依赖声明
//!
//! privileged 内部依赖: syscall, sync, proc, sgeg, driver
//! functions 依赖: `functions::fs` (安全代理)
//!
//! ## 阶段 4b 变更
//!
//! VFS 具象实现 (vfs/ramfs/devfs/initramfs) 已下沉 `functions::fs`;
//! privileged 仅保留契约机制面 (`vfs` 下的 `ops_trait` / `types_pod`)
//! 与轮询策略契约 (`vfs_poll_trait`).

pub mod vfs;
pub mod vfs_poll_trait;

pub use vfs::*;
