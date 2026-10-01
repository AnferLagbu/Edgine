#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。
//! overlayfs 文件系统实现

use crate::framework::sync::IrqSpinLock as Mutex;
use crate::services::fs::KernelError;
use crate::services::fs::ramfs_core::RamFsDirEntry;
use crate::services::fs::vfs_manager::VFS_MANAGER;
use crate::services::fs::vfs_types::{
    FileSystem, KernelResult, VFS_MAX_NAME, VfsDirEntry, VfsFileType, VfsSeekWhence, VfsStat,
};
use alloc::string::String;

/// whiteout 文件标记 (文件名以 "." 开头表示已删除)
const WHITEOUT_PREFIX: u8 = b'.';

/// copy_up 分块复制块大小 — 用栈缓冲逐块搬运, 避免申请大块临时内存。
const COPY_UP_CHUNK: usize = 4096;

/// 写意图 open 标志掩码 (Linux 原生 flags: `O_WRONLY|O_RDWR|O_TRUNC|O_APPEND`)。
///
/// 命中即触发 copy_up; 仅以 `O_RDONLY` 打开时保持只读直通下层。
const WRITE_INTENT_MASK: u32 = 0x0001 | 0x0002 | 0x0200 | 0x0400;

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

/// 在父目录路径下拼接子项, 返回形如 `/b` 或 `/a/b` 的绝对路径。
///
/// `parent_path` 取自 `split_parent_name`, 取值恒为 `/` 或以非 `/` 结尾的
/// 绝对路径, 故无需处理尾部斜杠。
fn join_child(parent_path: &str, name: &str) -> String {
    if parent_path == "/" {
        let mut p = String::from("/");
        p.push_str(name);
        p
    } else {
        let mut p = String::from(parent_path);
        p.push('/');
        p.push_str(name);
        p
    }
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
    pub upper_data: crate::services::fs::ramfs_core::RamFsData,
    /// workdir 的 ramfs 数据
    pub work_data: crate::services::fs::ramfs_core::RamFsData,
    /// lowerdir 路径 (只读引用)
    pub lower_path: String,
    /// 是否已挂载 (替代 `Option` 的 `None` 语义, 作为 `NotInitialized` 判据)
    pub mounted: bool,
}

impl OverlayFsData {
    /// BSS 常量初始化 — 全部字段置空, 仅供 `static` 初始化位置调用。
    ///
    /// 关键约束: `OverlayFsData` 内含 `upper_data` + `work_data` 两个 `RamFsData`,
    /// 其大表已改为惰性堆置, 静态体量主要来自各自内联页池索引 (`data_area`, 各约
    /// 16 KiB)。故 `fs_mount` 一律就地写字段, 不得按值构造本结构。
    pub const fn empty() -> Self {
        Self {
            mount: OverlayMount {
                upperdir: String::new(),
                lowerdir: String::new(),
                workdir: String::new(),
                merged: String::new(),
            },
            upper_data: crate::services::fs::ramfs_core::RamFsData::new(),
            work_data: crate::services::fs::ramfs_core::RamFsData::new(),
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

    /// 判定 upperdir 父目录下是否存在 `.<name>` whiteout 文件。
    ///
    /// whiteout 以 ".<原名>" 命名, 命中的路径在 merged 视图中视为已删除。
    fn upper_has_whiteout(&self, parent_path: &str, name: &str) -> bool {
        let mut whiteout_name = String::from(char::from(WHITEOUT_PREFIX));
        whiteout_name.push_str(name);
        let whiteout_path = join_child(parent_path, &whiteout_name);
        match self.upper_data.resolve_path(&whiteout_path) {
            Some(node_id) => self.upper_data.nodes[node_id as usize].used,
            None => false,
        }
    }

    /// 将 merged 相对路径映射为 lowerdir 的绝对路径。
    ///
    /// `lower_path` 为 lowerdir 挂载点 (如 `/lower`); `rel_path` 取自 VFS 相对
    /// 路径 (如 `/a/b`)。返回形如 `/lower/a/b` 的绝对路径, 根路径返回挂载点本身。
    fn lower_full_path(&self, rel_path: &str) -> String {
        let rel = rel_path.trim_start_matches('/');
        if rel.is_empty() {
            return String::from(self.lower_path.as_str());
        }
        let mut full = String::from(self.lower_path.as_str());
        full.push('/');
        full.push_str(rel);
        full
    }

    /// 定位 lowerdir 中的目标文件, 返回 (下层 FileSystem, 该挂载下的相对路径)。
    ///
    /// lowerdir 必须是真实挂载 (ramfs/tmpfs) 方可解析; 未挂载或注册为
    /// 无 trait object 的旧式 FS 时返回 `None`。
    fn lower_target(&self, rel_path: &str) -> Option<(&'static dyn FileSystem, String)> {
        let lower_full = self.lower_full_path(rel_path);
        let (mount_idx, _fs_type, fs) = VFS_MANAGER.resolve_mount_fs(&lower_full)?;
        let fs = fs?;
        let rel = VFS_MANAGER.get_relative_path(&lower_full, mount_idx);
        Some((fs, String::from(rel)))
    }

    /// 解析路径，确定文件来自哪个层
    ///
    /// 优先级: upperdir 命中 > upperdir whiteout 标记 > lowerdir 命中 > 未命中。
    /// whiteout 命中表示该下层文件已被删除, 调用方应按 `FileNotFound` 处理。
    pub fn resolve_layer(&self, path: &str, pwm: u64) -> OverlayEntry {
        let (parent_path, name) = split_parent_name(path);

        // 1. upperdir 命中 — 反映上层真实文件 (未命中再判定 whiteout。
        //    被删除文件本身上层不存在, 仅留有 ".<name>" 兄弟节点)。
        if let Some(node_id) = self.upper_data.resolve_path(path) {
            let node = &self.upper_data.nodes[node_id as usize];
            if node.used {
                return OverlayEntry {
                    name: String::from(name),
                    file_type: node.file_type,
                    in_upper: true,
                    is_whiteout: name.starts_with(char::from(WHITEOUT_PREFIX)),
                    lower_inode: None,
                    upper_inode: Some(node_id),
                };
            }
        }

        // 2. whiteout 判定 — upperdir 父目录下存在 ".<name>" 即视为已删除。
        if self.upper_has_whiteout(parent_path, name) {
            return OverlayEntry {
                name: String::from(name),
                file_type: 0,
                in_upper: false,
                is_whiteout: true,
                lower_inode: None,
                upper_inode: None,
            };
        }

        // 3. lowerdir 命中 — 经 VFS_MANAGER 只读查询下层挂载。
        if let Some((lower_fs, lower_rel)) = self.lower_target(path) {
            if let Ok(st) = lower_fs.fs_stat(&lower_rel, pwm) {
                return OverlayEntry {
                    name: String::from(name),
                    file_type: st.file_type,
                    in_upper: false,
                    is_whiteout: false,
                    lower_inode: Some(st.node_id),
                    upper_inode: None,
                };
            }
        }

        // 4. 未命中 — 两层均无此路径。
        OverlayEntry {
            name: String::from(name),
            file_type: 0,
            in_upper: false,
            is_whiteout: false,
            lower_inode: None,
            upper_inode: None,
        }
    }

    /// copy_up: 将文件从 lowerdir 复制到 upperdir
    ///
    /// 幂等: upperdir 已存在同名文件时直接返回其节点号。仅支持普通文件
    /// (目录递归复制未实装)。内容以 `COPY_UP_CHUNK` 为块用栈缓冲搬运, 避免
    /// 申请大块临时内存; 复制完成后同步下层权限位与所有者。
    ///
    /// # Errors
    ///
    /// - 两层均无此路径时返回 `KernelError::FileNotFound`;
    /// - 下层打开/读取失败时透传对应 `KernelError`;
    /// - 目标非普通文件, 或 upperdir 缺失其父目录链时返回 `KernelError::NotSupported`;
    /// - upperdir 空间不足时返回 `KernelError::NoSpace`。
    pub fn copy_up(&mut self, path: &str, pwm: u64) -> Result<u32, KernelError> {
        // 1. 幂等守卫: upperdir 已存在则直接返回。
        if let Some(node_id) = self.upper_data.resolve_path(path) {
            if self.upper_data.nodes[node_id as usize].used {
                return Ok(node_id);
            }
        }

        // 2. 定位并打开只读下层文件。
        let (lower_fs, lower_rel) = self.lower_target(path).ok_or(KernelError::FileNotFound)?;
        let lower_inode = lower_fs.fs_open(&lower_rel, 0, pwm)?;

        // 3. 仅普通文件可拷贝。
        // SIMPLIFIED: 目录/符号链接/设备的 copy_up 未实装 (目录需递归复制子树);
        //            影响面 = 仅普通文件支持写打开; 待后续按需扩展目录复制.
        let stat = lower_inode.stat(pwm)?;
        if stat.file_type != VfsFileType::File.as_u8() {
            return Err(KernelError::NotSupported);
        }

        // 4. 在 upperdir 创建同名文件。父目录链必须已存在于 upperdir。
        let (parent_path, name) = split_parent_name(path);
        if self.upper_data.resolve_path(parent_path).is_none() {
            // SIMPLIFIED: 目录 copy_up 未实装 — upperdir 缺失父目录链时无法落盘;
            //            影响面 = 嵌套于纯下层目录的文件暂不支持写打开.
            return Err(KernelError::NotSupported);
        }
        let new_node = self
            .upper_data
            .create_file(parent_path, name, pwm)
            .ok_or(KernelError::NoSpace)?;

        // 5. 分块复制内容 (4 KiB 栈缓冲)。
        let size = u64::from(stat.size);
        let mut offset = 0u64;
        let mut buf = [0u8; COPY_UP_CHUNK];
        while offset < size {
            let want = core::cmp::min(COPY_UP_CHUNK as u64, size - offset) as usize;
            let read = lower_inode.read(offset, &mut buf[..want], pwm)?;
            if read == 0 {
                break;
            }
            let mut write_off = offset;
            let written = self
                .upper_data
                .write(new_node, &mut write_off, &buf[..read], pwm);
            if written < 0 {
                return Err(KernelError::PermissionDenied);
            }
            if written as usize != read {
                return Err(KernelError::NoSpace);
            }
            offset += read as u64;
        }

        // 6. 复制文件属性 (权限位 / 所有者 / 组), 与下层保持一致。
        {
            let node = &mut self.upper_data.nodes[new_node as usize];
            node.perm = stat.perm;
            node.owner_pwm = stat.owner_pwm;
            node.group_pwm = stat.group_pwm;
        }

        Ok(new_node)
    }

    /// 创建 whiteout 文件 (标记删除)
    ///
    /// # Errors
    ///
    /// 当 upperdir 无法分配新节点 (空间不足) 时返回 `KernelError::NoSpace`。
    pub fn create_whiteout(&mut self, path: &str) -> Result<u32, KernelError> {
        // whiteout 是上层父目录下名为 ".<原名>" 的空文件。
        // SIMPLIFIED: upperdir 父目录若仅存在于 lowerdir, 则无法创建 whiteout
        //            (需目录 copy-up); 影响面 = 该类路径的删除会退化为 NoSpace;
        //            待目录 copy-up 实装后补齐.
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
/// 用 `const fn empty()` 做 BSS 常量初始化 (同 `services::fs::ramfs_core::RAMFS_DATA`
/// 范式), 避免在内核栈上构造约 170 KiB 的 `OverlayFsData`。
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

    fn set_times(&self, atime: u64, mtime: u64, pwm: u64) -> KernelResult<()> {
        // OverlayFS: 时间戳写回 upperdir 对应节点 (属主/特权判据由
        // `RamFsData::set_times` 在锁域内完成, 与 chmod/chown 同源)。
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;
        let rc = fs.upper_data.set_times(self.node_id, atime, mtime, pwm);
        if rc == 0 {
            Ok(())
        } else {
            Err(KernelError::from_i32(-rc))
        }
    }

    fn node_id(&self) -> u32 {
        self.node_id
    }

    fn mount_idx(&self) -> u32 {
        self.mount_idx
    }
}

// ============================================================================
// OverlayLowerInode — 只读下层 Inode 代理
// ============================================================================

/// 只读下层 Inode 代理 — 包装 lowerdir 挂载的真实 inode。
///
/// 当 merged 视图中的文件仅存在于 lowerdir 且以只读方式打开时, 由本类型
/// 直通下层 inode 完成读/属性/寻址, 不触碰 upperdir; 任何写操作显式返回
/// `ReadOnlyFilesystem`。写打开应改走 copy_up 提升为 `OverlayFsInode`。
struct OverlayLowerInode {
    /// 下层挂载的真实 inode (经 `FileSystem::fs_open` 取得)
    inner: alloc::sync::Arc<dyn Inode>,
}

impl Inode for OverlayLowerInode {
    fn read(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        self.inner.read(offset, buf, pwm)
    }

    fn write(&self, _offset: u64, _buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn stat(&self, pwm: u64) -> KernelResult<VfsStat> {
        self.inner.stat(pwm)
    }

    fn truncate(&self, _size: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn set_times(&self, _atime: u64, _mtime: u64, _pwm: u64) -> KernelResult<()> {
        // 只读下层代理: 时间戳写回下层需 metadata copy-up (未实装), 显式拒绝.
        Err(KernelError::ReadOnlyFilesystem)
    }

    fn seek(&self, offset: i64, whence: VfsSeekWhence, current_offset: u64) -> KernelResult<u64> {
        self.inner.seek(offset, whence, current_offset)
    }

    fn is_dir(&self) -> bool {
        self.inner.is_dir()
    }

    fn readdir(&self, offset: u64) -> KernelResult<(String, VfsFileType, bool)> {
        self.inner.readdir(offset)
    }

    fn node_id(&self) -> u32 {
        self.inner.node_id()
    }

    fn mount_idx(&self) -> u32 {
        self.inner.mount_idx()
    }

    fn pread_inode(&self, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize> {
        self.inner.pread_inode(offset, buf, pwm)
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
        // 幂等守卫: 重复挂载直接返回, 避免重复初始化 upper/work 层。
        if fs.mounted {
            return Ok(());
        }
        // 就地写小 String 字段, 避免按值构造约 170 KiB 的 `OverlayFsData` 栈临时。
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
        pwm: u64,
    ) -> KernelResult<alloc::sync::Arc<dyn crate::services::fs::inode::Inode>> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        let entry = fs.resolve_layer(rel_path, pwm);

        if entry.is_whiteout {
            return Err(KernelError::FileNotFound);
        }

        // upper 命中: 直接返回上层 inode (读写皆走 upper).
        if entry.in_upper {
            let node_id = entry.upper_inode.unwrap_or(0);
            return Ok(alloc::sync::Arc::new(OverlayFsInode::new(
                node_id,
                0,
                entry.file_type,
            )));
        }

        // 两层皆未命中.
        if entry.lower_inode.is_none() {
            return Err(KernelError::FileNotFound);
        }

        // 写意图: 触发 copy_up, 之后读写均作用于 upper 新节点.
        // SIMPLIFIED: 写意图掩码沿用 Linux 原生 flags 编码 (RDONLY=0, WRONLY=1, RDWR=2,
        //            CREAT=0x40, TRUNC=0x200, APPEND=0x400); 影响面 = 仅按 syscall 路径
        //            传入的 flags 判定; 待统一 flags 编码后收敛.
        let write_intent = flags & WRITE_INTENT_MASK != 0;
        if write_intent {
            if entry.file_type == VfsFileType::Dir.as_u8() {
                // 目录不支持写打开 (无 IsADirectory 变体, 以 InvalidArgument 表达).
                return Err(KernelError::InvalidArgument);
            }
            let node_id = fs.copy_up(rel_path, pwm)?;
            return Ok(alloc::sync::Arc::new(OverlayFsInode::new(
                node_id,
                0,
                entry.file_type,
            )));
        }

        // 只读: 直通下层, 包装为只读 inode (不产生 upper 副本).
        let (lower_fs, lower_rel) = fs.lower_target(rel_path).ok_or(KernelError::FileNotFound)?;
        let inner = lower_fs.fs_open(&lower_rel, flags, pwm)?;
        Ok(alloc::sync::Arc::new(OverlayLowerInode { inner }))
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

    fn fs_stat(&self, rel_path: &str, pwm: u64) -> KernelResult<VfsStat> {
        let fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 解析路径，获取文件属性
        let entry = fs.resolve_layer(rel_path, pwm);

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
        } else if entry.lower_inode.is_some() {
            // 只命中 lowerdir: 经 VFS_MANAGER 委托下层 fs_stat.
            let (lower_fs, lower_rel) =
                fs.lower_target(rel_path).ok_or(KernelError::FileNotFound)?;
            lower_fs.fs_stat(&lower_rel, pwm)
        } else {
            Err(KernelError::FileNotFound)
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

    fn fs_utimensat(&self, rel_path: &str, atime: u64, mtime: u64, pwm: u64) -> KernelResult<()> {
        let mut fs = OVERLAY_FS.lock();
        fs.ensure_mounted()?;

        // 层级路由: upper 命中 → 写 upper 节点; whiteout / 两层皆无 → 不存在;
        // 仅存于 lower → 只读下层不可写时间戳 (metadata copy-up 未实装, 显式拒绝)。
        let entry = fs.resolve_layer(rel_path, pwm);
        match entry.upper_inode {
            Some(node_id) => {
                let rc = fs.upper_data.set_times(node_id, atime, mtime, pwm);
                if rc == 0 {
                    Ok(())
                } else {
                    Err(KernelError::from_i32(-rc))
                }
            }
            None if entry.is_whiteout => Err(KernelError::FileNotFound),
            None if entry.lower_inode.is_some() => Err(KernelError::ReadOnlyFilesystem),
            None => Err(KernelError::FileNotFound),
        }
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

        let entry = fs.resolve_layer(rel_path, pwm);
        // whiteout 命中 = 该路径已在 merged 视图被删除; 两层皆无 = 本就不存在.
        if entry.is_whiteout || (!entry.in_upper && entry.lower_inode.is_none()) {
            return Err(KernelError::FileNotFound);
        }

        if entry.in_upper {
            // 文件在 upperdir，直接删除.
            let result = fs.upper_data.unlink(rel_path, pwm);
            if result != 0 {
                return Err(KernelError::FileNotFound);
            }
            // upper 删除后若下层仍有同名副本, 必须补 whiteout 遮蔽,
            // 否则 merged 视图会重新暴露被删的下层文件.
            let lower_has_same = fs
                .lower_target(rel_path)
                .is_some_and(|(lower_fs, lower_rel)| lower_fs.fs_stat(&lower_rel, pwm).is_ok());
            if lower_has_same {
                fs.create_whiteout(rel_path)?;
            }
            Ok(())
        } else {
            // 文件仅在 lowerdir，创建 whiteout 遮蔽.
            fs.create_whiteout(rel_path)?;
            Ok(())
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

/// overlayfs FileSystem 全局实例 (供 services 注册表经 `overlay_fs()` 暴露)
static OVERLAY_FS_INSTANCE: OverlayFsFileSystem = OverlayFsFileSystem;

/// 获取 overlayfs FileSystem trait object (services 注册表用)
pub fn overlay_fs() -> &'static dyn FileSystem {
    &OVERLAY_FS_INSTANCE
}
