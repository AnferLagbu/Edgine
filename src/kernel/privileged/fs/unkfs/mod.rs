//! `UNKFS` — privileged 侧机制支持
//!
//! 具象 UNKFS 实现 (ZFS 风格引擎, 29 文件) 归 `functions::fs::unkfs`;
//! privileged 挂载/格式化路径经 `backend_trait::unkfs_fs()` 消费
//! FileSystem trait object, 不再反向 re-export functions 子模块
//! (DECISION-K 项 6: 注入归零).
//!
//! 本模块仅保留:
//! - `arc_safe`: ARC 缓存裸指针→切片的 safe 封装 (框架层必要 unsafe),
//!   functions `arc.rs` 反向依赖本模块 (functions→privileged 合法方向)

pub mod arc_safe;
