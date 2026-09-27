#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! overlayfs 文件系统实现

use crate::framework::fs::KernelError;
use crate::framework::fs::ramfs::RamFsDirEntry;
use crate::framework::sync::IrqSpinLock as Mutex;
use crate::services::fs::vfs_types::{
    FileSystem, KernelResult, VFS_MAX_NAME, VfsDirEntry, VfsSeekWhence, VfsStat,
};
use alloc::string::String;

/// whiteout 文件标记 (文件名以 "." 开头表示已删除)
const WHITEOUT_PREFIX: u8 = b'.';

/// 拆分路径为 (父目录, 名称) — 与 `ramfs::fs_mkdir` 同构。
///
/// 无 `/` 时父目录视为 `/`; 唯一 `/` 在首位时父目录为 `/`。
fn split_parent_name(path: &str) -> (&str, &str) {
    path.rfind('/').map_or(("/", path), |pos| {
        if pos == 0 {
            ("/", &path[1..])
        } else {
            (&path[..pos], &path[pos + 1..])
        }
    })
}

/// overlayfs 目录项
#[derive(Debug, Clone)]
pub struct OverlayEntry {
    /// 文件名
    pub name: String,
    /// 文件类型 (0=文件, 1=目录)
    pub file_type: u8,
    /// 是否在 upperdir 中
    pub in_upper: bool,
    /// 是否为 whiteout (已删除)
    pub is_whiteout: bool,
    /// 原始 inode 号 (来自 lowerdir)
    pub lower_inode: Option<u32>,
    /// upperdir inode 号 (如果存在)
    pub upper_inode: Option<u32>,
}

/// overlayfs 挂载配置
#[derive(Debug, Clone)]
pub struct OverlayMount {
    /// 上层目录路径 (写入目标)
    pub upperdir: String,
    /// 下层目录路径 (只读源)
    pub lowerdir: String,
    /// 工作层路径 (copy_up 临时存储)
    pub workdir: String,
    /// 合并后的视图路径
    pub merged: String,
}

/// overlayfs 数据结构
pub struct OverlayFsData {
    /// 挂载配置
    pub mount: OverlayMount,
    /// upperdir 的 ramfs 数据
    pub upper_data: crate::framework::fs::ramfs::RamFsData,
    /// workdir 的 ramfs 数据
    pub work_data: crate::framework::fs::ramfs::RamFsData,
    /// lowerdir 路径 (只读引用)
    pub lower_path: String,
    /// 是否已挂载 (替代 `Option` 的 `None` 语义, 作为 `NotInitialized` 判据)
    pub mounted: bool,
}

impl OverlayFsData {
    /// BSS 常量初始化 — 全部字段置空, 仅供 `static` 初始化位置调用。
    ///
    /// 关键约束: `OverlayFsData` 体量约 16 MiB (upper_data + work_data 各约
    /// 8 MiB)。若在普通函数中按值构造会占用内核栈, 故 `fs_mount` 一律就地写
    /// 字段, 不得按值构造本结构。
    pub const fn empty() -> Self {
        Self {
            mount: OverlayMount {
                upperdir: String::new(),
                lowerdir: String::new(),
                workdir: String::new(),
                merged: String::new(),
            },
            upper_data: crate::framework::fs::ramfs::RamFsData::new(),
            work_data: crate::framework::fs::ramfs::RamFsData::new(),
            lower_path: String::new(),
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

    /// 解析路径，确定文件来自哪个层
    pub fn resolve_layer(&self, path: &str) -> OverlayEntry {
        let (_, name) = split_parent_name(path);
        // 1. 检查 upperdir
        if let Some(node_id) = self.upper_data.resolve_path(path) {
            let node = &self.upper_data.nodes[node_id as usize];
            if node.used {
                // whiteout 判定: 只看文件名是否以 "." 开头, 而非整段路径。
                let is_whiteout = name.starts_with(char::from(WHITEOUT_PREFIX));
                return OverlayEntry {
                    name: String::from(name),
                    file_type: node.file_type,
                    in_upper: true,
                    is_whiteout,
                    lower_inode: None,
                    upper_inode: Some(node_id),
                };
            }
        }

        // 2. 检查 lowerdir (通过 VFS 接口)
        // 注意: lowerdir 是只读的，需要通过 VFS 读取
        // SIMPLIFIED: lowerdir 查询未接入 (需经 VFS_MANAGER 读下层挂载); 影响面 =
        // merged 视图暂不反映下层文件; 待 copy_up 实装 (O-2) 时按挂载表补齐.
        OverlayEntry {
            name: String::from(name),
            file_type: 0, // 默认文件
            in_upper: false,
            is_whiteout: false,
            lower_inode: None,
            upper_inode: None,
        }
    }

    /// copy_up: 将文件从 lowerdir 复制到 upperdir
    ///
    /// # Errors
    ///
    /// 当前为 O-2 占位实现, 恒返回 `KernelError::NotSupported`;
    /// 待 O-2 实装后, lowerdir 缺失或下层读取失败时返回对应 `KernelError`。
    pub fn copy_up(&mut self, path: &str) -> Result<u32, KernelError> {
        // 1. 检查 upperdir 是否已存在
        if let Some(node_id) = self.upper_data.resolve_path(path) {
            let node = &self.upper_data.nodes[node_id as usize];
            if node.used {
                return Ok(node_id);
            }
        }

        // 2. 从 lowerdir 读取文件内容
        // 注意: 需要通过 VFS 读取 lowerdir 的文件
        // 这里简化处理，实际需要调用 lowerdir 的 FileSystem trait

        // 3. 在 upperdir 创建新文件
        // 4. 复制文件内容
        // 5. 复制文件属性

        Err(KernelError::NotSupported)
    }

    /// 创建 whiteout 文件 (标记删除)
    ///
    /// # Errors
    ///
    /// 当 upperdir 无法分配新节点 (空间不足) 时返回 `KernelError::NoSpace`。
    pub fn create_whiteout(&mut self, path: &str) -> Result<u32, KernelError> {
        // whiteout 是上层父目录下名为 ".<原名>" 的空文件。
        let (parent_path, name) = split_parent_name(path);
        let mut whiteout_name = String::from(char::from(WHITEOUT_PREFIX));
        whiteout_name.push_str(name);
        self.upper_data
            .create_file(parent_path, &whiteout_name, 0)
            .ok_or(KernelError::NoSpace)
    }
}

/// overlayfs 文件系统实例 (全局单例)
///
/// 用 `const fn empty()` 做 BSS 常量初始化 (同 `framework::fs::ramfs::RAMFS_DATA`
/// 范式), 避免在内核栈上构造约 16 MiB 的 `OverlayFsData`。
static OVERLAY_FS: Mutex<OverlayFsData> = Mutex::new(OverlayFsData::empty());

// ============================================================================
// OverlayFsInode — OverlayFS 文件 Inode 实现
// ============================================================================

use crate::services::fs::inode::Inode;

/// OverlayFS 文件 Inode — 委托给 upperdir 的 RamFsData
pub struct OverlayFsInode {
    node_id: u32,
    mount_idx: u32,
    file_type: u8,
}

impl OverlayFsInode {
    pub fn new(node_id: u32, mount_idx: u32, file_type: u8) -> Self {
        Self {
            node_id,
            mount_idx,
            file_type,
        }
    }
}

impl Inode for OverlayFsInode {
    fn read(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;
        let mut off = offset;
        let result = fs.upper_data.read(self.node_id, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn write(&self, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;
        let mut off = offset;
        let result = fs.upper_data.write(self.node_id, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn stat(&self, pwm: u64) -> KernelResult<VfsStat> {
        let fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;
        match fs.upper_data.get_stat(self.node_id, pwm) {
            Ok(s) => Ok(s),
            Err(_) => Err(KernelError::FileNotFound),
        }
    }

    fn truncate(&self, size: u64, _pwm: u64) -> KernelResult<()> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;
        let rc = fs.upper_data.truncate(self.node_id, size, 0);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        let file_size = {
            let fs = OVERLAY_FS.lock();
            fs.ensure_mounted()?;
            u64::from(fs.upper_data.get_file_size(self.node_id).unwrap_or(0))
        };
        let new_offset = match whence {
            VfsSeekWhence::Set => offset as u64,
            VfsSeekWhence::Cur => current_offset.saturating_add(offset as u64),
            VfsSeekWhence::End => file_size.saturating_add(offset as u64),
        };
        Ok(new_offset)
    }

    fn is_dir(&self) -> bool {
        self.file_type == 1
    }

    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        // OverlayFS: 委托给下层文件系统
        // 未来可实现 copy-up + 时间戳更新 (登记分册 9 B09-10)
        Ok(())
    }

    fn node_id(&self) -> u32 {
        self.node_id
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

/// overlayfs FileSystem trait 实现
pub struct OverlayFsFileSystem;

impl FileSystem for OverlayFsFileSystem {
    fn name(&self) -> &'static str {
        "overlay"
    }

    fn fs_init(&self) -> KernelResult<()> {
        Ok(())
    }

    fn fs_mount(&self, path: &str) -> KernelResult<()> {
        // 解析挂载选项 (upperdir, lowerdir, workdir)
        // 这里简化处理，实际需要解析 mount 命令的选项
        let mut fs = OVERLAY_FS.lock();
        // 就地写小 String 字段, 避免按值构造约 16 MiB 的 `OverlayFsData` 栈临时。
        fs.mount.upperdir = String::from("/upper");
        fs.mount.lowerdir = String::from("/lower");
        fs.mount.workdir = String::from("/work");
        fs.mount.merged = String::from(path);
        fs.lower_path = String::from("/lower");
        // upper/work 层必须显式 mount 才建立根节点 (`RamFsData::new()` 仅清零)。
        if fs.upper_data.mount("/") != 0 || fs.work_data.mount("/") != 0 {
            return Err(KernelError::Io);
        }
        fs.mounted = true;
        Ok(())
    }

    fn fs_open(
        &self,
        rel_path: &str,
        flags: u32,
        _pwm: u64,
    ) -> KernelResult<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        let entry = fs.resolve_layer(rel_path);

        if entry.is_whiteout {
            return Err(KernelError::FileNotFound);
        }

        if !entry.in_upper && (flags & 0x0003 != 0) {
            fs.copy_up(rel_path)?;
        }

        if entry.in_upper {
            let node_id = entry.upper_inode.unwrap_or(0);
            Ok(alloc::sync::Arc::new(OverlayFsInode::new(
                node_id,
                0,
                entry.file_type,
            )))
        } else {
            Err(KernelError::NotSupported)
        }
    }

    fn fs_close(&self, _handle: u32) -> KernelResult<()> {
        Ok(())
    }

    fn fs_read(&self, handle: u32, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 从 upperdir 读取
        let mut off = offset;
        let result = fs.upper_data.read(handle, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_write(&self, handle: u32, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 写入 upperdir
        let mut off = offset;
        let result = fs.upper_data.write(handle, &mut off, buf, pwm);
        if result < 0 {
            Err(KernelError::Io)
        } else {
            Ok(result as usize)
        }
    }

    fn fs_stat(&self, rel_path: &str, _pwm: u64) -> KernelResult<VfsStat> {
        let fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 解析路径，获取文件属性
        let entry = fs.resolve_layer(rel_path);

        if entry.is_whiteout {
            return Err(KernelError::FileNotFound);
        }

        // 从 upperdir 或 lowerdir 获取属性
        if entry.in_upper {
            let node_id = entry.upper_inode.unwrap_or(0);
            let node = &fs.upper_data.nodes[node_id as usize];
            if !node.used {
                return Err(KernelError::FileNotFound);
            }

            Ok(VfsStat {
                node_id,
                mode: node.perm,
                uid: 0,
                gid: 0,
                size: node.size,
                atime: node.atime,
                mtime: node.mtime,
                ctime: node.ctime,
                owner_pwm: node.owner_pwm,
                group_pwm: node.group_pwm,
                perm: node.perm,
                file_type: node.file_type,
                sensitivity: 0,
            })
        } else {
            // 从 lowerdir 获取属性 (需要通过 VFS)
            Err(KernelError::NotSupported)
        }
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
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 在 upperdir 创建目录
        let (parent_path, name) = split_parent_name(rel_path);
        if name.is_empty() {
            return Err(KernelError::InvalidArgument);
        }
        let result = fs.upper_data.mkdir(parent_path, name, pwm);
        if result == 0 {
            Ok(())
        } else {
            Err(KernelError::Io)
        }
    }

    fn fs_unlink(&self, rel_path: &str, pwm: u64) -> KernelResult<()> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 检查文件是否在 lowerdir
        let entry = fs.resolve_layer(rel_path);
        if !entry.in_upper {
            // 文件在 lowerdir，需要创建 whiteout
            fs.create_whiteout(rel_path)?;
            return Ok(());
        }

        // 文件在 upperdir，直接删除
        let result = fs.upper_data.unlink(rel_path, pwm);
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
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 读取 upperdir 目录项 (offset 为字节偏移, 与 ramfs 约定一致)
        let mut dir_offset = offset;
        let dirent_size = core::mem::size_of::<RamFsDirEntry>();
        let mut raw_buf = alloc::vec![0u8; dirent_size];
        let result = fs.upper_data.read(handle, &mut dir_offset, &mut raw_buf, 0);
        let raw_entry = RamFsDirEntry::read_at(&raw_buf, 0);
        if result <= 0 || raw_entry.node == 0 {
            // SIMPLIFIED: lowerdir 目录项合流未接入 (需经 VFS_MANAGER 读下层挂载);
            // 影响面 = merged 视图暂只反映 upperdir; 待 O-2 copy_up/lowerdir 查询实装时补齐.
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
        Ok(true)
    }

    // L4 重构: 扩展方法实现 (override trait 默认实现)
    fn fs_resolve_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
    ) -> Option<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        Some(alloc::sync::Arc::new(OverlayFsInode::new(
            inode_id, mount_idx, 0,
        )))
    }
}

/// 初始化 overlayfs 文件系统
pub fn init() {
    // overlayfs 需要手动挂载
}
