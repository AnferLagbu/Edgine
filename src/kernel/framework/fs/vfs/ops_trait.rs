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
//! - framework 提供默认回退 (`FallbackVfsOps`), 全部为空操作 (no-op).
//! - services 在 `fs::init` 中经 `register_vfs_ops()` 注册实现.
//!
//! ## 阶段 4b 变更
//!
//! VFS 具象实现已下沉 `services::fs`, framework 侧不再持有可转发的机制函数,
//! `FallbackVfsOps` 由「转发 framework/fs 函数」改为「全 no-op 桩」.

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

/// 框架内建回退实现 — 全部为空操作
///
/// 在 services 注册实现之前使用 (早期启动阶段). 阶段 4b 起 VFS 具象实现
/// 已下沉 services, framework 内不再持有可转发的机制函数, 故此处全部 no-op.
///
/// # 回退安全性
///
/// 早期启动阶段早于任何 fd / 文件锁 / mmap 使用, 消费点仅可能在
/// `services::fs::init` 前被调用:
/// - `release_pid_locks` / `close_all_fds` / `close_cloexec_fds` / `inc_open_file_ref`:
///   此时无进程持有 fd 或锁, no-op 语义正确.
/// - `fd_file_type`: 返回 `None` 表示 fd 非法, 消费点 (`epoll`) 会走错误分支.
/// - `pread_inode`: 返回 `0`, 消费点 (`mm/page_fault`) 依 `n > 0` 判定成功,
///   `n <= 0` 保持零页填充, 语义安全.
//
// SIMPLIFIED: FallbackVfsOps 全部空实现; 影响面为「services 未注册 VfsOps 前的早期启动窗口」; 若未来出现 boot 早期即需 VFS 能力的路径, 需为对应方法补 framework 侧最小机制实现.
pub struct FallbackVfsOps;

impl VfsOps for FallbackVfsOps {
    fn release_pid_locks(&self, _pid: u32) {}

    fn close_all_fds(&self) {}

    fn close_cloexec_fds(&self) {}

    fn inc_open_file_ref(&self, _handle_id: u32) {}

    fn fd_file_type(&self, _fd: i32) -> Option<u8> {
        None
    }

    fn pread_inode(
        &self,
        _mount_idx: Option<usize>,
        _inode_id: u32,
        _offset: u64,
        _dst: &mut [u8],
        _pwm: u64,
    ) -> i32 {
        0
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
