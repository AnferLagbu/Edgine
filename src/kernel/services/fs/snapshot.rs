#![deny(unsafe_code)]
//! 快照 (snapshot) 系统调用处理器
//!
//! 提供 snapshot_create/snapshot_destroy/snapshot_rollback/snapshot_clone 系统调用的实现。
//! 调用 UNKFS 层的快照方法。

use crate::services::syscall::types::Errno;

/// 创建快照
///
/// # Errors
/// 当 `name_ptr` 为空时返回 `EFAULT`; 其余错误 (如重名、底层失败等) 以对应的 `Errno` 返回.
pub fn snapshot_create_syscall(name_ptr: u64) -> Result<usize, Errno> {
    if name_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    // 通过 framework 层获取快照名称
    let name = crate::services::fs::api::snapshot_get_name(name_ptr);
    let result = crate::services::fs::unkfs::unkfs::get_unkfs().snapshot_create(&name);

    if result >= 0 {
        Ok(result as usize)
    } else {
        Err(Errno::from_ret(i64::from(result)))
    }
}

/// 销毁快照
///
/// # Errors
/// 当快照不存在或底层销毁失败时以对应的 `Errno` 返回.
pub fn snapshot_destroy_syscall(snap_id: u64) -> Result<usize, Errno> {
    let result = crate::services::fs::unkfs::unkfs::get_unkfs().snapshot_destroy(snap_id);

    if result >= 0 {
        Ok(result as usize)
    } else {
        Err(Errno::from_ret(i64::from(result)))
    }
}

/// 回滚快照
///
/// # Errors
/// 当快照不存在或底层回滚失败时以对应的 `Errno` 返回.
pub fn snapshot_rollback_syscall(snap_id: u64) -> Result<usize, Errno> {
    let result = crate::services::fs::unkfs::unkfs::get_unkfs().snapshot_rollback(snap_id);

    if result >= 0 {
        Ok(result as usize)
    } else {
        Err(Errno::from_ret(i64::from(result)))
    }
}

/// 从快照创建克隆
///
/// # Errors
/// 当 `name_ptr` 为空时返回 `EFAULT`; 其余错误 (如克隆名冲突、底层失败等) 以对应的 `Errno` 返回.
pub fn snapshot_clone_syscall(snap_id: u64, name_ptr: u64) -> Result<usize, Errno> {
    if name_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    // 通过 framework 层获取克隆名称
    let name = crate::services::fs::api::snapshot_get_name(name_ptr);
    let result = crate::services::fs::unkfs::unkfs::get_unkfs().clone_create(snap_id, &name);

    if result >= 0 {
        Ok(result as usize)
    } else {
        Err(Errno::from_ret(i64::from(result)))
    }
}
