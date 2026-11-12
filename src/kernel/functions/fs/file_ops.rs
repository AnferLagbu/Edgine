#![deny(unsafe_code)]
//! 文件操作策略 — ioctl / clock_gettime / poll / chown / truncate / ftruncate / flock
//!
//! 从 privileged/syscall/mod.rs 迁移的策略代码:
//! - ioctl_syscall: 设备 I/O 控制
//! - poll_syscall: 轮询
//! - chown_syscall: 文件属主修改
//! - truncate_syscall / ftruncate_syscall: 文件截断
//! - flock_syscall: BSD 风格文件锁
//!
//! 注: clock_gettime 已迁往 functions::timer::clock (B05-26 时间归位).
//!
//! ## 框内核边界
//! - 100% safe Rust
//! - 通过 privileged 公开 API 访问
//! - 无 unsafe, 无裸指针

use crate::privileged::syscall::Errno;

const TIOCGWINSZ: u64 = 0x5413;
const TCGETS: u64 = 0x5401;

/// ioctl(fd, request, arg) 策略
///
/// fd 路由: userfaultfd **对象** → `functions::fs::uffd` 的 `UFFDIO_*` 处理;
/// 其余走终端/设备策略.
#[must_use]
pub fn ioctl_syscall(fd: i32, request: u64, arg: u64) -> i64 {
    if arg == 0 {
        return Errno::Einval.as_ret();
    }
    // userfaultfd ioctl: 对象类型路由 (Linux 语义: uffd 的 ioctl 只认 UFFDIO_*).
    // 在 OFT 锁外执行, 避免 UFFDIO_COPY / ZEROPAGE 的用户拷贝与页供给落入临界区.
    if let Some((idx, _nonblock)) = super::uffd::resolve_uffd(fd) {
        return super::uffd::ioctl_uffd(idx, request, arg);
    }
    // VFS fd: 交给对象 io_control; 对象未实现 (NotSupported / 无对象) 时
    // 回落终端/默认策略, 保持 stdio 与常规文件既有 ENOTTY 语义.
    if let Some(handle_id) =
        crate::functions::fs::vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))
    {
        let cmd = crate::functions::intcast::low_u32_from_u64(request);
        let arg_usize = crate::functions::intcast::usize_from_u64(arg);
        let outcome = crate::functions::fs::OPEN_FILE_TABLE
            .with_file(handle_id, |of| of.file().io_control(cmd, arg_usize));
        match outcome {
            Some(Ok(ret)) => return crate::functions::intcast::i64_from_usize(ret),
            Some(Err(crate::functions::fs::KernelError::NotSupported)) | None => {}
            Some(Err(e)) => return i64::from(e.as_vfs_ret()),
        }
    }
    match request {
        TIOCGWINSZ => {
            #[repr(C)]
            #[derive(Copy, Clone)]
            struct Winsize {
                row: u16,
                col: u16,
                xpixel: u16,
                ypixel: u16,
            }
            let ws = Winsize {
                row: 25,
                col: 80,
                xpixel: 0,
                ypixel: 0,
            };
            if !crate::privileged::syscall::api::write_struct_to_user(arg, &ws) {
                return Errno::Efault.as_ret();
            }
            0
        }
        TCGETS => Errno::Enosys.as_ret(),
        _ => Errno::Enotty.as_ret(),
    }
}

/// poll(fds, nfds, timeout) 策略
#[must_use]
pub fn poll_syscall(fds_ptr: u64, nfds: u32, _timeout: i32) -> i64 {
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    if fds_ptr == 0 || nfds == 0 {
        return 0;
    }

    let mut ready: i32 = 0;
    let pfd_size = core::mem::size_of::<PollFd>() as u64;

    for i in 0..nfds as usize {
        let offset = i as u64 * pfd_size;
        let mut pfd = PollFd {
            fd: -1,
            events: 0,
            revents: 0,
        };

        if !crate::privileged::syscall::api::read_struct_from_user(fds_ptr + offset, &mut pfd) {
            continue;
        }

        pfd.revents = 0;
        if pfd.fd < 0 {
            // 写回原位
            let _ = crate::privileged::syscall::api::write_struct_to_user(fds_ptr + offset, &pfd);
            continue;
        }
        // 统一对象分发 (fd → OpenFile → File::poll 快照), socket 亦经 SocketFile::poll.
        // fd 有效性改由 per-process fd 表判定 (映射存在即有效, 不存在则不就绪);
        // 每个 fd 至多计一次 ready (POSIX poll 返回有事件的 fd 数).
        if let Some(handle_id) = crate::functions::fs::vfs_get_fd_handle(
            crate::functions::intcast::usize_from_i32(pfd.fd),
        ) {
            let requested = crate::functions::fs::PollEvents::from_bits_truncate(pfd.events);
            pfd.revents = crate::functions::fs::OPEN_FILE_TABLE
                .with_file(handle_id, |of| of.file().poll(requested))
                .map_or(0, |events| events.bits());
            if pfd.revents != 0 {
                ready += 1;
            }
        }

        let _ = crate::privileged::syscall::api::write_struct_to_user(fds_ptr + offset, &pfd);
    }
    i64::from(ready)
}

/// ppoll(fds, nfds, `tmo_p`, sigmask, sigsetsize) 策略 (T1 G5 实装)
///
/// `poll` + `timespec` 超时 + 临时信号屏蔽字 (Linux ABI):
/// - `tmo_p == NULL` → 无限等待; 非 NULL 时解析 `struct timespec` 并校验
/// - `sigmask != NULL` → 等待期间临时替换屏蔽字, 结束后恢复
///   (`functions::proc::signal::with_temporary_sigmask`)
///
/// SIMPLIFIED: 超时未阻塞 — 复用 [`poll_syscall`] 的"单次扫描"语义 (既有 `SYS_POLL`
/// 现状: 事件就绪即返回, 否则立即返回 0, timeout 不生效), 解析出的毫秒值仅作为
/// 参数下传. 影响面: 无就绪事件时 ppoll 立即返回 0 而非等到超时/事件, 用户态
/// 轮询式多路复用仍可用 (与 `epoll_wait` 的 timeout > 0 缺口一致). 何时需扩展:
/// hrtimer 定时唤醒接入 poll 等待队列后, 由 `poll_syscall` 消费该 timeout 值.
#[must_use]
pub fn ppoll_syscall(
    fds_ptr: u64,
    nfds: u32,
    tmo_p: u64,
    sigmask_ptr: u64,
    sigsetsize: u64,
) -> i64 {
    /// `struct timespec` 中 `tv_nsec` 合法上界 (半开区间)
    const NSEC_PER_SEC: i64 = 1_000_000_000;

    let mut timeout_ms: i32 = -1;
    if tmo_p != 0 {
        let Some(sec_raw) = crate::privileged::syscall::api::read_u64_from_user(tmo_p) else {
            return Errno::Efault.as_ret();
        };
        let Some(nsec_raw) = crate::privileged::syscall::api::read_u64_from_user(tmo_p + 8) else {
            return Errno::Efault.as_ret();
        };
        if sec_raw > i64::MAX as u64 || nsec_raw > i64::MAX as u64 {
            return Errno::Einval.as_ret();
        }
        let (sec, nsec) = (sec_raw.cast_signed(), nsec_raw.cast_signed());
        if sec < 0 || !(0..NSEC_PER_SEC).contains(&nsec) {
            return Errno::Einval.as_ret();
        }
        // 毫秒向上取整; 超 i32 上限饱和 (Linux 由调用方保证取值范围)
        let ms = sec
            .saturating_mul(1000)
            .saturating_add((nsec + 999_999) / 1_000_000);
        timeout_ms = crate::functions::intcast::low_i32_from_i64(ms.min(i64::from(i32::MAX)));
    }

    match crate::functions::proc::signal::with_temporary_sigmask(sigmask_ptr, sigsetsize, || {
        Ok(poll_syscall(fds_ptr, nfds, timeout_ms))
    }) {
        Ok(ret) => ret,
        Err(e) => e.as_ret(),
    }
}

/// chown(path, uid, gid) 策略
#[must_use]
pub fn chown_syscall(path_ptr: u64, uid: u32, gid: u32) -> i64 {
    if path_ptr == 0 || !crate::privileged::syscall::api::validate_user_ptr(path_ptr) {
        return Errno::Efault.as_ret();
    }
    let path = path_ptr as *const u8;
    let tbl = crate::privileged::sgeg::identity::get_table();
    // B06-02: UID/GID 未注册时返回 EINVAL, 不得默认 root (原 map_or(0, ...) 存在提权漏洞)
    let owner_pwm = match tbl.find_by_uid(uid) {
        Some(e) => e.get_pwm().0,
        None => return Errno::Einval.as_ret(),
    };
    let group_pwm = match tbl.find_by_uid(gid) {
        Some(e) => e.get_pwm().0,
        None => return Errno::Einval.as_ret(),
    };
    let pwm = crate::privileged::sgeg::pwm_get_current();
    i64::from(crate::functions::fs::vfs_chown_ext(
        path, owner_pwm, group_pwm, pwm,
    ))
}

/// truncate(path, length) 策略
#[must_use]
pub fn truncate_syscall(path_ptr: u64, length: i64) -> i64 {
    if path_ptr == 0 || !crate::privileged::syscall::api::validate_user_ptr(path_ptr) || length < 0
    {
        return Errno::Einval.as_ret();
    }
    let path = path_ptr as *const u8;
    let fd = crate::functions::fs::vfs_open(path, 0o2, crate::privileged::sgeg::pwm_get_current());
    if fd < 0 {
        return Errno::Enoent.as_ret();
    }
    let result =
        crate::functions::fs::vfs_truncate_internal(fd.cast_unsigned(), length.cast_unsigned());
    let _ = crate::functions::fs::vfs_close(fd.cast_unsigned());
    if result < 0 { Errno::Eio.as_ret() } else { 0 }
}

/// ftruncate(fd, length) 策略
#[must_use]
pub fn ftruncate_syscall(fd: i32, length: i64) -> i64 {
    if fd < 0 || length < 0 {
        return Errno::Einval.as_ret();
    }
    let result =
        crate::functions::fs::vfs_truncate_internal(fd.cast_unsigned(), length.cast_unsigned());
    if result < 0 { Errno::Eio.as_ret() } else { 0 }
}

/// `AT_FDCWD` — 相对路径基于当前工作目录 (Linux ABI)
const AT_FDCWD: i32 = -100;

/// fchownat(dirfd, path, uid, gid, flags) 策略 (T1 G1 实装)
///
/// SIMPLIFIED: 仅支持 `dirfd == AT_FDCWD` (绝对路径语义, 复用 `chown_syscall`
/// 的 UID/GID→PWM 查表 + `vfs_chown_ext`); 非 `AT_FDCWD` 的目录 fd 相对路径
/// 需 VFS 目录 fd 解析机制, 暂不支持. `flags` (`AT_SYMLINK_NOFOLLOW`) 忽略
/// (`vfs_chown_ext` 不跟随 symlink).
#[must_use]
pub fn fchownat_syscall(dirfd: i32, path_ptr: u64, uid: u32, gid: u32, _flags: i32) -> i64 {
    if dirfd != AT_FDCWD {
        return Errno::Enotsup.as_ret();
    }
    chown_syscall(path_ptr, uid, gid)
}

/// fallocate(fd, mode, offset, len) 策略 (T1 G1 实装)
///
/// SIMPLIFIED: 仅支持 `mode == 0` (分配并扩展文件大小到 `offset+len`,
/// Linux fallocate 默认语义); 其他 mode 标志 (`KEEP_SIZE` 等) 暂不实现.
/// 基于 `vfs_fstat_safe` + `vfs_truncate_internal` (仅扩展不缩小) 近似.
#[must_use]
pub fn fallocate_syscall(fd: i32, mode: i32, offset: u64, len: u64) -> i64 {
    if mode != 0 {
        return Errno::Enosys.as_ret();
    }
    if fd < 0 {
        return Errno::Ebadf.as_ret();
    }
    let Some(end) = offset.checked_add(len) else {
        return Errno::Efbig.as_ret();
    };
    // 仅扩展不缩小: 目标大小超过当前 size 时才截断扩展
    let cur_size = crate::functions::fs::api::vfs_fstat_safe(
        fd.cast_unsigned(),
        crate::privileged::sgeg::pwm_get_current(),
    )
    .map_or(0, |st| u64::from(st.size));
    if end > cur_size {
        let r = crate::functions::fs::vfs_truncate_internal(fd.cast_unsigned(), end);
        if r < 0 {
            return Errno::Eio.as_ret();
        }
    }
    0
}

/// flock(fd, operation) 策略
pub fn flock_syscall(fd: i32, operation: i32) -> i64 {
    use crate::functions::fs::{FlockResult, sys_flock as do_flock};

    if fd < 0 {
        return Errno::Ebadf.as_ret();
    }

    // inode 号改由 OpenFile 元数据源提供 (fd → handle → OpenFile.inode_id)
    let ino = {
        let Some(handle_id) =
            crate::functions::fs::vfs_get_fd_handle(crate::functions::intcast::usize_from_i32(fd))
        else {
            return Errno::Ebadf.as_ret();
        };
        match crate::functions::fs::OPEN_FILE_TABLE
            .with_file(handle_id, crate::functions::fs::OpenFile::inode_id)
            .flatten()
        {
            Some(node_id) => node_id,
            None => return Errno::Ebadf.as_ret(),
        }
    };

    let pid = crate::privileged::proc::process_get_current_pid();

    match do_flock(fd, operation, pid, ino) {
        FlockResult::Ok => 0,
        FlockResult::WouldBlock => Errno::Eagain.as_ret(),
        FlockResult::NoSpace => Errno::Enolck.as_ret(),
        FlockResult::Invalid | FlockResult::NotHeld => Errno::Einval.as_ret(),
    }
}

// ============================================================================
// 单元测试 (DECISION-080 双轨: 纯逻辑测试归源侧 #[cfg(test)])
// ============================================================================

#[cfg(test)]
mod tests {
    use super::ppoll_syscall;

    /// 越界用户指针 (`>= USER_ADDR_MAX`): 被 `check_user_buf` 拒绝而非解引用
    const BAD_USER_PTR: u64 = 0x8000_0000_0000_0000;

    /// `ppoll` 参数校验 (nfds == 0 短路 / sigsetsize 校验)
    #[test]
    fn test_ppoll_arg_validation() {
        // nfds == 0 → 0 (无 fd 可扫)
        assert_eq!(ppoll_syscall(0, 0, 0, 0, 0), 0, "nfds=0 应返回 0");
        // 非法 sigsetsize → EINVAL
        assert_eq!(
            ppoll_syscall(0, 0, 0, BAD_USER_PTR, 4),
            -22,
            "sigsetsize != 8 应返回 EINVAL"
        );
        // 越界 timespec 指针 → EFAULT
        assert_eq!(
            ppoll_syscall(0, 0, BAD_USER_PTR, 0, 0),
            -14,
            "非法 timespec 指针应返回 EFAULT"
        );
    }
}
