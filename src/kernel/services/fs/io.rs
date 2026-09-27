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

/// pipe 系统调用安全代理
///
/// `fds` 指向用户空间 i32`[2]` 数组 (8 字节)
///
/// # Errors
/// 当 `fds` 为空指针时返回 `EFAULT`; 其余错误由底层 `sys_pipe` 以对应 `Errno` 传播.
pub fn pipe_syscall(fds: u64) -> Result<usize, Errno> {
    if fds == 0 {
        return Err(Errno::EFAULT);
    }
    let ret = crate::framework::syscall::io::sys_pipe(fds);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// pipe2 系统调用安全代理 (支持 `flags`)
///
/// `fds` 指向用户空间 i32`[2]` 数组 (8 字节)
///
/// # Errors
/// 当 `fds` 为空指针时返回 `EFAULT`; 其余错误由底层 `sys_pipe2` 以对应 `Errno` 传播.
pub fn pipe2_syscall(fds: u64, flags: i32) -> Result<usize, Errno> {
    if fds == 0 {
        return Err(Errno::EFAULT);
    }
    let ret = crate::framework::syscall::io::sys_pipe2(fds, flags);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// dup 安全代理
///
/// # Errors
/// 当 `oldfd` 为负数时返回 `EBADF`; 其余错误由底层 `sys_dup` 以对应 `Errno` 传播.
pub fn dup_syscall(oldfd: i32) -> Result<usize, Errno> {
    if oldfd < 0 {
        return Err(Errno::EBADF);
    }
    let ret = crate::framework::syscall::io::sys_dup(oldfd);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// dup2 安全代理
///
/// # Errors
/// 当 `oldfd` 或 `newfd` 为负数时返回 `EBADF`; 其余错误由底层 `sys_dup2` 以对应 `Errno` 传播.
pub fn dup2_syscall(oldfd: i32, newfd: i32) -> Result<usize, Errno> {
    if oldfd < 0 || newfd < 0 {
        return Err(Errno::EBADF);
    }
    let ret = crate::framework::syscall::io::sys_dup2(oldfd, newfd);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// dup3 安全代理 (支持 `flags`)
///
/// # Errors
/// 当 `oldfd` 或 `newfd` 为负数时返回 `EBADF`; `oldfd == newfd` 时返回 `EINVAL`
/// (dup3 语义要求两 fd 不同); 其余错误由底层 `sys_dup3` 以对应 `Errno` 传播.
pub fn dup3_syscall(oldfd: i32, newfd: i32, flags: i32) -> Result<usize, Errno> {
    if oldfd < 0 || newfd < 0 {
        return Err(Errno::EBADF);
    }
    let ret = crate::framework::syscall::io::sys_dup3(oldfd, newfd, flags);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// fcntl 安全代理
///
/// # Errors
/// 当 `fd` 为负数时返回 `EBADF`; 其余错误由底层 `sys_fcntl` 以对应 `Errno` 传播.
pub fn fcntl_syscall(fd: i32, cmd: i32, arg: u64) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::EBADF);
    }
    let ret = crate::framework::syscall::io::sys_fcntl(fd, cmd, arg);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
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
