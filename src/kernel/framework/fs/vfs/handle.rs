//! VFS 句柄 (fd) 操作 — 从 `api.rs` 拆出的物理子模块
//!
//! 归属: fd 句柄相关 `#[no_mangle] pub extern "C"` 函数 (open/close/read/
//! write/seek/readdir/truncate/fstat/fchmod/fchown/dup/dup2) 及其 safe 包装
//! 与 fd 表操作. `api.rs` 通过 `pub use handle::*;` 保持对外符号名与调用
//! 路径不变 (`#[no_mangle]` 全局符号不受模块位置影响).

use super::api::{
    PCACHE_FAST_MAX_BYTES, PCACHE_FAST_MIN_BYTES, ptr_to_str, split_parent_name, with_cstr,
};
use super::open_file_table::OPEN_FILE_TABLE;
use super::types::{
    KernelError, OpenFile, VFS_MAX_FDS, VFS_MAX_PATH, VfsDirEntry, VfsOpenFlags, VfsSeekWhence,
    VfsStat,
};
use super::vfs::VFS_MANAGER;
use crate::framework::fd_notify;
use crate::framework::mm::{PAGE_SIZE, pcache};
use crate::framework::userptr::{UserReadPtr, UserRefMut, UserWritePtr};

// ============================================================================
// VFS 核心接口 (内部)
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_open_internal(path: *const u8, flags: u32, pwm: u64) -> i32 {
    let path = ptr_to_str(path);
    let mut pbuf = [0u8; VFS_MAX_PATH];
    let Some(path) = VFS_MANAGER.resolve_user_path(path, &mut pbuf) else {
        return -1;
    };

    let (mount_idx, _fs_type, fs_opt) = match VFS_MANAGER.resolve_mount_fs(path) {
        Some(r) => r,
        None => return -1,
    };
    let rel_path = VFS_MANAGER.get_relative_path(path, mount_idx);

    // E6-4: trait object 分发 (优先于 fs_type match)
    if let Some(fs) = fs_opt {
        match fs.fs_open(rel_path, flags, pwm) {
            Ok(inode) => {
                // Plan B: fs_open 直接返回 Inode trait object
                let node_id = inode.node_id();
                let file_type = inode.stat(pwm).map_or(0, |s| s.file_type);
                let open_file = OpenFile::new(inode, flags, pwm, file_type);

                // 插入全局 OpenFile 表
                let handle_id = match OPEN_FILE_TABLE.alloc(open_file) {
                    Some(id) => id,
                    None => return -1,
                };

                // 在进程 fd 表中分配 fd (per-process fd 表待实现, 登记分册 9 B09-10)
                // 当前简化: 使用全局 fd 索引
                let fd_idx = if let Some(i) = VFS_MANAGER.alloc_fd() {
                    i
                } else {
                    OPEN_FILE_TABLE.close(handle_id);
                    return -1;
                };

                // 存储 handle_id 到 fd 表
                VFS_MANAGER.set_fd_handle(fd_idx, handle_id);
                // T5 甲批 C-1: 填充 fd 表元数据。缺此半则 `get_fd_info`
                // (flock 的 ino 识别 / mmap-by-fd 的 fd_to_inode_id) 恒得 0,
                // `get_fd_mount_idx` 因 path 为空而反查失败, close 时
                // pcache 失效也按 node 0 执行。
                VFS_MANAGER.set_fd(fd_idx, node_id, 0, flags, pwm, file_type, path);

                fd_idx as i32
            }
            Err(KernelError::FileNotFound) if (flags & VfsOpenFlags::CREAT.bits()) != 0 => {
                // CREAT: 文件不存在, 尝试创建
                let (parent_path, name) = split_parent_name(rel_path);
                match fs.fs_create(parent_path, name, pwm) {
                    Ok(inode) => {
                        let file_type = inode.stat(pwm).map_or(0, |s| s.file_type);
                        let inode_id = inode.node_id();
                        let open_file = OpenFile::new(inode, flags, pwm, file_type);

                        let handle_id = match OPEN_FILE_TABLE.alloc(open_file) {
                            Some(id) => id,
                            None => return -1,
                        };

                        let fd_idx = if let Some(i) = VFS_MANAGER.alloc_fd() {
                            i
                        } else {
                            OPEN_FILE_TABLE.close(handle_id);
                            return -1;
                        };

                        VFS_MANAGER.set_fd_handle(fd_idx, handle_id);
                        // T5 甲批 C-1: 同 fs_open 分支, 填充 fd 表元数据
                        VFS_MANAGER.set_fd(fd_idx, inode_id, 0, flags, pwm, file_type, path);

                        // inotify: 父目录 IN_CREATE + 新文件 IN_OPEN
                        let parent_ino = fs.fs_resolve_path(parent_path).unwrap_or(0);
                        super::inotify::inotify_notify(
                            parent_ino,
                            super::inotify::IN_CREATE,
                            name,
                            false,
                        );
                        super::inotify::inotify_notify(
                            inode_id,
                            super::inotify::IN_OPEN,
                            "",
                            false,
                        );
                        fd_idx as i32
                    }
                    Err(_) => -1,
                }
            }
            Err(e) => e.as_i32(),
        }
    } else {
        // E6-5: fallback 已移除, 所有文件系统均通过 trait object 分发
        KernelError::NotSupported.as_i32()
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
// 注意: 保持 Rust ABI — vfs_close_internal 为内核内部调用 (fd 表原子回收),
//        TD-03 契约测试按 Rust ABI 签名匹配该函数体.
#[unsafe(no_mangle)]
#[expect(clippy::no_mangle_with_rust_abi)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn vfs_close_internal(fd_idx: u32) -> i32 {
    let fd_idx_us = fd_idx as usize;
    if fd_idx_us >= VFS_MAX_FDS {
        return -1;
    }
    // TD-03: 原子 claim-and-clear — 同一把锁内同时快照 node_id/flags/handle_id 并清 used,
    // 避免双核同时 close 同一 fd 导致 pcache/inotify 二次触发.
    let snapshot = {
        let mut fd_table = VFS_MANAGER.fd_table.lock();
        if fd_table[fd_idx_us].used {
            let snap = (
                fd_table[fd_idx_us].node_id,
                fd_table[fd_idx_us].flags,
                fd_table[fd_idx_us].handle_id,
            );
            // 在锁内清零 used 标志 — 后续 alloc 不会复用, 杜绝双 close 穿透
            fd_table[fd_idx_us].used = false;
            fd_table[fd_idx_us].fd = 0;
            fd_table[fd_idx_us].node_id = 0;
            fd_table[fd_idx_us].offset = 0;
            fd_table[fd_idx_us].handle_id = u32::MAX;
            fd_table[fd_idx_us].cloexec = false;
            Some(snap)
        } else {
            None // 已关闭或未使用, 直接返回 0
        }
    };
    let (node_id, flags, handle_id) = match snapshot {
        Some(s) => s,
        None => return 0,
    };

    // 减少 OpenFile 引用计数 (POSIX dup 语义)
    if handle_id != u32::MAX {
        OPEN_FILE_TABLE.close(handle_id);
    }

    // B2: 释放该 fd 关联 inode 的全部 pcache 缓存页, 避免内存泄漏
    pcache::pcache_invalidate_inode(node_id);
    // inotify: 文件关闭通知
    let close_mask =
        if (flags & VfsOpenFlags::WRONLY.bits()) != 0 || (flags & VfsOpenFlags::RDWR.bits()) != 0 {
            super::inotify::IN_CLOSE_WRITE
        } else {
            super::inotify::IN_CLOSE_NOWRITE
        };
    super::inotify::inotify_notify(node_id, close_mask, "", false);
    // C1: fd 关闭 → 唤醒该 fd 注册的所有 epoll 等待者 (EPOLLHUP|EPOLLERR)
    fd_notify::notify_fd_close(fd_idx as i32);
    0
}

/// 关闭全部标记 CLOEXEC 的 fd — execve 成功路径调用 (POSIX close-on-exec)。
///
/// 带 `FD_CLOEXEC` 的 fd 在 exec 成功后关闭, 其余保留。
/// 先收集 CLOEXEC fd 索引再逐个 `vfs_close_internal`, 避免持 `fd_table`
/// 锁递归 (vfs_close_internal 内部会再次获取同一把锁 → 自锁死)。
pub fn vfs_close_cloexec_fds() {
    let targets: alloc::vec::Vec<u32> = {
        let fd_table = VFS_MANAGER.fd_table.lock();
        fd_table
            .iter()
            .enumerate()
            .filter(|(_, f)| f.used && f.cloexec)
            .map(|(i, _)| i as u32)
            .collect()
    };
    for fd in targets {
        vfs_close_internal(fd);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 用户内存代理, 指针/长度上下文保证
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_read_internal(fd_idx: u32, buf: *mut u8, count: u32) -> i32 {
    if buf.is_null() || count == 0 {
        return -1;
    }

    // Plan B: 通过 OpenFile 的 Inode trait 执行 I/O
    // 获取 handle_id
    let handle_id = match VFS_MANAGER.get_fd_handle(fd_idx as usize) {
        Some(hid) => hid,
        None => return -1,
    };

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let mut user_buf = unsafe { UserWritePtr::new(buf, count as usize) };

    // 通过 OpenFile 获取 offset 和 Inode
    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        let offset = open_file.get_offset();
        let pwm = open_file.pwm;

        // B2: pcache 快路径 (仅 node_id 已知时, 通过兼容方法获取)
        let node_id = open_file.inode_id();
        // 检查是否是 4KB 对齐读取 (pcache 快路径)
        let is_aligned_4k = u64::from(count) >= PCACHE_FAST_MIN_BYTES as u64
            && u64::from(count) <= PCACHE_FAST_MAX_BYTES as u64
            && u64::from(count).is_multiple_of(PAGE_SIZE)
            && offset.is_multiple_of(PAGE_SIZE);

        if is_aligned_4k {
            let npages = (u64::from(count) / PAGE_SIZE) as usize;
            let first_pi = offset / PAGE_SIZE;

            let mut all_hit = true;
            for i in 0..npages {
                if pcache::pcache_lookup(node_id, first_pi + i as u64).is_none() {
                    all_hit = false;
                    break;
                }
            }

            if all_hit {
                let mut all_ok = true;
                for i in 0..npages {
                    // SAFETY: 4KB 对齐保证 buf.add(i*PAGE_SIZE) 落在 [buf, buf+count) 内
                    let dst = unsafe {
                        core::slice::from_raw_parts_mut(
                            buf.add(i * PAGE_SIZE as usize),
                            PAGE_SIZE as usize,
                        )
                    };
                    if !pcache::pcache_read_to_slice(node_id, first_pi + i as u64, dst) {
                        all_ok = false;
                        break;
                    }
                }
                if all_ok {
                    return count as i32;
                }
            }
        }

        // 慢速路径: Inode trait 分发
        open_file
            .inode()
            .read(offset, user_buf.as_mut_slice(), pwm)
            .map_or(-1, |n| {
                let new_offset = offset + n as u64;
                open_file.set_offset(new_offset);
                n as i32
            })
    });

    result.unwrap_or(-1)
}

/// 按 `inode_id` 直接读取文件数据 (B2: mmap prewarm 用)
///
/// 区别于 `vfs_read_internal`: 不依赖 fd, 而是按 inode 寻址.
/// 用于 mmap 创建 VMA 时, 同步预热 Page Cache (prewarm 全部页).
///
/// 参数:
/// - `node_id`: ramfs 内部 inode 编号
/// - `file_offset`: 文件内字节偏移 (调用方保证页对齐)
/// - `dst`: 目标缓冲区 (长度由调用方提供, 通常为 `PAGE_SIZE`)
/// - `pwm`: 权限字; 由 `pwm_has_capability` / `check_privilege` 在内部做权限校验,
///   0 表示无会话,framework 层 ramfs.read 应当返回 EACCES 而非降级为管理员。
///
/// 返回: 实际读取字节数, 负数表示错误.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
// 注意: 保持 Rust ABI — 参数含 `Option<usize>` / `&mut [u8]` 等非 FFI-safe 类型
#[unsafe(no_mangle)]
#[expect(clippy::no_mangle_with_rust_abi)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn vfs_pread_inode(
    mount_idx: Option<usize>,
    node_id: u32,
    file_offset: u64,
    dst: &mut [u8],
    pwm: u64,
) -> i32 {
    // SAFETY: 调用方保证 dst 在生命周期内有效; 长度由调用方控制.
    let mut user_buf = unsafe { UserWritePtr::new(dst.as_mut_ptr(), dst.len()) };

    // P3-I-19: 走 FileSystem trait 分发. 旧实现直接访问 RAMFS_DATA,
    // 非 RamFS (NestFS/DevFS 等) 挂载 mmap 时无法工作. 现按 mount_idx
    // 派发, 无挂载则返回 -1 (EIO). mmap prewarm 由 page_fault 传入
    // vma.mount_idx (mmap 时已解析).
    let mount_idx = match mount_idx {
        Some(i) => i,
        None => return -1,
    };
    let fs = {
        let mounts = VFS_MANAGER.mounts.lock();
        match mounts.get(mount_idx) {
            Some(m) if m.used => m.get_fs(),
            _ => return -1,
        }
    };
    let fs = match fs {
        Some(f) => f,
        None => return -1,
    };
    fs.fs_pread_inode(node_id, file_offset, user_buf.as_mut_slice(), pwm)
        .map_or(-1, |n| n as i32)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_truncate_internal(fd: u32, size: u64) -> i32 {
    let fd_idx = fd as usize;
    if fd_idx >= VFS_MAX_FDS {
        return -1;
    }

    // Plan B: 通过 OpenFile 的 Inode trait 执行
    let handle_id = match VFS_MANAGER.get_fd_handle(fd_idx as usize) {
        Some(hid) => hid,
        None => return -1,
    };

    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        let pwm = open_file.pwm;
        match open_file.inode().truncate(size, pwm) {
            Ok(()) => {
                let node_id = open_file.inode_id();
                super::inotify::inotify_notify(node_id, super::inotify::IN_MODIFY, "", false);
                0
            }
            Err(_) => -1,
        }
    });

    result.unwrap_or(-1)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_write_internal(fd_idx: u32, buf: *const u8, count: u32) -> i32 {
    if buf.is_null() || count == 0 {
        return -1;
    }

    // Plan B: 通过 OpenFile 的 Inode trait 执行 I/O
    let handle_id = match VFS_MANAGER.get_fd_handle(fd_idx as usize) {
        Some(hid) => hid,
        None => return -1,
    };

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let user_buf = unsafe { UserReadPtr::new(buf, count as usize) };

    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        // O_APPEND: 写入前自动 seek 到文件末尾 (POSIX 原子 append)
        let offset = if (open_file.get_flags() & super::types::VfsOpenFlags::APPEND.bits()) != 0 {
            open_file
                .inode()
                .stat(open_file.pwm)
                .map_or_else(|_| open_file.get_offset(), |stat| u64::from(stat.size))
        } else {
            open_file.get_offset()
        };
        let pwm = open_file.pwm;
        let node_id = open_file.inode_id();

        open_file
            .inode()
            .write(offset, user_buf.as_slice(), pwm)
            .map_or(-1, |n| {
                let new_offset = offset + n as u64;
                open_file.set_offset(new_offset);
                // inotify + fd_notify 通知
                fd_notify::notify_fd_close(fd_idx as i32);
                super::inotify::inotify_notify(node_id, super::inotify::IN_MODIFY, "", false);
                n as i32
            })
    });

    result.unwrap_or(-1)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_readdir_internal(fd: u32, entry: *mut VfsDirEntry) -> i32 {
    if entry.is_null() {
        return -1;
    }

    // Plan B: 通过 OpenFile 的 Inode trait 执行
    let handle_id = match VFS_MANAGER.get_fd_handle(fd as usize) {
        Some(hid) => hid,
        None => return -1,
    };

    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        let offset = open_file.get_offset();

        match open_file.inode().readdir(offset) {
            Ok((name, file_type, has_more)) => {
                if !has_more {
                    return 0;
                }
                let mut dir_entry = VfsDirEntry::default();
                dir_entry.set_name(&name);
                dir_entry.file_type = file_type.as_u8();
                // SAFETY: 调用方保证指针/类型有效
                let mut entry_ref = unsafe { UserRefMut::new(entry) };
                *entry_ref.as_mut() = dir_entry;
                let new_offset = offset
                    + core::mem::size_of::<crate::framework::fs::ramfs::RamFsDirEntry>() as u64;
                open_file.set_offset(new_offset);
                1
            }
            Err(_) => -1,
        }
    });

    result.unwrap_or(-1)
}

// ============================================================================
// 公共 VFS API
// ============================================================================

/// Safe 包装: `vfs_open` (接受 &str 路径)
pub fn vfs_open_safe(path: &str, flags: u32, pwm: u64) -> i32 {
    with_cstr(path, |ptr| vfs_open_internal(ptr, flags, pwm))
}

/// Safe 包装: `vfs_read` (接受可变切片)
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub fn vfs_read_safe(fd: u32, buf: &mut [u8]) -> i32 {
    // SAFETY: buf 是调用方拥有的有效可写缓冲区
    vfs_read(fd, buf.as_mut_ptr(), buf.len() as u32)
}

/// Safe 包装: `vfs_write` (接受不可变切片)
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub fn vfs_write_safe(fd: u32, buf: &[u8]) -> i32 {
    // SAFETY: buf 是调用方拥有的有效只读缓冲区
    vfs_write(fd, buf.as_ptr(), buf.len() as u32)
}

// ============================================================================
// pread / pwrite — 显式 offset I/O (T1 G1, preadv/pwritev 机制)
// ============================================================================

/// pread — 从显式 offset 读取, 不更新 fd 当前偏移 (POSIX pread 语义)
///
/// SIMPLIFIED: 不走 pcache 快路径 (直接 Inode trait 分发), 影响面: 4KB
/// 对齐大读性能低于 vfs_read 快路径; 何时需扩展: 复用 vfs_read_internal 的
/// 对齐检测 + pcache 快路径后.
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub fn vfs_pread(fd: u32, buf: *mut u8, count: u32, offset: u64) -> i32 {
    if buf.is_null() || count == 0 {
        return -1;
    }
    let Some(handle_id) = VFS_MANAGER.get_fd_handle(fd as usize) else {
        return -1;
    };
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let mut user_buf = unsafe { UserWritePtr::new(buf, count as usize) };
    OPEN_FILE_TABLE
        .with_file(handle_id, |open_file| {
            let pwm = open_file.pwm;
            open_file
                .inode()
                .read(offset, user_buf.as_mut_slice(), pwm)
                .map_or(-1, |n| n as i32)
        })
        .unwrap_or(-1)
}

/// pwrite — 从显式 offset 写入, 不更新 fd 当前偏移 (POSIX pwrite 语义)
///
/// 忽略 O_APPEND (pwrite 不受 append 模式影响, 写固定 offset).
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub fn vfs_pwrite(fd: u32, buf: *const u8, count: u32, offset: u64) -> i32 {
    if buf.is_null() || count == 0 {
        return -1;
    }
    let Some(handle_id) = VFS_MANAGER.get_fd_handle(fd as usize) else {
        return -1;
    };
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let user_buf = unsafe { UserReadPtr::new(buf, count as usize) };
    OPEN_FILE_TABLE
        .with_file(handle_id, |open_file| {
            let pwm = open_file.pwm;
            let node_id = open_file.inode_id();
            open_file
                .inode()
                .write(offset, user_buf.as_slice(), pwm)
                .map_or(-1, |n| {
                    // inotify IN_MODIFY 通知 (与 vfs_write 一致)
                    super::inotify::inotify_notify(node_id, super::inotify::IN_MODIFY, "", false);
                    n as i32
                })
        })
        .unwrap_or(-1)
}

/// Safe 包装: `vfs_close`
pub fn vfs_close_safe(fd: u32) -> i32 {
    vfs_close(fd)
}

/// Safe 包装: `vfs_seek`
pub fn vfs_seek_safe(fd: u32, offset: i32, whence: u32) -> i32 {
    vfs_seek(fd, offset, whence)
}

#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
/// Safe 包装: `vfs_readdir`
pub fn vfs_readdir_safe(fd: u32, entry: &mut super::types::VfsDirEntry) -> i32 {
    // SAFETY: entry 是调用方拥有的有效可写结构体
    vfs_readdir(fd, entry as *mut _)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vfs_open(path: *const u8, flags: u32, pwm: u64) -> i32 {
    vfs_open_internal(path, flags, pwm)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vfs_close(fd: u32) -> i32 {
    vfs_close_internal(fd)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vfs_read(fd: u32, buf: *mut u8, count: u32) -> i32 {
    vfs_read_internal(fd, buf, count)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vfs_write(fd: u32, buf: *const u8, count: u32) -> i32 {
    vfs_write_internal(fd, buf, count)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn vfs_readdir(fd: u32, entry: *mut VfsDirEntry) -> i32 {
    vfs_readdir_internal(fd, entry)
}

// ============================================================================
// fchmod — 按 fd 修改文件权限
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_fchmod(fd: u32, mode: u16) -> i32 {
    // Plan B: 通过 OpenFile 的 Inode trait 执行
    let handle_id = match VFS_MANAGER.get_fd_handle(fd as usize) {
        Some(hid) => hid,
        None => return -9,
    };

    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        match open_file.inode().chmod(mode, open_file.pwm) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    });

    result.unwrap_or(-9)
}

// ============================================================================
// fchown — 按 fd 修改文件所有者
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_fchown(fd: u32, owner_pwm: u64, group_pwm: u64, pwm: u64) -> i32 {
    // Plan B: 通过 OpenFile 的 Inode trait 执行
    let handle_id = match VFS_MANAGER.get_fd_handle(fd as usize) {
        Some(hid) => hid,
        None => return -9,
    };

    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        match open_file.inode().chown(owner_pwm, group_pwm, pwm) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    });

    result.unwrap_or(-9)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_seek(fd: u32, offset: i32, whence: u32) -> i32 {
    let whence = match VfsSeekWhence::from_u32(whence) {
        Some(w) => w,
        None => return KernelError::InvalidArgument.as_i32(),
    };

    // Plan B: 通过 OpenFile 的 Inode trait 执行
    let handle_id = match VFS_MANAGER.get_fd_handle(fd as usize) {
        Some(hid) => hid,
        None => return KernelError::InvalidArgument.as_i32(),
    };

    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        let current_offset = open_file.get_offset();
        open_file
            .inode()
            .seek(i64::from(offset), whence, current_offset)
            .map_or(KernelError::InvalidArgument.as_i32(), |new_offset| {
                open_file.set_offset(new_offset);
                new_offset as i32
            })
    });

    result.unwrap_or(KernelError::InvalidArgument.as_i32())
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_fd_table() -> *const u8 {
    VFS_MANAGER.fd_table.lock().as_ptr() as *const u8
}

// ============================================================================
// fstat — 从 fd 获取文件属性
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn vfs_fstat(fd: u32, st: *mut VfsStat, _pwm: u64) -> i32 {
    if st.is_null() {
        return -1;
    }

    // Plan B: 通过 OpenFile 的 Inode trait 执行
    let handle_id = match VFS_MANAGER.get_fd_handle(fd as usize) {
        Some(hid) => hid,
        None => return -9,
    };

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let mut st_ref = unsafe { UserRefMut::new(st) };

    let result = OPEN_FILE_TABLE.with_file(handle_id, |open_file| {
        let pwm = open_file.pwm;
        open_file.inode().stat(pwm).map_or(-1, |stat| {
            *st_ref.as_mut() = stat;
            0
        })
    });

    let result = result.unwrap_or(-1);

    if result == 0 {
        let tbl = crate::framework::credo::identity::get_table();
        let r = st_ref.as_mut();
        r.uid = tbl.uid_of(r.owner_pwm);
        r.gid = tbl.gid_of(r.group_pwm);
        if r.gid == 0xFFFF_FFFF {
            r.gid = r.uid;
        }
    }

    result
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
/// Safe 包装: services 层用, 返回 `VfsStat` 而非 raw pointer.
pub fn vfs_fstat_safe(fd: u32, pwm: u64) -> Option<VfsStat> {
    let mut st = VfsStat::default();
    let r = vfs_fstat(fd, &mut st as *mut VfsStat, pwm);
    if r < 0 { None } else { Some(st) }
}

// ============================================================================
// fd handle_id 操作 (POSIX 打开文件描述)
// ============================================================================

/// 设置 fd 的 `OpenFile` `handle_id`
pub fn vfs_set_fd_handle(fd_idx: usize, handle_id: u32) {
    VFS_MANAGER.set_fd_handle(fd_idx, handle_id);
}

/// 获取 fd 的 `OpenFile` `handle_id`
pub fn vfs_get_fd_handle(fd_idx: usize) -> Option<u32> {
    VFS_MANAGER.get_fd_handle(fd_idx)
}

// ============================================================================
// dup / dup2 — 文件描述符复制
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn vfs_dup(oldfd: u32) -> i32 {
    let old_usize = oldfd as usize;
    if old_usize >= 256 {
        return -9;
    }
    let mut fd_table = VFS_MANAGER.fd_table.lock();
    if !fd_table[old_usize].used {
        return -9;
    }

    // POSIX dup: 共享 OpenFile (offset/flags 共享)
    let handle_id = fd_table[old_usize].handle_id;

    for i in 0..256usize {
        if !fd_table[i].used {
            // 复制 fd 表条目, 但共享同一个 OpenFile
            fd_table[i] = fd_table[old_usize].clone();
            fd_table[i].fd = i as u32;
            // 增加 OpenFile 引用计数
            if handle_id != u32::MAX {
                OPEN_FILE_TABLE.inc_ref(handle_id);
            }
            return i as i32;
        }
    }
    -24 // EMFILE
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn vfs_dup2(oldfd: u32, newfd: u32) -> i32 {
    let old_usize = oldfd as usize;
    let new_usize = newfd as usize;
    if old_usize >= 256 || new_usize >= 256 {
        return -9;
    }
    let mut fd_table = VFS_MANAGER.fd_table.lock();
    if !fd_table[old_usize].used {
        return -9;
    }
    if new_usize == old_usize {
        return newfd as i32;
    }

    // POSIX dup2: 关闭旧 newfd (如果有), 然后共享 OpenFile
    let old_handle_id = fd_table[old_usize].handle_id;

    // 如果 newfd 已使用, 先关闭它
    if fd_table[new_usize].used {
        let old_new_handle_id = fd_table[new_usize].handle_id;
        if old_new_handle_id != u32::MAX {
            OPEN_FILE_TABLE.close(old_new_handle_id);
        }
    }

    // 复制 fd 表条目, 共享同一个 OpenFile
    fd_table[new_usize] = fd_table[old_usize].clone();
    fd_table[new_usize].fd = new_usize as u32;

    // 增加 OpenFile 引用计数
    if old_handle_id != u32::MAX {
        OPEN_FILE_TABLE.inc_ref(old_handle_id);
    }

    newfd as i32
}
