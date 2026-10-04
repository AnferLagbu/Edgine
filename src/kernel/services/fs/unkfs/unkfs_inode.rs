#![deny(unsafe_code)]

use super::unkfs_data::{UnkfsData, get_unkfs};
use crate::services::fs::inode::Inode;
use crate::services::fs::{KernelError, KernelResult, VfsFileType, VfsSeekWhence, VfsStat};

/// `UNKFS` 文件 Inode — 直接持有 fd 编号
pub struct UnkfsInode {
    fd: u32,
    mount_idx: u32,
    rel_path: alloc::string::String,
}

impl UnkfsInode {
    pub fn new(fd: u32, mount_idx: u32, rel_path: &str) -> Self {
        Self {
            fd,
            mount_idx,
            rel_path: alloc::string::String::from(rel_path),
        }
    }
}

impl Inode for UnkfsInode {
    fn read(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        let unkfs = get_unkfs();
        let result = unkfs.read(self.fd, buf, buf.len() as u32);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn write(&self, _offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        let unkfs = get_unkfs();
        let result = unkfs.write(self.fd, buf, buf.len() as u32);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn stat(&self, pwm: u64) -> KernelResult<VfsStat> {
        let unkfs = get_unkfs();
        unkfs
            .stat(&self.rel_path, pwm)
            .map_or(Err(KernelError::FileNotFound), |obj| {
                Ok(VfsStat {
                    node_id: obj.obj_id as u32,
                    mode: obj.pwm_perm,
                    size: obj.size as u32,
                    owner_pwm: obj.owner_pwm,
                    group_pwm: obj.group_pwm,
                    perm: obj.pwm_perm,
                    sensitivity: obj.sensitivity,
                    atime: obj.atime,
                    mtime: obj.mtime,
                    ctime: obj.ctime,
                    file_type: if obj.is_dir() {
                        VfsFileType::Dir.as_u8()
                    } else {
                        VfsFileType::File.as_u8()
                    },
                    ..Default::default()
                })
            })
    }

    fn truncate(&self, _size: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        let new_offset = match whence {
            VfsSeekWhence::Set => offset as u64,
            VfsSeekWhence::Cur => current_offset.saturating_add(offset as u64),
            VfsSeekWhence::End => current_offset.saturating_add(offset as u64),
        };
        Ok(new_offset)
    }

    fn is_dir(&self) -> bool {
        false
    }

    // B06-08: 显式覆盖 chmod/chown (委托给底层 UnkfsData, 底层支持路径级权限修改)
    fn chmod(&self, mode: u16, pwm: u64) -> KernelResult<()> {
        let unkfs = get_unkfs();
        if unkfs.chmod(&self.rel_path, mode, pwm) == 0 {
            Ok(())
        } else {
            Err(KernelError::PermissionDenied)
        }
    }

    fn chown(&self, owner_pwm: u64, group_pwm: u64, pwm: u64) -> KernelResult<()> {
        let unkfs = get_unkfs();
        if unkfs.chown_ext(&self.rel_path, owner_pwm, group_pwm, pwm) == 0 {
            Ok(())
        } else {
            Err(KernelError::PermissionDenied)
        }
    }

    // B06-08 同源: 委托底层 UnkfsData (按路径定位 DMU 对象并落盘)
    fn set_times(&self, atime: u64, mtime: u64, pwm: u64) -> KernelResult<()> {
        let unkfs = get_unkfs();
        unkfs.set_times(&self.rel_path, atime, mtime, pwm)
    }

    fn node_id(&self) -> u32 {
        self.fd
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

// ============================================================================
// E6-4: FileSystem trait 实现
// ============================================================================

impl crate::services::fs::FileSystem for UnkfsData {
    fn name(&self) -> &'static str {
        "unkfs"
    }

    fn fs_init(&self) -> crate::services::fs::KernelResult<()> {
        if !self.is_initialized() {
            self.init();
        }
        Ok(())
    }

    fn fs_mount(&self, _path: &str) -> crate::services::fs::KernelResult<()> {
        if !self.is_initialized() {
            self.init();
        }
        Ok(())
    }

    fn fs_format(&self) -> crate::services::fs::KernelResult<()> {
        // DECISION-K 项 6: 封装原 framework fsformat 路径的字段级访问,
        // 磁盘选择策略 (已发现驱动器优先, 回退启动盘) 归 services.
        let (drive_id, part_start) = self.drives_discovered.lock().first().copied().unwrap_or((
            self.disk_drive.load(core::sync::atomic::Ordering::Acquire),
            self.partition_start
                .load(core::sync::atomic::Ordering::Acquire),
        ));
        self.format_drive(drive_id, part_start);
        if self.is_disk_mode() {
            Ok(())
        } else {
            Err(crate::services::fs::KernelError::NotSupported)
        }
    }

    fn fs_open(
        &self,
        rel_path: &str,
        flags: u32,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>>
    {
        match self.open(rel_path, flags, pwm) {
            Ok(fd) => Ok(alloc::sync::Arc::new(UnkfsInode::new(
                fd as u32, 0, rel_path,
            ))),
            Err(e) => Err(e),
        }
    }

    fn fs_close(&self, handle: u32) -> crate::services::fs::KernelResult<()> {
        let result = self.close(handle);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_read(
        &self,
        handle: u32,
        offset: u64,
        buf: &mut [u8],
        _pwm: u64,
    ) -> crate::services::fs::KernelResult<usize> {
        let _ = offset;
        let result = self.read(handle, buf, buf.len() as u32);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_write(
        &self,
        handle: u32,
        offset: u64,
        buf: &[u8],
        _pwm: u64,
    ) -> crate::services::fs::KernelResult<usize> {
        let _ = offset;
        let result = self.write(handle, buf, buf.len() as u32);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_stat(
        &self,
        rel_path: &str,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<crate::services::fs::VfsStat> {
        self.stat(rel_path, pwm)
            .map_or(Err(KernelError::FileNotFound), |obj| {
                Ok(crate::services::fs::VfsStat {
                    node_id: obj.obj_id as u32,
                    mode: obj.pwm_perm,
                    size: obj.size as u32,
                    owner_pwm: obj.owner_pwm,
                    group_pwm: obj.group_pwm,
                    perm: obj.pwm_perm,
                    sensitivity: obj.sensitivity,
                    atime: obj.atime,
                    mtime: obj.mtime,
                    ctime: obj.ctime,
                    file_type: if obj.is_dir() {
                        crate::framework::fs::VfsFileType::Dir.as_u8()
                    } else {
                        crate::framework::fs::VfsFileType::File.as_u8()
                    },
                    ..Default::default()
                })
            })
    }

    // POSIX utimensat 通路: 委托 UnkfsData::set_times (按路径定位 DMU 对象)
    fn fs_utimensat(
        &self,
        rel_path: &str,
        atime: u64,
        mtime: u64,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<()> {
        self.set_times(rel_path, atime, mtime, pwm)
    }

    fn fs_chmod(
        &self,
        rel_path: &str,
        mode: u16,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<()> {
        let result = self.chmod(rel_path, mode, pwm);
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
    ) -> crate::services::fs::KernelResult<()> {
        let result = self.chown_ext(rel_path, owner_pwm, group_pwm, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::PermissionDenied)
        }
    }

    fn fs_mkdir(&self, rel_path: &str, pwm: u64) -> crate::services::fs::KernelResult<()> {
        let result = self.mkdir(rel_path, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_unlink(&self, rel_path: &str, pwm: u64) -> crate::services::fs::KernelResult<()> {
        let result = self.unlink(rel_path, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::FileNotFound)
        }
    }

    fn fs_rmdir(&self, rel_path: &str, pwm: u64) -> crate::services::fs::KernelResult<()> {
        let result = self.unlink(rel_path, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::FileNotFound)
        }
    }

    fn fs_rename(
        &self,
        old_path: &str,
        new_path: &str,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<()> {
        let result = self.rename(old_path, new_path, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_readdir(
        &self,
        _handle: u32,
        _offset: u64,
        _entry: &mut crate::services::fs::VfsDirEntry,
    ) -> crate::services::fs::KernelResult<bool> {
        Err(KernelError::NotSupported)
    }

    fn fs_symlink(
        &self,
        target: &str,
        link_path: &str,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<()> {
        let result = self.symlink(target, link_path, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_readlink(
        &self,
        rel_path: &str,
        buf: &mut [u8],
    ) -> crate::services::fs::KernelResult<usize> {
        let result = self.readlink(rel_path, buf, 0);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_link(
        &self,
        old_path: &str,
        new_path: &str,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<()> {
        let result = self.link(old_path, new_path, pwm);
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
        whence: crate::services::fs::VfsSeekWhence,
        _current: u64,
    ) -> crate::services::fs::KernelResult<u64> {
        let result = self.seek(handle, offset, whence as u32);
        if result < 0 {
            Err(KernelError::InvalidArgument)
        } else {
            Ok(result as u64)
        }
    }

    // P3-I-18: trait fs_sync 包装 self.sync() (i32 → KernelResult<()>).
    fn fs_sync(&self) -> crate::services::fs::KernelResult<()> {
        let r = self.sync();
        if r == 0 { Ok(()) } else { Err(KernelError::Io) }
    }

    fn fs_resolve_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
    ) -> Option<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        Some(alloc::sync::Arc::new(UnkfsInode::new(
            inode_id, mount_idx, "",
        )))
    }

    // ---- 扩展属性 ----

    fn fs_setxattr(
        &self,
        rel_path: &str,
        name: &str,
        value: &[u8],
        pwm: u64,
    ) -> crate::services::fs::KernelResult<()> {
        let result = self.setxattr(rel_path, name, value, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_getxattr(
        &self,
        rel_path: &str,
        name: &str,
        buf: &mut [u8],
        pwm: u64,
    ) -> crate::services::fs::KernelResult<usize> {
        let result = self.getxattr(rel_path, name, buf, pwm);
        if result < 0 {
            Err(KernelError::FileNotFound)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_listxattr(
        &self,
        rel_path: &str,
        buf: &mut [u8],
        pwm: u64,
    ) -> crate::services::fs::KernelResult<usize> {
        let result = self.listxattr(rel_path, buf, pwm);
        if result < 0 {
            Err(KernelError::FileNotFound)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_removexattr(
        &self,
        rel_path: &str,
        name: &str,
        pwm: u64,
    ) -> crate::services::fs::KernelResult<()> {
        let result = self.removexattr(rel_path, name, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::FileNotFound)
        }
    }
}
