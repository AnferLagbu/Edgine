//! VFS 挂载 / 生命周期 / freg / 同步 / 格式化
//!
//! 归属: 挂载相关内部接口 (初始化/挂载/卸载/freg/同步/格式化),
//! 以及 RamFS 挂载去重标志 `RAMFS_MOUNTED`. 本模块由 `api.rs` 经
//! `pub use vfs_mount::*;` 汇聚, 保持调用路径不变.

use super::api::ptr_to_str;
use super::backend_trait::{current_fs_backend, unkfs_fs};
use super::vfs_manager::VFS_MANAGER;
use super::vfs_types::{FileSystem, FsType, IntoI32, KernelError, VFS_MAX_MOUNTS};
use crate::services::fs::devfs::DEVFS_DATA;
use crate::services::fs::ramfs_core::{RAMFS_DATA, ramfs_fs};

static RAMFS_MOUNTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// ============================================================================
// VFS 核心接口 (内部)
// ============================================================================

pub extern "C" fn vfs_init_internal() {
    super::vfs_manager::init();
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
pub extern "C" fn vfs_mount_internal(path: *const u8, fs_name: *const u8) -> i32 {
    let path = ptr_to_str(path);
    let fs_name = ptr_to_str(fs_name);
    let fs_type = FsType::from_name(fs_name);

    match fs_type {
        FsType::RamFs => {
            if !RAMFS_MOUNTED.swap(true, core::sync::atomic::Ordering::SeqCst) {
                {
                    let mut ramfs = RAMFS_DATA.lock();
                    if ramfs.mount(path) != 0 {
                        return KernelError::Io.as_i32();
                    }
                } // 显式 drop ramfs 释放锁
            }
        }
        FsType::Unkfs => {
            // UNKFS 初始化经注册的 FileSystem trait (fs_init 内含 is_initialized 检查);
            // 未注册 (services::fs::init 之前) 时静默跳过, 挂载在下方 unkfs_fs() 处 fail-closed
            if let Some(fs) = unkfs_fs() {
                let _ = fs.fs_init();
            }
        }
        FsType::DevFs => {
            // DevFS 初始化由 init()/init_with_egdf_bridge() 完成
        }
        FsType::Ext2 => {
            // ext2 挂载由 Ext2FileSystem::fs_mount 处理
        }
        FsType::ExFat => {
            // exfat 挂载由 ExfatFileSystem::fs_mount 处理
        }
        FsType::TmpFs | FsType::OverlayFs => {
            // tmpfs/overlayfs 挂载策略归 services: 经注册表解析 FileSystem trait object
            // 并调用其 fs_mount 完成挂载; 未注册 (services::fs::init 之前) 时 fail-closed.
            match current_fs_backend().resolve_fs(fs_name) {
                Some(fs) => {
                    if fs.fs_mount(path).is_err() {
                        return KernelError::Io.as_i32();
                    }
                }
                None => return KernelError::NotInitialized.as_i32(),
            }
        }

        FsType::Unknown => return KernelError::NotSupported.as_i32(),
    }

    // 带 trait object 挂载: 各类型经注册表/全局实例解析 FileSystem trait object
    let fs: &'static dyn FileSystem = match fs_type {
        FsType::RamFs => ramfs_fs(),
        FsType::Unkfs => match unkfs_fs() {
            Some(fs) => fs,
            // fail-closed: services::fs::init 注册前 UNKFS 不可挂载
            None => return KernelError::NotInitialized.as_i32(),
        },
        FsType::DevFs => &DEVFS_DATA,
        FsType::TmpFs | FsType::OverlayFs => match current_fs_backend().resolve_fs(fs_name) {
            Some(fs) => fs,
            // fail-closed: services::fs::init 注册前不可挂载
            None => return KernelError::NotInitialized.as_i32(),
        },
        _ => return VFS_MANAGER.mount(path, fs_name).as_i32(),
    };
    VFS_MANAGER.mount_with_fs(path, fs_name, fs).as_i32()
}

pub extern "C" fn vfs_unmount_internal(path: *const u8) -> i32 {
    let path = ptr_to_str(path);
    VFS_MANAGER.unmount(path).as_i32()
}

// I-22: 15 个 `unkfs_*_internal` 函数无调用方, 已随 P3-I-18 迁移至 `vfs_sync` (FileSystem
// trait fs_sync 分发) 后彻底废弃. 旧路径仅 C-FFI 兼容, 无 FFI 调用方, 移除以减小 TCB
// 面积 (约 150 行, 含 unsafe).

// ============================================================================
// FREG 接口
// ============================================================================

pub extern "C" fn vfs_freg_capture() {
    VFS_MANAGER.capture_snapshot();
}

pub extern "C" fn vfs_freg_restore() -> i32 {
    VFS_MANAGER.restore_from_snapshot();
    1
}

// ============================================================================
// 公共 VFS API
// ============================================================================

pub extern "C" fn vfs_init() {
    vfs_init_internal();
}

pub extern "C" fn vfs_mount(path: *const u8, fs_name: *const u8) -> i32 {
    vfs_mount_internal(path, fs_name)
}

/// T-05: safe 挂载接口 — services 层策略调用
///
/// 接受 Rust 字符串切片, 返回 i32 错误码 (0=成功, 负数=errno).
pub fn vfs_mount_safe(path: &str, fs_name: &str) -> i32 {
    // 构造 null 终止的 C 字符串
    let mut path_buf = alloc::vec::Vec::with_capacity(path.len() + 1);
    path_buf.extend_from_slice(path.as_bytes());
    path_buf.push(0);
    let mut fs_buf = alloc::vec::Vec::with_capacity(fs_name.len() + 1);
    fs_buf.extend_from_slice(fs_name.as_bytes());
    fs_buf.push(0);
    vfs_mount_internal(path_buf.as_ptr(), fs_buf.as_ptr())
}

pub extern "C" fn vfs_umount_internal(path: *const u8, _flags: i32) -> i32 {
    if path.is_null() {
        return -22; // -EINVAL
    }
    let path = ptr_to_str(path);
    match VFS_MANAGER.unmount(path) {
        Ok(()) => 0,
        Err(_) => -2, // -ENOENT
    }
}

pub extern "C" fn vfs_umount(path: *const u8, flags: i32) -> i32 {
    vfs_umount_internal(path, flags)
}

// 注意: 保持 Rust ABI — vfs_sync 为内核内部调用 (fs_sync trait 分发),
//        P3-I-18 契约测试按 Rust ABI 签名匹配该函数.
pub fn vfs_sync() -> i32 {
    // P3-I-18: 遍历所有挂载点, 通过 FileSystem trait 的 fs_sync 分发.
    // 替换原 unkfs_sync_internal() 单 FS 写死的实现.
    let mounts = VFS_MANAGER.mounts.lock();
    let mut last_err: i32 = 0;
    let mut synced: u32 = 0;
    for i in 0..VFS_MAX_MOUNTS {
        let m = &mounts[i];
        if !m.used {
            continue;
        }
        if let Some(fs) = m.get_fs() {
            // SAFETY: 见本函数旧实现, fs_sync 不引用 raw pointer, 是
            // 内部纯粹计算 (UNKFS 走 txg commit). 互斥由 VFS_MANAGER 维护.
            match fs.fs_sync() {
                Ok(()) => {
                    synced += 1;
                }
                Err(e) => {
                    last_err = e.as_i32();
                    // 继续遍历其它挂载点, 不因单个 FS 失败而中断
                }
            }
        }
    }
    if synced == 0 {
        // 没有任何挂载点: 仍然返回 0 保持兼容性 (老代码语义)
        // 业务上 mount 0 个 FS 时 vfs_sync 是 no-op
    }
    last_err
}

pub extern "C" fn vfs_format_internal(path: *const u8, fs_type: *const u8) -> i32 {
    let fs_type_str = ptr_to_str(fs_type);
    let _path = ptr_to_str(path);

    if fs_type_str.is_empty() {
        return -1;
    }

    // Parse filesystem type
    if fs_type_str == "unkfs" || fs_type_str == "UNKFS" {
        // DECISION-K 项 6: 格式化策略经 FileSystem::fs_format 分发 (services 实装),
        // framework 不再直接访问 UNKFS 内部字段
        match unkfs_fs() {
            Some(fs) => {
                if fs.fs_format().is_ok() {
                    return 0;
                }
                return -1;
            }
            None => return -1,
        }
    } else if fs_type_str == "ramfs" || fs_type_str == "RamFS" {
        // RamFS 无需格式化, 始终为内存文件系统
        return 0;
    }

    -1
}
