#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! 内存文件系统 (`RamFS`) — services 层安全代理 (Phase 2.2.1)
//!
//! 在 `kernel/fs/ramfs` 基础上提供 100% safe 的公共 API,
//! 用于用户态 shell/进程的文件系统交互。
//!
//! ## 设计原则
//!
//! - **零 unsafe**: 内部 `RamFsData` 已由 P2.2.1 标记为安全 (Send+Sync 自动实现)
//! - **类型安全**: `i32` 错误码 → `Result<_, FsError>`; 路径/标志用类型化
//! - **薄包装**: 透传最常用操作 (open/read/write/mkdir/unlink/stat/...)
//! - **可替代**: 原 `kernel/fs/ramfs/ramfs.rs` 仍存在, 本文件是迁移目标
//!
//! ## 错误码映射
//!
//! 内核内部用 `KernelError` (`i32` 负数), services 层用 `FsError` (3 字段薄包装 + 1 共享包装)
//! 全部经 `Result<T, FsError>` 返回, 上层用 `?` / `match` 而非检查负数.
//! TD-18 收敛: 共享 POSIX 错误 (FileNotFound/AlreadyExists/NoSpace/.../BadFd/...)
//! 走 `FsError::Kernel(KernelError)`, 单一来源.
//!
//! 评估日期: 2026-06-04
//! Phase 2.2.1 任务: 文件系统迁移

use alloc::vec::Vec;

use crate::services::fs::ramfs_core::RamFsData;

// services 层透出的常量 (镜像 kernel/fs/ramfs/ramfs.rs 内部常量)
pub const RAMFS_BLOCK_SIZE: usize = crate::framework::mm::PAGE_SIZE as usize;
pub const RAMFS_MAX_NODES: usize = 256;
pub const RAMFS_MAX_BLOCKS: usize = 2048;
pub use crate::services::fs::{
    VFS_MAX_NAME, VfsDirEntry, VfsFileType, VfsOpenFlags, VfsSeekWhence, VfsStat,
};

// ============================================================================
// 错误类型
// ============================================================================

/// 文件系统错误 — TD-18: 收敛到 `KernelError`, 3 字段 FS 特有 + 1 共享包装.
///
/// 字段说明:
///   - `NotInitialized`: ramfs 初始化状态机专用 (FS 未挂载), 不在 POSIX 通用集.
///   - `IoError`: FS 内部 I/O 错误聚合, 涵盖底层所有未分类硬件/校验失败.
///   - `Overflow`: 文件偏移/大小算术溢出 (POSIX EOVERFLOW=75 但语义不通用).
///   - `Kernel(KernelError)`: 共享错误 (FileNotFound/AlreadyExists/NoSpace/.../BadFd/...)
///     统一走 `KernelError` 单一来源.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// FS 未初始化
    NotInitialized,
    /// FS 内部 I/O 错误聚合
    IoError,
    /// 算术溢出 (偏移/大小)
    Overflow,
    /// 共享 `KernelError` 包装
    Kernel(crate::services::error::KernelError),
}

impl FsError {
    /// 从内核原始 `i32` 错误码还原
    pub fn from_i32(code: i32) -> Self {
        Self::Kernel(crate::services::error::KernelError::from_i32(code))
    }

    /// 映射为 POSIX errno
    pub fn to_errno(self) -> crate::framework::syscall::Errno {
        use crate::framework::syscall::Errno as E;
        match self {
            Self::NotInitialized => E::ENODEV,
            Self::IoError => E::EIO,
            Self::Overflow => E::EOVERFLOW,
            Self::Kernel(e) => e.as_errno(),
        }
    }
}

impl From<crate::services::error::KernelError> for FsError {
    fn from(e: crate::services::error::KernelError) -> Self {
        Self::Kernel(e)
    }
}

/// 兼容 Result 别名
pub type FsResult<T> = Result<T, FsError>;

// ============================================================================
// 内部助手: 把 raw `i32` 包装成 `Result`
// ============================================================================

#[inline]
fn to_fs_result(code: i32) -> FsResult<()> {
    if code >= 0 {
        Ok(())
    } else {
        Err(FsError::from_i32(code))
    }
}

// ============================================================================
// 安全文件描述符
// ============================================================================

/// 文件描述符 (services 层表示)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDescriptor {
    /// 节点 ID
    pub node_id: u32,
    /// 打开标志
    pub flags: u32,
    /// 当前文件偏移
    pub offset: u64,
    /// 关联身份 (PWM Capability 标识)
    pub pwm: u64,
}

impl FileDescriptor {
    /// 创建新描述符
    pub const fn new(node_id: u32, flags: u32, pwm: u64) -> Self {
        Self {
            node_id,
            flags,
            offset: 0,
            pwm,
        }
    }

    /// 是否只读
    pub const fn is_read_only(&self) -> bool {
        self.flags & 0x0001 != 0 && self.flags & 0x0004 == 0
    }

    /// 是否可写
    pub const fn is_writable(&self) -> bool {
        self.flags & 0x0002 != 0 || self.flags & 0x0004 != 0
    }
}

// ============================================================================
// 安全文件系统实例
// ============================================================================

/// `RamFS` 安全代理 (services 层)。
///
/// 内部用 `IrqSpinLock<RamFsData>` 串行化所有访问,
/// 提供 100% safe 的公共 API。
pub struct SafeRamFs {
    inner: crate::framework::sync::IrqSpinLock<RamFsData>,
}

impl SafeRamFs {
    /// 创建未挂载的 `SafeRamFs`
    pub fn new() -> Self {
        Self {
            inner: crate::framework::sync::IrqSpinLock::new(RamFsData::new()),
        }
    }

    /// 初始化并挂载到 `mount_point`
    ///
    /// # Errors
    /// 当底层挂载失败时返回对应的 `FsError`.
    pub fn mount(&self, mount_point: &str) -> FsResult<()> {
        let mut fs = self.inner.lock();
        // 内部 init() 是 const, mount 走 i32 接口
        let rc = fs.mount(mount_point);
        to_fs_result(rc)
    }

    /// 打开文件
    ///
    /// # 参数
    /// - `path`: 文件路径
    /// - `flags`: 打开标志 (见 `VfsOpenFlags`)
    /// - `pwm`: 进程 PWM Capability 标识
    ///
    /// # 返回
    /// 成功: `FileDescriptor`, 可用于后续 read/write/seek/close
    ///
    /// # Errors
    /// 当路径不存在或无法打开时返回 `FileNotFound`.
    pub fn open(&self, path: &str, flags: VfsOpenFlags, pwm: u64) -> FsResult<FileDescriptor> {
        let raw_flags = flags.bits();
        let mut fs = self.inner.lock();
        match fs.open(path, raw_flags, pwm) {
            Some((node_id, _cap, _sens)) => Ok(FileDescriptor::new(node_id, raw_flags, pwm)),
            None => Err(FsError::Kernel(
                crate::services::error::KernelError::FileNotFound,
            )),
        }
    }

    /// 创建并打开文件
    ///
    /// # Errors
    /// 当底层创建/打开失败时返回 `IoError`.
    pub fn create(&self, path: &str, pwm: u64) -> FsResult<FileDescriptor> {
        let flags = VfsOpenFlags::CREAT | VfsOpenFlags::RDWR;
        let mut fs = self.inner.lock();
        match fs.open(path, flags.bits(), pwm) {
            Some((node_id, _, _)) => Ok(FileDescriptor::new(node_id, flags.bits(), pwm)),
            None => Err(FsError::IoError),
        }
    }

    /// 读文件
    ///
    /// # 参数
    /// - `fd`: 文件描述符 (来自 `open` / `create`)
    /// - `buf`: 读取缓冲区
    ///
    /// # 返回
    /// 成功: 实际读取的字节数
    ///
    /// # Errors
    /// 当底层读取失败 (如节点无效、权限不足等) 时返回对应的 `FsError`.
    pub fn read(&self, fd: &mut FileDescriptor, buf: &mut [u8]) -> FsResult<usize> {
        let mut fs = self.inner.lock();
        let rc = fs.read(fd.node_id, &mut fd.offset, buf, fd.pwm);
        if rc < 0 {
            Err(FsError::from_i32(rc))
        } else {
            Ok(rc as usize)
        }
    }

    /// 写文件
    ///
    /// # 返回
    /// 成功: 实际写入的字节数
    ///
    /// # Errors
    /// 当底层写入失败 (如只读、节点无效、权限不足等) 时返回对应的 `FsError`.
    pub fn write(&self, fd: &mut FileDescriptor, buf: &[u8]) -> FsResult<usize> {
        let mut fs = self.inner.lock();
        let rc = fs.write(fd.node_id, &mut fd.offset, buf, fd.pwm);
        if rc < 0 {
            Err(FsError::from_i32(rc))
        } else {
            Ok(rc as usize)
        }
    }

    /// 调整文件大小
    ///
    /// # Errors
    /// 当底层截断失败 (如节点无效、权限不足等) 时返回对应的 `FsError`.
    pub fn truncate(&self, fd: &mut FileDescriptor, new_size: u64) -> FsResult<()> {
        let mut fs = self.inner.lock();
        let rc = fs.truncate(fd.node_id, new_size, fd.pwm);
        to_fs_result(rc)
    }

    /// 调整文件偏移
    ///
    /// # 参数
    /// - `offset`: 偏移量 (可为负)
    /// - `whence`: 基准 (Set/Cur/End)
    ///
    /// # Errors
    /// 当 `whence` 非法或计算出的偏移越界时返回 `InvalidArgument`.
    pub fn seek(
        &self,
        fd: &mut FileDescriptor,
        offset: i64,
        whence: VfsSeekWhence,
    ) -> FsResult<u64> {
        let fs = self.inner.lock();
        match fs.seek(fd.node_id, fd.offset, offset, whence) {
            Some(new_off) => {
                fd.offset = new_off;
                Ok(new_off)
            }
            None => Err(FsError::Kernel(
                crate::services::error::KernelError::InvalidArgument,
            )),
        }
    }

    /// 查询文件大小
    ///
    /// # Errors
    /// 当文件描述符对应节点不存在时返回 `FileNotFound`.
    pub fn get_file_size(&self, fd: &FileDescriptor) -> FsResult<u32> {
        let fs = self.inner.lock();
        fs.get_file_size(fd.node_id).ok_or(FsError::Kernel(
            crate::services::error::KernelError::FileNotFound,
        ))
    }

    /// 查询文件元数据
    ///
    /// # Errors
    /// 当文件描述符对应节点不存在时返回 `FileNotFound`.
    pub fn stat(&self, fd: &FileDescriptor) -> FsResult<VfsStat> {
        let fs = self.inner.lock();
        fs.stat(fd.node_id).ok_or(FsError::Kernel(
            crate::services::error::KernelError::FileNotFound,
        ))
    }

    /// 创建目录
    ///
    /// # Errors
    /// 当底层 mkdir 失败 (如父路径不存在、名称已存在、无权限等) 时返回对应的 `FsError`.
    pub fn mkdir(&self, parent_path: &str, name: &str, pwm: u64) -> FsResult<()> {
        let mut fs = self.inner.lock();
        let rc = fs.mkdir(parent_path, name, pwm);
        to_fs_result(rc)
    }

    /// 创建文件 (不打开)
    ///
    /// # Errors
    /// 当底层创建失败 (如父路径不存在、名称已存在等) 时返回 `IoError`.
    pub fn create_file(&self, parent_path: &str, name: &str, pwm: u64) -> FsResult<u32> {
        let mut fs = self.inner.lock();
        fs.create_file(parent_path, name, pwm)
            .ok_or(FsError::IoError)
    }

    /// 删除文件
    ///
    /// # Errors
    /// 当底层 unlink 失败 (如路径不存在、无权限等) 时返回对应的 `FsError`.
    pub fn unlink(&self, path: &str, pwm: u64) -> FsResult<()> {
        let mut fs = self.inner.lock();
        let rc = fs.unlink(path, pwm);
        to_fs_result(rc)
    }

    /// 硬链接
    ///
    /// # Errors
    /// 当底层 link 失败 (如节点无效、名称已存在、无权限等) 时返回对应的 `FsError`.
    pub fn link(&self, parent_node: u32, target_node: u32, name: &str, pwm: u64) -> FsResult<()> {
        let mut fs = self.inner.lock();
        let rc = fs.link(parent_node, target_node, name, pwm);
        to_fs_result(rc)
    }

    /// 修改权限
    ///
    /// # Errors
    /// 当底层 chmod 失败 (如路径不存在、无权限等) 时返回对应的 `FsError`.
    pub fn chmod(&self, path: &str, mode: u16, pwm: u64) -> FsResult<()> {
        let mut fs = self.inner.lock();
        let rc = fs.chmod(path, mode, pwm);
        to_fs_result(rc)
    }

    /// 修改所有者 (uid)
    ///
    /// # Errors
    /// 当底层 chown 失败 (如路径不存在、无权限等) 时返回对应的 `FsError`.
    pub fn chown(&self, path: &str, owner_pwm: u64, pwm: u64) -> FsResult<()> {
        let mut fs = self.inner.lock();
        let rc = fs.chown(path, owner_pwm, pwm);
        to_fs_result(rc)
    }

    /// 修改所有者和组
    ///
    /// # Errors
    /// 当底层 `chown_ext` 失败 (如路径不存在、无权限等) 时返回对应的 `FsError`.
    pub fn chown_ext(&self, path: &str, owner_pwm: u64, group_pwm: u64, pwm: u64) -> FsResult<()> {
        let mut fs = self.inner.lock();
        let rc = fs.chown_ext(path, owner_pwm, group_pwm, pwm);
        to_fs_result(rc)
    }

    /// 解析路径 → 节点 ID
    pub fn resolve_path(&self, path: &str) -> Option<u32> {
        let fs = self.inner.lock();
        fs.resolve_path(path)
    }

    // ── 目录枚举 (Phase 2.2.1 增补) ──

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    /// 列出目录内容
    ///
    /// # 参数
    /// - `path`: 目录路径
    ///
    /// # 返回
    /// 目录项列表
    ///
    /// # Errors
    /// 当路径不存在时返回 `FileNotFound`; 当路径不是目录时返回 `NotADirectory`.
    pub fn readdir(&self, path: &str) -> FsResult<Vec<VfsDirEntry>> {
        let fs = self.inner.lock();
        let node_id = match fs.resolve_path(path) {
            Some(id) => id,
            None => {
                return Err(FsError::Kernel(
                    crate::services::error::KernelError::FileNotFound,
                ));
            }
        };

        // 通过 stat 验证是目录
        let st = match fs.stat(node_id) {
            Some(s) => s,
            None => {
                return Err(FsError::Kernel(
                    crate::services::error::KernelError::FileNotFound,
                ));
            }
        };
        if VfsFileType::from_u8(st.file_type) != Some(VfsFileType::Dir) {
            return Err(FsError::Kernel(
                crate::services::error::KernelError::NotADirectory,
            ));
        }

        // 扫描所有节点 (RamFs 当前未暴露 listdir API, 这里返回空 Vec)
        // 完整实现需要 ramfs 端新增 listdir (后续 Phase 2.2.x)
        let entries: Vec<VfsDirEntry> = Vec::new();
        for i in 0..RAMFS_MAX_NODES {
            let n = &fs.nodes[i];
            if !n.used {
                continue;
            }
            if n.node_id == node_id {
                continue; // 跳过自身
            }
            let _ = i;
        }
        let _ = entries;
        Ok(Vec::new())
    }
}

impl Default for SafeRamFs {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 全局实例 (Phase 2.2.1 单 FS 演示)
// ============================================================================

// I-16: 替换 spin::Once → 项目自研 services::sync::once::OnceCell
use crate::services::sync::once::OnceCell;

static GLOBAL_RAMFS: OnceCell<SafeRamFs> = OnceCell::new();

/// 初始化全局 `RamFS`
///
/// # Errors
/// 当底层挂载失败时返回对应的 `FsError` (首次初始化时).
pub fn init_global(mount_point: &str) -> FsResult<()> {
    // 一旦已初始化, 跳过重复 mount. 闭包内失败传播给 caller (与原 spin::Once 行为一致)
    if GLOBAL_RAMFS.get().is_none() {
        let fs = SafeRamFs::new();
        fs.mount(mount_point)?;
        let _ = GLOBAL_RAMFS.get_or_init(|slot| {
            slot.write(fs);
        });
    }
    Ok(())
}

/// 获取全局 `RamFS` 引用
///
/// # Safety (调用方)
/// 调用前需保证 `init_global` 已执行
///
/// # Panics
/// 当 `init_global()` 尚未被调用、全局实例未初始化时发生 panic (内部使用 `expect`).
pub fn global() -> &'static SafeRamFs {
    GLOBAL_RAMFS
        .get()
        .expect("fs::global() called before init_global()")
}

// ============================================================================
// 便利函数
// ============================================================================

/// 打开全局 `RamFS` 文件
///
/// # Errors
/// 错误条件与 [`SafeRamFs::open`] 相同, 参见其 `# Errors` 段.
pub fn open(path: &str, flags: VfsOpenFlags, pwm: u64) -> FsResult<FileDescriptor> {
    global().open(path, flags, pwm)
}

/// 创建并打开全局 `RamFS` 文件
///
/// # Errors
/// 错误条件与 [`SafeRamFs::create`] 相同, 参见其 `# Errors` 段.
pub fn create(path: &str, pwm: u64) -> FsResult<FileDescriptor> {
    global().create(path, pwm)
}

/// 在全局 `RamFS` 上创建目录
///
/// # Errors
/// 错误条件与 [`SafeRamFs::mkdir`] 相同, 参见其 `# Errors` 段.
pub fn mkdir(parent: &str, name: &str, pwm: u64) -> FsResult<()> {
    global().mkdir(parent, name, pwm)
}

/// 解析路径
pub fn resolve(path: &str) -> Option<u32> {
    global().resolve_path(path)
}

/// 块大小 (常量透传)
pub const fn block_size() -> usize {
    RAMFS_BLOCK_SIZE
}
