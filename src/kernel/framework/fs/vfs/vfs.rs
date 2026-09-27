//! VFS 管理器 (挂载表 + 路径解析) — framework 层完整实现
//!
//! ## B09-12/DECISION-H13 P1-B4 迁移记录 (2026-08-31)
//!
//! VfsManager 是 VFS 核心机制 (挂载点表 + 路径解析), 按"机制归
//! framework"原则从 `services::fs::vfs_manager` 迁回本文件. 0 语义变更.
//! `services::fs::vfs_manager` 改为 re-export 本文件保持调用方兼容.
//!
//! ## 架构
//!
//! VfsManager 管理挂载点表、当前工作目录与根前缀,
//! 提供 mount/unmount/resolve 等纯机制操作。
//! 不含 unsafe, 不直接操作硬件。
//!
//! per-process fd 表 (B-9.5) 已下沉至 `Process.fd_table` (`framework::proc`),
//! 本文件不再持有 fd 表; fd→`OpenFile` 映射经 `framework::fs::vfs_get_fd_handle`
//! 读取当前进程 fd 表. 原全局 fd 表尺寸常量 `VFS_MAX_FDS` 已随其退役删除,
//! fd 上限唯一由 `framework::proc::fd_table::MAX_FDS_PER_PROCESS` 承载.

use crate::framework::fs::vfs::types::{
    FileSystem, FsType, KernelError, VFS_MAX_MOUNTS, VFS_MAX_PATH,
};
use crate::framework::sync::IrqSpinLock as Mutex;
use alloc::string::String;

pub struct VfsMount {
    pub path: [u8; VFS_MAX_PATH],
    fs_type: FsType,
    /// E6-4: trait object 分发 (优先于 `fs_type` match)
    fs: Option<&'static dyn FileSystem>,
    pub used: bool,
}

impl Clone for VfsMount {
    fn clone(&self) -> Self {
        Self {
            path: self.path,
            fs_type: self.fs_type,
            fs: self.fs,
            used: self.used,
        }
    }
}

impl VfsMount {
    pub const fn new() -> Self {
        Self {
            path: [0; VFS_MAX_PATH],
            fs_type: FsType::Unknown,
            fs: None,
            used: false,
        }
    }

    pub fn set_path(&mut self, path: &str) {
        let bytes = path.as_bytes();
        let len = bytes.len().min(VFS_MAX_PATH - 1);
        self.path[..len].copy_from_slice(&bytes[..len]);
        self.path[len] = 0;
    }

    pub fn get_path(&self) -> &str {
        let end = self
            .path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(VFS_MAX_PATH);
        core::str::from_utf8(&self.path[..end]).unwrap_or("")
    }

    pub fn set_fs_type(&mut self, name: &str) {
        self.fs_type = FsType::from_name(name);
    }

    pub fn get_fs_type(&self) -> FsType {
        self.fs_type
    }

    pub fn get_fs_name(&self) -> &str {
        self.fs_type.as_str()
    }

    /// E6-4: 获取 trait object (优先于 `fs_type` match)
    pub fn get_fs(&self) -> Option<&'static dyn FileSystem> {
        self.fs
    }

    /// E6-4: 设置 trait object (注册文件系统时调用)
    pub fn set_fs(&mut self, fs: &'static dyn FileSystem) {
        self.fs = Some(fs);
    }
}

pub struct ResolvedMount {
    pub mount_idx: usize,
    pub rel_path: &'static str,
    pub fs_type: FsType,
    /// E6-4: trait object (优先于 `fs_type`)
    pub fs: Option<&'static dyn FileSystem>,
}

pub struct VfsManager {
    pub mounts: Mutex<[VfsMount; VFS_MAX_MOUNTS]>,
    cwd: Mutex<[u8; VFS_MAX_PATH]>,
    /// 根前缀 (`chroot` / `pivot_root` 机制) — 用户视图 "/" 对应的真实路径
    root: Mutex<[u8; VFS_MAX_PATH]>,
    initialized: Mutex<bool>,
    snapshot: Mutex<Option<VfsSnapshot>>,
}

/// 默认根前缀 (含义: 用户视图与挂载表真实路径一致)
const DEFAULT_ROOT: [u8; VFS_MAX_PATH] = {
    let mut root = [0u8; VFS_MAX_PATH];
    root[0] = b'/';
    root
};

#[derive(Clone)]
struct VfsSnapshot {
    mounts: [VfsMount; VFS_MAX_MOUNTS],
    cwd: [u8; VFS_MAX_PATH],
    root: [u8; VFS_MAX_PATH],
}

/// 返回无尾斜杠路径 `s` 的父路径长度; 根之下统一收敛到 1 ("/")
fn truncate_to_parent(s: &[u8]) -> usize {
    match s.iter().rposition(|&b| b == b'/') {
        Some(0) | None => 1,
        Some(idx) => idx,
    }
}

impl VfsManager {
    pub const fn new() -> Self {
        Self {
            mounts: Mutex::new([
                VfsMount::new(),
                VfsMount::new(),
                VfsMount::new(),
                VfsMount::new(),
                VfsMount::new(),
                VfsMount::new(),
                VfsMount::new(),
                VfsMount::new(),
            ]),
            cwd: Mutex::new([0; VFS_MAX_PATH]),
            root: Mutex::new(DEFAULT_ROOT),
            initialized: Mutex::new(false),
            snapshot: Mutex::new(None),
        }
    }

    pub fn init(&self) {
        let mut mounts = self.mounts.lock();
        for mount in mounts.iter_mut() {
            mount.used = false;
            mount.set_path("");
            mount.fs_type = FsType::Unknown;
        }

        let mut cwd = self.cwd.lock();
        cwd[0] = b'/';
        cwd[1] = 0;

        // 根前缀重置为 "/" (chroot/pivot_root 状态不跨初始化存活)
        let mut root = self.root.lock();
        root[0] = b'/';
        root[1] = 0;

        *self.initialized.lock() = true;
    }

    pub fn find_mount(&self, path: &str) -> Option<usize> {
        let mounts = self.mounts.lock();
        let mut best_idx: Option<usize> = None;
        let mut best_len = 0usize;

        for (i, mount) in mounts.iter().enumerate() {
            if !mount.used {
                continue;
            }

            let mount_path = mount.get_path();

            if path == mount_path {
                if mount_path.len() > best_len {
                    best_len = mount_path.len();
                    best_idx = Some(i);
                }
            } else if path.starts_with(mount_path) {
                let next_char = path.as_bytes().get(mount_path.len());
                if mount_path == "/" || next_char == Some(&b'/') {
                    if mount_path.len() > best_len {
                        best_len = mount_path.len();
                        best_idx = Some(i);
                    }
                }
            }
        }

        best_idx
    }

    pub fn get_relative_path<'a>(&self, path: &'a str, mount_idx: usize) -> &'a str {
        let mounts = self.mounts.lock();
        if mount_idx >= VFS_MAX_MOUNTS {
            return path;
        }

        let mount_path = mounts[mount_idx].get_path();
        let rel_path = &path[mount_path.len()..];

        let rel_path = rel_path.trim_start_matches('/');

        if rel_path.is_empty() { "/" } else { rel_path }
    }

    pub fn resolve_mount(&self, path: &str) -> Option<(usize, FsType)> {
        let mount_idx = self.find_mount(path)?;
        let fs_type = {
            let mounts = self.mounts.lock();
            if mount_idx < VFS_MAX_MOUNTS && mounts[mount_idx].used {
                mounts[mount_idx].get_fs_type()
            } else {
                FsType::Unknown
            }
        };
        if fs_type == FsType::Unknown {
            return None;
        }
        Some((mount_idx, fs_type))
    }

    /// E6-4: 解析挂载点, 同时返回 trait object (优先于 `fs_type`)
    pub fn resolve_mount_fs(
        &self,
        path: &str,
    ) -> Option<(usize, FsType, Option<&'static dyn FileSystem>)> {
        let mount_idx = self.find_mount(path)?;
        let (fs_type, fs) = {
            let mounts = self.mounts.lock();
            if mount_idx < VFS_MAX_MOUNTS && mounts[mount_idx].used {
                (mounts[mount_idx].get_fs_type(), mounts[mount_idx].get_fs())
            } else {
                (FsType::Unknown, None)
            }
        };
        if fs_type == FsType::Unknown && fs.is_none() {
            return None;
        }
        Some((mount_idx, fs_type, fs))
    }

    /// 挂载文件系统到指定路径.
    ///
    /// # Errors
    /// 当该路径已挂载时返回 `AlreadyExists`; 当挂载表已满时返回 `NoSpace`.
    pub fn mount(&self, path: &str, fs_name: &str) -> Result<(), KernelError> {
        let mut mounts = self.mounts.lock();

        for mount in mounts.iter() {
            if mount.used && mount.get_path() == path {
                return Err(KernelError::AlreadyExists);
            }
        }

        for mount in mounts.iter_mut() {
            if !mount.used {
                mount.set_path(path);
                mount.set_fs_type(fs_name);
                mount.used = true;

                return Ok(());
            }
        }

        Err(KernelError::NoSpace)
    }

    /// E6-4: 带 trait object 的挂载 (优先于 `fs_name` match)
    ///
    /// # Errors
    /// 当该路径已挂载时返回 `AlreadyExists`; 当挂载表已满时返回 `NoSpace`.
    pub fn mount_with_fs(
        &self,
        path: &str,
        fs_name: &str,
        fs: &'static dyn FileSystem,
    ) -> Result<(), KernelError> {
        let mut mounts = self.mounts.lock();

        for mount in mounts.iter() {
            if mount.used && mount.get_path() == path {
                return Err(KernelError::AlreadyExists);
            }
        }

        for mount in mounts.iter_mut() {
            if !mount.used {
                mount.set_path(path);
                mount.set_fs_type(fs_name);
                mount.set_fs(fs);
                mount.used = true;

                return Ok(());
            }
        }

        Err(KernelError::NoSpace)
    }

    /// 卸载指定路径的挂载.
    ///
    /// # Errors
    /// 当该路径未挂载时返回 `FileNotFound`.
    pub fn unmount(&self, path: &str) -> Result<(), KernelError> {
        let mut mounts = self.mounts.lock();

        for mount in mounts.iter_mut() {
            if mount.used && mount.get_path() == path {
                // 卸载前清空 dcache/icache
                crate::framework::fs::vfs::dcache::flush_all();
                mount.used = false;
                return Ok(());
            }
        }

        Err(KernelError::FileNotFound)
    }

    pub fn set_cwd(&self, path: &str) {
        let mut cwd = self.cwd.lock();
        let bytes = path.as_bytes();
        let len = bytes.len().min(VFS_MAX_PATH - 1);
        cwd[..len].copy_from_slice(&bytes[..len]);
        cwd[len] = 0;
    }

    pub fn get_cwd(&self) -> String {
        let cwd = self.cwd.lock();
        let end = cwd.iter().position(|&b| b == 0).unwrap_or(VFS_MAX_PATH);
        String::from(core::str::from_utf8(&cwd[..end]).unwrap_or("/"))
    }

    /// 读取当前根前缀 (真实路径; 默认 "/")
    pub fn get_root(&self) -> String {
        let root = self.root.lock();
        let end = root.iter().position(|&b| b == 0).unwrap_or(VFS_MAX_PATH);
        String::from(core::str::from_utf8(&root[..end]).unwrap_or("/"))
    }

    /// 设置根前缀 (`chroot` / `pivot_root` 机制)
    ///
    /// `real_root` 必须是**真实路径** (调用方先经 `resolve_user_path` 归一化).
    /// 语义与 Linux `chroot` 一致: 切根后当前工作目录重置为视图根 "/".
    // SIMPLIFIED: 仅切根 + 重置 cwd, 不做挂载表/已打开 fd 的根可达性校验 (Linux
    // 亦不强制); 影响面: 已打开 fd 仍可访问旧根之外的对象; 何时需扩展: 需要
    // Linux `pivot_root` 的 "旧根不可达" 强语义时补充 fd 遍历校验.
    pub fn set_root(&self, real_root: &str) {
        let bytes = real_root.as_bytes();
        let mut root = self.root.lock();
        let len = bytes.len().min(VFS_MAX_PATH - 1);
        root[..len].copy_from_slice(&bytes[..len]);
        root[len] = 0;
        if len == 0 {
            root[0] = b'/';
            root[1] = 0;
        }
        drop(root);
        self.set_cwd("/");
    }

    /// 单一权威路径归一化 (视图路径) — 就地写入 `out`, 返回有效长度
    ///
    /// 语义 (`chroot` / `pivot_root` 逃逸防护的关键):
    /// - 绝对路径以视图根 "/" 为起点; 相对路径以当前视图 cwd 为起点
    /// - 逐组件处理: `.` 忽略; `..` 上溯一级但**钳制在视图根内** (不可逃逸)
    /// - 结果恒以 '/' 开头且无尾随 '/' (视图根除外)
    ///
    /// 返回 `None` 表示结果超出 `VFS_MAX_PATH` (调用方按 ENAMETOOLONG 处理).
    fn normalize_view_path_into(&self, path: &str, out: &mut [u8; VFS_MAX_PATH]) -> Option<usize> {
        let bytes = path.as_bytes();
        let absolute = bytes.first() == Some(&b'/');

        // 基准 (视图根 或 视图 cwd) 写入 `out`
        let mut i = usize::from(absolute);
        let mut len = if absolute {
            out[0] = b'/';
            1
        } else {
            let cwd = self.get_cwd();
            let b = cwd.as_bytes();
            if b.first() == Some(&b'/') {
                let mut n = b.len().min(VFS_MAX_PATH - 1);
                // 去除尾随 '/' ("/home/" → "/home"), 避免拼接出双斜杠
                if n > 1 && b[n - 1] == b'/' {
                    n -= 1;
                }
                out[..n].copy_from_slice(&b[..n]);
                n
            } else {
                out[0] = b'/';
                1
            }
        };

        // 逐组件归一化
        while i < bytes.len() {
            let start = i;
            while i < bytes.len() && bytes[i] != b'/' {
                i += 1;
            }
            let comp = &bytes[start..i];
            i += 1;
            match comp {
                b"" | b"." => {}
                b".." => {
                    // 上溯一级; len == 1 (视图根) 时保持原地, 实现根内钳制
                    if len > 1 {
                        len = truncate_to_parent(&out[..len]);
                    }
                }
                _ => {
                    if len > 1 {
                        if len + 1 >= VFS_MAX_PATH {
                            return None;
                        }
                        out[len] = b'/';
                        len += 1;
                    }
                    if len + comp.len() >= VFS_MAX_PATH {
                        return None;
                    }
                    out[len..len + comp.len()].copy_from_slice(comp);
                    len += comp.len();
                }
            }
        }
        Some(len)
    }

    /// 用户路径 → 视图路径 (仅归一化, 不加根前缀) — 供 `chdir` 保存 cwd
    ///
    /// cwd 语义为视图路径 (根前缀之外的相对基准), 故与 `resolve_user_path` 区分.
    pub fn resolve_view_path<'a>(
        &self,
        path: &str,
        out: &'a mut [u8; VFS_MAX_PATH],
    ) -> Option<&'a str> {
        let len = self.normalize_view_path_into(path, out)?;
        core::str::from_utf8(&out[..len]).ok()
    }

    /// 用户路径 → 真实路径 (视图归一化 + 根前缀拼接) — 就地写入 `out`
    ///
    /// 所有接受用户路径的 VFS 入口必须经此函数解析 (单一权威入口).
    /// 默认根 "/" 时返回值与改造前逐字节等价.
    /// 返回 `None` 表示路径超长或非 UTF-8 边界 (调用方按 ENAMETOOLONG/EINVAL 处理).
    pub fn resolve_user_path<'a>(
        &self,
        path: &str,
        out: &'a mut [u8; VFS_MAX_PATH],
    ) -> Option<&'a str> {
        let mut view = [0u8; VFS_MAX_PATH];
        let view_len = self.normalize_view_path_into(path, &mut view)?;

        let root = *self.root.lock();
        let root_len = root.iter().position(|&b| b == 0).unwrap_or(VFS_MAX_PATH);

        // 默认根: 视图路径即真实路径 (无额外分配/拷贝语义变化)
        if root_len <= 1 {
            out[..view_len].copy_from_slice(&view[..view_len]);
            return core::str::from_utf8(&out[..view_len]).ok();
        }

        // 拼接: 根前缀 + 视图路径 (视图路径恒以 '/' 开头, 故取 [1..] 作为尾部)
        let tail = if view_len > 1 {
            &view[1..view_len]
        } else {
            &view[..0]
        };
        if root_len + 1 + tail.len() >= VFS_MAX_PATH {
            return None;
        }
        out[..root_len].copy_from_slice(&root[..root_len]);
        let len = if tail.is_empty() {
            root_len
        } else {
            out[root_len] = b'/';
            out[root_len + 1..root_len + 1 + tail.len()].copy_from_slice(tail);
            root_len + 1 + tail.len()
        };
        core::str::from_utf8(&out[..len]).ok()
    }

    pub fn capture_snapshot(&self) {
        let mounts_data = {
            let m = self.mounts.lock();
            m.clone()
        };
        let cwd_data = *self.cwd.lock();
        let root_data = *self.root.lock();
        *self.snapshot.lock() = Some(VfsSnapshot {
            mounts: mounts_data,
            cwd: cwd_data,
            root: root_data,
        });
    }

    #[expect(
        clippy::assigning_clones,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn restore_from_snapshot(&self) {
        if let Some(ref snap) = *self.snapshot.lock() {
            *self.mounts.lock() = snap.mounts.clone();
            *self.cwd.lock() = snap.cwd;
            *self.root.lock() = snap.root;
        }
    }
}

pub static VFS_MANAGER: VfsManager = VfsManager::new();

/// VFS 子系统初始化 — 注册 barrier 回调 + 初始化 VFS_MANAGER
///
/// 保留在 framework 层 (引用 framework::barrier).
pub fn init() {
    VFS_MANAGER.init();

    // barrier 回调注册 (必须在 framework 层, 因为引用 framework::barrier)
    if let Some(dom) = crate::framework::barrier::RECOVERY_MANAGER.lock().find(2) {
        *dom.capture_cb.lock() = Some(vfs_barrier_capture_cb);
        *dom.rollback_cb.lock() = Some(vfs_barrier_rollback_cb);
    }
}

fn vfs_barrier_capture_cb() {
    VFS_MANAGER.capture_snapshot();
}

fn vfs_barrier_rollback_cb() -> bool {
    VFS_MANAGER.restore_from_snapshot();
    true
}
