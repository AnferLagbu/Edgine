//! VFS 挂载 / 生命周期 / freg / 同步 / 格式化
//!
//! 归属: 挂载相关内部接口 (初始化/挂载/卸载/freg/同步/格式化),
//! 以及 RamFS 挂载去重标志 `RAMFS_MOUNTED`. 本模块由 `api.rs` 经
//! `pub use vfs_mount::*;` 汇聚, 保持调用路径不变.

#![deny(missing_docs)]

use super::api::ptr_to_str;
use super::backend_trait::{current_fs_backend, egfs_fs};
use super::vfs_manager::VFS_MANAGER;
use super::vfs_types::{FileSystem, FsType, IntoI32, KernelError, VFS_MAX_MOUNTS};
use crate::functions::fs::devfs::DEVFS_DATA;
use crate::functions::fs::ramfs_core::{RAMFS_DATA, ramfs_fs};

static RAMFS_MOUNTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// ============================================================================
// VFS 核心接口 (内部)
// ============================================================================

/// C ABI 内部挂载入口: 按文件系统名分发挂载, 返回 0 或负 errno
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
        FsType::Egfs => {
            // EGFS 初始化经注册的 FileSystem trait (fs_init 内含 is_initialized 检查);
            // 未注册 (functions::fs::init 之前) 时静默跳过, 挂载在下方 egfs_fs() 处 fail-closed
            if let Some(fs) = egfs_fs() {
                let _ = fs.fs_init();
            }
        }
        // DevFS 初始化由 init()/init_with_egdf_bridge() 完成;
        // ext2 / exfat 挂载分别由 Ext2FileSystem / ExfatFileSystem::fs_mount 处理
        FsType::DevFs | FsType::Ext2 | FsType::ExFat => {}
        FsType::TmpFs | FsType::OverlayFs => {
            // tmpfs/overlayfs 挂载策略归 functions: 经注册表解析 FileSystem trait object
            // 并调用其 fs_mount 完成挂载; 未注册 (functions::fs::init 之前) 时 fail-closed.
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
        FsType::Egfs => match egfs_fs() {
            Some(fs) => fs,
            // fail-closed: functions::fs::init 注册前 EGFS 不可挂载
            None => return KernelError::NotInitialized.as_i32(),
        },
        FsType::DevFs => &DEVFS_DATA,
        FsType::TmpFs | FsType::OverlayFs => match current_fs_backend().resolve_fs(fs_name) {
            Some(fs) => fs,
            // fail-closed: functions::fs::init 注册前不可挂载
            None => return KernelError::NotInitialized.as_i32(),
        },
        _ => return VFS_MANAGER.mount(path, fs_name).as_i32(),
    };
    VFS_MANAGER.mount_with_fs(path, fs_name, fs).as_i32()
}

// I-22: 15 个 `egfs_*_internal` 函数无调用方, 已随 P3-I-18 迁移至 `vfs_sync` (FileSystem
// trait fs_sync 分发) 后彻底废弃. 旧路径仅 C-FFI 兼容, 无 FFI 调用方, 移除以减小 TCB
// 面积 (约 150 行, 含 unsafe).
//
// 同族收尾 (本轮经用户裁定): 本模块曾另挂 6 个同形态 `pub extern "C"` 包裹
// (`vfs_init` / `vfs_init_internal` / `vfs_unmount_internal` / `vfs_freg_capture` /
// `vfs_freg_restore` / `vfs_format_internal`) —— 均无 `#[no_mangle]` ⇒ 结构上不可能
// 被 C 侧调用, 且全仓零引用 (分组登记见
// `docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-13 预存组 (二)). 真实入口各自另存:
// VFS 初始化由 `src/kernel/lib.rs` 编排直连 `vfs_manager::init()`, FREG 快照/恢复
// 经 `vfs_manager.rs` 自持的 `vfs_freg_capture_cb` / `vfs_freg_restore_cb` 注册,
// 卸载走 `VFS_MANAGER.unmount`, 格式化经 `FileSystem::fs_format` trait 分发.

// ============================================================================
// 公共 VFS API
// ============================================================================

/// C ABI 导出: 挂载文件系统 (path / `fs_name` 为 null 终止的 C 字符串)
#[must_use]
pub extern "C" fn vfs_mount(path: *const u8, fs_name: *const u8) -> i32 {
    vfs_mount_internal(path, fs_name)
}

/// T-05: safe 挂载接口 — functions 层策略调用
///
/// 接受 Rust 字符串切片, 返回 i32 错误码 (0=成功, 负数=errno).
#[must_use]
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

/// C ABI 内部卸载入口: 卸载指定挂载点, 返回 0 或负 errno
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

/// C ABI 导出: 卸载文件系统 (path 为 null 终止的 C 字符串)
#[must_use]
pub extern "C" fn vfs_umount(path: *const u8, flags: i32) -> i32 {
    vfs_umount_internal(path, flags)
}

// 注意: 保持 Rust ABI — vfs_sync 为内核内部调用 (fs_sync trait 分发),
//        P3-I-18 契约测试按 Rust ABI 签名匹配该函数.
/// 遍历所有挂载点并同步各文件系统, 返回 0 或最近一次负 errno
pub fn vfs_sync() -> i32 {
    // P3-I-18: 遍历所有挂载点, 通过 FileSystem trait 的 fs_sync 分发.
    // 替换原 egfs_sync_internal() 单 FS 写死的实现.
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
            // 内部纯粹计算 (EGFS 走 txg commit). 互斥由 VFS_MANAGER 维护.
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

// `vfs_format_internal` 已删除 (同族 C-FFI 残壳, 见上方注释块): 格式化能力
// 的唯一入口是 `FileSystem::fs_format` trait 分发 (DECISION-K 项 6), 本函数的
// egfs / ramfs 分支在 trait 侧已有等价实装.
