/// B 方案拆分: 句柄操作 (open/close/read/write/...) 保留在本模块
pub mod api;
/// T-05: VFS 后端决策 trait
pub mod backend_trait;
pub mod dcache;
pub mod flock;
/// B 方案拆分: fd 句柄操作 (open/close/read/write/seek/...)
pub mod handle;
/// B09-12/P1-B3: Inode trait 完整定义 (从 services 迁回)
pub mod inode;
pub mod inotify;
/// B 方案拆分: 挂载/生命周期/同步/格式化 (从 api.rs 拆出)
pub mod mount;
/// B09-12/P1-B5: 全局 OpenFile 表 (从 services 迁回)
pub mod open_file_table;
/// B 方案拆分: 路径/目录/链接/元数据/cwd (从 api.rs 拆出)
pub mod path;
pub mod types;
pub mod vfs;

/// B09-12/P1-B3: Inode trait 顶层 re-export (framework::fs::Inode)
pub use inode::Inode;
/// B09-12/P1-B5: OpenFileTable 顶层 re-export (framework::fs::OPEN_FILE_TABLE)
pub use open_file_table::{OPEN_FILE_TABLE, OpenFileTable};
pub use types::*;
pub use vfs::*;

// 公共接口 re-export — 避免跨子系统直接访问内部子模块
pub use api::{
    vfs_chmod, vfs_chown, vfs_chown_ext, vfs_close, vfs_close_all_fds, vfs_close_cloexec_fds,
    vfs_dup, vfs_dup2, vfs_fchmod, vfs_fchown, vfs_fstat, vfs_get_cwd, vfs_get_fd_handle, vfs_link,
    vfs_mkdir, vfs_mount, vfs_mount_safe, vfs_open, vfs_pread, vfs_pread_inode, vfs_pwrite,
    vfs_read, vfs_read_internal, vfs_read_safe, vfs_readdir, vfs_readlink, vfs_rename, vfs_rmdir,
    vfs_seek, vfs_set_cwd, vfs_stat, vfs_stat_internal, vfs_symlink, vfs_sync,
    vfs_truncate_internal, vfs_umount, vfs_unlink, vfs_utimensat_safe, vfs_write,
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
