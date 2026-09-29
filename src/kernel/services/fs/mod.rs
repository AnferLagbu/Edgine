#![deny(unsafe_code)]
//! 文件系统 — services 层策略主体
//!
//! VFS Manager + Inode trait 抽象, services 侧原生 FS 模块: ramfs/tmpfs/overlayfs
//! /procfs/devfs/sysfs/systree/cgroupfs/configfs/devpts/anonymous 等, 以及
//! virtiofs/ext2/exfat/nestfs 共 15 个. Plan B 契约实现在 services::fs::inode.
//! 0 unsafe, 全部块设备/页缓存底层走 framework.
//!
//! 历史: 2026-06 之前 v2.5 状态评估已过时, 当前已远超当时范围. 详细
//! 进度见 docs/plan/progress-active-tasks.md.

pub mod access;
/// 匿名文件系统 (memfd 基础)
pub mod anonymous;
/// VFS 对外 API (syscall 边界, 汇聚 handle/vfs_mount/vfs_path)
pub mod api;
/// T-05: VFS 后端决策 trait
pub mod backend_trait;
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
/// fd 句柄操作 (open/close/read/write/seek/...)
pub mod handle;
/// initramfs 解包 (从 framework/fs 下沉)
pub mod initramfs;
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
pub mod procfs;
pub mod procfs_core;
pub mod ramfs;
/// ramfs 核心实现 (RamFsData / RamFsFileSystem / ramfs_fs)
pub mod ramfs_core;
pub mod sendfile;
/// 快照 (snapshot) 系统调用处理器
pub mod snapshot;
pub mod stat;
pub mod sysfs;
/// G9: 动态系统树
pub mod systree;
/// tmpfs 临时文件系统 (基于 ramfs 的内存文件系统)
pub mod tmpfs;
/// VFS 管理器 (挂载表 + FD 表 + 路径解析)
pub mod vfs_manager;
/// VFS 挂载/生命周期/同步/格式化 (原 framework/fs/vfs/mount.rs 下沉)
pub mod vfs_mount;
/// VFS 路径/目录/链接/元数据/cwd (原 framework/fs/vfs/path.rs 下沉)
pub mod vfs_path;
pub mod vfs_poll_policy;
/// T6-9: VFS 公共类型 (原 framework/fs/vfs/types.rs)
pub mod vfs_types;
pub mod virtiofs;
/// 扩展属性 (xattr) 系统调用处理器
pub mod xattr;

// ============================================================================
// T-05: VFS 后端决策策略
// ============================================================================

// 注: `FsBackend` / `register_fs_backend` / `register_nestfs_fs` / `Inode` /
// `FileSystem` / `KernelError` 由文件尾部顶层扁平 re-export 引入作用域, 此处
// 不再重复 `use`, 避免重名冲突 (E0252).
use crate::services::fs::api as vfs_api;

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
        fs_id: u32,
    ) -> Result<alloc::sync::Arc<dyn Inode>, KernelError> {
        // 具象 RamFsInode 归 services (DECISION-K 项 5): framework RamFsData
        // 经此工厂钩子请求 services 构造 Inode trait object.
        Ok(crate::services::fs::inode::new_ramfs_inode(
            inode_id, mount_idx, fs_id,
        ))
    }

    fn resolve_fs(&self, fs_name: &str) -> Option<&'static dyn FileSystem> {
        // services 文件系统注册表: 按 fs_type 名称返回对应 FileSystem 实例.
        // 新增文件系统仅需在此增加一行映射, framework 无需改动.
        match fs_name {
            "tmpfs" => Some(crate::services::fs::tmpfs::tmpfs_fs()),
            "overlay" => Some(crate::services::fs::overlayfs::overlay_fs()),
            _ => None,
        }
    }
}

/// services 层 VFS 操作契约实现 — framework 机制面经此访问 VFS 逻辑
///
/// 阶段 4b (DECISION-W): VFS 具象实现整体下沉 services 后, framework 侧的
/// 4 类消费点 (进程退出/execve/fork/epoll/mmap) 经 `VfsOps` 契约回呼本实现.
/// 所有方法转发到 services 本地 VFS 函数, 无 unsafe, 无 framework 内部状态.
pub struct ServicesVfsOps;

impl crate::framework::fs::VfsOps for ServicesVfsOps {
    fn release_pid_locks(&self, pid: u32) {
        // flock 与 POSIX 记录锁分属两套表, 进程退出时须全部释放
        flock_release_pid(pid);
        posix_lock_release_pid(pid);
    }

    fn close_all_fds(&self) {
        vfs_close_all_fds();
    }

    fn close_cloexec_fds(&self) {
        vfs_close_cloexec_fds();
    }

    fn inc_open_file_ref(&self, handle_id: u32) {
        OPEN_FILE_TABLE.inc_ref(handle_id);
    }

    fn fd_file_type(&self, fd: i32) -> Option<u8> {
        let handle_id = vfs_get_fd_handle(fd as usize)?;
        OPEN_FILE_TABLE.with_file(handle_id, |of| of.file_type)
    }

    fn pread_inode(
        &self,
        mount_idx: Option<usize>,
        inode_id: u32,
        offset: u64,
        dst: &mut [u8],
        pwm: u64,
    ) -> i32 {
        vfs_pread_inode(mount_idx, inode_id, offset, dst, pwm)
    }
}

static VFS_OPS_IMPL: ServicesVfsOps = ServicesVfsOps;

/// `services::fs` 初始化 — 注册策略到 framework (由 crate root 编排点调用)
///
/// 注册内容 (DECISION-K 项 6 注册点前置):
/// - FsBackend 挂载决策策略 (`register_fs_backend`)
/// - VFS poll 策略 (`register_default_vfs_poll_policy`)
/// - NestFS FileSystem 实例 (`register_nestfs_fs`) + 热插拔监听器
///   (`nestfs_hotplug_register`, HOTPLUG_MANAGER 为自足 static, 时序仅要求
///   早于首个热插拔中断事件 — kernel_init 早期注册满足)
/// - VfsOps 契约实现 (`register_vfs_ops`, 阶段 4b: framework 消费点回呼入口)
pub fn init() {
    static POLICY: ServicesFsBackend = ServicesFsBackend;
    let _ = register_fs_backend(&POLICY);
    let _ = crate::services::fs::vfs_poll_policy::register_default_vfs_poll_policy();
    let _ = register_nestfs_fs(crate::services::fs::nestfs::nestfs::get_nestfs());
    crate::services::fs::nestfs::nestfs::nestfs_hotplug_register();
    let _ = crate::framework::fs::register_vfs_ops(&VFS_OPS_IMPL);
}

// ============================================================================
// 顶层扁平 re-export — 镜像原 framework/fs 表面, 保持调用方路径兼容
// (清单源自原 framework/fs/vfs/mod.rs 的 re-export, 现指向 services 本地实现;
//  `types::*` → `vfs_types::*`, `vfs::*` → `vfs_manager::*`)
// ============================================================================

/// Plan B: Inode trait 顶层 re-export
pub use inode::Inode;
/// 全局 OpenFile 表顶层 re-export
pub use open_file_table::{OPEN_FILE_TABLE, OpenFileTable};
pub use vfs_manager::*;
pub use vfs_types::*;

// 公共接口 re-export — 避免跨子系统直接访问内部子模块
pub use api::{
    vfs_chmod, vfs_chown, vfs_chown_ext, vfs_close, vfs_close_all_fds, vfs_close_cloexec_fds,
    vfs_close_safe,
    vfs_dup, vfs_dup2, vfs_fchmod, vfs_fchown, vfs_fstat, vfs_get_cwd, vfs_get_fd_handle, vfs_link,
    vfs_mkdir, vfs_mount, vfs_mount_safe, vfs_open, vfs_open_safe, vfs_pread, vfs_pread_inode,
    vfs_pwrite,
    vfs_read, vfs_read_internal, vfs_read_safe, vfs_readdir, vfs_readlink, vfs_rename, vfs_rmdir,
    vfs_seek, vfs_set_cwd, vfs_stat, vfs_stat_internal, vfs_symlink, vfs_sync,
    vfs_truncate_internal, vfs_umount, vfs_unlink, vfs_unlink_safe, vfs_utimensat_safe, vfs_write,
    vfs_write_internal, vfs_write_pod, vfs_write_safe,
};
pub use flock::{
    F_GETLK, F_SETLK, F_SETLKW, FlockResult, PosixLockConflict, PosixLockResult, flock_release_fd,
    flock_release_pid, posix_lock_release_pid, sys_flock, sys_posix_lock,
};
pub use inotify::{
    inotify_release, is_inotify_fd, sys_inotify_add_watch, sys_inotify_init1, sys_inotify_read,
    sys_inotify_rm_watch,
};
// T-05: 后端决策策略 re-export
pub use backend_trait::{
    FallbackFsBackend, FsBackend, current_fs_backend, nestfs_fs, register_fs_backend,
    register_nestfs_fs,
};
