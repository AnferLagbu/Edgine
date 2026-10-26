#![deny(unsafe_code)]
//! `UNKFS` (Hypervisor File System) — functions 层完整实现
//!
//! 从 privileged 层迁移而来 (E6-6 阶段 2).
//! 所有业务逻辑在此; ARC 缓存经 `Arc<[u8]>` 共享句柄访问, privileged 侧无残留适配层.

pub mod arc;
pub mod arc_trait;
pub mod bp;
pub mod checksum;
pub mod compress;
pub mod dataset;
pub mod dedup;
pub mod dmu;
pub mod dva;
pub mod metaslab;
pub mod raidz;
pub mod snapshot;
pub mod spa;
pub mod txg;
pub mod unkfs;
pub mod unkfs_data;
pub mod unkfs_inode;
pub mod vdev;
pub mod zap;
pub mod zil;
pub mod zil_persist;
