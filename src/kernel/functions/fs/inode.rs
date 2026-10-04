//! Inode trait 定义 + 具象 Inode 实现 — functions 层
//!
//! ## 阶段 4b 迁移记录
//!
//! Inode trait (VFS 句柄级操作抽象) 随 VFS 完整下沉从
//! `privileged::fs::vfs::inode` 迁入本文件. 具象实现
//! (AnonymousInode / RamFsInode) 与各文件系统的
//! `impl Inode for ...` 同处 functions 层.
//!
//! ## 与 FileSystem trait 的关系
//!
//! - `FileSystem` trait: 路径级操作 (mount/open/stat by path)
//! - `Inode` trait: 句柄级操作 (read/write/stat by open file)
//! - `FileSystem::fs_open` 返回 `Arc<dyn Inode>`, VFS 层将其封装为 `OpenFile`
//!
//! ## offset 管理
//!
//! read/write 接收 offset 参数, 由 `OpenFile` 层管理共享偏移.
//! Inode 实现者只需按给定 offset 执行 I/O, 不需要管理偏移状态.
//! 这保证 POSIX dup 共享 offset 语义: 多个 fd 通过同一个 OpenFile
//! 共享 offset, Inode 本身是无状态的 I/O 执行器.

#![deny(unsafe_code)]

use alloc::sync::Arc;

use super::vfs_types::{KernelError, KernelResult, VfsFileType, VfsSeekWhence, VfsStat};

pub trait Inode: Send + Sync {
    /// 读取文件数据
    ///
    /// - `offset`: 文件内字节偏移
    /// - `buf`: 目标缓冲区
    /// - `pwm`: 权限凭证
    /// 返回: 实际读取的字节数
    ///
    /// # Errors
    /// 当偏移超出文件末尾、底层 I/O 失败或权限不足时返回 `KernelError` (具体由实现者决定).
    fn read(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize>;

    /// 写入文件数据
    ///
    /// - `offset`: 文件内字节偏移
    /// - `buf`: 源数据
    /// - `pwm`: 权限凭证
    /// 返回: 实际写入的字节数
    ///
    /// # Errors
    /// 当文件只读、超出文件系统容量或底层 I/O 失败时返回 `KernelError` (具体由实现者决定).
    fn write(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize>;

    /// 获取文件属性
    ///
    /// # Errors
    /// 当 inode 已失效或底层元数据读取失败时返回 `KernelError` (具体由实现者决定).
    fn stat(&self, pwm: u64) -> KernelResult<VfsStat>;

    /// 截断文件到指定大小
    ///
    /// # Errors
    /// 当文件只读、无权限或底层元数据更新失败时返回 `KernelError` (具体由实现者决定).
    fn truncate(&self, size: u64, pwm: u64) -> KernelResult<()>;

    /// 计算 seek 后的新偏移
    ///
    /// - `offset`: seek 偏移量 (字节)
    /// - `whence`: 定位基准 (`SEEK_SET/SEEK_CUR/SEEK_END`)
    /// - `current_offset`: 当前文件偏移
    /// 返回: 新的文件偏移
    ///
    /// # Errors
    /// 当 seek 结果超出可表示范围或 `whence` 非法时返回 `KernelError` (具体由实现者决定).
    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64>;

    /// 是否为目录
    fn is_dir(&self) -> bool;

    /// 读取目录项 (仅目录 Inode 实现)
    ///
    /// - `offset`: 目录偏移 (字节)
    /// 返回: (`entry_name`, `file_type`, `has_more`), 失败返回 `KernelError`
    ///
    /// # Errors
    /// 默认实现返回 `NotADirectory`; 当 inode 不是目录或底层目录读取失败时返回 `KernelError`.
    fn readdir(&self, _offset: u64) -> KernelResult<(alloc::string::String, VfsFileType, bool)> {
        Err(KernelError::NotADirectory)
    }

    /// 创建子目录 (仅目录 Inode 实现)
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限、名称已存在或底层创建失败时返回 `KernelError`.
    fn mkdir(&self, _name: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 删除文件 (仅目录 Inode 实现)
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限、文件不存在或底层删除失败时返回 `KernelError`.
    fn unlink(&self, _name: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 重命名 (仅目录 Inode 实现)
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限、源/目标无效或底层重命名失败时返回 `KernelError`.
    fn rename(&self, _old_name: &str, _new_name: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 创建硬链接 (仅目录 Inode 实现)
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限、名称已存在或底层链接创建失败时返回 `KernelError`.
    fn link(&self, _name: &str, _target: &dyn Inode, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 创建符号链接 (仅目录 Inode 实现)
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限、名称已存在或底层符号链接创建失败时返回 `KernelError`.
    fn symlink(&self, _name: &str, _target: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 读取符号链接目标 (仅符号链接 Inode 实现)
    ///
    /// # Errors
    /// 默认实现返回 `InvalidArgument`; 当 inode 不是符号链接或目标读取失败时返回 `KernelError`.
    fn readlink(&self, _buf: &mut [u8]) -> KernelResult<usize> {
        Err(KernelError::InvalidArgument)
    }

    /// 修改权限
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported` (不支持按 inode 修改权限的 FS 显式报错, 不再静默成功); 当无权限或底层元数据更新失败时返回 `KernelError` (具体由实现者决定).
    fn chmod(&self, _mode: u16, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 修改所有者
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported` (不支持按 inode 修改所有者的 FS 显式报错, 不再静默成功); 当无权限或底层元数据更新失败时返回 `KernelError` (具体由实现者决定).
    fn chown(&self, _owner_pwm: u64, _group_pwm: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 设置文件时间戳 (atime, mtime)
    ///
    /// - `atime`: 访问时间 (纳秒), `u64::MAX` 表示不修改
    /// - `mtime`: 修改时间 (纳秒), `u64::MAX` 表示不修改
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限或底层元数据更新失败时返回 `KernelError` (具体由实现者决定).
    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    /// 获取底层 `inode_id` (供 pcache 等需要 inode 标识的场景使用)
    fn node_id(&self) -> u32;

    /// 获取挂载点索引 (供 mmap 等需要挂载点信息的场景使用)
    fn mount_idx(&self) -> u32;

    /// 按 `inode_id` 直接读取 (mmap prewarm 用, 不经过 `OpenFile` offset)
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当底层读取失败时返回 `KernelError` (具体由实现者决定).
    fn pread_inode(&self, _offset: u64, _buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }
}

// AnonymousInode 依赖 AnonymousFs (functions 侧策略)
use super::anonymous::ANONYMOUS_FS;

// ============================================================================
// 通用实现: 匿名 Inode (memfd / 无路径文件)
// ============================================================================

/// 匿名文件 Inode — memfd / 无路径文件的 Inode 实现
pub struct AnonymousInode {
    inode_id: u32,
    /// B06-06: `u32::MAX` 为"匿名文件无挂载点"哨兵; 风险路径 (mmap) 已走 `Option<usize>`,
    /// 本字段透传值全仓库无调用点 (见 OpenFile::mount_idx).
    mount_idx: u32,
}

impl AnonymousInode {
    /// 创建新的匿名 Inode
    pub fn new(inode_id: u32) -> Self {
        Self {
            inode_id,
            mount_idx: u32::MAX, // 匿名文件无挂载点
        }
    }
}

impl Inode for AnonymousInode {
    fn read(&self, offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        // B06-13: 直接透传 AnonymousFs 底层错误, 不再吞为 Io
        ANONYMOUS_FS.read_at(self.inode_id, offset, buf)
    }

    fn write(&self, offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        // B06-13: 直接透传 AnonymousFs 底层错误, 不再吞为 Io
        ANONYMOUS_FS.write_at(self.inode_id, offset, buf)
    }

    fn stat(&self, _pwm: u64) -> KernelResult<VfsStat> {
        let size = ANONYMOUS_FS.get_size(self.inode_id).unwrap_or(0);
        Ok(VfsStat {
            node_id: self.inode_id,
            size,
            file_type: VfsFileType::File.as_u8(),
            ..VfsStat::default()
        })
    }

    fn truncate(&self, size: u64, _pwm: u64) -> KernelResult<()> {
        if ANONYMOUS_FS.truncate(self.inode_id, size) {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        let file_size = u64::from(ANONYMOUS_FS.get_size(self.inode_id).unwrap_or(0));
        let new_offset = match whence {
            VfsSeekWhence::Set => offset as u64,
            VfsSeekWhence::Cur => current_offset.saturating_add(offset as u64),
            VfsSeekWhence::End => file_size.saturating_add(offset as u64),
        };
        Ok(new_offset)
    }

    fn is_dir(&self) -> bool {
        // B06-14: 匿名文件恒为普通文件 — AnonymousFs::alloc_inode 仅分配 File 类型 inode (见 anonymous.rs),
        // 故硬编码 false 是设计保证而非缺陷, 无需按类型动态判断。
        false
    }

    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        // AnonymousInode: 匿名文件, 无持久时间戳
        Ok(())
    }

    fn node_id(&self) -> u32 {
        self.inode_id
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

// ============================================================================
// RamFs Inode adapter — 将 RamFsData 包装为 Inode trait
// ============================================================================

/// `RamFS` 文件 Inode — 将全局 `RamFsData` 包装为 Inode trait
///
/// 每个实例对应一个 `RamFS` 中的文件节点.
/// 通过全局 `RAMFS_DATA` 锁实现内部可变性.
pub struct RamFsInode {
    inode_id: u32,
    mount_idx: u32,
    /// dcache/icache 命名空间标识 (所属 RamFS 实例)
    fs_id: u32,
}

impl RamFsInode {
    /// 创建新的 `RamFS` Inode
    pub fn new(inode_id: u32, mount_idx: u32, fs_id: u32) -> Self {
        Self {
            inode_id,
            mount_idx,
            fs_id,
        }
    }
}

impl Inode for RamFsInode {
    fn read(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        use crate::functions::fs::ramfs_core::RAMFS_DATA;
        let mut ramfs = RAMFS_DATA.lock();
        let (bytes_read, _new_offset) = ramfs.read_at_offset(self.inode_id, offset, buf, pwm);
        if bytes_read == 0 && offset >= u64::from(ramfs.get_file_size(self.inode_id).unwrap_or(0)) {
            // EOF
            Ok(0)
        } else if bytes_read > 0 {
            Ok(bytes_read)
        } else {
            Err(KernelError::Io)
        }
    }

    fn write(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        use crate::functions::fs::ramfs_core::RAMFS_DATA;
        let mut ramfs = RAMFS_DATA.lock();
        let (bytes_written, _new_offset) = ramfs.write_at_offset(self.inode_id, offset, buf, pwm);
        if bytes_written > 0 {
            Ok(bytes_written)
        } else {
            Err(KernelError::Io)
        }
    }

    #[expect(
        clippy::items_after_statements,
        reason = "items_after_statements: item 紧邻使用点声明便于阅读上下文; 当前优先 expect"
    )]
    fn stat(&self, pwm: u64) -> KernelResult<VfsStat> {
        // icache 快速路径: 避免 RAMFS_DATA 锁
        if let Some(cached) = crate::functions::fs::dcache::icache_lookup(self.fs_id, self.inode_id)
        {
            return Ok(VfsStat {
                node_id: cached.ino,
                file_type: cached.file_type,
                perm: cached.perm,
                size: cached.size,
                mtime: cached.mtime,
                ctime: cached.ctime,
                owner_pwm: cached.owner_pwm,
                group_pwm: cached.group_pwm,
                ..VfsStat::default()
            });
        }
        // 缓存未命中: 回退到完整 stat
        use crate::functions::fs::ramfs_core::RAMFS_DATA;
        let ramfs = RAMFS_DATA.lock();
        let st = ramfs.get_stat(self.inode_id, pwm)?;
        // 填充 icache
        crate::functions::fs::dcache::icache_insert(
            self.fs_id,
            self.inode_id,
            st.file_type,
            st.perm,
            st.size as u32,
            st.mtime,
            st.ctime,
            st.owner_pwm,
            st.group_pwm,
        );
        Ok(st)
    }

    fn truncate(&self, size: u64, pwm: u64) -> KernelResult<()> {
        use crate::functions::fs::ramfs_core::RAMFS_DATA;
        let mut ramfs = RAMFS_DATA.lock();
        let rc = ramfs.truncate(self.inode_id, size, pwm);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        use crate::functions::fs::ramfs_core::RAMFS_DATA;
        let ramfs = RAMFS_DATA.lock();
        let file_size = u64::from(ramfs.get_file_size(self.inode_id).unwrap_or(0));
        let new_offset = match whence {
            VfsSeekWhence::Set => offset as u64,
            VfsSeekWhence::Cur => current_offset.saturating_add(offset as u64),
            VfsSeekWhence::End => file_size.saturating_add(offset as u64),
        };
        Ok(new_offset)
    }

    fn is_dir(&self) -> bool {
        use crate::functions::fs::ramfs_core::RAMFS_DATA;
        let ramfs = RAMFS_DATA.lock();
        // B06-09: 用 RAMFS_MAX_NODES 常量替代硬编码 256, 用 VfsFileType::Dir 替代魔法数 1
        if (self.inode_id as usize) < crate::functions::fs::ramfs_core::RAMFS_MAX_NODES {
            ramfs.nodes[self.inode_id as usize].file_type == VfsFileType::Dir.as_u8()
        } else {
            false
        }
    }

    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        // RamFS: 内存文件系统, 无持久时间戳, 直接返回成功
        Ok(())
    }

    fn node_id(&self) -> u32 {
        self.inode_id
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }

    fn pread_inode(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        use crate::functions::fs::ramfs_core::RAMFS_DATA;
        let mut ramfs = RAMFS_DATA.lock();
        let (bytes_read, _) = ramfs.read_at_offset(self.inode_id, offset, buf, pwm);
        Ok(bytes_read)
    }
}

// ============================================================================
// Inode 构造工厂
// ============================================================================

/// 创建匿名 Inode 的 Arc 包装
pub fn new_anonymous_inode(inode_id: u32) -> Arc<dyn Inode> {
    Arc::new(AnonymousInode::new(inode_id))
}

/// 创建 `RamFS` Inode 的 Arc 包装
pub fn new_ramfs_inode(inode_id: u32, mount_idx: u32, fs_id: u32) -> Arc<dyn Inode> {
    Arc::new(RamFsInode::new(inode_id, mount_idx, fs_id))
}
