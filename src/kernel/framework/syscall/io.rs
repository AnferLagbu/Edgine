//! IO 系统调用 (TCB)
//!
//! POSIX 标准的 IO/管道/文件控制:
//! - pipe / pipe2: 匿名管道创建
//! - dup / dup2 / dup3: 文件描述符复制
//! - fcntl: 文件控制

use crate::framework::fs::vfs as vfs_api;
use crate::framework::ipc::pipe as ipc_pipe;
use crate::framework::syscall::Errno;
use crate::framework::syscall::raw;

// ============================================================================
// 管道
// ============================================================================

/// pipe 系统调用
///
/// `fds` 指向 i32`[2]` 数组, 返回读端和写端文件描述符.
pub fn sys_pipe(fds: u64) -> i64 {
    if fds == 0 || !raw::check_user_buf(fds, 8) {
        return Errno::EFAULT.as_ret();
    }

    let mut pipefd: [i32; 2] = [0; 2];
    // SAFETY: pipefd 是栈上有效数组
    let result = unsafe { ipc_pipe::ipc_pipe_create(pipefd.as_mut_ptr()) };
    if result < 0 {
        return Errno::EBUSY.as_ret();
    }
    if pipefd[0] < 0 || pipefd[1] < 0 {
        return Errno::EBUSY.as_ret();
    }
    // SAFETY: fds 由 check_user_buf 验证为可写, 大小 8 = 2 × i32
    unsafe {
        raw::write_u32(fds as *mut u32, pipefd[0] as u32);
        raw::write_u32((fds + 4) as *mut u32, pipefd[1] as u32);
    }
    0
}

/// pipe2 系统调用 (支持 flags)
///
/// # Safety
/// `fds` 必须由调用方先通过 `check_user_buf` 验证为可写的用户缓冲区.
///
/// SIMPLIFIED: flags (`O_CLOEXEC`/`O_NONBLOCK`) 当前忽略, 语义等同 `pipe`;
/// 影响面: `pipe2(O_CLOEXEC)` 创建的 fd 不设置 close-on-exec 标志;
/// 何时需扩展: 在 FD 表实现 close-on-exec 标志后接入 flags 语义.
pub fn sys_pipe2(fds: u64, flags: i32) -> i64 {
    let _ = flags;
    sys_pipe(fds)
}

// ============================================================================
// dup
// ============================================================================

/// dup — 复制文件描述符 (返回新 fd, 取最小可用值)
pub fn sys_dup(oldfd: i32) -> i64 {
    if oldfd < 0 {
        return Errno::EBADF.as_ret();
    }
    i64::from(vfs_api::vfs_dup(oldfd as u32))
}

/// dup2 — 复制文件描述符到 newfd
///
/// 若 newfd 已打开则先关闭. 若 oldfd == newfd 则不关闭直接返回.
pub fn sys_dup2(oldfd: i32, newfd: i32) -> i64 {
    if oldfd < 0 || newfd < 0 {
        return Errno::EBADF.as_ret();
    }
    if oldfd == newfd {
        return i64::from(newfd);
    }
    let result = vfs_api::vfs_dup2(oldfd as u32, newfd as u32);
    if result < 0 {
        return Errno::EBADF.as_ret();
    }
    i64::from(result)
}

/// dup3 — dup2 扩展版, 支持 flags
///
/// flags: `O_CLOEXEC` 等. 当前简化: 等同 dup2.
pub fn sys_dup3(oldfd: i32, newfd: i32, flags: i32) -> i64 {
    if oldfd < 0 || newfd < 0 {
        return Errno::EBADF.as_ret();
    }
    if oldfd == newfd {
        return Errno::EINVAL.as_ret(); // dup3 要求 oldfd != newfd
    }
    // flags 当前被忽略 (未实现 O_CLOEXEC 处理)
    let _ = flags;
    let result = vfs_api::vfs_dup2(oldfd as u32, newfd as u32);
    if result < 0 {
        return Errno::EBADF.as_ret();
    }
    i64::from(result)
}

// ============================================================================
// fcntl
// ============================================================================

const F_DUPFD: i32 = 0;
const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;

/// fd 级 close-on-exec 标志位 (F_GETFD/F_SETFD 的掩码, 与 POSIX `FD_CLOEXEC` 一致)
const FD_CLOEXEC: u64 = 1;

/// fcntl 系统调用
pub fn sys_fcntl(fd: i32, cmd: i32, arg: u64) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
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
                Some(bits) => bits as i64,
                None => Errno::EBADF.as_ret(),
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
                Some(_) => 0,
                None => Errno::EBADF.as_ret(),
            }
        }
        F_GETFL => {
            // B-9.5: fd 元数据改源 OpenFile (per-process fd 表取 handle_id)
            let Some(handle_id) = vfs_api::vfs_get_fd_handle(fd as usize) else {
                return Errno::EBADF.as_ret();
            };
            match crate::framework::fs::OPEN_FILE_TABLE
                .with_file(handle_id, vfs_api::OpenFile::get_flags)
            {
                Some(flags) => i64::from(flags),
                None => Errno::EBADF.as_ret(),
            }
        }
        F_SETFL => 0,
        F_DUPFD => sys_dup2(fd, arg as i32),
        // POSIX record locks (F_SETLK / F_GETLK / F_SETLKW)  // fcntl 文件锁命令
        5 | 6 | 7 => sys_fcntl_posix_lock(fd, cmd, arg),
        _ => Errno::EINVAL.as_ret(),
    }
}

#[expect(
    clippy::comparison_chain,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// fcntl POSIX record lock 处理
///
/// `arg` 指向用户空间的 `flock` 结构体:
///   `l_type`:  i16  (`F_RDLCK=0`, `F_WRLCK=1`, `F_UNLCK=2`)  // 锁类型
///   `l_whence`: i16 (`0=SEEK_SET`, `1=SEEK_CUR`, `2=SEEK_END`)  // 偏移基准
///   `l_start`: i64
///   `l_len`:   i64  (0=到文件末尾)
///   `l_pid`:   i32  (`F_GETLK` 返回冲突锁的 PID)
fn sys_fcntl_posix_lock(fd: i32, cmd: i32, arg: u64) -> i64 {
    use crate::framework::fs::{F_GETLK, PosixLockResult, sys_posix_lock};

    // flock 结构体布局 (与 Linux 兼容):
    // offset 0:  l_type   i16
    // offset 2:  l_whence i16
    // offset 4:  l_start  i64
    // offset 12: l_len    i64
    // offset 20: l_pid    i32
    const FLOCK_STRUCT_SIZE: usize = 24;

    if arg == 0 || !crate::framework::syscall::raw::check_user_buf(arg, FLOCK_STRUCT_SIZE as u64) {
        return Errno::EFAULT.as_ret();
    }

    // 读取用户空间 flock 结构体
    // SAFETY: arg 已通过 check_user_buf 验证
    let (l_type, l_whence, l_start, l_len) = unsafe {
        let ptr = arg as *const u8;
        let l_type = i16::from_ne_bytes([*ptr, *ptr.add(1)]);
        let l_whence = i16::from_ne_bytes([*ptr.add(2), *ptr.add(3)]);
        let l_start = i64::from_ne_bytes([
            *ptr.add(4),
            *ptr.add(5),
            *ptr.add(6),
            *ptr.add(7),
            *ptr.add(8),
            *ptr.add(9),
            *ptr.add(10),
            *ptr.add(11),
        ]);
        let l_len = i64::from_ne_bytes([
            *ptr.add(12),
            *ptr.add(13),
            *ptr.add(14),
            *ptr.add(15),
            *ptr.add(16),
            *ptr.add(17),
            *ptr.add(18),
            *ptr.add(19),
        ]);
        (l_type, l_whence, l_start, l_len)
    };

    // 验证 l_type
    if !(0..=2).contains(&l_type) {
        return Errno::EINVAL.as_ret();
    }

    // 获取 fd 对应的 inode 号 (B-9.5: per-process fd 表 → OpenFile)
    let ino = {
        let Some(handle_id) = vfs_api::vfs_get_fd_handle(fd as usize) else {
            return Errno::EBADF.as_ret();
        };
        match crate::framework::fs::OPEN_FILE_TABLE
            .with_file(handle_id, crate::framework::fs::OpenFile::inode_id)
        {
            Some(ino) => ino,
            None => return Errno::EBADF.as_ret(),
        }
    };

    // 计算 l_start (基于 l_whence)
    let start = match l_whence {
        0 => l_start as u64, // SEEK_SET
        1 => {
            // SEEK_CUR: 当前 offset + l_start (B-9.5: offset 源自共享 OpenFile)
            let Some(handle_id) = vfs_api::vfs_get_fd_handle(fd as usize) else {
                return Errno::EBADF.as_ret();
            };
            match crate::framework::fs::OPEN_FILE_TABLE
                .with_file(handle_id, crate::framework::fs::OpenFile::get_offset)
            {
                Some(offset) => (offset as i64 + l_start) as u64,
                None => return Errno::EBADF.as_ret(),
            }
        }
        2 => {
            // SEEK_END: v1 简化, 不支持 (需要文件大小)
            return Errno::EINVAL.as_ret();
        }
        _ => return Errno::EINVAL.as_ret(),
    };

    let len = if l_len < 0 {
        // 负长度: 从 start 向前锁
        // v1 简化: 不支持负长度
        return Errno::EINVAL.as_ret();
    } else if l_len == 0 {
        0 // 到文件末尾
    } else {
        l_len as u64
    };

    let pid = crate::framework::proc::process_get_current_pid();

    match sys_posix_lock(pid, ino, cmd, i32::from(l_type), start, len) {
        Ok(None) => 0,
        Ok(Some(conflict)) => {
            if cmd == F_GETLK {
                // F_GETLK: 写回冲突信息到用户空间
                // SAFETY: arg 已通过 check_user_buf 验证
                unsafe {
                    let ptr = arg as *mut u8;
                    // 设置 l_type 为冲突锁的类型
                    let ct = conflict.lock_type as i16;
                    *ptr = ct as u8;
                    *ptr.add(1) = (ct >> 8) as u8;
                    // 设置 l_pid 为冲突锁的 PID
                    let cpid = conflict.pid as i32;
                    *ptr.add(20) = cpid as u8;
                    *ptr.add(21) = (cpid >> 8) as u8;
                    *ptr.add(22) = (cpid >> 16) as u8;
                    *ptr.add(23) = (cpid >> 24) as u8;
                }
                0
            } else {
                // F_SETLK / F_SETLKW: 锁被占用
                Errno::EAGAIN.as_ret()
            }
        }
        Err(PosixLockResult::Invalid) => Errno::EINVAL.as_ret(),
        Err(PosixLockResult::NoSpace) => Errno::ENOLCK.as_ret(),
        Err(PosixLockResult::WouldBlock) => Errno::EAGAIN.as_ret(),
    }
}
