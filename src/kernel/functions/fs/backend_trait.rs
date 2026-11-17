//! VFS 后端决策 trait — 策略-机制分离接口
//!
//! T-05: VFS 后端选择策略 (根据 fs_type 选择挂载方式) 由 functions 实现,
//! privileged 仅保留挂载点管理、inode 操作表定义等机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged/functions 共享类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackFsBackend`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_fs_backend()` 注册自己的策略实现
//!
//! ## 策略边界
//!
//! privileged 保留 (机制):
//! - 挂载点查找 (最长前缀匹配)
//! - VfsMount 数据结构管理
//! - fd 分配与偏移管理
//! - dcache / inotify / flock 机制
//!
//! functions 实现 (策略):
//! - 根据 fs_type 名称选择挂载方式 (trait object 或字符串匹配)
//! - 挂载权限检查
//! - 文件系统注册表管理

use crate::functions::fs::inode::Inode;
use crate::functions::fs::vfs_types::{FS_REGISTRY_MAX_ENTRIES, FileSystem, KernelError};

/// 文件系统后端决策接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait FsBackend: Send + Sync {
    /// 挂载文件系统到指定路径
    ///
    /// functions 根据 `fs_name` 查找对应的 `FileSystem` 实现,
    /// 然后调用 privileged 的 mount API 完成挂载.
    /// 对于可获取 `&'static dyn FileSystem` 的后端, 调用 `mount_with_fs`;
    /// 对于需要内部同步的后端 (如 Mutex 保护的), 调用 `vfs_mount`.
    /// # Errors
    /// 找不到对应的文件系统实现或挂载失败时返回 Err。
    fn mount_fs(&self, fs_name: &str, path: &str) -> Result<(), KernelError>;

    /// 是否允许挂载到指定路径
    ///
    /// functions 可实现权限检查 (如只允许 root 挂载到 /).
    fn allow_mount(&self, path: &str, fs_name: &str) -> bool;

    /// 创建 `RamFS` Inode 实例 (工厂钩子, DECISION-K 项 5)
    ///
    /// privileged `RamFsData` (机制) 在 `fs_open` / `fs_create` / `fs_resolve_inode`
    /// 中需要产出 Inode trait object; 具象 `RamFsInode` 是 functions 层实现,
    /// privileged 不直接依赖, 经由此钩子由 functions 构造注入.
    /// `fs_id` 为该 `RamFS` 实例的 dcache/icache 命名空间标识 (随实例挂载分配).
    /// # Errors
    /// 后端未注册或拒绝构造时返回 Err (回退策略 fail-closed)。
    fn make_ramfs_inode(
        &self,
        inode_id: u32,
        mount_idx: u32,
        fs_id: u32,
    ) -> Result<alloc::sync::Arc<dyn Inode>, KernelError>;

    /// 按名称解析文件系统实例 (functions 文件系统注册表)
    ///
    /// functions 维护 `fs_name -> &'static dyn FileSystem` 注册表; privileged
    /// 挂载路径经此解析 trait object, 无需认识任何具体文件系统类型。
    /// 未注册的名称返回 `None` (fail-closed)。
    fn resolve_fs(&self, fs_name: &str) -> Option<&'static dyn FileSystem>;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略 — 无任何文件系统实现, 拒绝所有挂载
///
/// 在 functions 注册策略之前, VFS 使用此策略.
/// 早期启动阶段无文件系统可用, 所有挂载请求被拒绝.
pub struct FallbackFsBackend;

impl FsBackend for FallbackFsBackend {
    fn mount_fs(&self, _fs_name: &str, _path: &str) -> Result<(), KernelError> {
        Err(KernelError::NotInitialized)
    }

    fn allow_mount(&self, _path: &str, _fs_name: &str) -> bool {
        false
    }

    fn make_ramfs_inode(
        &self,
        _inode_id: u32,
        _mount_idx: u32,
        _fs_id: u32,
    ) -> Result<alloc::sync::Arc<dyn Inode>, KernelError> {
        Err(KernelError::NotInitialized)
    }

    fn resolve_fs(&self, _fs_name: &str) -> Option<&'static dyn FileSystem> {
        None
    }
}

static FALLBACK_BACKEND: FallbackFsBackend = FallbackFsBackend;

/// 全局策略注册表 — functions 通过 `register_fs_backend` 注册
static FS_BACKEND: crate::privileged::sync::OnceLock<&'static dyn FsBackend> =
    crate::privileged::sync::OnceLock::new();

/// 注册 VFS 后端决策策略 (由 `functions::fs::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
/// # Errors
/// 策略已被注册过时返回 Err。
pub fn register_fs_backend(policy: &'static dyn FsBackend) -> Result<(), &'static dyn FsBackend> {
    match FS_BACKEND.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的 VFS 后端决策策略 (未注册时返回内建回退)
#[inline]
pub fn current_fs_backend() -> &'static dyn FsBackend {
    match FS_BACKEND.get() {
        Some(&p) => p,
        None => &FALLBACK_BACKEND,
    }
}

// ============================================================================
// EGFS FileSystem 注册表 (DECISION-K 项 6: 注入归零)
// ============================================================================

/// 全局 EGFS `FileSystem` 注册表 — `functions::fs::init` 注册 `EgfsData` 实例
static EGFS_FS: crate::privileged::sync::OnceLock<&'static dyn FileSystem> =
    crate::privileged::sync::OnceLock::new();

/// 注册 EGFS `FileSystem` 实例 (由 `functions::fs::init` 调用)
///
/// privileged 挂载/格式化路径经 `egfs_fs()` 消费 trait object,
/// 不再反向依赖 functions 具象 `EgfsData`.
/// # Errors
/// 已被注册过时返回 Err。
pub fn register_egfs_fs(fs: &'static dyn FileSystem) -> Result<(), &'static dyn FileSystem> {
    match EGFS_FS.set(fs) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取注册的 EGFS `FileSystem` (未注册时返回 None — fail-closed)
#[inline]
pub fn egfs_fs() -> Option<&'static dyn FileSystem> {
    match EGFS_FS.get() {
        Some(&fs) => Some(fs),
        None => None,
    }
}

// ============================================================================
// 通用文件系统注册表 (S-1: 对照 Linux register_filesystem / get_fs_type)
// ============================================================================
//
// SIMPLIFIED: 注册表在 `functions::fs::init()` 期一次性构建, 运行期只读 ——
// 本内核无运行期可装载模块, 不需要注册/注销的并发面, 故不引入读写锁
// (Linux 用 file_systems_lock 是因为它允许模块在运行期增删)。
// 影响面: 新增 FS 需在 init 的注册点加一行 + 不在 vfs_mount 里改分发;
// 何时需扩展: 引入可装载 FS 模块时改为锁保护的动态表。

/// 按名称索引的文件系统实例表 — 名称到 `FileSystem` trait object 的单源映射
///
/// 与 `FsType` 的分工: `FsType` 是挂载记账标签 (枚举, 见 `VfsMount::fs_type`),
/// 本表是实例解析的唯一来源; 二者不再各自维护一份硬编码清单。
pub struct FsRegistry {
    /// 已注册条目 (前 `len` 项有效)
    entries: [Option<(&'static str, &'static dyn FileSystem)>; FS_REGISTRY_MAX_ENTRIES],
    /// 已注册条目数
    len: usize,
}

impl FsRegistry {
    /// 构造空注册表
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [None; FS_REGISTRY_MAX_ENTRIES],
            len: 0,
        }
    }

    /// 按名称查找 `FileSystem` 实例 (未注册返回 None)
    #[must_use]
    pub fn lookup(&self, fs_name: &str) -> Option<&'static dyn FileSystem> {
        self.entries[..self.len]
            .iter()
            .flatten()
            .find(|(name, _)| *name == fs_name)
            .map(|(_, fs)| *fs)
    }

    /// 注册一个文件系统实例 (名称重复或表满时返回 Err)
    ///
    /// # Errors
    /// 同名已注册返回 `AlreadyExists`; 条目数已达上限返回 `NoSpace`.
    pub fn register_filesystem(
        &mut self,
        fs_name: &'static str,
        fs: &'static dyn FileSystem,
    ) -> Result<(), KernelError> {
        if self.lookup(fs_name).is_some() {
            return Err(KernelError::AlreadyExists);
        }
        if self.len >= FS_REGISTRY_MAX_ENTRIES {
            return Err(KernelError::NoSpace);
        }
        self.entries[self.len] = Some((fs_name, fs));
        self.len += 1;
        Ok(())
    }
}

impl Default for FsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局文件系统注册表 — `functions::fs::init` 期一次性安装
static FS_REGISTRY: crate::privileged::sync::OnceLock<FsRegistry> =
    crate::privileged::sync::OnceLock::new();

/// 安装文件系统注册表 (仅首次生效; 重复调用返回 `false` 并保留原表)
///
/// 以 `bool` 而非 `Result<(), FsRegistry>` 返回: 后者 Err 携带整张表 (>256 字节),
/// 触发 `clippy::result_large_err`, 而调用方本就只需知道是否安装成功.
#[must_use]
pub fn set_fs_registry(registry: FsRegistry) -> bool {
    FS_REGISTRY.set(registry).is_ok()
}

/// 按名称解析文件系统实例 (注册表未安装或名称未注册时返回 None — fail-closed)
#[inline]
pub fn lookup_filesystem(fs_name: &str) -> Option<&'static dyn FileSystem> {
    FS_REGISTRY
        .get()
        .and_then(|registry| registry.lookup(fs_name))
}

// ============================================================================
// 单元测试 (DECISION-080 双轨: 纯逻辑测试归源侧 #[cfg(test)])
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // DECISION-K 项 6 回归测试 (第二十四批): functions::fs::init 注册激活
    //
    // 回归背景: functions::fs::init 此前全库无调用者, 第二十三批 ramfs 回迁引入
    // 的 make_ramfs_inode 钩子恒命中 FallbackFsBackend → Err(NotInitialized),
    // ramfs open/create 生产路径被回退策略拦截.

    #[test]
    fn test_fs_backend_registered_make_inode() {
        // 激活注册 (幂等: 重复注册 Err 被忽略)
        crate::functions::fs::init();
        // 钩子必须返回真实 Inode — FallbackFsBackend 恒 Err, 本断言锁定回归
        let result = current_fs_backend().make_ramfs_inode(0, 0, 0);
        assert!(
            result.is_ok(),
            "make_ramfs_inode 命中回退策略 — functions::fs::init 未生效"
        );
    }

    #[test]
    fn test_egfs_fs_registered() {
        crate::functions::fs::init();
        let Some(fs) = egfs_fs() else {
            panic!("egfs_fs() 未注册 — functions::fs::init 未生效");
        };
        assert_eq!(fs.name(), "egfs", "egfs name mismatch");
        // 注: fs_format 行为不在单测覆盖 (内存模式调 format_drive 有底层 IO 副作用),
        // 语义等价性由 fsformat 路径代码搬移保证, QEMU boot 覆盖挂载分发链路
    }

    // S-1: 通用注册表行为锁 — 注册/查找/未命中
    #[test]
    fn test_fs_registry_register_and_lookup() {
        let mut registry = FsRegistry::new();
        assert!(registry.lookup("ramfs").is_none(), "空表不应命中任何名称");
        registry
            .register_filesystem("ramfs", crate::functions::fs::ramfs_core::ramfs_fs())
            .expect("首次注册 ramfs 应成功");
        let fs = registry.lookup("ramfs").expect("已注册名称应可查找");
        assert_eq!(fs.name(), "ramfs");
        assert!(registry.lookup("egfs").is_none(), "未注册名称须返回 None");
    }

    // S-1: 同名重复注册必须拒绝 (fail-closed, 不允许静默覆盖)
    #[test]
    fn test_fs_registry_rejects_duplicate_name() {
        let mut registry = FsRegistry::new();
        registry
            .register_filesystem("ramfs", crate::functions::fs::ramfs_core::ramfs_fs())
            .expect("首次注册应成功");
        let dup =
            registry.register_filesystem("ramfs", crate::functions::fs::ramfs_core::ramfs_fs());
        assert!(matches!(dup, Err(KernelError::AlreadyExists)));
    }

    // S-1: 表满后注册 fail-closed (`NoSpace`), 不覆盖既有条目
    #[test]
    fn test_fs_registry_capacity_fail_closed() {
        static NAMES: [&'static str; FS_REGISTRY_MAX_ENTRIES] =
            ["fs0", "fs1", "fs2", "fs3", "fs4", "fs5", "fs6", "fs7"];
        let mut registry = FsRegistry::new();
        for &name in &NAMES {
            assert!(
                registry
                    .register_filesystem(name, crate::functions::fs::ramfs_core::ramfs_fs())
                    .is_ok(),
                "容量内注册应成功"
            );
        }
        let overflow =
            registry.register_filesystem("fs8", crate::functions::fs::ramfs_core::ramfs_fs());
        assert!(matches!(overflow, Err(KernelError::NoSpace)));
        assert!(registry.lookup("fs0").is_some(), "表满不应破坏既有条目");
    }
}
