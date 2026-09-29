#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! ext2 `FileSystem` trait 实现

use super::read::Ext2Fs;
use crate::framework::credo::api as pwm_api;
use crate::framework::sync::IrqSpinLock as Mutex;
use crate::services::fs::KernelError;
use crate::services::fs::vfs_types::{
    FileSystem, KernelResult, VfsDirEntry, VfsSeekWhence, VfsStat,
};

/// ext2 文件系统实例 (全局单例)
static EXT2_FS: Mutex<Option<Ext2Fs>> = Mutex::new(None);

// ============================================================================
// Ext2 Inode — ext2 文件 Inode 实现
// ============================================================================

use crate::services::fs::inode::Inode;

/// ext2 文件 Inode — 直接持有 inode 编号
pub struct Ext2Inode {
    inode_num: u32,
    mount_idx: u32,
}

impl Ext2Inode {
    pub fn new(inode_num: u32, mount_idx: u32) -> Self {
        Self {
            inode_num,
            mount_idx,
        }
    }
}

impl Inode for Ext2Inode {
    fn read(&self, offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;
        fs.read_file(self.inode_num, offset, buf)
    }

    fn write(&self, offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;
        fs.write_file(self.inode_num, offset, buf)
    }

    fn stat(&self, _pwm: u64) -> KernelResult<VfsStat> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;
        let inode = fs.read_inode(self.inode_num)?;
        Ok(VfsStat {
            node_id: self.inode_num,
            mode: inode.perm(),
            uid: u32::from(inode.i_uid),
            gid: u32::from(inode.i_gid),
            size: inode.i_size,
            atime: u64::from(inode.i_atime),
            mtime: u64::from(inode.i_mtime),
            ctime: u64::from(inode.i_ctime),
            owner_pwm: 0,
            group_pwm: 0,
            perm: inode.perm(),
            file_type: inode.file_type(),
            sensitivity: 0,
        })
    }

    fn truncate(&self, _size: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        let file_size = {
            let mut fs_guard = EXT2_FS.lock();
            let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;
            let inode = fs.read_inode(self.inode_num)?;
            u64::from(inode.i_size)
        };
        let new_offset = match whence {
            VfsSeekWhence::Set => offset as u64,
            VfsSeekWhence::Cur => current_offset.saturating_add(offset as u64),
            VfsSeekWhence::End => file_size.saturating_add(offset as u64),
        };
        Ok(new_offset)
    }

    fn is_dir(&self) -> bool {
        if let Some(mut fs_guard) = EXT2_FS.try_lock() {
            if let Some(fs) = fs_guard.as_mut() {
                if let Ok(inode) = fs.read_inode(self.inode_num) {
                    return inode.file_type() == 1; // DIR
                }
            }
        }
        false
    }

    /// ext2 磁盘 inode 时间戳写回
    ///
    /// `atime` / `mtime` 为 `u64::MAX` 时表示该字段不修改 (utimensat 的
    /// `UTIME_OMIT` 语义). ext2 磁盘格式存 32 位秒, 超出范围按 `u32::MAX` 截顶.
    ///
    /// ## 属主判据 (丙批审查 B1 整改)
    ///
    /// 与 `NestfsData::set_times` 同判据 (属主或特权级 0). ext2 磁盘 inode 无
    /// `owner_pwm` 字段, 属主身份改由 `i_uid` 与该 pwm 映射的 uid 比对; 判据与
    /// `save_inode` 同处 `EXT2_FS` 锁域, 不引入二次路径解析 (无 TOCTOU 窗口).
    /// `pwm` 未注册时 `pwm_get_uid` 返回 `u32::MAX` 且特权级为 `0xFF`, 判为拒绝.
    fn set_times(&self, atime: u64, mtime: u64, pwm: u64) -> KernelResult<()> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;
        let mut inode = fs.read_inode(self.inode_num)?;
        if u32::from(inode.i_uid) != pwm_api::pwm_get_uid(pwm)
            && pwm_api::pwm_get_privilege_level(pwm) != 0
        {
            return Err(KernelError::PermissionDenied);
        }
        if atime != u64::MAX {
            inode.i_atime = u32::try_from(atime).unwrap_or(u32::MAX);
        }
        if mtime != u64::MAX {
            inode.i_mtime = u32::try_from(mtime).unwrap_or(u32::MAX);
            // i_ctime = 元数据变更时刻, 语义独立于 i_mtime —— 取**当前秒值**,
            // 换算方式与 `services/fs/stat.rs::utimensat_syscall` 同源
            // (tick / frequency). 不可改用 TSC 周期计数: 那是周期数而非秒,
            // 落 32 位磁盘字段会被截顶成 `u32::MAX`.
            let now_secs = crate::framework::syscall::api::get_ticks()
                / u64::from(crate::framework::timer::get_frequency());
            inode.i_ctime = u32::try_from(now_secs).unwrap_or(u32::MAX);
        }
        fs.save_inode(self.inode_num, &inode)
    }

    fn node_id(&self) -> u32 {
        self.inode_num
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

/// ext2 `FileSystem` trait 实现
pub struct Ext2FileSystem;

impl FileSystem for Ext2FileSystem {
    fn name(&self) -> &'static str {
        "ext2"
    }

    fn fs_init(&self) -> KernelResult<()> {
        // ext2 不需要特殊初始化
        Ok(())
    }

    fn fs_mount(&self, _path: &str) -> KernelResult<()> {
        // ext2 挂载需要指定设备
        // 当前实现: 假设设备 0
        let fs = Ext2Fs::open(0).map_err(|_| KernelError::Io)?;
        let mut guard = EXT2_FS.lock();
        *guard = Some(fs);
        Ok(())
    }

    fn fs_open(
        &self,
        rel_path: &str,
        _flags: u32,
        _pwm: u64,
    ) -> KernelResult<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        let inode_num = fs.lookup_path(rel_path)?;

        Ok(alloc::sync::Arc::new(Ext2Inode::new(inode_num, 0)))
    }

    fn fs_close(&self, _handle: u32) -> KernelResult<()> {
        Ok(())
    }

    fn fs_read(&self, handle: u32, offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        fs.read_file(handle, offset, buf)
    }

    fn fs_write(&self, handle: u32, offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        fs.write_file(handle, offset, buf)
    }

    fn fs_stat(&self, rel_path: &str, _pwm: u64) -> KernelResult<VfsStat> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        let inode_num = fs.lookup_path(rel_path)?;
        let inode = fs.read_inode(inode_num)?;

        Ok(VfsStat {
            node_id: inode_num,
            mode: inode.perm(),
            uid: u32::from(inode.i_uid),
            gid: u32::from(inode.i_gid),
            size: inode.i_size,
            atime: u64::from(inode.i_atime),
            mtime: u64::from(inode.i_mtime),
            ctime: u64::from(inode.i_ctime),
            owner_pwm: 0,
            group_pwm: 0,
            perm: inode.perm(),
            file_type: inode.file_type(),
            sensitivity: 0,
        })
    }

    // POSIX utimensat 通路: 路径 → inode → 磁盘写回 (供 syscall 层 `vfs_utimensat`)
    fn fs_utimensat(&self, rel_path: &str, atime: u64, mtime: u64, pwm: u64) -> KernelResult<()> {
        let inode_num = {
            let mut fs_guard = EXT2_FS.lock();
            let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;
            fs.lookup_path(rel_path)?
        };
        Ext2Inode::new(inode_num, 0).set_times(atime, mtime, pwm)
    }

    fn fs_chmod(&self, _rel_path: &str, _mode: u16, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn fs_chown(
        &self,
        _rel_path: &str,
        _owner_pwm: u64,
        _group_pwm: u64,
        _pwm: u64,
    ) -> KernelResult<()> {
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn fs_mkdir(&self, rel_path: &str, _pwm: u64) -> KernelResult<()> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        // 解析路径
        let (parent_path, name) = rel_path.rfind('/').map_or(("/", rel_path), |pos| {
            if pos == 0 {
                ("/", &rel_path[1..])
            } else {
                (&rel_path[..pos], &rel_path[pos + 1..])
            }
        });

        fs.mkdir(parent_path, name)?;
        Ok(())
    }

    fn fs_unlink(&self, rel_path: &str, _pwm: u64) -> KernelResult<()> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        // 解析路径
        let (parent_path, name) = rel_path.rfind('/').map_or(("/", rel_path), |pos| {
            if pos == 0 {
                ("/", &rel_path[1..])
            } else {
                (&rel_path[..pos], &rel_path[pos + 1..])
            }
        });

        // 获取父目录 inode
        let parent_inode_num = fs.lookup_path(parent_path)?;
        let inode_num = fs.remove_dir_entry(parent_inode_num, name)?;

        // 释放 inode 和块
        let inode = fs.read_inode(inode_num)?;
        for i in 0..12 {
            if inode.i_block[i] != 0 {
                fs.deallocate_block(inode.i_block[i])?;
            }
        }

        let group_idx =
            (inode_num - fs.super_block.first_inode()) / fs.super_block.s_inodes_per_group;
        if (group_idx as usize) < fs.block_groups.len() {
            super::alloc::free_inode(
                fs.device_idx,
                &fs.super_block,
                &fs.block_groups[group_idx as usize],
                inode_num,
            )?;
        }

        Ok(())
    }

    fn fs_rmdir(&self, rel_path: &str, _pwm: u64) -> KernelResult<()> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        // 解析路径
        let (parent_path, name) = rel_path.rfind('/').map_or(("/", rel_path), |pos| {
            if pos == 0 {
                ("/", &rel_path[1..])
            } else {
                (&rel_path[..pos], &rel_path[pos + 1..])
            }
        });

        fs.rmdir(parent_path, name)?;
        Ok(())
    }

    fn fs_rename(&self, _old_path: &str, _new_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_readdir(&self, handle: u32, offset: u64, entry: &mut VfsDirEntry) -> KernelResult<bool> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        let entries = fs.read_dir(handle)?;
        let idx = offset as usize;

        if idx >= entries.len() {
            return Ok(false);
        }

        let ext2_entry = &entries[idx];
        entry.node = ext2_entry.inode;
        entry.file_type = ext2_entry.file_type;
        entry.set_name(ext2_entry.get_name());

        Ok(true)
    }

    // L4 重构: 扩展方法实现 (override trait 默认实现)
    fn fs_symlink(&self, target: &str, link_path: &str, _pwm: u64) -> KernelResult<()> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        fs.symlink(target, link_path)?;
        Ok(())
    }

    fn fs_readlink(&self, rel_path: &str, buf: &mut [u8]) -> KernelResult<usize> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        let inode_num = fs.lookup_path(rel_path)?;
        let target = fs.readlink(inode_num)?;

        let copy_len = target.len().min(buf.len());
        buf[..copy_len].copy_from_slice(&target[..copy_len]);

        Ok(copy_len)
    }

    fn fs_link(&self, old_path: &str, new_path: &str, _pwm: u64) -> KernelResult<()> {
        let mut fs_guard = EXT2_FS.lock();
        let fs = fs_guard.as_mut().ok_or(KernelError::NotInitialized)?;

        fs.link(old_path, new_path)?;
        Ok(())
    }

    fn fs_resolve_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
    ) -> Option<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        Some(alloc::sync::Arc::new(Ext2Inode::new(inode_id, mount_idx)))
    }
}

/// 初始化 ext2 文件系统
pub fn init() {
    // ext2 需要手动挂载, 不自动初始化
}
