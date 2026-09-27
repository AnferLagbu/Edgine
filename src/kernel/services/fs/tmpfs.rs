#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! tmpfs 基于内存的文件系统
//!
//! 结构: 复用 framework `RamFsData` 作为存储引擎, 在其上叠加容量上限
//! (`max_size`/`used_size`) 策略, 提供 POSIX 风格临时文件语义。

use alloc::sync::Arc;

use crate::framework::fs::KernelError;
use crate::framework::fs::ramfs::{RamFsData, RamFsDirEntry};
use crate::framework::sync::IrqSpinLock as Mutex;
use crate::services::fs::inode::Inode;
use crate::services::fs::vfs_types::{
    FileSystem, KernelResult, VFS_MAX_NAME, VfsDirEntry, VfsFileType, VfsSeekWhence, VfsStat,
};

// ============================================================================
// TmpFs Inode — 临时文件 Inode 实现
// ============================================================================

/// 临时文件 Inode — TmpFS 的 Inode 实现
pub struct TmpFsInode {
    node_id: u32,
    mount_idx: u32,
}

impl TmpFsInode {
    /// 构造 TmpFS Inode
    pub fn new(node_id: u32, mount_idx: u32) -> Self {
        Self { node_id, mount_idx }
    }
}

impl Inode for TmpFsInode {
    fn read(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;
        let mut off = offset;
        let result = fs.inner.read(self.node_id, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn write(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;
        let mut off = offset;
        let result = fs.inner.write(self.node_id, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn stat(&self, pwm: u64) -> KernelResult<VfsStat> {
        let fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;
        fs.inner.get_stat(self.node_id, pwm)
    }

    fn truncate(&self, size: u64, pwm: u64) -> KernelResult<()> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;
        let rc = fs.inner.truncate(self.node_id, size, pwm);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        let file_size = {
            let fs = TMPFS_DATA.lock();
            fs.ensure_mounted()?;
            u64::from(fs.inner.get_file_size(self.node_id).unwrap_or(0))
        };
        let new_offset = match whence {
            VfsSeekWhence::Set => offset as u64,
            VfsSeekWhence::Cur => current_offset.saturating_add(offset as u64),
            VfsSeekWhence::End => file_size.saturating_add(offset as u64),
        };
        Ok(new_offset)
    }

    fn is_dir(&self) -> bool {
        let fs = TMPFS_DATA.lock();
        let idx = self.node_id as usize;
        // 越界节点一律视为非目录, 避免索引 panic (替代原先硬编码上界 256)
        idx < fs.inner.nodes.len() && fs.inner.nodes[idx].file_type == VfsFileType::Dir.as_u8()
    }

    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        // TmpFS: 内存文件系统, 无持久时间戳
        Ok(())
    }

    fn node_id(&self) -> u32 {
        self.node_id
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

/// tmpfs 默认最大大小 (64MB)
const TMPFS_DEFAULT_MAX_SIZE: u64 = 64 * 1024 * 1024;

/// tmpfs 数据结构
pub struct TmpFsData {
    /// 内部 ramfs 数据
    pub inner: RamFsData,
    /// 最大容量限制 (字节)
    max_size: u64,
    /// 当前已用空间 (字节)
    used_size: u64,
    /// 是否已挂载 (替代 `Option` 的 `None` 语义, 作为 `NotInitialized` 判据)
    mounted: bool,
}

impl TmpFsData {
    /// BSS 常量初始化 — 全部字段置空, 仅供 `static` 初始化位置调用。
    ///
    /// 关键约束: `TmpFsData` 体量约 8 MiB (内含 `RamFsData`)。若在普通函数中
    /// 按值构造会占用内核栈, 故 `fs_mount` 一律就地写字段, 不得按值构造本结构。
    pub const fn empty() -> Self {
        Self {
            inner: RamFsData::new(),
            max_size: TMPFS_DEFAULT_MAX_SIZE,
            used_size: 0,
            mounted: false,
        }
    }

    /// 未挂载时返回 `NotInitialized`。
    ///
    /// # Errors
    ///
    /// 实例尚未挂载 (即 `fs_mount` 未调用) 时返回 `KernelError::NotInitialized`。
    pub fn ensure_mounted(&self) -> Result<(), KernelError> {
        if self.mounted {
            Ok(())
        } else {
            Err(KernelError::NotInitialized)
        }
    }

    /// 获取最大容量
    pub fn max_size(&self) -> u64 {
        self.max_size
    }

    /// 获取已用空间
    pub fn used_size(&self) -> u64 {
        self.used_size
    }

    /// 获取可用空间
    pub fn free_size(&self) -> u64 {
        self.max_size.saturating_sub(self.used_size)
    }

    /// 检查是否有足够空间
    pub fn has_space(&self, size: u64) -> bool {
        self.used_size + size <= self.max_size
    }

    /// 增加已用空间
    pub fn add_used(&mut self, size: u64) {
        self.used_size = self.used_size.saturating_add(size);
    }

    /// 减少已用空间
    pub fn sub_used(&mut self, size: u64) {
        self.used_size = self.used_size.saturating_sub(size);
    }
}

/// tmpfs 文件系统实例 (全局单例, BSS 零构造)
static TMPFS_DATA: Mutex<TmpFsData> = Mutex::new(TmpFsData::empty());

/// tmpfs FileSystem trait 实现
pub struct TmpFsFileSystem;

impl FileSystem for TmpFsFileSystem {
    fn name(&self) -> &'static str {
        "tmpfs"
    }

    fn fs_init(&self) -> KernelResult<()> {
        Ok(())
    }

    fn fs_mount(&self, path: &str) -> KernelResult<()> {
        let mut fs = TMPFS_DATA.lock();
        // 幂等: 重复挂载不重初始化存储, 以免擦除已有文件
        if fs.mounted {
            return Ok(());
        }
        // inner 层必须显式 mount 才建立根节点 (`RamFsData::new()` 仅清零)
        if fs.inner.mount(path) != 0 {
            return Err(KernelError::Io);
        }
        fs.used_size = 0;
        fs.mounted = true;
        Ok(())
    }

    fn fs_open(&self, rel_path: &str, flags: u32, pwm: u64) -> KernelResult<Arc<dyn Inode>> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;

        let (node_id, _offset, _file_type) = fs
            .inner
            .open(rel_path, flags, pwm)
            .ok_or(KernelError::FileNotFound)?;

        Ok(Arc::new(TmpFsInode::new(node_id, 0)))
    }

    fn fs_close(&self, _handle: u32) -> KernelResult<()> {
        Ok(())
    }

    fn fs_read(&self, handle: u32, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;

        let mut off = offset;
        let result = fs.inner.read(handle, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_write(&self, handle: u32, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;

        // 检查空间限制
        if !fs.has_space(buf.len() as u64) {
            return Err(KernelError::NoSpace);
        }

        let mut off = offset;
        let result = fs.inner.write(handle, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            fs.add_used(result as u64);
            Ok(result as usize)
        }
    }

    fn fs_stat(&self, rel_path: &str, _pwm: u64) -> KernelResult<VfsStat> {
        let fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;

        let node_id = fs
            .inner
            .resolve_path(rel_path)
            .ok_or(KernelError::FileNotFound)?;
        fs.inner.stat(node_id).ok_or(KernelError::FileNotFound)
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

    fn fs_mkdir(&self, rel_path: &str, pwm: u64) -> KernelResult<()> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;

        // 拆分父目录与名称 (与 ramfs::fs_mkdir 同构)
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
        let result = fs.inner.mkdir(parent_path, name, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_unlink(&self, rel_path: &str, pwm: u64) -> KernelResult<()> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;

        let result = fs.inner.unlink(rel_path, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::FileNotFound)
        }
    }

    fn fs_rmdir(&self, _rel_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_rename(&self, _old_path: &str, _new_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    fn fs_readdir(&self, handle: u32, offset: u64, entry: &mut VfsDirEntry) -> KernelResult<bool> {
        let mut fs = TMPFS_DATA.lock();
        fs.ensure_mounted()?;

        // 读取目录项 (offset 为字节偏移, 与 ramfs 约定同构)
        let mut dir_offset = offset;
        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let mut raw_buf = alloc::vec![0u8; dirent_size];
        let result = fs.inner.read(handle, &mut dir_offset, &mut raw_buf, 0);
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
    fn fs_resolve_inode(&self, inode_id: u32, mount_idx: u32) -> Option<Arc<dyn Inode>> {
        Some(Arc::new(TmpFsInode::new(inode_id, mount_idx)))
    }
}

/// tmpfs FileSystem 全局实例 (供 services 注册表经 `tmpfs_fs()` 暴露)
static TMPFS_FS: TmpFsFileSystem = TmpFsFileSystem;

/// 获取 tmpfs FileSystem trait object (services 注册表用)
pub fn tmpfs_fs() -> &'static dyn FileSystem {
    &TMPFS_FS
}

/// 初始化 tmpfs 文件系统
pub fn init() {
    // tmpfs 需要手动挂载
}
