#![deny(unsafe_code)]
//! IO 系统调用 — services 层安全代理
//!
//! ## 范围
//!
//! - read/write: 文件 I/O (T2 批 1 自 framework 回退层迁移)
//! - pipe: 匿名管道创建
//! - dup/dup2: 文件描述符复制
//! - fcntl: 文件控制
//!
//! ## 安全边界
//!
//! - services 层: 验证参数类型/范围, 用户缓冲区校验, fd 路由策略
//!   (console / eventfd / signalfd / timerfd / inotify / VFS)
//! - framework 层: 用户内存拷贝机制 (`mm::copy_user`), 特殊 fd 权威实现,
//!   实际访问 VFS / 创建内核对象

use crate::framework::syscall::Errno;
use crate::framework::syscall::raw;

/// 将 i64 返回码 (负数 = -errno) 转为 services 层 Result
#[inline]
fn ret_to_result(r: i64) -> Result<usize, Errno> {
    if r < 0 {
        Err(Errno::from_ret(r))
    } else {
        Ok(r as usize)
    }
}

/// read 系统调用策略
///
/// fd 路由: 0 = stdin (键盘), eventfd/signalfd/timerfd/inotify = 各自权威
/// 实现, 其余走 VFS. fd 1/2 为 stdout/stderr 只写, 读返回 `EBADF`.
///
/// # Errors
/// - `buf` 为空或 `count` 为 0 → `EINVAL`
/// - 用户缓冲区未通过校验 → `EFAULT`
/// - fd 为 1/2 → `EBADF`; 其余错误由底层实现以对应 `Errno` 传播.
pub fn read_syscall(fd: i32, buf: u64, count: u64) -> Result<usize, Errno> {
    if buf == 0 || count == 0 {
        return Err(Errno::EINVAL);
    }
    if !raw::check_user_buf(buf, count) {
        return Err(Errno::EFAULT);
    }
    if fd == 1 || fd == 2 {
        return Err(Errno::EBADF);
    }
    if fd == 0 {
        // stdin: 键盘输入 (x86_64 生产构建); 其余配置无输入源, 返回 EOF
        #[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]
        if let Some(c) = raw::read_keyboard_byte() {
            // copy_to_user: 异常表兜底的 safe 用户内存写入 (1 字节)
            return match crate::framework::mm::copy_user::copy_to_user(buf, &[c], 1) {
                Ok(_) => Ok(1),
                Err(()) => Err(Errno::EFAULT),
            };
        }
        return Ok(0);
    }
    // 特殊 fd 路由: 顺序与原 framework 实现一致
    if crate::framework::syscall::eventfd::is_eventfd_fd(fd) {
        return ret_to_result(crate::framework::syscall::eventfd::sys_eventfd_read(
            fd, buf,
        ));
    }
    if crate::framework::syscall::signalfd::is_signalfd_fd(fd) {
        return ret_to_result(crate::framework::syscall::signalfd::sys_signalfd_read(
            fd, buf,
        ));
    }
    if crate::framework::syscall::timerfd::is_timerfd_fd(fd) {
        return ret_to_result(crate::framework::syscall::timerfd::sys_timerfd_read(
            fd, buf,
        ));
    }
    if crate::framework::fs::is_inotify_fd(fd) {
        return ret_to_result(crate::framework::fs::sys_inotify_read(
            i64::from(fd),
            buf as *mut u8,
            count as usize,
        ));
    }
    // T1 G4: userfaultfd 事件读取 (无事件时阻塞等待缺页通知)
    if crate::framework::mm::is_uffd_fd(fd) {
        return crate::services::mm::uffd::read_event(fd, buf, count);
    }
    // 常规 VFS 读 (vfs_read 将数据写入调用方地址空间的 buf)
    ret_to_result(i64::from(crate::framework::fs::api::vfs_read(
        fd as u32,
        buf as *mut u8,
        count as u32,
    )))
}

/// write 系统调用策略
///
/// fd 路由: 1/2 = 控制台 (串口输出), eventfd = 计数器写入, 其余走 VFS.
/// 用户数据经 `mm::copy_user::copy_from_user` (异常表兜底) 分块拷入内核.
///
/// # Errors
/// - `buf` 为空或 `count` 为 0 → `EINVAL`
/// - 用户缓冲区未通过校验 → `EFAULT`
/// - eventfd 写入不足 8 字节 → `EINVAL`; 其余错误由底层实现以对应
///   `Errno` 传播.
pub fn write_syscall(fd: i32, buf: u64, count: u64) -> Result<usize, Errno> {
    if buf == 0 || count == 0 {
        return Err(Errno::EINVAL);
    }
    if !raw::check_user_buf(buf, count) {
        return Err(Errno::EFAULT);
    }
    if fd == 1 || fd == 2 {
        // 控制台: 分块拷贝用户数据 → 串口输出 (单次处理上限 4KB)
        let total = (count as usize).min(4096);
        let mut kernel_buf = [0u8; 256];
        let mut off: usize = 0;
        while off < total {
            let chunk = (total - off).min(kernel_buf.len());
            match crate::framework::mm::copy_user::copy_from_user(
                &mut kernel_buf[..chunk],
                buf + off as u64,
                chunk,
            ) {
                Ok(n) if n > 0 => {
                    crate::framework::klog::serial_write_bytes(&kernel_buf[..n]);
                    off += n;
                }
                _ => return Err(Errno::EFAULT),
            }
        }
        return Ok(count as usize);
    }
    if crate::framework::syscall::eventfd::is_eventfd_fd(fd) {
        if count < 8 {
            return Err(Errno::EINVAL);
        }
        let mut val_buf = [0u8; 8];
        match crate::framework::mm::copy_user::copy_from_user(&mut val_buf, buf, 8) {
            Ok(8) => {}
            _ => return Err(Errno::EFAULT),
        }
        return ret_to_result(crate::framework::syscall::eventfd::sys_eventfd_write(
            fd,
            u64::from_ne_bytes(val_buf),
        ));
    }
    // 文件写入: 分块拷贝用户数据到内核缓冲区, 再走 VFS
    let total = (count as usize).min(4096);
    let mut kernel_buf = [0u8; 256];
    let mut written: usize = 0;
    while written < total {
        let chunk = (total - written).min(kernel_buf.len());
        let copied = match crate::framework::mm::copy_user::copy_from_user(
            &mut kernel_buf[..chunk],
            buf + written as u64,
            chunk,
        ) {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        let n = crate::framework::fs::api::vfs_write_safe(fd as u32, &kernel_buf[..copied]);
        if n < 0 {
            return Err(Errno::from_ret(i64::from(n)));
        }
        written += copied;
    }
    Ok(written)
}

/// pipe 系统调用
///
/// `fds` 指向用户空间 i32`[2]` 数组 (8 字节), 返回读端和写端文件描述符.
///
/// 阶段 2-A (framekernel 范式): 管道创建机制经
/// `framework::ipc::pipe::ipc_pipe_create` (策略分发) 完成, 用户态写回在此用
/// safe API 完成.
///
/// # Errors
/// - `fds` 为空指针或缓冲区未通过校验 → `EFAULT`
/// - 策略创建失败 → `EBUSY`
pub fn pipe_syscall(fds: u64) -> Result<usize, Errno> {
    if fds == 0 || !raw::check_user_buf(fds, 8) {
        return Err(Errno::EFAULT);
    }
    // 保持原 `sys_pipe` 行为: 策略失败/未注册统一返回 EBUSY.
    let (rfd, wfd) = crate::framework::ipc::pipe::ipc_pipe_create().map_err(|_| Errno::EBUSY)?;
    if rfd < 0 || wfd < 0 {
        return Err(Errno::EBUSY);
    }
    let pipefd: [i32; 2] = [rfd, wfd];
    if !crate::framework::syscall::api::write_struct_to_user(fds, &pipefd) {
        return Err(Errno::EFAULT);
    }
    Ok(0)
}

/// pipe2 系统调用 (支持 `flags`)
///
/// `fds` 指向用户空间 i32`[2]` 数组 (8 字节)
///
/// # Errors
/// 与 [`pipe_syscall`] 一致.
///
/// SIMPLIFIED: flags (`O_CLOEXEC`/`O_NONBLOCK`) 当前忽略, 语义等同 `pipe`;
/// 影响面: `pipe2(O_CLOEXEC)` 创建的 fd 不设置 close-on-exec 标志;
/// 何时需扩展: 在 FD 表实现 close-on-exec 标志后接入 flags 语义.
pub fn pipe2_syscall(fds: u64, flags: i32) -> Result<usize, Errno> {
    let _ = flags;
    pipe_syscall(fds)
}

/// dup 系统调用 — 复制文件描述符 (返回新 fd, 取最小可用值)
///
/// # Errors
/// 当 `oldfd` 为负数或底层 `vfs_dup` 失败时返回对应 `Errno`.
pub fn dup_syscall(oldfd: i32) -> Result<usize, Errno> {
    if oldfd < 0 {
        return Err(Errno::EBADF);
    }
    ret_to_result(i64::from(crate::framework::fs::vfs::vfs_dup(oldfd as u32)))
}

/// dup2 系统调用 — 复制文件描述符到 `newfd`
///
/// 若 `newfd` 已打开则先关闭. 若 `oldfd == newfd` 则不关闭直接返回.
///
/// # Errors
/// 当 `oldfd` 或 `newfd` 为负数, 或底层 `vfs_dup2` 失败时返回 `EBADF`.
pub fn dup2_syscall(oldfd: i32, newfd: i32) -> Result<usize, Errno> {
    if oldfd < 0 || newfd < 0 {
        return Err(Errno::EBADF);
    }
    if oldfd == newfd {
        return Ok(newfd as usize);
    }
    let result = crate::framework::fs::vfs::vfs_dup2(oldfd as u32, newfd as u32);
    if result < 0 {
        return Err(Errno::EBADF);
    }
    Ok(result as usize)
}

/// dup3 系统调用 (支持 `flags`)
///
/// # Errors
/// 当 `oldfd` 或 `newfd` 为负数时返回 `EBADF`; `oldfd == newfd` 时返回 `EINVAL`
/// (dup3 语义要求两 fd 不同); 底层 `vfs_dup2` 失败返回 `EBADF`.
pub fn dup3_syscall(oldfd: i32, newfd: i32, flags: i32) -> Result<usize, Errno> {
    if oldfd < 0 || newfd < 0 {
        return Err(Errno::EBADF);
    }
    if oldfd == newfd {
        return Err(Errno::EINVAL); // dup3 要求 oldfd != newfd
    }
    // flags 当前被忽略 (未实现 O_CLOEXEC 处理)
    let _ = flags;
    let result = crate::framework::fs::vfs::vfs_dup2(oldfd as u32, newfd as u32);
    if result < 0 {
        return Err(Errno::EBADF);
    }
    Ok(result as usize)
}

/// `fcntl` 命令: `F_DUPFD`
const F_DUPFD: i32 = 0;
/// `fcntl` 命令: `F_GETFD`
const F_GETFD: i32 = 1;
/// `fcntl` 命令: `F_SETFD`
const F_SETFD: i32 = 2;
/// `fcntl` 命令: `F_GETFL`
const F_GETFL: i32 = 3;
/// `fcntl` 命令: `F_SETFL`
const F_SETFL: i32 = 4;

/// fd 级 close-on-exec 标志位 (F_GETFD/F_SETFD 的掩码, 与 POSIX `FD_CLOEXEC` 一致)
const FD_CLOEXEC: u64 = 1;

/// fcntl 系统调用
///
/// # Errors
/// 当 `fd` 为负数时返回 `EBADF`; 各命令的具体错误由对应分支返回.
pub fn fcntl_syscall(fd: i32, cmd: i32, arg: u64) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    match cmd {
        F_GETFD => {
            // B-9.5/B-8.3: fd 级标志改源 per-process FdTable.cloexec (权威表);
            // FD_CLOEXEC 为唯一 fd 级标志, 仅当 fd 已分配时返回, 否则 EBADF.
            let result = crate::framework::proc::with_current_fd_table(|t| {
                t.get_handle_id(fd as usize)?;
                Some(if t.is_cloexec(fd as usize) {
                    FD_CLOEXEC
                } else {
                    0
                })
            })
            .flatten();
            match result {
                Some(bits) => Ok(bits as usize),
                None => Err(Errno::EBADF),
            }
        }
        F_SETFD => {
            // B-9.5/B-8.3: 写入 per-process FdTable.cloexec; fd 未分配返回 EBADF.
            let result = crate::framework::proc::with_current_fd_table(|t| {
                t.get_handle_id(fd as usize)?;
                t.set_cloexec(fd as usize, (arg & FD_CLOEXEC) != 0);
                Some(0)
            })
            .flatten();
            match result {
                Some(_) => Ok(0),
                None => Err(Errno::EBADF),
            }
        }
        F_GETFL => {
            // B-9.5: fd 元数据改源 OpenFile (per-process fd 表取 handle_id)
            let Some(handle_id) = crate::framework::fs::vfs::vfs_get_fd_handle(fd as usize) else {
                return Err(Errno::EBADF);
            };
            match crate::framework::fs::OPEN_FILE_TABLE
                .with_file(handle_id, crate::framework::fs::OpenFile::get_flags)
            {
                Some(flags) => ret_to_result(i64::from(flags)),
                None => Err(Errno::EBADF),
            }
        }
        F_SETFL => Ok(0),
        F_DUPFD => dup2_syscall(fd, arg as i32),
        // POSIX record locks (F_SETLK / F_GETLK / F_SETLKW)  // fcntl 文件锁命令
        5 | 6 | 7 => sys_fcntl_posix_lock(fd, cmd, arg),
        _ => Err(Errno::EINVAL),
    }
}

#[expect(
    clippy::comparison_chain,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// fcntl POSIX record lock 处理
///
/// `arg` 指向用户空间的 `flock` 结构体 (24 字节):
///   `l_type`:  i16  (`F_RDLCK=0`, `F_WRLCK=1`, `F_UNLCK=2`)  // 锁类型
///   `l_whence`: i16 (`0=SEEK_SET`, `1=SEEK_CUR`, `2=SEEK_END`)  // 偏移基准
///   `l_start`: i64
///   `l_len`:   i64  (0=到文件末尾)
///   `l_pid`:   i32  (`F_GETLK` 返回冲突锁的 PID)
///
/// # Errors
/// 用户缓冲区未通过校验 → `EFAULT`; 参数非法 → `EINVAL`;
/// 锁冲突 (`F_SETLK`) → `EAGAIN`; 锁表耗尽 → `ENOLCK`.
fn sys_fcntl_posix_lock(fd: i32, cmd: i32, arg: u64) -> Result<usize, Errno> {
    use crate::framework::fs::{F_GETLK, PosixLockResult, sys_posix_lock};

    // flock 结构体布局 (与 Linux 兼容):
    // offset 0:  l_type   i16
    // offset 2:  l_whence i16
    // offset 4:  l_start  i64
    // offset 12: l_len    i64
    // offset 20: l_pid    i32
    const FLOCK_STRUCT_SIZE: usize = 24;

    // 读用户空间 flock 原始字节 (safe API, 内部含 check_user_buf 校验);
    // 直接按偏移手工解码, 保留原始字节以支持 F_GETLK 原地回写未改字段.
    let mut raw = [0u8; FLOCK_STRUCT_SIZE];
    if !crate::framework::syscall::api::read_struct_from_user(arg, &mut raw) {
        return Err(Errno::EFAULT);
    }
    let l_type = i16::from_ne_bytes([raw[0], raw[1]]);
    let l_whence = i16::from_ne_bytes([raw[2], raw[3]]);
    let l_start = i64::from_ne_bytes([
        raw[4], raw[5], raw[6], raw[7], raw[8], raw[9], raw[10], raw[11],
    ]);
    let l_len = i64::from_ne_bytes([
        raw[12], raw[13], raw[14], raw[15], raw[16], raw[17], raw[18], raw[19],
    ]);

    // 验证 l_type
    if !(0..=2).contains(&l_type) {
        return Err(Errno::EINVAL);
    }

    // 获取 fd 对应的 inode 号 (B-9.5: per-process fd 表 → OpenFile)
    let ino = {
        let Some(handle_id) = crate::framework::fs::vfs::vfs_get_fd_handle(fd as usize) else {
            return Err(Errno::EBADF);
        };
        match crate::framework::fs::OPEN_FILE_TABLE
            .with_file(handle_id, crate::framework::fs::OpenFile::inode_id)
        {
            Some(ino) => ino,
            None => return Err(Errno::EBADF),
        }
    };

    // 计算 l_start (基于 l_whence)
    let start = match l_whence {
        0 => l_start as u64, // SEEK_SET
        1 => {
            // SEEK_CUR: 当前 offset + l_start (B-9.5: offset 源自共享 OpenFile)
            let Some(handle_id) = crate::framework::fs::vfs::vfs_get_fd_handle(fd as usize) else {
                return Err(Errno::EBADF);
            };
            match crate::framework::fs::OPEN_FILE_TABLE
                .with_file(handle_id, crate::framework::fs::OpenFile::get_offset)
            {
                Some(offset) => (offset as i64 + l_start) as u64,
                None => return Err(Errno::EBADF),
            }
        }
        2 => {
            // SEEK_END: v1 简化, 不支持 (需要文件大小)
            return Err(Errno::EINVAL);
        }
        _ => return Err(Errno::EINVAL),
    };

    let len = if l_len < 0 {
        // 负长度: 从 start 向前锁; v1 简化, 不支持负长度
        return Err(Errno::EINVAL);
    } else if l_len == 0 {
        0 // 到文件末尾
    } else {
        l_len as u64
    };

    let pid = crate::framework::proc::process_get_current_pid();

    match sys_posix_lock(pid, ino, cmd, i32::from(l_type), start, len) {
        Ok(None) => Ok(0),
        Ok(Some(conflict)) => {
            if cmd == F_GETLK {
                // F_GETLK: 写回冲突锁类型 (offset 0-1) 与 pid (offset 20-23),
                // 保留结构体其余字段原值.
                let ct = conflict.lock_type as i16;
                raw[0..2].copy_from_slice(&ct.to_ne_bytes());
                let cpid = conflict.pid as i32;
                raw[20..24].copy_from_slice(&cpid.to_ne_bytes());
                if !crate::framework::syscall::api::write_struct_to_user(arg, &raw) {
                    return Err(Errno::EFAULT);
                }
                Ok(0)
            } else {
                // F_SETLK / F_SETLKW: 锁被占用
                Err(Errno::EAGAIN)
            }
        }
        Err(PosixLockResult::Invalid) => Err(Errno::EINVAL),
        Err(PosixLockResult::NoSpace) => Err(Errno::ENOLCK),
        Err(PosixLockResult::WouldBlock) => Err(Errno::EAGAIN),
    }
}

/// `copy_file_range` — 在两个文件描述符之间复制数据
///
/// 简化实现: 使用 read/write 循环 (非零拷贝)
///
/// # Arguments
/// * `fd_in` - 源文件描述符
/// * `off_in` - 源偏移量指针
/// * `fd_out` - 目标文件描述符
/// * `off_out` - 目标偏移量指针
/// * `len` - 复制长度
///
/// # Returns
/// 成功返回复制的字节数，失败返回 Errno
///
/// # Errors
/// 当 `fd_in` 或 `fd_out` 为负数时返回 `EBADF`;
/// 当底层 read/write 失败且尚未复制任何字节时返回对应的 `Errno`.
pub fn copy_file_range_syscall(
    fd_in: i32,
    _off_in: u64,
    fd_out: i32,
    _off_out: u64,
    len: usize,
) -> Result<usize, Errno> {
    // 参数验证
    if fd_in < 0 || fd_out < 0 {
        return Err(Errno::EBADF);
    }
    if len == 0 {
        return Ok(0);
    }

    // 限制单次复制大小 (避免栈溢出)
    let chunk_size = len.min(4096);
    let mut buf = alloc::vec![0u8; chunk_size];
    let mut total_copied = 0usize;

    loop {
        let remaining = len - total_copied;
        if remaining == 0 {
            break;
        }

        let to_read = remaining.min(chunk_size);

        // 从源 fd 读取
        let read_ret =
            crate::framework::fs::api::vfs_read(fd_in as u32, buf.as_mut_ptr(), to_read as u32);
        if read_ret < 0 {
            if total_copied > 0 {
                return Ok(total_copied);
            }
            return Err(Errno::from_ret(i64::from(read_ret)));
        }
        let bytes_read = read_ret as usize;
        if bytes_read == 0 {
            break; // EOF
        }

        // 写入目标 fd
        let write_ret =
            crate::framework::fs::api::vfs_write(fd_out as u32, buf.as_ptr(), bytes_read as u32);
        if write_ret < 0 {
            if total_copied > 0 {
                return Ok(total_copied);
            }
            return Err(Errno::from_ret(i64::from(write_ret)));
        }

        total_copied += bytes_read as usize;

        // 如果写入的字节数少于读取的, 停止
        if (write_ret as usize) < bytes_read {
            break;
        }
    }

    Ok(total_copied)
}

// ============================================================================
// readv / writev / close_range (T1 G1, syscall-followup 功能实装)
// ============================================================================

/// Linux `IOV_MAX` — 单次向量 I/O 的最大 iovec 数
pub(crate) const IOV_MAX: u64 = 1024;

/// 从用户空间读取 iovec 数组 (每项 `{iov_base: u64, iov_len: u64}` 共 16 字节)
///
/// 经 `framework::syscall::api::read_struct_from_user` (异常表兜底的 safe 读)
/// 逐条读取, services 0 unsafe.
///
/// # Errors
/// - `iovcnt` 超 `IOV_MAX` → `EINVAL` (Linux 语义)
/// - `iov_ptr` 无效或读取失败 → `EFAULT`
pub(crate) fn read_iovecs(iov_ptr: u64, iovcnt: u64) -> Result<alloc::vec::Vec<(u64, u64)>, Errno> {
    if iovcnt > IOV_MAX {
        return Err(Errno::EINVAL);
    }
    if iovcnt == 0 {
        return Ok(alloc::vec::Vec::new());
    }
    if iov_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    let mut iovs = alloc::vec::Vec::with_capacity(iovcnt as usize);
    for i in 0..iovcnt {
        let mut entry = [0u64; 2];
        if !crate::framework::syscall::api::read_struct_from_user(iov_ptr + i * 16, &mut entry) {
            return Err(Errno::EFAULT);
        }
        iovs.push((entry[0], entry[1]));
    }
    Ok(iovs)
}

/// readv(fd, iov, iovcnt) — 向量读 (T1 G1 实装)
///
/// 逐 iovec 段委托 `read_syscall` (复用 fd 路由: stdin/eventfd/signalfd/
/// timerfd/inotify/VFS 与用户缓冲区校验). 段读得少于请求字节视为 EOF 提前返回.
///
/// # Errors
/// - `iovcnt` 超 `IOV_MAX` → `EINVAL`
/// - iovec 数组读取失败 → `EFAULT`
/// - 单段错误由 `read_syscall` 以对应 `Errno` 传播 (fd 1/2 → `EBADF` 等).
pub fn readv_syscall(fd: i32, iov_ptr: u64, iovcnt: u64) -> Result<usize, Errno> {
    let iovs = read_iovecs(iov_ptr, iovcnt)?;
    let mut total = 0usize;
    for (base, len) in iovs {
        if len == 0 {
            continue;
        }
        let n = read_syscall(fd, base, len)?;
        total += n;
        if n < len as usize {
            break; // EOF 或部分读
        }
    }
    Ok(total)
}

/// writev(fd, iov, iovcnt) — 向量写 (T1 G1 实装)
///
/// 逐 iovec 段委托 `write_syscall` (复用 fd 路由: 控制台/eventfd/VFS).
/// 段写得少于请求字节视为部分写提前返回 (pipe/控制台语义).
///
/// # Errors
/// - `iovcnt` 超 `IOV_MAX` → `EINVAL`
/// - iovec 数组读取失败 → `EFAULT`
/// - 单段错误由 `write_syscall` 以对应 `Errno` 传播.
pub fn writev_syscall(fd: i32, iov_ptr: u64, iovcnt: u64) -> Result<usize, Errno> {
    let iovs = read_iovecs(iov_ptr, iovcnt)?;
    let mut total = 0usize;
    for (base, len) in iovs {
        if len == 0 {
            continue;
        }
        let n = write_syscall(fd, base, len)?;
        total += n;
        if n < len as usize {
            break; // 部分写
        }
    }
    Ok(total)
}

/// close_range(first, last, flags) — 批量关闭 fd (T1 G1 实装)
///
/// 遍历当前进程 fd 表 `[first, last]` 中占用条目, 逐个 `vfs_close`
/// (原子 claim-and-clear + inotify/epoll 通知, 机制在 framework).
///
/// # Errors
/// - `first > last` → `EINVAL`
/// - `flags` 含未知位 → `EINVAL`
/// - `flags` 为 `CLOSE_RANGE_UNSHARE`/`CLOSE_RANGE_CLOEXEC` → `ENOSYS`
///   (SIMPLIFIED: 仅支持直接关闭; UNSHARE (复制 fd 表后关) 与 CLOEXEC
///   (置 close-on-exec) 超出本工程范围, 后续扩展)
pub fn close_range_syscall(first: u32, last: u32, flags: u32) -> Result<usize, Errno> {
    const CLOSE_RANGE_UNSHARE: u32 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;
    const CLOSE_RANGE_ALLOWED: u32 = CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC;

    if first > last {
        return Err(Errno::EINVAL);
    }
    if flags & !CLOSE_RANGE_ALLOWED != 0 {
        return Err(Errno::EINVAL);
    }
    // SIMPLIFIED: UNSHARE/CLOEXEC 暂不实现 (见函数文档)
    if flags != 0 {
        return Err(Errno::ENOSYS);
    }

    // 先收集占用 fd 列表 (释放表锁后再逐个关闭, vfs_close 内部自锁避免死锁)
    let mut fds = alloc::vec::Vec::new();
    if let Some(all) =
        crate::framework::proc::with_current_fd_table(crate::framework::proc::FdTable::get_all_fds)
    {
        for (local, _handle) in all {
            // 有意窄化: 本地 fd 编号 (< MAX_FDS_PER_PROCESS) 截断到 u32
            let fd = local as u32;
            if fd >= first && fd <= last {
                fds.push(fd);
            }
        }
    }
    let mut closed = 0usize;
    for fd in fds {
        crate::framework::fs::api::vfs_close(fd);
        closed += 1;
    }
    Ok(closed)
}

/// preadv(fd, iov, iovcnt, pos) — 显式偏移向量读 (T1 G1 实装)
///
/// 逐 iovec 段委托 framework 机制 `vfs_pread` (显式 offset 读, 不更新 fd
/// 当前偏移). 段读得少于请求字节视为 EOF 提前返回.
///
/// # Errors
/// - `pos` 为负 → `EINVAL`
/// - iovec 段校验失败 → `EFAULT`; 其余错误由底层以对应 `Errno` 传播.
pub fn preadv_syscall(fd: i32, iov_ptr: u64, iovcnt: u64, pos: i64) -> Result<usize, Errno> {
    let iovs = read_iovecs(iov_ptr, iovcnt)?;
    if pos < 0 {
        return Err(Errno::EINVAL);
    }
    let mut total = 0usize;
    let mut offset = pos as u64;
    for (base, len) in iovs {
        if len == 0 {
            continue;
        }
        if !raw::check_user_buf(base, len) {
            return Err(Errno::EFAULT);
        }
        // 有意窄化: 资源类型转换, 单段长度截断到 u32 (与 vfs_read 一致)
        let count = core::cmp::min(len, u64::from(u32::MAX)) as u32;
        let n = crate::framework::fs::api::vfs_pread(fd as u32, base as *mut u8, count, offset);
        if n < 0 {
            if total > 0 {
                break;
            }
            return Err(Errno::from_ret(i64::from(n)));
        }
        total += n as usize;
        offset += n as u64;
        if n == 0 || u64::from(n as u32) < len {
            break; // EOF 或部分读
        }
    }
    Ok(total)
}

/// pwritev(fd, iov, iovcnt, pos) — 显式偏移向量写 (T1 G1 实装)
///
/// 逐 iovec 段委托 framework 机制 `vfs_pwrite` (显式 offset 写, 不更新 fd
/// 当前偏移, 不受 O_APPEND 影响). 段写得少于请求字节视为部分写提前返回.
///
/// # Errors
/// - `pos` 为负 → `EINVAL`
/// - iovec 段校验失败 → `EFAULT`; 其余错误由底层以对应 `Errno` 传播.
pub fn pwritev_syscall(fd: i32, iov_ptr: u64, iovcnt: u64, pos: i64) -> Result<usize, Errno> {
    let iovs = read_iovecs(iov_ptr, iovcnt)?;
    if pos < 0 {
        return Err(Errno::EINVAL);
    }
    let mut total = 0usize;
    let mut offset = pos as u64;
    for (base, len) in iovs {
        if len == 0 {
            continue;
        }
        if !raw::check_user_buf(base, len) {
            return Err(Errno::EFAULT);
        }
        // 有意窄化: 资源类型转换, 单段长度截断到 u32 (与 vfs_write 一致)
        let count = core::cmp::min(len, u64::from(u32::MAX)) as u32;
        let n = crate::framework::fs::api::vfs_pwrite(fd as u32, base as *const u8, count, offset);
        if n < 0 {
            if total > 0 {
                break;
            }
            return Err(Errno::from_ret(i64::from(n)));
        }
        total += n as usize;
        offset += n as u64;
        if u64::from(n as u32) < len {
            break; // 部分写
        }
    }
    Ok(total)
}
