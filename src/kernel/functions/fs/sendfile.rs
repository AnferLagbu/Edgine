//! sendfile / splice — 零拷贝数据传输 (functions 层权威实现, 100% safe Rust, 0 unsafe)
//!
//! ## 设计
//!
//! v1 采用内核缓冲区中转策略 (类似 Linux 2.2 sendfile):
//! - **sendfile**: 从 `in_fd` (VFS 文件) 读取数据, 写入 `out_fd` (VFS 文件/pipe)
//! - **splice**: 在 pipe fd 与 VFS fd 之间传输数据
//!
//! 数据流: `in_fd` → 内核 bounce buffer → `out_fd`
//! 避免 user ↔ kernel 两次拷贝, 但仍有一次内核内拷贝.
//! 未来 v2 可实现 page-flipping 真零拷贝 (pipe buffer → pcache 页引用).
//!
//! ## 归属 (框内核范式, 阶段 2-A)
//!
//! 本模块为 `SYS_sendfile` / `SYS_splice` 的权威实现, 原
//! `privileged::syscall::sendfile` 已删除 (机制归 privileged, 功能归 functions).
//! 用户指针读写经 `privileged::syscall::api` 安全包装 (read/write_u64_to_user),
//! VFS 数据面经内核缓冲 I/O (`vfs_read_kernel` / `vfs_write_kernel`).
//!
//! ## FD 类型识别
//!
//! - VFS fd: 当前进程 fd 表中有映射 (fd → `OpenFileTable` handle 存在)
//! - Pipe fd: 同样位于 VFS fd 空间, 由 `OpenFile` 的 `pipe_key` 标记区分
//!   (`functions::fs::pipe_fd`)
//! - Eventfd/Signalfd/Timerfd/Inotify: 各自独立 FD 空间, 不参与 sendfile/splice
//!
//! ## 安全约束
//!
//! - bounce buffer 在栈上 (8KB), 不会 OOM
//! - 所有 fd 验证在操作前完成
//! - offset 更新在传输成功后

#![deny(unsafe_code)]
#![deny(missing_docs)]

use crate::functions::fs::OPEN_FILE_TABLE;
use crate::functions::fs::api as vfs_api;
use crate::functions::fs::vfs_get_fd_handle;
use crate::privileged::syscall::Errno;

/// sendfile 传输的 bounce buffer 大小 (8KB)
const BOUNCE_SIZE: usize = 8192;

/// splice 标志
pub const SPLICE_F_MOVE: u32 = 1;
/// splice 非阻塞标志 (无数据可搬时立即返回)。
pub const SPLICE_F_NONBLOCK: u32 = 2;
/// splice 提示后续还有数据 (用于网络发送减少包数)。
pub const SPLICE_F_MORE: u32 = 4;
/// splice 提示页可赠予 (调用方放弃对源页的引用)。
pub const SPLICE_F_GIFT: u32 = 8;

// ============================================================================
// FD 类型判断
// ============================================================================

/// 判断 fd 是否为 VFS 文件 fd (非 pipe/eventfd/signalfd/timerfd/inotify/UDS)
///
/// 0/1/2 不预排除: `dup2` 可把普通文件/管道重定向到标准流编号.
fn is_vfs_file_fd(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    let fd_usize = fd as usize;
    // 排除特殊 FD 空间
    if (100..116).contains(&fd) {
        return false; // UDS
    }
    if (200..216).contains(&fd) {
        return false; // eventfd
    }
    if (220..236).contains(&fd) {
        return false; // signalfd
    }
    if (240..256).contains(&fd) {
        return false; // timerfd
    }
    if crate::functions::fs::inotify::is_inotify_fd(fd) {
        return false;
    }
    if super::pipe_fd::is_pipe_fd(fd) {
        return false;
    }
    // 检查当前进程 fd 表 (fd → OpenFileTable handle 映射存在即有效)
    vfs_get_fd_handle(fd_usize).is_some()
}

/// 判断 fd 是否为 pipe fd (VFS fd 空间内的管道端点)
fn is_pipe_fd(fd: i32) -> bool {
    super::pipe_fd::is_pipe_fd(fd)
}

// ============================================================================
// sendfile 系统调用
// ============================================================================

/// `sys_sendfile` — 在两个文件描述符之间传输数据
///
/// `out_fd`: 目标 fd (VFS 文件或 pipe 写端)
/// `in_fd`: 源 fd (必须是 VFS 文件, 支持 offset)
/// `offset_ptr`: 用户空间 offset 指针 (若非 NULL, 读取/更新偏移; 若 NULL, 使用 fd 当前偏移)
/// `count`: 传输字节数
///
/// 返回实际传输的字节数, 或负的错误码.
pub fn sys_sendfile(out_fd: i32, in_fd: i32, offset_ptr: u64, count: usize) -> i64 {
    // 参数验证
    if count == 0 {
        return 0;
    }

    // in_fd 必须是 VFS 文件
    if !is_vfs_file_fd(in_fd) {
        return Errno::EBADF.as_ret();
    }

    // out_fd 必须是 VFS 文件或 pipe 写端
    let out_is_vfs = is_vfs_file_fd(out_fd);
    let out_is_pipe = is_pipe_fd(out_fd);
    if !out_is_vfs && !out_is_pipe {
        return Errno::EBADF.as_ret();
    }

    // in_fd 的 OpenFile handle (offset 读写经 OpenFile 元数据源)
    let Some(in_handle) = vfs_get_fd_handle(in_fd as usize) else {
        return Errno::EBADF.as_ret();
    };

    // 读取用户空间 offset (若提供); 经 privileged 安全 API, 先校验后读取
    let mut offset: u64 = if offset_ptr != 0 {
        match crate::privileged::syscall::api::read_u64_from_user(offset_ptr) {
            Some(v) => v,
            None => return Errno::EFAULT.as_ret(),
        }
    } else {
        // 使用 fd 当前偏移 (OpenFile 元数据源)
        match OPEN_FILE_TABLE.with_file(in_handle, crate::functions::fs::OpenFile::get_offset) {
            Some(off) => off,
            None => return Errno::EBADF.as_ret(),
        }
    };

    let mut total_sent: usize = 0;
    let mut bounce = [0u8; BOUNCE_SIZE];

    while total_sent < count {
        let chunk = core::cmp::min(BOUNCE_SIZE, count - total_sent);

        // 1. 从 in_fd 读取到 bounce buffer (先临时设定 in_fd 的 offset)
        let _ = OPEN_FILE_TABLE.with_file(in_handle, |of| of.set_offset(offset));
        let nread = vfs_api::vfs_read_kernel(in_fd as u32, &mut bounce[..chunk]);
        if nread < 0 {
            if total_sent == 0 {
                return i64::from(nread);
            }
            break;
        }
        if nread == 0 {
            break; // EOF
        }
        let nread_usize = nread as usize;

        // 2. 从 bounce buffer 写入 out_fd (VFS 文件与管道端点同一路径)
        let nwritten = vfs_api::vfs_write_kernel(out_fd as u32, &bounce[..nread_usize]);
        if nwritten < 0 {
            // 写入失败: in_fd offset 回退到本次读取前, 已发送部分按成功返回
            let _ = OPEN_FILE_TABLE.with_file(in_handle, |of| of.set_offset(offset));
            if total_sent == 0 {
                return i64::from(nwritten);
            }
            break;
        }
        if nwritten == 0 {
            let _ = OPEN_FILE_TABLE.with_file(in_handle, |of| of.set_offset(offset));
            break;
        }

        let written_usize = nwritten as usize;
        // in_fd 实际消费量 = 成功写入量 (部分写时回退多余读取)
        offset += written_usize as u64;
        total_sent += written_usize;
        let _ = OPEN_FILE_TABLE.with_file(in_handle, |of| of.set_offset(offset));

        // 如果读取量小于请求量, 说明 EOF
        if nread_usize < chunk {
            break;
        }
    }

    // 更新用户空间 offset (若提供); 经 privileged 安全 API, 先校验后写入
    if offset_ptr != 0 && total_sent > 0 {
        let _ = crate::privileged::syscall::api::write_u64_to_user(offset_ptr, offset);
    }

    if total_sent == 0 && count > 0 {
        // 没有传输任何数据, 可能是 EOF
        return 0; // sendfile 在 EOF 时返回 0
    }

    total_sent as i64
}

// ============================================================================
// splice 系统调用
// ============================================================================

/// `sys_splice` — 在 pipe 与文件之间传输数据
///
/// `fd_in`: 输入 fd (pipe 读端 或 VFS 文件)
/// `off_in`: 输入偏移指针 (pipe 时必须为 NULL)
/// `fd_out`: 输出 fd (VFS 文件 或 pipe 写端)
/// `off_out`: 输出偏移指针 (pipe 时必须为 NULL)
/// `len`: 传输字节数
/// `flags`: `SPLICE_F`_* 标志
///
/// 至少一端必须是 pipe.
/// 返回实际传输的字节数, 或负的错误码.
pub fn sys_splice(
    fd_in: i32,
    _off_in: u64,
    fd_out: i32,
    _off_out: u64,
    len: usize,
    flags: u32,
) -> i64 {
    // 参数验证
    if len == 0 {
        return 0;
    }

    // flags 保留位检查
    if flags & !(SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT) != 0 {
        return Errno::EINVAL.as_ret();
    }

    let in_is_pipe = is_pipe_fd(fd_in);
    let in_is_vfs = is_vfs_file_fd(fd_in);
    let out_is_pipe = is_pipe_fd(fd_out);
    let out_is_vfs = is_vfs_file_fd(fd_out);

    // 至少一端必须是 pipe
    if !in_is_pipe && !out_is_pipe {
        return Errno::EINVAL.as_ret();
    }

    // 输入端必须有效
    if !in_is_pipe && !in_is_vfs {
        return Errno::EBADF.as_ret();
    }
    // 输出端必须有效
    if !out_is_pipe && !out_is_vfs {
        return Errno::EBADF.as_ret();
    }

    // pipe 读写不需要 IPC 策略: 管道端点同为 VFS fd, 走统一内核缓冲 I/O

    // pipe → pipe 不支持 (v1)
    if in_is_pipe && out_is_pipe {
        return Errno::EINVAL.as_ret();
    }

    let mut total_spliced: usize = 0;
    let mut bounce = [0u8; BOUNCE_SIZE];

    while total_spliced < len {
        let chunk = core::cmp::min(BOUNCE_SIZE, len - total_spliced);

        // 1. 从 fd_in 读取到 bounce buffer (VFS 文件与管道端点同一路径)
        let nread = vfs_api::vfs_read_kernel(fd_in as u32, &mut bounce[..chunk]);
        if nread < 0 {
            if total_spliced == 0 {
                return i64::from(nread);
            }
            break;
        }
        if nread == 0 {
            break; // EOF / 管道空
        }
        let nread_usize = nread as usize;

        // 2. 从 bounce buffer 写入 fd_out
        let nwritten = vfs_api::vfs_write_kernel(fd_out as u32, &bounce[..nread_usize]);
        if nwritten < 0 {
            if total_spliced == 0 {
                return i64::from(nwritten);
            }
            break;
        }
        if nwritten == 0 {
            break;
        }

        total_spliced += nwritten as usize;

        // 读取量小于请求量, 说明源 EOF 或 pipe 空
        if nread_usize < chunk {
            break;
        }
    }

    if total_spliced == 0 && len > 0 {
        // 区分 EOF 和 EAGAIN
        if (flags & SPLICE_F_NONBLOCK) != 0 {
            return Errno::EAGAIN.as_ret();
        }
        return 0; // EOF
    }

    total_spliced as i64
}

// ============================================================================
// 内核测试
// ============================================================================

// 阶段 4c: 原 kernel_test 注册回调 (register_sendfile_tests) 改写为源侧
// #[cfg(test)] — fd/pipe/flags 校验均为纯逻辑断言, host 侧 cargo test 为唯一
// 归属 (DECISION-080).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sendfile_ebadf() {
        // in_fd 不是 VFS 文件 → EBADF
        let result = sys_sendfile(1, -1, 0, 1024);
        assert!(result < 0, "sendfile with bad in_fd should fail");
    }

    #[test]
    fn test_splice_einval_no_pipe() {
        // 两端都不是 pipe → EINVAL
        let result = sys_splice(3, 0, 4, 0, 1024, 0);
        assert!(result < 0, "splice with no pipe end should fail");
    }

    #[test]
    fn test_splice_einval_bad_flags() {
        // 非法 flags → EINVAL
        let result = sys_splice(0, 0, 0, 0, 1024, 0xFF);
        assert!(result < 0, "splice with bad flags should fail");
    }
}
