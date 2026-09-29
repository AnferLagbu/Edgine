#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! exFAT `FileSystem` trait 实现

use super::read::ExfatFs;
use crate::framework::sync::IrqSpinLock as Mutex;
use crate::services::fs::KernelError;
use crate::services::fs::vfs_types::{
    FileSystem, KernelResult, VfsDirEntry, VfsSeekWhence, VfsStat,
};

/// exFAT 文件系统实例 (全局单例)
static EXFAT_FS: Mutex<Option<ExfatFs>> = Mutex::new(None);

// ============================================================================
// ExfatInode — exFAT 文件 Inode 实现
// ============================================================================

use crate::services::fs::inode::Inode;

/// exFAT 文件 Inode — 直接持有 cluster 编号
pub struct ExfatInode {
    cluster: u32,
    mount_idx: u32,
}

impl ExfatInode {
    pub fn new(cluster: u32, mount_idx: u32) -> Self {
        Self { cluster, mount_idx }
    }
}

impl Inode for ExfatInode {
    fn read(&self, offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        let fs_guard = EXFAT_FS.lock();
        let fs = fs_guard.as_ref().ok_or(KernelError::NotInitialized)?;
        fs.read_file(self.cluster, offset, buf)
    }

    fn write(&self, offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        let fs_guard = EXFAT_FS.lock();
        let fs = fs_guard.as_ref().ok_or(KernelError::NotInitialized)?;
        fs.write_file(self.cluster, offset, buf)
    }

    fn stat(&self, _pwm: u64) -> KernelResult<VfsStat> {
        Ok(VfsStat {
            node_id: self.cluster,
            mode: 0o777,
            perm: 0o777,
            file_type: 0,
            ..VfsStat::default()
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
            VfsSeekWhence::End => current_offset.saturating_add(offset as u64), // exFAT 简化: 无 size 信息
        };
        Ok(new_offset)
    }

    fn is_dir(&self) -> bool {
        false // 简化: exFAT 目录判断需查 FAT 表
    }

    // `set_times` 刻意不覆写: 原覆写直接 `Ok(())` 既不写回也不做事, 对上层
    // 伪装成功 (与 B1 同类"静默成功"风险) — 删覆写后落回 trait 默认
    // `Err(KernelError::NotSupported)`, 如实上报"不支持"。
    //
    // exFAT 正式任务 (C1/T6-③; 登记 docs/plan/archive/audit-fix-09-hard-rules-deadcode.md D-5)
    // 三项前置仍缺:
    //   1. 无目录项定位 — `ExfatFs::lookup_path` 只返回首簇号, 丢弃目录项
    //      所在扇区偏移, 无法回写条目 (exfat/read.rs);
    //   2. 无时间戳编解码 — `ExfatDirEntry` 未解析 exFAT 时间戳 (2 秒精度
    //      + 10ms + UTC 偏移) 且无 SetChecksum 重算 (exfat/dir.rs);
    //   3. 无验证载体 — `ExfatFileSystem` 未注册到任何文件系统表, 且无
    //      exFAT 镜像/块设备 mock.

    fn node_id(&self) -> u32 {
        self.cluster
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

/// exFAT `FileSystem` trait 实现
pub struct ExfatFileSystem;

impl FileSystem for ExfatFileSystem {
    fn name(&self) -> &'static str {
        "exfat"
    }

    fn fs_init(&self) -> KernelResult<()> {
        Ok(())
    }

    fn fs_mount(&self, _path: &str) -> KernelResult<()> {
        let fs = ExfatFs::open(0).map_err(|_| KernelError::Io)?;
        let mut guard = EXFAT_FS.lock();
        *guard = Some(fs);
        Ok(())
    }

    fn fs_open(
        &self,
        rel_path: &str,
        _flags: u32,
        _pwm: u64,
    ) -> KernelResult<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        let fs_guard = EXFAT_FS.lock();
        let fs = fs_guard.as_ref().ok_or(KernelError::NotInitialized)?;

        let cluster = fs.lookup_path(rel_path)?;

        Ok(alloc::sync::Arc::new(ExfatInode::new(cluster, 0)))
    }

    fn fs_close(&self, _handle: u32) -> KernelResult<()> {
        Ok(())
    }

    fn fs_read(&self, handle: u32, offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        let fs_guard = EXFAT_FS.lock();
        let fs = fs_guard.as_ref().ok_or(KernelError::NotInitialized)?;

        fs.read_file(handle, offset, buf)
    }

    fn fs_write(&self, handle: u32, offset: u64, buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        let fs_guard = EXFAT_FS.lock();
        let fs = fs_guard.as_ref().ok_or(KernelError::NotInitialized)?;

        fs.write_file(handle, offset, buf)
    }

    fn fs_stat(&self, rel_path: &str, _pwm: u64) -> KernelResult<VfsStat> {
        let fs_guard = EXFAT_FS.lock();
        let fs = fs_guard.as_ref().ok_or(KernelError::NotInitialized)?;

        let cluster = fs.lookup_path(rel_path)?;

        Ok(VfsStat {
            node_id: cluster,
            mode: 0o777,
            uid: 0,
            gid: 0,
            size: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            owner_pwm: 0,
            group_pwm: 0,
            perm: 0o777,
            file_type: 0,
            sensitivity: 0,
        })
    }

    fn fs_chmod(&self, _rel_path: &str, _mode: u16, _pwm: u64) -> KernelResult<()> {
        Ok(())
    }

    fn fs_chown(
        &self,
        _rel_path: &str,
        _owner_pwm: u64,
        _group_pwm: u64,
        _pwm: u64,
    ) -> KernelResult<()> {
        Ok(())
    }

    fn fs_mkdir(&self, _rel_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_unlink(&self, _rel_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_rmdir(&self, _rel_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_rename(&self, _old_path: &str, _new_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_readdir(&self, handle: u32, offset: u64, entry: &mut VfsDirEntry) -> KernelResult<bool> {
        let fs_guard = EXFAT_FS.lock();
        let fs = fs_guard.as_ref().ok_or(KernelError::NotInitialized)?;

        let entries = fs.read_dir_entries(handle)?;
        let idx = offset as usize;

        if idx >= entries.len() {
            return Ok(false);
        }

        let ext2_entry = &entries[idx];
        entry.node = idx as u32;
        entry.file_type = u8::from(ext2_entry.file_attributes() & 0x10 != 0);
        entry.set_name(&alloc::format!("entry_{idx}"));

        Ok(true)
    }

    // L4 重构: 扩展方法实现 (override trait 默认实现)
    fn fs_symlink(&self, _target: &str, _link_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn fs_readlink(&self, _rel_path: &str, _buf: &mut [u8]) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn fs_link(&self, _old_path: &str, _new_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn fs_resolve_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
    ) -> Option<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        Some(alloc::sync::Arc::new(ExfatInode::new(inode_id, mount_idx)))
    }
}

/// 初始化 exFAT 文件系统
pub fn init() {
    // exFAT 需要手动挂载
}
