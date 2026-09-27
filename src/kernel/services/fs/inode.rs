//! 具象 Inode 实现 — services 层 (AnonymousInode / RamFsInode / LegacyInode)
//!
//! ## B09-12/DECISION-H13 P1-B3 迁移记录 (2026-08-31)
//!
//! Inode trait 定义已迁回 `framework::fs::vfs::inode` (机制归 framework).
//! 本文件保留具象实现, `impl` 引用 framework trait (services→framework 合法方向).

#![deny(unsafe_code)]

// Inode trait 定义在 framework, 此处 re-export 保持 `services::fs::inode::Inode`
// 调用方兼容 (services→framework 单向依赖).
pub use crate::framework::fs::vfs::inode::Inode;

use alloc::sync::Arc;

use crate::framework::fs::vfs::types::{
    KernelError, KernelResult, VfsFileType, VfsSeekWhence, VfsStat,
};

// AnonymousInode 依赖 AnonymousFs (services 侧策略)
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
        use crate::framework::fs::ramfs::RAMFS_DATA;
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
        use crate::framework::fs::ramfs::RAMFS_DATA;
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
        if let Some(cached) = crate::services::fs::dcache::icache_lookup(self.fs_id, self.inode_id)
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
        use crate::framework::fs::ramfs::RAMFS_DATA;
        let ramfs = RAMFS_DATA.lock();
        let st = ramfs.get_stat(self.inode_id, pwm)?;
        // 填充 icache
        crate::services::fs::dcache::icache_insert(
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
        use crate::framework::fs::ramfs::RAMFS_DATA;
        let mut ramfs = RAMFS_DATA.lock();
        let rc = ramfs.truncate(self.inode_id, size, pwm);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        use crate::framework::fs::ramfs::RAMFS_DATA;
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
        use crate::framework::fs::ramfs::RAMFS_DATA;
        let ramfs = RAMFS_DATA.lock();
        // B06-09: 用 RAMFS_MAX_NODES 常量替代硬编码 256, 用 VfsFileType::Dir 替代魔法数 1
        if (self.inode_id as usize) < crate::framework::fs::ramfs::RAMFS_MAX_NODES {
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
        use crate::framework::fs::ramfs::RAMFS_DATA;
        let mut ramfs = RAMFS_DATA.lock();
        let (bytes_read, _) = ramfs.read_at_offset(self.inode_id, offset, buf, pwm);
        Ok(bytes_read)
    }
}

// ============================================================================
// LegacyInode — 过渡期适配器: 将 FileSystem opaque handle 包装为 Inode trait
// ============================================================================
//
// Plan B 过渡期间, FileSystem::fs_open 仍返回 FsOpenResult { handle: u32 }.
// LegacyInode 将 handle + mount_idx 包装为 Inode trait object,
// 委托给 VFS_MANAGER 中注册的 FileSystem trait 方法.

/// 过渡期 Inode 适配器 — 将 `FileSystem` 的 opaque handle 包装为 Inode trait
///
/// **B06-12: 废弃标记** — 本类型是 Plan B 过渡产物: 原设计在 `FileSystem::fs_open`
/// 仍返回 opaque handle 时用作适配。当前全部 8 个 FS 均已实现 `fs_resolve_inode`
/// (返回原生 `Arc<dyn Inode>`), 本类型仅作为 `open_by_handle_at` 的防御性回退
/// (file_handle.rs, 正常路径不会触发)。stat/chmod/chown 仍走 `rel_path` 路径级操作,
/// 违反"Plan B Inode 不依赖路径"原则; 长期应随各 FS `fs_resolve_inode` 完善后删除。
///
/// 每次调用 Inode 方法时, 通过 `mount_idx` 查找 `FileSystem` trait object,
/// 委托给对应的 `fs_*` 方法. 性能不是最优, 但保证正确性.
pub struct LegacyInode {
    handle: u32,
    mount_idx: u32,
    /// B06-15: 文件类型 (AtomicU8, stat 成功后刷新, 保证 is_dir 反映实际类型而非构造时快照)
    file_type: core::sync::atomic::AtomicU8,
    /// 文件相对路径 (供 stat/chmod/chown 等需要路径的操作使用)
    rel_path: alloc::string::String,
}

impl LegacyInode {
    /// 从 `FsOpenResult` 创建 `LegacyInode`
    pub fn from_fs_result(handle: u32, mount_idx: u32, file_type: u8, rel_path: &str) -> Self {
        Self {
            handle,
            mount_idx,
            file_type: core::sync::atomic::AtomicU8::new(file_type),
            rel_path: alloc::string::String::from(rel_path),
        }
    }
}

impl Inode for LegacyInode {
    fn read(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_read(self.handle, offset, buf, pwm)
        })
    }

    fn write(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_write(self.handle, offset, buf, pwm)
        })
    }

    fn stat(&self, pwm: u64) -> KernelResult<VfsStat> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        let st = fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_stat(&self.rel_path, pwm)
        })?;
        // B06-15: stat 成功后刷新 file_type, 保证 is_dir 反映实际文件类型 (不再停留在构造时快照)
        self.file_type
            .store(st.file_type, core::sync::atomic::Ordering::Release);
        Ok(st)
    }

    fn truncate(&self, size: u64, pwm: u64) -> KernelResult<()> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_truncate(self.handle, size, pwm)
        })
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_seek(self.handle, offset, whence, current_offset)
        })
    }

    fn is_dir(&self) -> bool {
        // B06-15: 读取 AtomicU8 类型 (stat 后刷新), 与 VfsFileType::Dir 比较
        self.file_type.load(core::sync::atomic::Ordering::Acquire) == VfsFileType::Dir.as_u8()
    }

    fn node_id(&self) -> u32 {
        self.handle
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }

    fn chmod(&self, mode: u16, pwm: u64) -> KernelResult<()> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_chmod(&self.rel_path, mode, pwm)
        })
    }

    fn chown(&self, owner_pwm: u64, group_pwm: u64, pwm: u64) -> KernelResult<()> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_chown(&self.rel_path, owner_pwm, group_pwm, pwm)
        })
    }

    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        // LegacyInode: 委托给底层 FileSystem
        // 默认返回 NotSupported (各文件系统可覆盖)
        Err(KernelError::NotSupported)
    }

    fn pread_inode(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        use crate::services::fs::vfs_manager::VFS_MANAGER;
        let fs = {
            let mounts = VFS_MANAGER.mounts.lock();
            if (self.mount_idx as usize) < mounts.len() && mounts[self.mount_idx as usize].used {
                mounts[self.mount_idx as usize].get_fs()
            } else {
                None
            }
        };
        fs.map_or(Err(KernelError::NotInitialized), |f| {
            f.fs_pread_inode(self.handle, offset, buf, pwm)
        })
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
