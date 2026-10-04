#![deny(unsafe_code)]
//! 文件句柄系统 — name_to_handle_at / open_by_handle_at
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 functions::fs::api。
//!
//! ## 职责
//!
//! - 实现 Linux 风格的文件句柄导出/导入
//! - 支持文件描述符跨进程传递
//! - 支持文件系统快照/备份
//!
//! ## 参考
//!
//! - Linux name_to_handle_at(2) 手册页
//! - Linux open_by_handle_at(2) 手册页

use crate::functions::fs::inode::Inode;
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_manager::VFS_MANAGER;
use crate::functions::fs::vfs_types::{OpenFile, VFS_MAX_PATH};
use crate::privileged::mm::copy_user::{copy_from_user, copy_to_user};
use crate::privileged::syscall::Errno;
use alloc::sync::Arc;

/// 文件句柄类型 (与 Linux 兼容)
pub const FILE_HANDLE_GHEST_ID: i32 = 0x01; // 通用句柄类型

/// 文件句柄大小
pub const FILE_HANDLE_SIZE: usize = 144; // 8 + 128 + 8 (对齐)

/// 句柄布局:
/// [0..4]  `inode_id` (u32 LE)
/// [4..8]  `mount_idx` (u32 LE)
/// `[8]`     `handle_type` (u8)
/// [9..12] reserved
/// [12..16] `handle_bytes` (u32 LE)
const HANDLE_INODE_OFF: usize = 0;
const HANDLE_MOUNT_OFF: usize = 4;
const HANDLE_TYPE_OFF: usize = 8;
const HANDLE_HBYTES_OFF: usize = 12;
const HANDLE_SERIALIZED_SIZE: usize = 16;

#[expect(
    clippy::items_after_statements,
    reason = "items_after_statements: item 紧邻使用点声明便于阅读上下文; 当前优先 expect"
)]
/// `name_to_handle_at` — 导出文件句柄
///
/// 将文件路径导出为可序列化的文件句柄, 用于跨进程传递或持久化.
///
/// # 参数
/// - `dirfd`: 目录文件描述符 (`AT_FDCWD` = -100)
/// - `path`: 文件路径
/// - `handle_type`: 句柄类型 (仅支持 `FILE_HANDLE_GHEST_ID`)
/// - `handle_buf`: 用户空间缓冲区 (输出)
/// - `mnt_id`: 挂载点 ID (输出)
/// - `flags`: 标志位 (`AT_EMPTY_PATH` = 0x1000)
///
/// # Errors
/// 当 `handle_buf` 为空或写入用户空间失败时返回 `EFAULT`;
/// 当 `handle_type` 不是 `FILE_HANDLE_GHEST_ID` 时返回 `ENOTSUP`;
/// 当路径为空、挂载解析失败或文件不存在时返回 `ENOENT`.
pub fn name_to_handle_at_syscall(
    _dirfd: i32,
    path_ptr: u64,
    handle_type: i32,
    handle_buf: u64,
    mnt_id: u64,
    _flags: u32,
) -> Result<i64, Errno> {
    if handle_buf == 0 {
        return Err(Errno::EFAULT);
    }
    if handle_type != FILE_HANDLE_GHEST_ID {
        return Err(Errno::ENOTSUP);
    }

    // 通过 VFS 解析路径, 获取 inode_id 和 mount_idx
    use crate::privileged::lib::CStrExt;
    let path_ptr_raw = path_ptr as *const u8;
    let path = path_ptr_raw.as_kstr();
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }

    // 用户路径归一化 (chroot / pivot_root 根语义): 视图路径 + 根前缀
    let mut pbuf = [0u8; VFS_MAX_PATH];
    let real_path = VFS_MANAGER
        .resolve_user_path(path, &mut pbuf)
        .ok_or(Errno::ENOENT)?;

    let (mount_idx, _fs_type, fs_opt) = VFS_MANAGER
        .resolve_mount_fs(real_path)
        .ok_or(Errno::ENOENT)?;
    let fs = fs_opt.ok_or(Errno::ENOENT)?;
    let rel_path = VFS_MANAGER.get_relative_path(real_path, mount_idx);

    // 通过 FileSystem trait 获取 inode_id
    let inode_id = fs.fs_resolve_path(rel_path).ok_or(Errno::ENOENT)?;

    // 构建句柄数据
    let mut handle_data = [0u8; FILE_HANDLE_SIZE];
    handle_data[HANDLE_INODE_OFF..HANDLE_INODE_OFF + 4].copy_from_slice(&inode_id.to_le_bytes());
    handle_data[HANDLE_MOUNT_OFF..HANDLE_MOUNT_OFF + 4]
        .copy_from_slice(&(mount_idx as u32).to_le_bytes());
    handle_data[HANDLE_TYPE_OFF] = FILE_HANDLE_GHEST_ID as u8;
    handle_data[HANDLE_HBYTES_OFF..HANDLE_HBYTES_OFF + 4].copy_from_slice(&8u32.to_le_bytes());

    // 写入用户空间
    copy_to_user(
        handle_buf,
        &handle_data[..HANDLE_SERIALIZED_SIZE],
        HANDLE_SERIALIZED_SIZE,
    )
    .map_err(|()| Errno::EFAULT)?;

    // 写入 mnt_id
    if mnt_id != 0 {
        let mnt_data = (mount_idx as u32).to_le_bytes();
        copy_to_user(mnt_id, &mnt_data, 4).map_err(|()| Errno::EFAULT)?;
    }

    Ok(0)
}

/// `open_by_handle_at` — 通过句柄打开文件
///
/// 使用之前导出的文件句柄打开文件.
///
/// # 参数
/// - `mount_fd`: 挂载点文件描述符
/// - `handle_ptr`: 用户空间句柄缓冲区
/// - `handle_type`: 句柄类型
/// - `flags`: 打开标志 (`O_RDONLY`, `O_WRONLY` 等)
///
/// # Errors
/// 当 `handle_ptr` 为空或从用户空间读取句柄失败时返回 `EFAULT`;
/// 当 `handle_type` 非法时返回 `ENOTSUP`;
/// 当句柄数据无效、挂载索引无效时返回 `EINVAL`;
/// 当打开文件表已满时返回 `ENOMEM`; 当 fd 分配失败时返回 `EMFILE`.
pub fn open_by_handle_at_syscall(
    _mount_fd: i32,
    handle_ptr: u64,
    handle_type: i32,
    flags: u32,
) -> Result<i64, Errno> {
    if handle_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if handle_type != FILE_HANDLE_GHEST_ID {
        return Err(Errno::ENOTSUP);
    }

    // 权限检查: open_by_handle_at 按句柄绕过路径权限打开任意 inode, 属特权操作
    // B06-03: 采用 SYSTEM 域 CAP_SYS_ADMIN (0x01), 与 mount/umount2 先例一致 (functions/fs/mount.rs:52)
    let pwm = crate::privileged::sgeg::session::get_current_pwm();
    if !crate::privileged::sgeg::api::pwm_has_capability(pwm, 0, 0x01) {
        return Err(Errno::EPERM);
    }

    // 从用户空间读取句柄
    let mut handle_data = [0u8; HANDLE_SERIALIZED_SIZE];
    copy_from_user(&mut handle_data, handle_ptr, HANDLE_SERIALIZED_SIZE)
        .map_err(|()| Errno::EFAULT)?;

    // 提取 inode_id 和 mount_idx
    let inode_id = u32::from_le_bytes(
        handle_data[HANDLE_INODE_OFF..HANDLE_INODE_OFF + 4]
            .try_into()
            .map_err(|_| Errno::EINVAL)?,
    );
    let mount_idx = u32::from_le_bytes(
        handle_data[HANDLE_MOUNT_OFF..HANDLE_MOUNT_OFF + 4]
            .try_into()
            .map_err(|_| Errno::EINVAL)?,
    );
    let handle_type_in = i32::from(handle_data[HANDLE_TYPE_OFF]);

    if handle_type_in != FILE_HANDLE_GHEST_ID {
        return Err(Errno::EINVAL);
    }

    // 验证 mount_idx 有效并获取 FileSystem trait object
    let fs = {
        let mounts = VFS_MANAGER.mounts.lock();
        if (mount_idx as usize) >= mounts.len() || !mounts[mount_idx as usize].used {
            return Err(Errno::EINVAL);
        }
        mounts[mount_idx as usize].get_fs().ok_or(Errno::EINVAL)?
    };

    // 通过 FileSystem trait 构造原生 Inode
    // fs_resolve_inode 返回原生 Arc<dyn Inode>; 句柄无法解析 (inode_id/mount_idx 失效)
    // 时返回 EINVAL, 与相邻 mount_idx 校验一致。
    let pwm = crate::privileged::sgeg::session::get_current_pwm();
    let inode: Arc<dyn Inode> = fs
        .fs_resolve_inode(inode_id, mount_idx)
        .ok_or(Errno::EINVAL)?;

    // 通过 stat 获取 file_type (避免硬编码)
    let file_type = inode.stat(pwm).map_or(0, |s| s.file_type);
    let open_file = OpenFile::new(inode, flags, pwm, file_type);

    // 插入全局 OpenFile 表
    let handle_id = OPEN_FILE_TABLE.alloc(open_file).ok_or(Errno::ENOMEM)?;

    // 分配 fd (写入当前进程 per-process fd 表)
    let Some(fd_idx) =
        crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, false)).flatten()
    else {
        // fd 表满或当前进程不可用: 回收已分配的 OpenFile handle, 避免泄漏
        OPEN_FILE_TABLE.close(handle_id);
        return Err(Errno::EMFILE);
    };

    Ok(fd_idx as i64)
}
