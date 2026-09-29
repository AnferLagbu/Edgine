#![deny(unsafe_code)]
//! `RamFS` 核心实现 — services 层 (VFS 完整下沉)
//!
//! RamFS 数据结构 (`RamFsData`) 是 VFS 挂载机制的底层基座, 随 VFS 整体
//! 下沉至 services. 0 unsafe, 100% safe Rust.
//!
//! ## 依赖注入边界
//!
//! 具象 `RamFsInode` 归 services (`services::fs::inode`), 本模块在
//! fs_open / fs_create / fs_resolve_inode 中经 `FsBackend::make_ramfs_inode`
//! 工厂钩子 (backend_trait) 构造注入.
//!
//! ## 历史
//! - E6-5: 迁至 services::fs::ramfs_core (0 unsafe 化)
//! - DECISION-K 项 5: 回迁 framework, Inode 构造改走 backend 工厂钩子
//! - VFS 完整下沉: 随 VFS 子系统整体下沉 services, 挂载点经 `ramfs_fs()` 暴露

pub mod ramfs_data;
pub mod ramfs_node;

pub use ramfs_data::*;
pub use ramfs_node::*;

use crate::services::fs::KernelError;
use crate::services::fs::backend_trait::current_fs_backend;
use crate::services::fs::inode::Inode;
use crate::services::fs::{
    FileSystem, KernelResult, VFS_MAX_NAME, VfsDirEntry, VfsFileType, VfsOpenFlags, VfsSeekWhence,
    VfsStat,
};
use crate::framework::sync::IrqSpinLock as Mutex;

pub(crate) const RAMFS_MAX_NODES: usize = 256;
pub(crate) const RAMFS_MAX_BLOCKS: usize = 2048;
pub(crate) const RAMFS_BLOCK_SIZE: usize = crate::framework::mm::PAGE_SIZE as usize;
pub(crate) const RAMFS_MAX_ACES: usize = 128;
pub(crate) const INDIRECT_BLOCKS_PER_BLOCK: usize = RAMFS_BLOCK_SIZE / 4;
pub(crate) const SENSITIVITY_PUBLIC: u8 = 0;
pub(crate) const FS_CAP_READ: u64 = 1 << 0;
pub(crate) const FS_CAP_WRITE: u64 = 1 << 1;
pub(crate) const FS_CAP_CREATE: u64 = 1 << 3;

// ============================================================================
// 全局实例
// ============================================================================

pub static RAMFS_DATA: Mutex<RamFsData> = Mutex::new(RamFsData::new());

pub fn init() {
    let mut ramfs = RAMFS_DATA.lock();
    ramfs.mount("/");
}

// ============================================================================
// FileSystem trait 实现 (Inode 经 backend 钩子由 services 注入)
// ============================================================================

/// RamFS FileSystem trait 实现的载体 (无状态句柄)
///
/// 全局数据经 `RAMFS_DATA` 单例访问, 句柄本身不持有状态.
pub struct RamFsFileSystem;

/// 经 backend 工厂钩子构造 RamFS Inode (具象实现由 services 注入)
///
/// 后端未注册 (早期启动) 或构造失败时返回 Err, fail-closed.
fn make_inode(
    inode_id: u32,
    mount_idx: u32,
    fs_id: u32,
) -> KernelResult<alloc::sync::Arc<dyn Inode>> {
    current_fs_backend().make_ramfs_inode(inode_id, mount_idx, fs_id)
}

impl FileSystem for RamFsFileSystem {
    fn name(&self) -> &'static str {
        "ramfs"
    }

    fn fs_init(&self) -> KernelResult<()> {
        Ok(())
    }

    fn fs_mount(&self, path: &str) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        if ramfs.mount(path) != 0 {
            return Err(KernelError::Io);
        }
        Ok(())
    }

    fn fs_open(
        &self,
        rel_path: &str,
        flags: u32,
        pwm: u64,
    ) -> KernelResult<alloc::sync::Arc<dyn Inode>> {
        let mount_idx = 0; // RamFs 默认挂载索引
        let mut ramfs = RAMFS_DATA.lock();
        let fs_id = ramfs.fs_id;
        match ramfs.open(rel_path, flags, pwm) {
            Some((node_id, _offset, _file_type)) => {
                if (flags & VfsOpenFlags::TRUNC.bits()) != 0 {
                    ramfs.truncate(node_id, 0, pwm);
                }
                drop(ramfs);
                make_inode(node_id, mount_idx, fs_id)
            }
            None => Err(KernelError::FileNotFound),
        }
    }

    fn fs_close(&self, _handle: u32) -> KernelResult<()> {
        Ok(())
    }

    fn fs_read(&self, handle: u32, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        let mut ramfs = RAMFS_DATA.lock();
        let mut new_offset = offset;
        let result = ramfs.read(handle, &mut new_offset, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_write(&self, handle: u32, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        let mut ramfs = RAMFS_DATA.lock();
        let mut new_offset = offset;
        let result = ramfs.write(handle, &mut new_offset, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_stat(&self, rel_path: &str, _pwm: u64) -> KernelResult<VfsStat> {
        let ramfs = RAMFS_DATA.lock();
        let fs_id = ramfs.fs_id;
        match ramfs.resolve_path(rel_path) {
            Some(node_id) => {
                drop(ramfs); // 释放锁, 尝试 icache
                if let Some(cached) =
                    crate::services::fs::dcache::icache_lookup(fs_id, node_id)
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
                let ramfs = RAMFS_DATA.lock();
                ramfs
                    .stat(node_id)
                    .inspect(|st| {
                        crate::services::fs::dcache::icache_insert(
                            fs_id,
                            node_id,
                            st.file_type,
                            st.perm,
                            st.size as u32,
                            st.mtime,
                            st.ctime,
                            st.owner_pwm,
                            st.group_pwm,
                        );
                    })
                    .ok_or(KernelError::FileNotFound)
            }
            None => Err(KernelError::FileNotFound),
        }
    }

    fn fs_chmod(&self, rel_path: &str, mode: u16, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        let result = ramfs.chmod(rel_path, mode, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::PermissionDenied)
        }
    }

    fn fs_chown(
        &self,
        rel_path: &str,
        owner_pwm: u64,
        group_pwm: u64,
        pwm: u64,
    ) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        let result = ramfs.chown_ext(rel_path, owner_pwm, group_pwm, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::PermissionDenied)
        }
    }

    fn fs_mkdir(&self, rel_path: &str, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        let (parent_path, name) = rel_path.rfind('/').map_or(("/", rel_path), |pos| {
            if pos == 0 {
                ("/", &rel_path[1..])
            } else {
                (&rel_path[..pos], &rel_path[pos + 1..])
            }
        });
        if name.is_empty() {
            return Err(KernelError::InvalidArgument);
        }
        let result = ramfs.mkdir(parent_path, name, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_unlink(&self, rel_path: &str, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        let result = ramfs.unlink(rel_path, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::FileNotFound)
        }
    }

    fn fs_rmdir(&self, rel_path: &str, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        ramfs
            .resolve_path(rel_path)
            .map_or(Err(KernelError::FileNotFound), |node_id| {
                let stat = ramfs.stat(node_id);
                match stat {
                    Some(s) if s.file_type == VfsFileType::Dir.as_u8() => {
                        let result = ramfs.truncate(node_id, 0, pwm);
                        if result == 0 {
                            Ok(())
                        } else {
                            Err(KernelError::Io)
                        }
                    }
                    _ => Err(KernelError::NotADirectory),
                }
            })
    }

    fn fs_rename(&self, old_path: &str, new_path: &str, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        ramfs.unlink(old_path, pwm);
        ramfs.link(0, 0, new_path, pwm);
        Ok(())
    }

    fn fs_readdir(&self, handle: u32, offset: u64, entry: &mut VfsDirEntry) -> KernelResult<bool> {
        let mut ramfs = RAMFS_DATA.lock();
        let mut dir_offset = offset;
        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let mut raw_buf = alloc::vec![0u8; dirent_size];
        let result = ramfs.read(handle, &mut dir_offset, &mut raw_buf, 0);
        let raw_entry = RamFsDirEntry::read_at(&raw_buf, 0);
        if result <= 0 || raw_entry.node == 0 {
            return Ok(false);
        }
        entry.node = raw_entry.node;
        entry.file_type = raw_entry.file_type;
        let name_len = raw_entry
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(VFS_MAX_NAME);
        let copy_len = name_len.min(VFS_MAX_NAME);
        entry.name[..copy_len].copy_from_slice(&raw_entry.name[..copy_len]);
        if name_len < VFS_MAX_NAME {
            entry.name[name_len] = 0;
        }
        Ok(raw_entry.node != 0)
    }

    // L4 重构: 扩展方法实现 (override trait 默认实现)
    // P3-I-19: vfs_pread_inode trait 分发. 直接按 inode 寻址 (mmap prewarm).
    fn fs_pread_inode(
        &self,
        node_id: u32,
        offset: u64,
        buf: &mut [u8],
        pwm: u64,
    ) -> KernelResult<usize> {
        let mut ramfs = RAMFS_DATA.lock();
        let mut new_offset = offset;
        let result = ramfs.read(node_id, &mut new_offset, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_symlink(&self, target: &str, link_path: &str, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        let (parent_path, name) = link_path.rfind('/').map_or(("/", link_path), |pos| {
            if pos == 0 {
                ("/", &link_path[1..])
            } else {
                (&link_path[..pos], &link_path[pos + 1..])
            }
        });
        if name.is_empty() || name.contains('/') {
            return Err(KernelError::InvalidArgument);
        }
        let result = ramfs.symlink(target, parent_path, name, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_readlink(&self, rel_path: &str, buf: &mut [u8]) -> KernelResult<usize> {
        let ramfs = RAMFS_DATA.lock();
        ramfs
            .resolve_path(rel_path)
            .map_or(Err(KernelError::FileNotFound), |node_id| {
                let result = ramfs.readlink(node_id, buf);
                if result < 0 {
                    Err(KernelError::Io)
                } else {
                    Ok(result as usize)
                }
            })
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    fn fs_link(&self, old_path: &str, new_path: &str, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        let target_node = match ramfs.resolve_path(old_path) {
            Some(n) => n,
            None => return Err(KernelError::FileNotFound),
        };
        if (target_node as usize) >= ramfs.nodes.len() || !ramfs.nodes[target_node as usize].used {
            return Err(KernelError::FileNotFound);
        }
        if ramfs.nodes[target_node as usize].file_type == VfsFileType::Dir as u8 {
            return Err(KernelError::PermissionDenied);
        }
        let (parent_path, name) = new_path.rfind('/').map_or(("/", new_path), |pos| {
            if pos == 0 {
                ("/", &new_path[1..])
            } else {
                (&new_path[..pos], &new_path[pos + 1..])
            }
        });
        if name.is_empty() || name.contains('/') {
            return Err(KernelError::InvalidArgument);
        }
        let parent_num = match ramfs.resolve_path(parent_path) {
            Some(n) => n,
            None => return Err(KernelError::FileNotFound),
        };
        let result = ramfs.link(parent_num, target_node, name, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_truncate(&self, handle: u32, size: u64, pwm: u64) -> KernelResult<()> {
        let mut ramfs = RAMFS_DATA.lock();
        let result = ramfs.truncate(handle, size, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_seek(
        &self,
        handle: u32,
        offset: i64,
        whence: VfsSeekWhence,
        current: u64,
    ) -> KernelResult<u64> {
        let ramfs = RAMFS_DATA.lock();
        ramfs
            .seek(handle, current, offset, whence)
            .ok_or(KernelError::InvalidArgument)
    }

    fn fs_resolve_path(&self, rel_path: &str) -> Option<u32> {
        let ramfs = RAMFS_DATA.lock();
        ramfs.resolve_path(rel_path)
    }

    fn fs_create(
        &self,
        parent_path: &str,
        name: &str,
        pwm: u64,
    ) -> KernelResult<alloc::sync::Arc<dyn Inode>> {
        let mount_idx = 0;
        let mut ramfs = RAMFS_DATA.lock();
        let fs_id = ramfs.fs_id;
        ramfs
            .create_file(parent_path, name, pwm)
            .map_or(Err(KernelError::NoSpace), |new_inode| {
                drop(ramfs);
                make_inode(new_inode, mount_idx, fs_id)
            })
    }

    fn fs_resolve_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
    ) -> Option<alloc::sync::Arc<dyn Inode>> {
        // fs_resolve_inode 无路径上下文, 取全局实例的 fs_id (RAMFS_DATA 单例)
        let fs_id = RAMFS_DATA.lock().fs_id;
        make_inode(inode_id, mount_idx, fs_id).ok()
    }
}

/// RamFS FileSystem 全局实例 (供挂载路径经 `ramfs_fs()` 暴露)
static RAMFS_FS: RamFsFileSystem = RamFsFileSystem;

/// 获取 RamFS FileSystem trait object (VFS 挂载路径用)
pub fn ramfs_fs() -> &'static dyn FileSystem {
    &RAMFS_FS
}
