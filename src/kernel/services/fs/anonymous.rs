#![deny(unsafe_code)]
//! 匿名文件系统 — memfd 基础
//!
//! 提供无路径的内存文件, 用于:
//! - memfd_create: 匿名内存文件
//! - 进程间共享内存
//! - 临时文件 (不依赖 tmpfs)

use crate::framework::sync::IrqSpinLock;
use crate::services::error::KernelError;
use crate::services::fs::ramfs_core::RamFsData;
use crate::services::fs::vfs_types::KernelResult;

/// 匿名文件系统
///
/// 复用 `RamFsData` 作为数据存储, 但不依赖路径.
/// 直接通过 `inode_id` 访问数据.
pub struct AnonymousFs {
    inner: IrqSpinLock<RamFsData>,
}

impl AnonymousFs {
    /// 创建新的 `AnonymousFs`
    pub const fn new() -> Self {
        Self {
            inner: IrqSpinLock::new(RamFsData::new()),
        }
    }

    /// 分配新的 inode (无需路径)
    pub fn alloc_inode(&self) -> Option<u32> {
        let mut inner = self.inner.lock();
        // 惰性堆置各表 (匿名 fs 不经 mount, 首次访问时确保表已就绪)
        inner.ensure_allocated();
        // 分配类型为 File (0), PWM 为 0 (无权限检查)
        inner.alloc_node(0, 0)
    }

    /// 读取 inode 数据
    ///
    /// # Errors
    /// 当 inode 不存在或权限不足时返回 `KernelError` (透传底层 ramfs 错误码).
    pub fn read_at(&self, node_id: u32, offset: u64, buf: &mut [u8]) -> KernelResult<usize> {
        let mut inner = self.inner.lock();
        inner.ensure_allocated();
        let mut offset = offset;
        let result = inner.read(node_id, &mut offset, buf, 0);
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(KernelError::from_i32(result))
        }
    }

    /// 写入 inode 数据
    ///
    /// # Errors
    /// 当 inode 不存在或权限不足时返回 `KernelError` (透传底层 ramfs 错误码).
    pub fn write_at(&self, node_id: u32, offset: u64, buf: &[u8]) -> KernelResult<usize> {
        let mut inner = self.inner.lock();
        inner.ensure_allocated();
        let mut offset = offset;
        let result = inner.write(node_id, &mut offset, buf, 0);
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(KernelError::from_i32(result))
        }
    }

    /// 获取 inode 大小
    pub fn get_size(&self, node_id: u32) -> Option<u32> {
        let mut inner = self.inner.lock();
        inner.ensure_allocated();
        inner.get_file_size(node_id)
    }

    /// 截断 inode
    pub fn truncate(&self, node_id: u32, new_size: u64) -> bool {
        let mut inner = self.inner.lock();
        inner.ensure_allocated();
        inner.truncate(node_id, new_size, 0) >= 0
    }
}

/// 匿名文件系统全局实例
pub static ANONYMOUS_FS: AnonymousFs = AnonymousFs::new();
