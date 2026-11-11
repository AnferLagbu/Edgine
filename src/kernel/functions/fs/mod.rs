#![deny(unsafe_code)]
//! 文件系统 — functions 层策略主体
//!
//! VFS Manager + Inode trait 抽象, functions 侧原生 FS 模块: ramfs/tmpfs/overlayfs
//! /procfs/devfs/sysfs/systree/cgroupfs/configfs/devpts/anonymous 等, 以及
//! virtiofs/ext2/exfat/egfs 共 15 个. Plan B 契约实现在 functions::fs::inode.
//! 0 unsafe, 全部块设备/页缓存底层走 privileged.
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
pub mod egfs;
/// 统一 fd 对象模型: eventfd 事件计数器对象
pub mod eventfd;
pub mod exfat;
pub mod ext2;
/// 统一 fd 对象接口 (`File` trait) + 文件系统薄适配器
pub mod file;
/// 文件句柄系统 (name_to_handle_at / open_by_handle_at)
pub mod file_handle;
/// 文件操作策略 — ioctl / clock_gettime / poll / chown / truncate / flock
pub mod file_ops;
pub mod flock;
/// fd 句柄操作 (open/close/read/write/seek/...)
pub mod handle;
/// initramfs 解包 (从 privileged/fs 下沉)
pub mod initramfs;
/// Plan B: Inode trait — 文件级操作抽象
pub mod inode;
pub mod inotify;
pub mod io;
pub mod link;
pub mod misc;
pub mod mode;
pub mod mount;
pub mod open;
/// 全局 OpenFile 表 (POSIX 打开文件描述)
pub mod open_file_table;
/// overlayfs 联合文件系统 (upperdir/lowerdir 合并视图)
pub mod overlayfs;
pub mod path;
/// 匿名管道端点 fd — 管道纳入 VFS fd 对象模型
pub mod pipe_fd;
pub mod procfs;
pub mod procfs_core;
pub mod ramfs;
/// ramfs 核心实现 (RamFsData / RamFsFileSystem / ramfs_fs)
pub mod ramfs_core;
pub mod sendfile;
/// 统一 fd 对象模型: signalfd 信号接收对象
pub mod signalfd;
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
/// VFS 挂载/生命周期/同步/格式化 (原 privileged/fs/vfs/mount.rs 下沉)
pub mod vfs_mount;
/// VFS 路径/目录/链接/元数据/cwd (原 privileged/fs/vfs/path.rs 下沉)
pub mod vfs_path;
pub mod vfs_poll_policy;
/// T6-9: VFS 公共类型 (原 privileged/fs/vfs/types.rs)
pub mod vfs_types;
pub mod virtiofs;
/// 扩展属性 (xattr) 系统调用处理器
pub mod xattr;

// ============================================================================
// T-05: VFS 后端决策策略
// ============================================================================

// 注: `FsBackend` / `register_fs_backend` / `register_egfs_fs` / `Inode` /
// `FileSystem` / `KernelError` 由文件尾部顶层扁平 re-export 引入作用域, 此处
// 不再重复 `use`, 避免重名冲突 (E0252).
use crate::functions::fs::api as vfs_api;

/// functions 层 VFS 后端决策策略
///
/// 维护文件系统注册表, 根据 `fs_type` 名称选择挂载方式.
/// 挂载权限: 当前允许所有挂载请求 (未来可扩展为权限检查).
pub struct FunctionsFsBackend;

impl FsBackend for FunctionsFsBackend {
    fn mount_fs(&self, fs_name: &str, path: &str) -> Result<(), KernelError> {
        // functions 根据 fs_name 选择挂载方式并调用 privileged safe API
        let rc = vfs_api::vfs_mount_safe(path, fs_name);
        if rc == 0 {
            Ok(())
        } else {
            // privileged 层返回负 errno，转换为精确的 KernelError
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
        // 具象 RamFsInode 归 functions (DECISION-K 项 5): privileged RamFsData
        // 经此工厂钩子请求 functions 构造 Inode trait object.
        Ok(crate::functions::fs::inode::new_ramfs_inode(
            inode_id, mount_idx, fs_id,
        ))
    }

    fn resolve_fs(&self, fs_name: &str) -> Option<&'static dyn FileSystem> {
        // functions 文件系统注册表: 按 fs_type 名称返回对应 FileSystem 实例.
        // 新增文件系统仅需在此增加一行映射, privileged 无需改动.
        match fs_name {
            "tmpfs" => Some(crate::functions::fs::tmpfs::tmpfs_fs()),
            "overlay" => Some(crate::functions::fs::overlayfs::overlay_fs()),
            _ => None,
        }
    }
}

/// functions 层 VFS 操作契约实现 — privileged 机制面经此访问 VFS 逻辑
///
/// 阶段 4b (DECISION-W): VFS 具象实现整体下沉 functions 后, privileged 侧的
/// 4 类消费点 (进程退出/execve/fork/epoll/mmap) 经 `VfsOps` 契约回呼本实现.
/// 所有方法转发到 functions 本地 VFS 函数, 无 unsafe, 无 privileged 内部状态.
pub struct FunctionsVfsOps;

impl crate::privileged::fs::VfsOps for FunctionsVfsOps {
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

    fn dec_open_file_ref(&self, handle_id: u32) {
        OPEN_FILE_TABLE.dec_ref(handle_id);
    }

    fn fd_file_type(&self, fd: i32) -> Option<u8> {
        let handle_id = vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))?;
        OPEN_FILE_TABLE.with_file(handle_id, |of| of.file_type)
    }

    fn fd_poll_special(&self, fd: i32, events: u32) -> Option<u32> {
        let handle_id = vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))?;
        OPEN_FILE_TABLE
            .with_file(handle_id, |of| {
                // inode 承载对象走 `fd_file_type` + VfsPollPolicy, 不在此处理
                if of.as_inode().is_some() {
                    return None;
                }
                let requested =
                    PollEvents::from_bits_truncate(crate::functions::intcast::low_i16_from_i32(
                        crate::functions::intcast::low_i32_from_u32(events),
                    ));
                Some(crate::functions::intcast::u32_from_i16(
                    of.file().poll(requested).bits(),
                ))
            })
            .flatten()
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

static VFS_OPS_IMPL: FunctionsVfsOps = FunctionsVfsOps;

/// `functions::fs` 初始化 — 注册策略到 privileged (由 crate root 编排点调用)
///
/// 注册内容 (DECISION-K 项 6 注册点前置):
/// - `FsBackend` 挂载决策策略 (`register_fs_backend`)
/// - VFS poll 策略 (`register_default_vfs_poll_policy`)
/// - EGFS `FileSystem` 实例 (`register_egfs_fs`) + 热插拔监听器
///   (`egfs_hotplug_register`, `HOTPLUG_MANAGER` 为自足 static, 时序仅要求
///   早于首个热插拔中断事件 — `kernel_init` 早期注册满足)
/// - `VfsOps` 契约实现 (`register_vfs_ops`, 阶段 4b: privileged 消费点回呼入口)
pub fn init() {
    static POLICY: FunctionsFsBackend = FunctionsFsBackend;
    let _ = register_fs_backend(&POLICY);
    let _ = crate::functions::fs::vfs_poll_policy::register_default_vfs_poll_policy();
    let _ = register_egfs_fs(crate::functions::fs::egfs::entry::get_egfs());
    crate::functions::fs::egfs::entry::egfs_hotplug_register();
    let _ = crate::privileged::fs::register_vfs_ops(&VFS_OPS_IMPL);
}

// ============================================================================
// 顶层扁平 re-export — 镜像原 privileged/fs 表面, 保持调用方路径兼容
// (清单源自原 privileged/fs/vfs/mod.rs 的 re-export, 现指向 functions 本地实现;
//  `types::*` → `vfs_types::*`, `vfs::*` → `vfs_manager::*`)
// ============================================================================

/// 统一 fd 对象接口顶层 re-export
pub use file::{File, MmapBacking, PollEvents, RegularFile};
/// Plan B: Inode trait 顶层 re-export
pub use inode::Inode;
/// 全局 `OpenFile` 表顶层 re-export
pub use open_file_table::{OPEN_FILE_TABLE, OpenFileTable};
pub use vfs_manager::*;
pub use vfs_types::*;

// 公共接口 re-export — 避免跨子系统直接访问内部子模块
pub use api::{
    vfs_chmod, vfs_chown, vfs_chown_ext, vfs_close, vfs_close_all_fds, vfs_close_cloexec_fds,
    vfs_close_safe, vfs_dup, vfs_dup2, vfs_fchmod, vfs_fchown, vfs_fstat, vfs_get_cwd,
    vfs_get_fd_handle, vfs_link, vfs_mkdir, vfs_mount, vfs_mount_safe, vfs_open, vfs_open_safe,
    vfs_pread, vfs_pread_inode, vfs_pwrite, vfs_read, vfs_read_internal, vfs_read_kernel,
    vfs_read_safe, vfs_readdir, vfs_readlink, vfs_rename, vfs_rmdir, vfs_seek, vfs_set_cwd,
    vfs_stat, vfs_stat_internal, vfs_symlink, vfs_sync, vfs_truncate_internal, vfs_umount,
    vfs_unlink, vfs_unlink_safe, vfs_utimensat_safe, vfs_write, vfs_write_internal,
    vfs_write_kernel, vfs_write_pod, vfs_write_safe,
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
    FallbackFsBackend, FsBackend, current_fs_backend, egfs_fs, register_egfs_fs,
    register_fs_backend,
};

/// 全局 FS 单例 / 进程-调度器态测试互斥锁
///
/// 以下用例共享全局可变状态, 而 cargo test runner 默认并行执行 ⇒ 必须串行化:
/// - `with_temp_process` (`functions::fs` 测试) 改写全局 `SCHEDULER.current`
///   与 `PROCESS_TABLE`;
/// - `ramfs_core::init()` 内部 `RamFsData::mount` 会清空整个 `RAMFS_DATA`
///   (节点/数据区/位图 `fill`), 并行调用会抹掉其他用例刚建的文件;
/// - `functions::proc::signal` 的临时屏蔽字用例读取当前 pid 并改写其屏蔽字,
///   若与 `with_temp_process` 并行, 可能读到正在拆除的临时进程 ⇒ use-after-free.
/// 口径参照 `privileged::timer::tick::FREQ_TEST_LOCK` (D 类并行隔离先例).
#[cfg(test)]
pub(crate) static FS_GLOBAL_TEST_LOCK: crate::privileged::sync::IrqSpinLock<()> =
    crate::privileged::sync::IrqSpinLock::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时安装当前进程 (含独立 FdTable), 执行闭包后无条件拆除现场
    ///
    /// B-9.5: fd 分配已下沉到 per-process `FdTable` (`with_current_fd_table`),
    /// 依赖当前进程上下文. host 侧共享测试集无当前进程 ⇒ open 恒失败.
    /// 安装/拆除模式参照 [test_proc.rs] 的 `test_fork_cow_failure_rolls_back`.
    fn with_temp_process<F: FnOnce()>(name: &str, f: F) {
        use crate::privileged::proc::PROCESS_TABLE;
        use crate::privileged::proc::raw;
        use crate::privileged::proc::scheduler::Scheduler;

        let Some(pid) = PROCESS_TABLE.allocate_pid() else {
            panic!("临时进程: 无空闲 pid");
        };
        let proc_ptr = raw::alloc_process(pid, name, None);
        if !PROCESS_TABLE.insert(proc_ptr) {
            raw::drop_boxed_process(proc_ptr);
            PROCESS_TABLE.free_pid(pid);
            panic!("临时进程: 插入进程表失败");
        }

        let prev_current = Scheduler::current();
        Scheduler::set_current(pid);

        f();

        Scheduler::set_current(prev_current.unwrap_or(0));
        let _ = PROCESS_TABLE.remove(pid);
        raw::drop_boxed_process(proc_ptr);
        PROCESS_TABLE.free_pid(pid);
    }

    /// T5 甲批 C-1 接线证据: 真实 open 路径 (`open_syscall` → `vfs_open` →
    /// `vfs_open_internal`) 必须把 fd 表元数据写全, 否则
    /// (flock 的 ino / mmap-by-fd 的映射后端凭据) 恒为空, 挂载点反查失败.
    ///
    /// B-9.5: 元数据源已由全局 `VfsManager.fd_table` 改源为 per-process fd 表
    /// (fd → `OpenFileTable` handle) + `OpenFile` 自身元数据.
    #[test]
    fn test_open_populates_fd_metadata() {
        use crate::functions::fs::ramfs_core::{RAMFS_DATA, init as ramfs_init};
        use crate::functions::fs::{OPEN_FILE_TABLE, api, vfs_get_fd_handle};

        let _lock = FS_GLOBAL_TEST_LOCK.lock();
        crate::functions::fs::init();
        ramfs_init();
        // 必须走真实挂载入口 (挂 trait object): `VFS_MANAGER.mount` 只登记 fs_type,
        // `resolve_mount_fs` 的 `fs` 仍为 None, vfs_open_internal 直接返回 NotSupported.
        // boot / host 均已挂载 "/" 时返回负值, 忽略即可.
        let _ = api::vfs_mount_safe("/", "ramfs");

        with_temp_process("fd-meta-t", || {
            let created = {
                let mut ramfs = RAMFS_DATA.lock();
                ramfs.create_file("/", "fd_meta_t", 0)
            };
            let Some(node_id) = created else {
                panic!("create_file 失败");
            };
            assert!(node_id != 0, "inode 编号不应为 0 (0 是未填充哨兵)");

            let fd = api::vfs_open_safe("/fd_meta_t", 0, 0);
            assert!(fd >= 0, "open /fd_meta_t 应成功");

            let Some(handle_id) = vfs_get_fd_handle(fd as usize) else {
                panic!("per-process fd 表应含该 fd 条目");
            };
            let Some((fd_node_id, backing)) = OPEN_FILE_TABLE
                .with_file(handle_id, |of| (of.inode_id(), of.file().mmap_backing()))
            else {
                panic!("OpenFile 表应含该 handle");
            };
            assert_eq!(
                fd_node_id,
                Some(node_id),
                "fd 元数据 node_id 应为真实 inode (元数据未接线时为 None)"
            );
            let Some(backing) = backing else {
                panic!("常规文件必须声明文件映射后端 (未接线时为 None ⇒ mmap 恒 ENODEV)");
            };
            assert_eq!(
                backing.inode_id, node_id,
                "映射后端 inode 应为真实 inode (mmap-by-fd 依赖)"
            );
            assert!(
                backing.mount_idx.is_some(),
                "映射后端应携带挂载点索引, 可反查 (mmap-by-fd 依赖)"
            );

            assert_eq!(api::vfs_close_safe(fd as u32), 0, "close 应成功");
        });
    }

    /// T5 乙批（C-1 证据补齐）: `set_fd` 接线的**下游链路**验证 —— 证明修的是下游
    /// 行为, 而非只把元数据填进了表。
    ///
    /// 下游消费者: `functions::mm::mmap::fd_mmap_backing` 是 `mmap_syscall` 文件
    /// 映射的**唯一**后端凭据来源, 取 `Err(ENODEV)` 时直接拒绝映射 (mmap.rs 文件
    /// 映射段)。凭据经 per-process fd 表 (fd → `OpenFileTable` handle) 由对象
    /// 自身声明 (`File::mmap_backing`)。
    #[test]
    fn test_fd_mmap_backing_downstream() {
        use crate::functions::fs::ramfs_core::{RAMFS_DATA, init as ramfs_init};
        use crate::functions::fs::{api, vfs_get_fd_handle};

        let _lock = FS_GLOBAL_TEST_LOCK.lock();
        crate::functions::fs::init();
        ramfs_init();
        // 真实挂载入口 (挂 trait object); 已挂载时返回负值, 忽略.
        let _ = api::vfs_mount_safe("/", "ramfs");

        with_temp_process("fd-down-t", || {
            let created = {
                let mut ramfs = RAMFS_DATA.lock();
                ramfs.create_file("/", "fd_down_t", 0)
            };
            let Some(node_id) = created else {
                panic!("create_file 失败");
            };
            assert!(node_id != 0, "inode 编号不应为 0");

            let fd = api::vfs_open_safe("/fd_down_t", 0, 0);
            assert!(fd >= 0, "open /fd_down_t 应成功");

            // 下游消费者: mmap 文件映射的后端凭据 — 未接线时为 ENODEV ⇒ mmap 拒绝
            let backing = crate::functions::mm::mmap::fd_mmap_backing(fd)
                .expect("常规文件 fd 应有文件映射后端 (未接线时为 ENODEV)");
            assert_eq!(
                backing.inode_id, node_id,
                "映射后端 inode 应为真实 inode (未接线时 mmap 文件映射恒 ENODEV)"
            );
            assert!(
                backing.mount_idx.is_some(),
                "映射后端应可反查挂载点 (未接线时为 None)"
            );
            assert!(
                vfs_get_fd_handle(fd as usize).is_some(),
                "per-process fd 表条目应存在"
            );

            assert_eq!(api::vfs_close_safe(fd as u32), 0, "close 应成功");
        });
    }

    /// dup2 覆盖已打开 fd 时, 被覆盖 `OpenFile` 必须走完整关闭链 —
    /// inotify `IN_CLOSE_NOWRITE` 是可观测证据; 直连 `OPEN_FILE_TABLE.close`
    /// 会跳过 pcache 失效 / inotify 通知 / epoll 唤醒.
    #[test]
    fn test_dup2_releases_overwritten_fd_via_close_chain() {
        use crate::functions::fs::inotify::{
            IN_CLOSE_NOWRITE, inotify_read_events, inotify_release, sys_inotify_add_watch,
            sys_inotify_init1,
        };
        use crate::functions::fs::ramfs_core::{RAMFS_DATA, init as ramfs_init};
        use crate::functions::fs::{api, vfs_dup2, vfs_get_fd_handle};

        let _lock = FS_GLOBAL_TEST_LOCK.lock();
        crate::functions::fs::init();
        ramfs_init();
        let _ = api::vfs_mount_safe("/", "ramfs");

        with_temp_process("dup2-close-t", || {
            let node_id = {
                let mut ramfs = RAMFS_DATA.lock();
                ramfs.create_file("/", "dup2_target_t", 0)
            };
            let Some(node_id) = node_id else {
                panic!("create_file 失败");
            };
            {
                let mut ramfs = RAMFS_DATA.lock();
                assert!(
                    ramfs.create_file("/", "dup2_src_t", 0).is_some(),
                    "create_file 失败"
                );
            }

            let target_fd = api::vfs_open_safe("/dup2_target_t", 0, 0);
            assert!(target_fd >= 0, "open target 应成功");
            let src_fd = api::vfs_open_safe("/dup2_src_t", 0, 0);
            assert!(src_fd >= 0, "open src 应成功");

            let ifd = sys_inotify_init1(0);
            assert!(ifd >= 0, "inotify_init1 应成功");
            let wd = sys_inotify_add_watch(ifd, node_id, IN_CLOSE_NOWRITE);
            assert!(wd >= 0, "add_watch 应成功");

            assert_eq!(
                vfs_dup2(src_fd as u32, target_fd as u32),
                target_fd,
                "dup2 应返回 newfd"
            );
            assert_eq!(
                vfs_get_fd_handle(target_fd as usize),
                vfs_get_fd_handle(src_fd as usize),
                "dup2 后 newfd 必须与 oldfd 共享同一 OpenFile"
            );

            let Some((events, _)) = inotify_read_events(ifd, 4096) else {
                panic!("被覆盖 fd 的关闭必须经关闭链产生 IN_CLOSE_NOWRITE (事件为空)");
            };
            assert_eq!(events.len(), 1, "dup2 覆盖关闭只应通知一次");
            assert_eq!(
                events[0].mask & IN_CLOSE_NOWRITE,
                IN_CLOSE_NOWRITE,
                "只读 fd 的覆盖关闭应为 IN_CLOSE_NOWRITE"
            );

            assert_eq!(api::vfs_close_safe(src_fd as u32), 0, "close src 应成功");
            assert_eq!(
                api::vfs_close_safe(target_fd as u32),
                0,
                "close target 应成功"
            );
            inotify_release(ifd);
        });
    }
}
