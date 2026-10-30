#![deny(unsafe_code)]
//! `EGFS` (Edgine File System) — functions 层完整实现
//!
//! 从 privileged 层迁移而来 (E6-6 阶段 2).
//! 所有业务逻辑在此; ARC 缓存经 `Arc<[u8]>` 共享句柄访问, privileged 侧无残留适配层.

/// 自适应替换缓存 (ARC): 缓冲区、键值与缓存状态管理.
pub mod arc;
/// ARC 的缓存接口抽象, 统一替换策略的调用面.
pub mod arc_trait;
/// 块指针: DVA、校验/压缩类型与出生事务组.
pub mod bp;
/// 校验和算法与统一的 `Checksum` 接口.
pub mod checksum;
/// 块压缩与解压的编解码实现.
pub mod compress;
/// 数据集的属性、状态与生命周期管理.
pub mod dataset;
/// 内容寻址 (CAS) 块去重.
pub mod dedup;
/// 数据管理单元 (DMU): 对象集、对象类型与块读写.
pub mod dmu;
/// 数据虚拟地址 (vdev + 偏移 + 长度) 的表示与编解码.
pub mod dva;
/// 文件描述符、打开模式与文件数据读写.
pub mod egfs_data;
/// inode 元数据: 属性与数据块引用.
pub mod egfs_inode;
/// 模块入口: 文件系统实例的挂载与统一访问 (文件名沿用 `egfs.rs`, 模块名取 `entry` 以避免
/// 模块名与父模块同名).
#[path = "egfs.rs"]
pub mod entry;
/// 分配空间单元 (metaslab): 空闲管理与统计.
pub mod metaslab;
/// RAID-Z 级别的数据/校验分条布局与重建.
pub mod raidz;
/// 快照的创建、查询与销毁.
pub mod snapshot;
/// 存储池抽象 (SPA): 配置、vdev 与事务组的聚合.
pub mod spa;
/// 事务组 (TXG): 写事务聚合与同步状态机.
pub mod txg;
/// 虚拟设备 (vdev): 底层设备状态与扇区读写.
pub mod vdev;
/// ZFS 属性处理器 (ZAP): 键值属性存储.
pub mod zap;
/// 写意图日志 (ZIL): 记录与重放.
pub mod zil;
/// ZIL 的 WAL 磁盘持久化层.
pub mod zil_persist;
