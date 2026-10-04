//! VFS 公共类型 — services 层完整定义
//!
//! ## 阶段 4b 迁移记录 (VFS 完整下沉 · DECISION-W)
//!
//! VFS 类型定义 (常量/枚举/结构体/FileSystem trait/OpenFile) 按"VFS 完整
//! 下沉 services"决策从 `framework::fs::vfs` 迁入本文件. 0 语义变更,
//! `KernelError` 引用保持 `framework::error::KernelError` 不变.
//! `VfsFileType` 为纯 POD 数据, 保留于 framework `types_pod`,
//! 本文件 re-export 保持路径兼容.
//!
//! ## 说明
//!
//! - Inode trait 定义于 `super::inode`, 本文件引用之.
//! - 各文件系统 (ramfs/unkfs/ext2/exfat/...) 在本模块树内实现本 trait.

pub const VFS_MAX_PATH: usize = 128;
pub const VFS_MAX_NAME: usize = 64;
pub const VFS_MAX_MOUNTS: usize = 8;

// 统一到 framework::error (P0-2 迁回)
pub use crate::framework::error::KernelError;

/// fs 层特有的类型别名 (向后兼容旧变体名)
pub type NotFound = crate::framework::error::KernelError;
pub type IoError = crate::framework::error::KernelError;
pub type OutOfMemory = crate::framework::error::KernelError;
pub type ReadOnly = crate::framework::error::KernelError;

impl KernelError {
    /// VFS 风格返回: 负 errno (POSIX `-|errno|` 约定)
    pub fn as_i32(self) -> i32 {
        -(self.as_errno() as i32)
    }
}

// 统一到 framework::error (阶段 4b: 定义迁回 framework, 本处 re-export)
pub use crate::framework::error::KernelResult;

pub trait IntoI32 {
    fn as_i32(self) -> i32;
}

impl IntoI32 for Result<(), KernelError> {
    fn as_i32(self) -> i32 {
        match self {
            Ok(()) => 0,
            Err(e) => e.as_i32(),
        }
    }
}

// 阶段 4b: `VfsFileType` 抽留 framework `types_pod`, 此处 re-export 保持路径兼容.
pub use crate::framework::fs::vfs::types_pod::VfsFileType;

pub const VFS_PERM_R: u16 = 0x04;
pub const VFS_PERM_W: u16 = 0x02;
pub const VFS_PERM_X: u16 = 0x01;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct VfsOpenFlags: u32 {
        const RDONLY = 0x0001;
        const WRONLY = 0x0002;
        const RDWR   = 0x0004;
        const CREAT  = 0x0100;
        const TRUNC  = 0x0200;
        const APPEND = 0x0400;
        /// fd 级 close-on-exec 标志 (对应 O_CLOEXEC, 与 Linux 数值一致);
        /// 由 `vfs_open_internal` 消费后从文件状态标志中剥离.
        const CLOEXEC = 0x0008_0000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsSeekWhence {
    Set,
    Cur,
    End,
}

impl VfsSeekWhence {
    /// 从 u32 构造 `VfsSeekWhence`
    ///
    /// 返回 `None` 表示非法值 (0-2 为合法值)
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Set),
            1 => Some(Self::Cur),
            2 => Some(Self::End),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    RamFs,
    NestFs,
    DevFs,
    Ext2,
    ExFat,
    TmpFs,
    OverlayFs,
    Unknown,
}

impl FsType {
    pub fn from_name(name: &str) -> Self {
        match name {
            "ramfs" => Self::RamFs,
            "unkfs" => Self::NestFs,
            "devfs" => Self::DevFs,
            "ext2" => Self::Ext2,
            "exfat" => Self::ExFat,
            "tmpfs" => Self::TmpFs,
            "overlay" => Self::OverlayFs,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RamFs => "ramfs",
            Self::NestFs => "unkfs",
            Self::DevFs => "devfs",
            Self::Ext2 => "ext2",
            Self::ExFat => "exfat",
            Self::TmpFs => "tmpfs",
            Self::OverlayFs => "overlay",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VfsStat {
    pub node_id: u32,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u32,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub owner_pwm: u64,
    pub group_pwm: u64,
    pub perm: u16,
    pub file_type: u8,
    pub sensitivity: u8,
}

impl Default for VfsStat {
    fn default() -> Self {
        Self {
            node_id: 0,
            mode: 0,
            uid: 0xFFFF_FFFF,
            gid: 0xFFFF_FFFF,
            size: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            owner_pwm: 0,
            group_pwm: 0,
            perm: 0,
            file_type: 0,
            sensitivity: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VfsDirEntry {
    pub node: u32,
    pub file_type: u8,
    pub name: [u8; VFS_MAX_NAME],
}

impl Default for VfsDirEntry {
    fn default() -> Self {
        Self {
            node: 0,
            file_type: 0,
            name: [0u8; VFS_MAX_NAME],
        }
    }
}

impl VfsDirEntry {
    pub fn new() -> Self {
        Self {
            node: 0,
            file_type: 0,
            name: [0; VFS_MAX_NAME],
        }
    }

    pub fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(VFS_MAX_NAME - 1);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name[len] = 0;
    }

    pub fn get_name(&self) -> &str {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(VFS_MAX_NAME);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }
}

// ============================================================================
// FileSystem trait — VFS 分发策略接口
// ============================================================================
//
// E6-4: 将 api.rs 中 14+ 个 `match fs_type` 分发替换为 trait object 分发.
// 新增文件系统只需实现本 trait, 无需修改 framework.
//
// 设计原则:
// - 统一 RamFS (node_id) / UNKFS (fd) 的差异: open 返回 FsOpenResult,
//   内部不透明 handle 由各 FS 自行解释
// - 所有方法接收 `&self` (非 `&mut self`), 内部可变性由各 FS 自行管理
//   (RamFS 用内部 Mutex, UNKFS 用内部原子操作)
// - pwm 参数由 VFS 层传入, FS 实现负责权限检查
//
// L4 重构: 核心方法必须实现, 扩展方法提供默认实现 (返回 NotSupported)
// - 核心方法: 生命周期 + 文件操作 + 元数据 + 目录操作 (必须实现)
// - 扩展方法: 符号链接 + 扩展操作 + 扩展属性 (默认返回 NotSupported, 可选择性 override)

/// `fs_open` 返回结果
#[derive(Debug, Clone, Copy)]
pub struct FsOpenResult {
    /// FS 内部不透明 handle (`RamFS` 填 `node_id`, `UNKFS` 填 fd)
    pub handle: u32,
    /// 文件初始偏移
    pub offset: u64,
    /// 文件类型 (`VfsFileType::as_u8`)
    pub file_type: u8,
}

/// 文件系统策略接口 — services 层实现, framework 层调用
///
/// 所有方法返回 `KernelResult<T>`, 由 VFS api.rs 统一转为 i32 错误码.
/// 新增文件系统只需实现本 trait 并注册到 `VFS_MANAGER`, 无需修改 framework.
///
/// L4 重构: 核心方法必须实现, 扩展方法提供默认实现 (返回 `NotSupported`).
/// 实现者可以选择性地 override 扩展方法, 减少实现负担.
pub trait FileSystem: Send + Sync {
    /// 文件系统名称 (如 "ramfs", "unkfs")
    fn name(&self) -> &'static str;

    // ---- 生命周期 ----

    /// 初始化文件系统 (mount 前调用)
    ///
    /// # Errors
    /// 当底层初始化失败时返回 `KernelError` (具体由实现者决定).
    fn fs_init(&self) -> KernelResult<()>;
    /// 挂载到指定路径
    ///
    /// # Errors
    /// 当路径非法、已被占用或底层挂载失败时返回 `KernelError` (具体由实现者决定).
    fn fs_mount(&self, path: &str) -> KernelResult<()>;

    // ---- 文件操作 ----

    /// 打开文件, 返回 Inode trait object
    ///
    /// Plan B: 返回 `Arc<dyn Inode>` 替代原来的 `FsOpenResult`.
    /// 调用者 (VFS API) 直接将返回的 Inode 封装为 `OpenFile`.
    ///
    /// # Errors
    /// 当路径不存在、无权限或底层打开失败时返回 `KernelError`.
    fn fs_open(&self, rel_path: &str, flags: u32, pwm: u64) -> KernelResult<Arc<dyn Inode>>;
    /// 关闭文件
    ///
    /// # Errors
    /// 当句柄无效或底层关闭失败时返回 `KernelError` (具体由实现者决定).
    fn fs_close(&self, handle: u32) -> KernelResult<()>;
    /// 读文件, 返回实际读取字节数
    ///
    /// # Errors
    /// 当句柄无效、偏移越界或底层 I/O 失败时返回 `KernelError`.
    fn fs_read(&self, handle: u32, offset: u64, buf: &mut [u8], pwm: u64) -> KernelResult<usize>;
    /// 写文件, 返回实际写入字节数
    ///
    /// # Errors
    /// 当句柄无效、文件只读或底层 I/O 失败时返回 `KernelError`.
    fn fs_write(&self, handle: u32, offset: u64, buf: &[u8], pwm: u64) -> KernelResult<usize>;

    // ---- 元数据 ----

    /// 获取文件属性
    ///
    /// # Errors
    /// 当路径不存在或底层元数据读取失败时返回 `KernelError`.
    fn fs_stat(&self, rel_path: &str, pwm: u64) -> KernelResult<VfsStat>;
    /// 修改文件权限
    ///
    /// # Errors
    /// 当路径不存在、无权限或底层元数据更新失败时返回 `KernelError`.
    fn fs_chmod(&self, rel_path: &str, mode: u16, pwm: u64) -> KernelResult<()>;
    /// 修改文件所有者
    ///
    /// # Errors
    /// 当路径不存在、无权限或底层元数据更新失败时返回 `KernelError`.
    fn fs_chown(
        &self,
        rel_path: &str,
        owner_pwm: u64,
        group_pwm: u64,
        pwm: u64,
    ) -> KernelResult<()>;

    // ---- 目录操作 ----

    /// 创建目录
    ///
    /// # Errors
    /// 当父路径不存在、名称已存在、无权限或底层创建失败时返回 `KernelError`.
    fn fs_mkdir(&self, rel_path: &str, pwm: u64) -> KernelResult<()>;
    /// 删除文件
    ///
    /// # Errors
    /// 当路径不存在、无权限或底层删除失败时返回 `KernelError`.
    fn fs_unlink(&self, rel_path: &str, pwm: u64) -> KernelResult<()>;
    /// 删除目录
    ///
    /// # Errors
    /// 当目录不存在、非空、无权限或底层删除失败时返回 `KernelError`.
    fn fs_rmdir(&self, rel_path: &str, pwm: u64) -> KernelResult<()>;
    /// 重命名
    ///
    /// # Errors
    /// 当源/目标路径无效、无权限或底层重命名失败时返回 `KernelError`.
    fn fs_rename(&self, old_path: &str, new_path: &str, pwm: u64) -> KernelResult<()>;
    /// 读取目录项, 返回 true 表示还有更多项
    ///
    /// # Errors
    /// 当句柄无效、不是目录或底层目录读取失败时返回 `KernelError`.
    fn fs_readdir(&self, handle: u32, offset: u64, entry: &mut VfsDirEntry) -> KernelResult<bool>;

    // ---- 扩展方法 (可选, 默认返回 NotSupported) ----

    // 符号链接
    /// 创建符号链接.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限、名称已存在或底层创建失败时返回 `KernelError`.
    fn fs_symlink(&self, _target: &str, _link_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }
    /// 读取符号链接目标.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当路径不是符号链接或目标读取失败时返回 `KernelError`.
    fn fs_readlink(&self, _rel_path: &str, _buf: &mut [u8]) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }
    /// 创建硬链接.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当无权限、名称已存在或底层链接创建失败时返回 `KernelError`.
    fn fs_link(&self, _old_path: &str, _new_path: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }

    // 扩展操作
    /// 设置文件时间戳.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当路径不存在、无权限或底层更新失败时返回 `KernelError`.
    fn fs_utimensat(
        &self,
        _rel_path: &str,
        _atime: u64,
        _mtime: u64,
        _pwm: u64,
    ) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }
    /// 截断文件到指定大小.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当句柄无效、无权限或底层截断失败时返回 `KernelError`.
    fn fs_truncate(&self, _handle: u32, _size: u64, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }
    /// 调整文件偏移.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当 `whence` 非法或计算偏移越界时返回 `KernelError`.
    fn fs_seek(
        &self,
        _handle: u32,
        _offset: i64,
        _whence: VfsSeekWhence,
        _current: u64,
    ) -> KernelResult<u64> {
        Err(KernelError::NotSupported)
    }
    fn fs_resolve_path(&self, _rel_path: &str) -> Option<u32> {
        None
    }
    fn fs_resolve_inode(&self, _inode_id: u32, _mount_idx: u32) -> Option<Arc<dyn Inode>> {
        None
    }
    /// 创建文件.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当父路径不存在、无权限或底层创建失败时返回 `KernelError`.
    fn fs_create(&self, parent_path: &str, name: &str, pwm: u64) -> KernelResult<Arc<dyn Inode>> {
        let _ = (parent_path, name, pwm);
        Err(KernelError::NotSupported)
    }
    /// 同步文件系统缓存.
    ///
    /// # Errors
    /// 默认实现恒返回 `Ok(())`; 当底层同步失败时返回 `KernelError`.
    fn fs_sync(&self) -> KernelResult<()> {
        Ok(())
    }
    /// 按 inode ID 直接读取.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当底层读取失败时返回 `KernelError`.
    fn fs_pread_inode(
        &self,
        _node_id: u32,
        _offset: u64,
        _buf: &mut [u8],
        _pwm: u64,
    ) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    // 扩展属性
    /// 设置扩展属性.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当路径不存在、无权限或底层设置失败时返回 `KernelError`.
    fn fs_setxattr(
        &self,
        _rel_path: &str,
        _name: &str,
        _value: &[u8],
        _pwm: u64,
    ) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }
    /// 读取扩展属性.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当属性不存在、缓冲区过小或底层读取失败时返回 `KernelError`.
    fn fs_getxattr(
        &self,
        _rel_path: &str,
        _name: &str,
        _buf: &mut [u8],
        _pwm: u64,
    ) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }
    /// 列出扩展属性.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当缓冲区过小或底层列出失败时返回 `KernelError`.
    fn fs_listxattr(&self, _rel_path: &str, _buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }
    /// 移除扩展属性.
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 当属性不存在、无权限或底层移除失败时返回 `KernelError`.
    fn fs_removexattr(&self, _rel_path: &str, _name: &str, _pwm: u64) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }
    /// 格式化底层介质 (UNKFS 磁盘模式使用).
    ///
    /// 封装原 framework fsformat 路径对 UNKFS 内部字段 (drives_discovered/
    /// disk_drive/partition_start) 的直接访问, 归位 services 策略
    /// (DECISION-K 项 6: 注入归零).
    ///
    /// # Errors
    /// 默认实现返回 `NotSupported`; 介质格式化失败或不支持时返回 `KernelError`.
    fn fs_format(&self) -> KernelResult<()> {
        Err(KernelError::NotSupported)
    }
}

// ============================================================================
// OpenFile — 打开文件描述 (POSIX open file description)
// ============================================================================
//
// 对应 POSIX "打开文件描述", 多个 fd 可共享 (dup 语义).
// offset 和 flags 在所有共享者之间共享.

use super::inode::Inode;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// 打开文件描述 — POSIX open file description 的 `Edgine` 实现
///
/// 多个 fd 可以指向同一个 `OpenFile` (通过 dup).
/// offset 和 flags 在所有共享者之间共享.
///
/// Plan B: 持有 `Arc<dyn Inode>` 替代原来的 `inode_id: u32`,
/// 实现完全的 POSIX 打开文件描述语义.
pub struct OpenFile {
    /// Inode trait object — 文件级 I/O 操作
    inode: Arc<dyn Inode>,
    /// 共享文件偏移 (原子操作, dup 共享)
    pub offset: AtomicU64,
    /// 共享状态标志 (`O_RDONLY`, `O_APPEND` 等, dup 共享)
    pub flags: u32,
    /// 权限凭证
    pub pwm: u64,
    /// 引用计数 (dup 增加, close 减少)
    pub refcount: AtomicU32,
    /// 文件类型 (`VfsFileType::as_u8`)
    pub file_type: u8,
    /// 是否匿名文件 (memfd)
    pub is_anonymous: bool,
}

impl core::fmt::Debug for OpenFile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OpenFile")
            .field("inode_id", &self.inode.node_id())
            .field("offset", &self.offset)
            .field("flags", &self.flags)
            .field("refcount", &self.refcount)
            .field("file_type", &self.file_type)
            .field("is_anonymous", &self.is_anonymous)
            .finish()
    }
}

impl OpenFile {
    /// 创建新的 `OpenFile` (Plan B: 使用 Inode trait object)
    pub fn new(inode: Arc<dyn Inode>, flags: u32, pwm: u64, file_type: u8) -> Self {
        Self {
            inode,
            offset: AtomicU64::new(0),
            flags,
            pwm,
            refcount: AtomicU32::new(1),
            file_type,
            is_anonymous: false,
        }
    }

    /// 创建匿名文件 `OpenFile` (memfd 用)
    pub fn new_anonymous(inode: Arc<dyn Inode>, flags: u32, pwm: u64, file_type: u8) -> Self {
        let mut of = Self::new(inode, flags, pwm, file_type);
        of.is_anonymous = true;
        of
    }

    // ---- 兼容旧 API (折中实现过渡期) ----

    /// 获取底层 `inode_id` (兼容旧代码, 供 pcache 等使用)
    ///
    /// 注意: 新代码应优先使用 `inode()` 方法.
    pub fn inode_id(&self) -> u32 {
        self.inode.node_id()
    }

    /// 获取挂载点索引 (兼容旧代码)
    pub fn mount_idx(&self) -> u32 {
        self.inode.mount_idx()
    }

    /// 获取 Inode trait object 引用
    pub fn inode(&self) -> &dyn Inode {
        &*self.inode
    }

    /// 获取 Inode Arc 引用 (用于 dup 时克隆)
    pub fn inode_arc(&self) -> &Arc<dyn Inode> {
        &self.inode
    }

    /// 增加引用计数 (dup 时调用)
    pub fn inc_ref(&self) {
        self.refcount.fetch_add(1, Ordering::Relaxed);
    }

    /// 减少引用计数 (close 时调用)
    /// 返回减少前的值, 如果返回 1 说明这是最后一个引用
    pub fn dec_ref(&self) -> u32 {
        self.refcount.fetch_sub(1, Ordering::Release)
    }

    /// 获取当前引用计数
    pub fn ref_count(&self) -> u32 {
        self.refcount.load(Ordering::Acquire)
    }

    /// 获取当前文件偏移
    pub fn get_offset(&self) -> u64 {
        self.offset.load(Ordering::Acquire)
    }

    /// 设置文件偏移
    pub fn set_offset(&self, offset: u64) {
        self.offset.store(offset, Ordering::Release);
    }

    /// 获取状态标志
    pub fn get_flags(&self) -> u32 {
        self.flags
    }

    /// 设置状态标志
    pub fn set_flags(&mut self, flags: u32) {
        self.flags = flags;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // UT-07 (2026-09-26): 注册侧 vfs::types 组全量迁入 —
    // framework/tests/test_vfs.rs 的 5 例 (FsType 名称映射与回写 /
    // VfsFileType 编码 / VfsSeekWhence 编码 / VfsDirEntry 名称存取)
    // 逐例归并等价判据, 断言数净增不净减; 该注册子块随后删除.

    #[test]
    fn fstype_from_name() {
        assert_eq!(FsType::from_name("ramfs"), FsType::RamFs, "ramfs");
        assert_eq!(FsType::from_name("unkfs"), FsType::NestFs, "unkfs");
        assert_eq!(FsType::from_name("ext4"), FsType::Unknown, "未知名称");
    }

    #[test]
    fn fstype_as_str() {
        assert_eq!(FsType::RamFs.as_str(), "ramfs", "RamFs 回写");
        assert_eq!(FsType::NestFs.as_str(), "unkfs", "NestFs 回写");
        assert_eq!(FsType::Unknown.as_str(), "unknown", "Unknown 回写");
    }

    #[test]
    fn vfs_file_type() {
        assert_eq!(VfsFileType::from_u8(0), Some(VfsFileType::File), "0=File");
        assert_eq!(VfsFileType::from_u8(1), Some(VfsFileType::Dir), "1=Dir");
        assert_eq!(VfsFileType::from_u8(2), Some(VfsFileType::Dev), "2=Dev");
        assert_eq!(
            VfsFileType::from_u8(3),
            Some(VfsFileType::Symlink),
            "3=Symlink"
        );
        assert_eq!(VfsFileType::from_u8(99), None, "非法值应为 None");
        assert_eq!(VfsFileType::Dir.as_u8(), 1, "Dir 编码");
    }

    #[test]
    fn vfs_seek_whence() {
        assert_eq!(
            VfsSeekWhence::from_u32(0),
            Some(VfsSeekWhence::Set),
            "0=Set"
        );
        assert_eq!(
            VfsSeekWhence::from_u32(1),
            Some(VfsSeekWhence::Cur),
            "1=Cur"
        );
        assert_eq!(
            VfsSeekWhence::from_u32(2),
            Some(VfsSeekWhence::End),
            "2=End"
        );
        assert_eq!(VfsSeekWhence::from_u32(99), None, "非法值应为 None");
    }

    #[test]
    fn vfs_dirent() {
        let mut dirent = VfsDirEntry::new();
        dirent.set_name("test.txt");
        assert_eq!(dirent.get_name(), "test.txt", "目录项名称");
    }
}
