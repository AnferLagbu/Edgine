//! VFS 操作契约 trait — services 实现, framework 消费
//!
//! 阶段 4a (DECISION-W): VFS 子系统整体下沉 services 的「契约先行」前置.
//! framework 保留机制面在过渡期经 `VfsOps` 契约访问 VFS 逻辑, services 于
//! `fs::init` 注册实现替换内建回退; 4b 下沉实现时消费点无需再改签名.
//!
//! ## 设计
//!
//! - trait 定义在 framework, 方法签名仅含 POD / framework 类型
//!   (`u32`/`i32`/`u64`/`usize`/`&mut [u8]`/`Option<POD>`), 不引用 services
//!   具象类型 (如 `OpenFile`), 保证依赖单向.
//! - framework 提供默认回退 (`FallbackVfsOps`), 转发现有 `framework/fs` 机制函数.
//! - services 在 `fs::init` 中经 `register_vfs_ops()` 注册实现.

/// VFS 操作契约 — services 实现, framework 消费
///
/// 所有方法均为 POD 边界, 不暴露 services 具象类型.
pub trait VfsOps: Send + Sync {
    /// 释放指定进程持有的全部文件锁 (flock + POSIX 锁).
    fn release_pid_locks(&self, pid: u32);

    /// 关闭当前进程 fd 表的全部 fd (进程退出路径).
    fn close_all_fds(&self);

    /// 关闭当前进程标记 CLOEXEC 的 fd (execve 成功路径).
    fn close_cloexec_fds(&self);

    /// 递增指定 OpenFile handle 的引用计数 (fork 继承路径).
    fn inc_open_file_ref(&self, handle_id: u32);

    /// 查询 fd 对应的 VFS 文件类型.
    ///
    /// 返回 `None` 表示 fd 非法或未映射; `Some(ft)` 为 `VfsFileType` 原始字节.
    fn fd_file_type(&self, fd: i32) -> Option<u8>;

    /// 从 inode 读取数据到缓冲区 (mmap demand paging 路径).
    ///
    /// 返回实际读取字节数, 负数表示错误.
    fn pread_inode(
        &self,
        mount_idx: Option<usize>,
        inode_id: u32,
        offset: u64,
        dst: &mut [u8],
        pwm: u64,
    ) -> i32;
}

/// 框架内建回退实现 — 转发现有 `framework/fs` 机制函数
///
/// 在 services 注册实现之前使用 (早期启动阶段).
pub struct FallbackVfsOps;

impl VfsOps for FallbackVfsOps {
    fn release_pid_locks(&self, pid: u32) {
        super::flock_release_pid(pid);
        super::posix_lock_release_pid(pid);
    }

    fn close_all_fds(&self) {
        super::vfs_close_all_fds();
    }

    fn close_cloexec_fds(&self) {
        super::vfs_close_cloexec_fds();
    }

    fn inc_open_file_ref(&self, handle_id: u32) {
        super::OPEN_FILE_TABLE.inc_ref(handle_id);
    }

    fn fd_file_type(&self, fd: i32) -> Option<u8> {
        let handle_id = super::vfs_get_fd_handle(fd as usize)?;
        super::OPEN_FILE_TABLE.with_file(handle_id, |of| of.file_type)
    }

    fn pread_inode(
        &self,
        mount_idx: Option<usize>,
        inode_id: u32,
        offset: u64,
        dst: &mut [u8],
        pwm: u64,
    ) -> i32 {
        super::vfs_pread_inode(mount_idx, inode_id, offset, dst, pwm)
    }
}

static FALLBACK_VFS_OPS: FallbackVfsOps = FallbackVfsOps;

/// 全局 VFS 操作契约注册表 — services 经 `register_vfs_ops` 注册
static VFS_OPS: crate::framework::sync::OnceLock<&'static dyn VfsOps> =
    crate::framework::sync::OnceLock::new();

/// 注册 VFS 操作实现 (由 `services::fs::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
/// # Errors
/// 实现已被注册过时返回 Err。
pub fn register_vfs_ops(ops: &'static dyn VfsOps) -> Result<(), &'static dyn VfsOps> {
    match VFS_OPS.set(ops) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前 VFS 操作实现 (未注册时返回内建回退)
#[inline]
pub fn current_vfs_ops() -> &'static dyn VfsOps {
    match VFS_OPS.get() {
        Some(&ops) => ops,
        None => &FALLBACK_VFS_OPS,
    }
}
