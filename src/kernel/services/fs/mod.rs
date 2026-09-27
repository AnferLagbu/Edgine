#![deny(unsafe_code)]
//! 文件系统 — services 层策略主体
//!
//! VFS Manager + Inode trait 抽象. 7 个原生 FS (ramfs/devfs/procfs/ext2/
//! exfat/tmpfs/overlayfs) + NestFS 在 services::fs::inode 实现 Plan B 契约.
//! 0 unsafe, 全部块设备/页缓存底层走 framework.
//!
//! 历史: 2026-06 之前 v2.5 状态评估已过时, 当前已远超当时范围. 详细
//! 进度见 docs/plan/progress-active-tasks.md.

pub mod access;
/// 匿名文件系统 (memfd 基础)
pub mod anonymous;
pub mod cgroupfs;
pub mod configfs;
pub mod dcache;
pub mod devfs;
/// G8: 容器辅助文件系统
pub mod devpts;
/// 目录与定位操作策略 — lseek / getdents
pub mod dir_ops;
pub mod exfat;
pub mod ext2;
/// 文件句柄系统 (name_to_handle_at / open_by_handle_at)
pub mod file_handle;
/// 文件操作策略 — ioctl / clock_gettime / poll / chown / truncate / flock
pub mod file_ops;
pub mod flock;
/// Plan B: Inode trait — 文件级操作抽象
pub mod inode;
pub mod inotify;
pub mod io;
pub mod link;
pub mod misc;
pub mod mode;
pub mod mount;
pub mod nestfs;
pub mod open;
/// 全局 OpenFile 表 (POSIX 打开文件描述)
pub mod open_file_table;
/// overlayfs 联合文件系统 (upperdir/lowerdir 合并视图)
pub mod overlayfs;
pub mod path;
/// Per-process FD 表
pub mod process_fd_table;
pub mod procfs;
pub mod procfs_core;
pub mod ramfs;
pub mod sendfile;
/// 快照 (snapshot) 系统调用处理器
pub mod snapshot;
pub mod stat;
pub mod sysfs;
/// G9: 动态系统树
pub mod systree;
/// VFS 管理器 (挂载表 + FD 表 + 路径解析)
pub mod vfs_manager;
pub mod vfs_poll_policy;
/// T6-9: VFS 公共类型 (原 framework/fs/vfs/types.rs)
pub mod vfs_types;
pub mod virtiofs;
/// 扩展属性 (xattr) 系统调用处理器
pub mod xattr;

// ============================================================================
// T-05: VFS 后端决策策略
// ============================================================================

use crate::framework::fs::vfs::api as vfs_api;
use crate::framework::fs::vfs::backend_trait::{
    FsBackend, register_fs_backend, register_nestfs_fs,
};
use crate::framework::fs::vfs::inode::Inode;
use crate::services::fs::vfs_types::KernelError;

/// services 层 VFS 后端决策策略
///
/// 维护文件系统注册表, 根据 `fs_type` 名称选择挂载方式.
/// 挂载权限: 当前允许所有挂载请求 (未来可扩展为权限检查).
pub struct ServicesFsBackend;

impl FsBackend for ServicesFsBackend {
    fn mount_fs(&self, fs_name: &str, path: &str) -> Result<(), KernelError> {
        // services 根据 fs_name 选择挂载方式并调用 framework safe API
        let rc = vfs_api::vfs_mount_safe(path, fs_name);
        if rc == 0 {
            Ok(())
        } else {
            // framework 层返回负 errno，转换为精确的 KernelError
            Err(KernelError::from_i32(-rc))
        }
    }

    fn allow_mount(&self, _path: &str, _fs_name: &str) -> bool {
        // 当前允许所有挂载; 未来可按路径/fs_type 做权限检查
        true
    }

    fn make_ramfs_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
    ) -> Result<alloc::sync::Arc<dyn Inode>, KernelError> {
        // 具象 RamFsInode 归 services (DECISION-K 项 5): framework RamFsData
        // 经此工厂钩子请求 services 构造 Inode trait object.
        Ok(crate::services::fs::inode::new_ramfs_inode(
            inode_id, mount_idx,
        ))
    }
}

/// `services::fs` 初始化 — 注册策略到 framework (由 crate root 编排点调用)
///
/// 注册内容 (DECISION-K 项 6 注册点前置):
/// - FsBackend 挂载决策策略 (`register_fs_backend`)
/// - VFS poll 策略 (`register_default_vfs_poll_policy`)
/// - NestFS FileSystem 实例 (`register_nestfs_fs`) + 热插拔监听器
///   (`nestfs_hotplug_register`, HOTPLUG_MANAGER 为自足 static, 时序仅要求
///   早于首个热插拔中断事件 — kernel_init 早期注册满足)
pub fn init() {
    static POLICY: ServicesFsBackend = ServicesFsBackend;
    let _ = register_fs_backend(&POLICY);
    let _ = crate::services::fs::vfs_poll_policy::register_default_vfs_poll_policy();
    let _ = register_nestfs_fs(crate::services::fs::nestfs::nestfs::get_nestfs());
    crate::services::fs::nestfs::nestfs::nestfs_hotplug_register();
}
