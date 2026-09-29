//! VFS 契约机制面 — 阶段 4b 后 framework 仅保留机制
//!
//! ## 阶段 4b 变更
//!
//! VFS 具象实现 (vfs/api/backend_trait/dcache/flock/handle/inotify/mount/
//! open_file_table/path/types/inode) 已整体下沉 `services::fs`; framework
//! 本模块仅保留:
//! - `ops_trait`: VFS 操作契约 trait (阶段 4a, DECISION-W)
//! - `types_pod`: framework 消费所需的文件类型 POD

/// T-05 阶段 4a: VFS 操作契约 trait (DECISION-W)
pub mod ops_trait;
/// 阶段 4b: VFS 文件类型 POD (framework 保留, 其余类型下沉 services)
pub mod types_pod;

pub use ops_trait::{FallbackVfsOps, VfsOps, current_vfs_ops, register_vfs_ops};
pub use types_pod::VfsFileType;
