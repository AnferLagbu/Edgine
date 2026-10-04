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
use crate::functions::fs::vfs_types::{FileSystem, KernelError};

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

    /// 创建 RamFS Inode 实例 (工厂钩子, DECISION-K 项 5)
    ///
    /// privileged `RamFsData` (机制) 在 fs_open / fs_create / fs_resolve_inode
    /// 中需要产出 Inode trait object; 具象 `RamFsInode` 是 functions 层实现,
    /// privileged 不直接依赖, 经由此钩子由 functions 构造注入.
    /// `fs_id` 为该 RamFS 实例的 dcache/icache 命名空间标识 (随实例挂载分配).
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
// UNKFS FileSystem 注册表 (DECISION-K 项 6: 注入归零)
// ============================================================================

/// 全局 UNKFS FileSystem 注册表 — `functions::fs::init` 注册 `UnkfsData` 实例
static UNKFS_FS: crate::privileged::sync::OnceLock<&'static dyn FileSystem> =
    crate::privileged::sync::OnceLock::new();

/// 注册 UNKFS FileSystem 实例 (由 `functions::fs::init` 调用)
///
/// privileged 挂载/格式化路径经 `unkfs_fs()` 消费 trait object,
/// 不再反向依赖 functions 具象 `UnkfsData`.
/// # Errors
/// 已被注册过时返回 Err。
pub fn register_unkfs_fs(fs: &'static dyn FileSystem) -> Result<(), &'static dyn FileSystem> {
    match UNKFS_FS.set(fs) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取注册的 UNKFS FileSystem (未注册时返回 None — fail-closed)
#[inline]
pub fn unkfs_fs() -> Option<&'static dyn FileSystem> {
    match UNKFS_FS.get() {
        Some(&fs) => Some(fs),
        None => None,
    }
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
    fn test_unkfs_fs_registered() {
        crate::functions::fs::init();
        let Some(fs) = unkfs_fs() else {
            panic!("unkfs_fs() 未注册 — functions::fs::init 未生效");
        };
        assert_eq!(fs.name(), "unkfs", "unkfs name mismatch");
        // 注: fs_format 行为不在单测覆盖 (内存模式调 format_drive 有底层 IO 副作用),
        // 语义等价性由 fsformat 路径代码搬移保证, QEMU boot 覆盖挂载分发链路
    }
}
