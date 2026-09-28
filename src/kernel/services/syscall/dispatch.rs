#![deny(unsafe_code)]
//! 系统调用分发策略 — services 层
//!
//! T5-1: 将 syscall 号 → 处理函数映射表 (分发策略) 从 framework 提取到 services.
//! framework 仅保留入口汇编 + unsafe 边界, services 拥有完整的分发映射.
//!
//! ## 架构
//!
//! ```text
//! 用户态 → framework 入口 (syscall/sysret 汇编)
//!        → framework::syscall_dispatch_from_frame (unsafe 边界)
//!        → services::syscall::dispatch::ServicesSyscallDispatch::dispatch (策略)
//!        → framework 回退 (未迁移的 syscall)
//! ```
//!
//! ## 迁移状态
//!
//! - 已迁移: 文件 I/O (含 read/write, sendfile, splice), 文件系统, 内存管理,
//!   进程 (含 execve, setrlimit, seccomp, prctl, tcgetpgrp, tcsetpgrp,
//!   unshare, setns, tgkill, waitid, robust_list), 信号, 网络, 凭证, 同步,
//!   定时器, 事件轮询,
//!   eventfd/signalfd/timerfd, io_uring (setup/enter) 与 eBPF (bpf),
//!   kexec (kexec_load) 等, Credo 私有 syscall (含 disk_install/hotplug),
//!   帧缓冲 (fb_open/fb_mmap/fb_release), 存储设备, inotify,
//!   内存建议与锁定, 进程创建/等待, 系统信息, CPU 亲和性, 进程优先级,
//!   设备固件 (fw_load/fw_get/fw_get_info/fw_detach),
//!   内核调试跟踪 (ftrace_enable/disable/read/stat, kgdb_enter)
//! - 待迁移: 路由/Netfilter, cgroup, NUMA, PM, TPM,
//!   CET, tickless, timesync, UEFI 等
//!
//! 评估日期: 2026-06-19

use crate::framework::syscall::Errno;
use crate::framework::syscall::dispatch_trait::{SyscallDispatch, register_syscall_dispatch};

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 services 层 Result 转为 i64 返回码
#[inline]
fn as_ret(r: Result<usize, Errno>) -> i64 {
    match r {
        Ok(v) => v as i64,
        Err(e) => e.as_ret(),
    }
}

// ============================================================================
// services 层系统调用分发策略
// ============================================================================

/// services 层系统调用分发策略
///
/// L-01: 已从 framework 迁移的 syscall 分支在此分发.
/// 返回 -ENOSYS (-38) 表示未处理, framework 回退处理.
pub struct ServicesSyscallDispatch;

impl SyscallDispatch for ServicesSyscallDispatch {
    fn dispatch(&self, num: u64, args: [u64; 6]) -> i64 {
        // M4: 按子系统拆分巨型 match，提高可读性和可维护性
        // 尝试各子系统分发函数，返回第一个匹配的结果
        if let Some(ret) = dispatch_fs(num, args) {
            return ret;
        }
        if let Some(ret) = dispatch_proc(num, args) {
            return ret;
        }
        if let Some(ret) = dispatch_net(num, args) {
            return ret;
        }
        if let Some(ret) = dispatch_mm(num, args) {
            return ret;
        }
        if let Some(ret) = dispatch_sync(num, args) {
            return ret;
        }
        if let Some(ret) = dispatch_credo(num, args) {
            return ret;
        }
        if let Some(ret) = dispatch_other(num, args) {
            return ret;
        }

        // 未匹配的 syscall — 返回 -ENOSYS 让 framework 回退处理
        crate::services::syscall::types::ENOSYS_RET
    }
}

// ============================================================================
// 子系统分发函数
// ============================================================================

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
/// 文件系统相关系统调用
fn dispatch_fs(num: u64, args: [u64; 6]) -> Option<i64> {
    use crate::services::syscall::types::{
        QX_SNAPSHOT_CLONE, QX_SNAPSHOT_CREATE, QX_SNAPSHOT_DESTROY, QX_SNAPSHOT_ROLLBACK,
        SYS_access, SYS_alarm, SYS_chdir, SYS_chmod, SYS_chown, SYS_chroot, SYS_clock_gettime,
        SYS_close, SYS_close_range, SYS_copy_file_range, SYS_creat, SYS_dup, SYS_dup2, SYS_dup3,
        SYS_faccessat, SYS_fallocate, SYS_fchmod, SYS_fchmodat, SYS_fchown, SYS_fchownat,
        SYS_fcntl, SYS_fdatasync, SYS_flock, SYS_fstat, SYS_fsync, SYS_ftruncate, SYS_getcwd,
        SYS_getdents, SYS_getitimer, SYS_getxattr, SYS_inotify_add_watch, SYS_inotify_init,
        SYS_inotify_init1, SYS_inotify_rm_watch, SYS_ioctl, SYS_link, SYS_linkat, SYS_listxattr,
        SYS_lseek, SYS_lstat, SYS_mkdir, SYS_mount, SYS_name_to_handle_at, SYS_newfstatat,
        SYS_open, SYS_open_by_handle_at, SYS_openat, SYS_pipe, SYS_pipe2, SYS_pivot_root, SYS_poll,
        SYS_ppoll, SYS_preadv, SYS_pwritev, SYS_read, SYS_readlink, SYS_readlinkat, SYS_readv,
        SYS_removexattr, SYS_rename, SYS_renameat, SYS_rmdir, SYS_select, SYS_sendfile,
        SYS_setitimer, SYS_setxattr, SYS_splice, SYS_stat, SYS_statx, SYS_symlink, SYS_symlinkat,
        SYS_sync, SYS_time, SYS_times, SYS_truncate, SYS_umask, SYS_umount2, SYS_unlink,
        SYS_unlinkat, SYS_utimensat, SYS_write, SYS_writev,
    };
    let [a0, a1, a2, a3, a4, a5] = args;

    Some(match num {
        // 文件 I/O
        // read/write: fd 严格校验 (用户态可传任意 u64, try_from 失败返回 -EINVAL)
        SYS_read => match i32::try_from(a0) {
            Ok(fd) => as_ret(crate::services::fs::io::read_syscall(fd, a1, a2)),
            Err(_) => Errno::EINVAL.as_ret(),
        },
        SYS_write => match i32::try_from(a0) {
            Ok(fd) => as_ret(crate::services::fs::io::write_syscall(fd, a1, a2)),
            Err(_) => Errno::EINVAL.as_ret(),
        },
        // readv/writev (T1 G1 实装): 向量 I/O, 逐 iovec 段委托 read/write
        SYS_readv => match i32::try_from(a0) {
            Ok(fd) => as_ret(crate::services::fs::io::readv_syscall(fd, a1, a2)),
            Err(_) => Errno::EINVAL.as_ret(),
        },
        SYS_writev => match i32::try_from(a0) {
            Ok(fd) => as_ret(crate::services::fs::io::writev_syscall(fd, a1, a2)),
            Err(_) => Errno::EINVAL.as_ret(),
        },
        // close_range (T1 G1 实装): 批量关闭 fd
        SYS_close_range => as_ret(crate::services::fs::io::close_range_syscall(
            a0 as u32, a1 as u32, a2 as u32,
        )),
        // preadv/pwritev (T1 G1 实装): 显式偏移向量 I/O (pos 为负时 -EINVAL)
        SYS_preadv => match i32::try_from(a0) {
            Ok(fd) => as_ret(crate::services::fs::io::preadv_syscall(
                fd, a1, a2, a3 as i64,
            )),
            Err(_) => Errno::EINVAL.as_ret(),
        },
        SYS_pwritev => match i32::try_from(a0) {
            Ok(fd) => as_ret(crate::services::fs::io::pwritev_syscall(
                fd, a1, a2, a3 as i64,
            )),
            Err(_) => Errno::EINVAL.as_ret(),
        },
        SYS_open => as_ret(crate::services::fs::open::open_syscall(
            a0, a1 as i32, a2 as i32,
        )),
        SYS_close => as_ret(crate::services::fs::open::close_syscall(a0 as i32)),
        SYS_stat => as_ret(crate::services::fs::stat::stat_syscall(a0, a1)),
        SYS_fstat => as_ret(crate::services::fs::stat::fstat_syscall(a0 as i32, a1)),
        SYS_lstat => as_ret(crate::services::fs::stat::lstat_syscall(a0, a1)),
        // statx (T1 G1 实装): 扩展文件状态 (Linux struct statx)
        SYS_statx => as_ret(crate::services::fs::stat::statx_syscall(
            a0 as i32, a1, a2 as u32, a3 as u32, a4,
        )),
        // utimensat (T1 G1 实装): 设置文件时间戳 (仅 AT_FDCWD)
        SYS_utimensat => crate::services::fs::stat::utimensat_syscall(a0 as i32, a1, a2, a3 as i32),
        SYS_creat => as_ret(crate::services::fs::open::creat_syscall(a0, a2 as i32)),

        // 文件系统操作
        SYS_mkdir => as_ret(crate::services::fs::mode::mkdir_syscall(a0, a1 as i32)),
        SYS_rmdir => as_ret(crate::services::fs::mode::rmdir_syscall(a0)),
        SYS_chmod => as_ret(crate::services::fs::mode::chmod_syscall(a0, a1 as u32)),
        SYS_fchmod => as_ret(crate::services::fs::mode::fchmod_syscall(
            a0 as i32, a1 as u32,
        )),
        SYS_umask => as_ret(crate::services::fs::mode::umask_syscall(a0 as u32)),
        SYS_access => as_ret(crate::services::fs::access::access_syscall(a0, a1 as i32)),
        SYS_unlink => as_ret(crate::services::fs::access::unlink_syscall(a0)),
        SYS_rename => as_ret(crate::services::fs::misc::rename_syscall(a0, a1)),
        SYS_symlink => as_ret(crate::services::fs::link::symlink_syscall(a0, a1)),
        SYS_readlink => as_ret(crate::services::fs::link::readlink_syscall(a0, a1, a2)),
        SYS_link => as_ret(crate::services::fs::link::link_syscall(a0, a1)),

        // *at() 系列
        SYS_openat => as_ret(crate::services::fs::open::open_syscall(
            a1, a2 as i32, a3 as i32,
        )),
        SYS_newfstatat => as_ret(crate::services::fs::stat::fstat_syscall(a1 as i32, a2)),
        SYS_unlinkat => as_ret(crate::services::fs::access::unlink_syscall(a1)),
        SYS_renameat => as_ret(crate::services::fs::misc::rename_syscall(a1, a3)),
        SYS_linkat => as_ret(crate::services::fs::link::link_syscall(a1, a3)),
        SYS_symlinkat => as_ret(crate::services::fs::link::symlink_syscall(a0, a2)),
        SYS_readlinkat => as_ret(crate::services::fs::link::readlink_syscall(a1, a2, a3)),
        SYS_fchmodat => as_ret(crate::services::fs::mode::chmod_syscall(a1, a2 as u32)),
        SYS_faccessat => as_ret(crate::services::fs::access::faccessat_syscall(
            a0 as i32, a1, a2 as i32, a3 as i32,
        )),
        SYS_fchown => as_ret(crate::services::fs::misc::fchown_syscall(a0 as i32, a1, a2)),
        // fchownat (T1 G1 实装): dirfd 相对路径 (当前仅 AT_FDCWD)
        SYS_fchownat => crate::services::fs::file_ops::fchownat_syscall(
            a0 as i32, a1, a2 as u32, a3 as u32, a4 as i32,
        ),

        // 同步与挂载
        SYS_sync => as_ret(crate::services::fs::misc::sync_syscall()),
        SYS_fsync => as_ret(crate::services::fs::misc::fsync_syscall(a0 as i32)),
        // fdatasync (分册 9 批次 3): 与 fsync 同语义复用 (VFS 整体同步, 无数据/元数据区分)
        SYS_fdatasync => as_ret(crate::services::fs::misc::fsync_syscall(a0 as i32)),
        SYS_mount => as_ret(crate::services::fs::mount::mount_syscall(a0, a1, a2)),
        SYS_umount2 => as_ret(crate::services::fs::mount::umount2_syscall(a0, a1 as i32)),

        // 路径
        SYS_getcwd => as_ret(crate::services::fs::path::getcwd_syscall(a0, a1)),
        SYS_chdir => as_ret(crate::services::fs::path::chdir_syscall(a0)),
        // chroot/pivot_root (T1 G7): 切换视图根, 根前缀经 resolve_user_path 生效
        SYS_chroot => as_ret(crate::services::fs::path::chroot_syscall(a0)),
        SYS_pivot_root => as_ret(crate::services::fs::path::pivot_root_syscall(a0, a1)),

        // 文件描述符操作
        SYS_pipe => as_ret(crate::services::fs::io::pipe_syscall(a0)),
        SYS_pipe2 => as_ret(crate::services::fs::io::pipe2_syscall(a0, a2 as i32)),
        SYS_dup => as_ret(crate::services::fs::io::dup_syscall(a0 as i32)),
        SYS_dup2 => as_ret(crate::services::fs::io::dup2_syscall(a0 as i32, a1 as i32)),
        SYS_dup3 => as_ret(crate::services::fs::io::dup3_syscall(
            a0 as i32, a1 as i32, a2 as i32,
        )),
        SYS_fcntl => as_ret(crate::services::fs::io::fcntl_syscall(
            a0 as i32, a1 as i32, a2,
        )),

        // 文件操作
        SYS_ioctl => crate::services::fs::file_ops::ioctl_syscall(a0 as i32, a1, a2),
        SYS_poll => crate::services::fs::file_ops::poll_syscall(a0, a1 as u32, a2 as i32),
        // ppoll (T1 G5 实装): poll + timespec 超时 + 临时信号屏蔽字
        SYS_ppoll => crate::services::fs::file_ops::ppoll_syscall(a0, a1 as u32, a2, a3, a4),
        SYS_select => crate::services::fs::file_ops::poll_syscall(a0, a1 as u32, a2 as i32),
        SYS_chown => crate::services::fs::file_ops::chown_syscall(a0, a1 as u32, a2 as u32),
        SYS_truncate => crate::services::fs::file_ops::truncate_syscall(a0, a1 as i64),
        SYS_ftruncate => crate::services::fs::file_ops::ftruncate_syscall(a0 as i32, a1 as i64),
        // fallocate (T1 G1 实装): 预分配 (仅 mode=0, 扩展文件大小)
        SYS_fallocate => {
            crate::services::fs::file_ops::fallocate_syscall(a0 as i32, a1 as i32, a2, a3)
        }
        SYS_flock => crate::services::fs::file_ops::flock_syscall(a0 as i32, a1 as i32),
        SYS_lseek => crate::services::fs::dir_ops::lseek_syscall(a0 as i32, a1 as i64, a2 as i32),
        SYS_getdents => crate::services::fs::dir_ops::getdents_syscall(a0 as i32, a1, a2),

        // inotify
        SYS_inotify_init1 => crate::services::fs::inotify::sys_inotify_init1(a0 as i32),
        // inotify_init (T1 G5 实装): 遗留接口, 等价 inotify_init1(0)
        SYS_inotify_init => crate::services::fs::inotify::sys_inotify_init1(0),
        SYS_inotify_add_watch => {
            crate::services::fs::inotify::sys_inotify_add_watch(a0 as i64, a1 as u32, a2 as u32)
        }
        SYS_inotify_rm_watch => {
            crate::services::fs::inotify::sys_inotify_rm_watch(a0 as i64, a1 as i32)
        }

        // 时间与统计
        SYS_clock_gettime => crate::services::timer::clock::clock_gettime_syscall(a0 as i32, a1),
        SYS_times => as_ret(crate::services::fs::misc::times_syscall(a0)),
        SYS_time => as_ret(crate::services::fs::misc::time_syscall(a0)),
        SYS_getitimer => as_ret(crate::services::fs::misc::getitimer_syscall(a0 as i32, a1)),
        SYS_alarm => as_ret(crate::services::fs::misc::alarm_syscall(a0 as u32)),
        SYS_setitimer => as_ret(crate::services::fs::misc::setitimer_syscall(
            a0 as i32, a1, a2,
        )),

        // 高级文件操作
        SYS_copy_file_range => as_ret(crate::services::fs::io::copy_file_range_syscall(
            a0 as i32,
            a1,
            a2 as i32,
            a3,
            a4 as usize,
        )),
        // sendfile / splice (T2 批 3, syscall-followup; 阶段 2-A framekernel):
        // 零拷贝数据传输, 权威实现已完整下沉 services::fs::sendfile (0 unsafe),
        // 用户指针经 framework 安全 API, VFS/IPC 经 framework 公开 safe API.
        SYS_sendfile => {
            crate::services::fs::sendfile::sys_sendfile(a0 as i32, a1 as i32, a2, a3 as usize)
        }
        SYS_splice => crate::services::fs::sendfile::sys_splice(
            a0 as i32,
            a1,
            a2 as i32,
            a3,
            a4 as usize,
            a5 as u32,
        ),
        SYS_name_to_handle_at => {
            // 显式错误透传: 具体 Errno 而非通用负值 (B05-41 返工)
            match crate::services::fs::file_handle::name_to_handle_at_syscall(
                a0 as i32, a1, a2 as i32, a3, a4 as u64, a5 as u32,
            ) {
                Ok(v) => v,
                Err(e) => e.as_ret(),
            }
        }
        SYS_open_by_handle_at => {
            // 显式错误透传: 具体 Errno 而非通用负值 (B05-41 返工)
            match crate::services::fs::file_handle::open_by_handle_at_syscall(
                a0 as i32, a1, a2 as i32, a3 as u32,
            ) {
                Ok(v) => v,
                Err(e) => e.as_ret(),
            }
        }

        // 扩展属性 (B09-17: QX_SETXATTR 890 → SYS_setxattr 188, Linux 编号空间归位)
        SYS_setxattr => as_ret(crate::services::fs::xattr::setxattr_syscall(
            a0,
            a1,
            a2,
            a3 as usize,
            a5,
        )),
        SYS_getxattr => as_ret(crate::services::fs::xattr::getxattr_syscall(
            a0,
            a1,
            a2,
            a3 as usize,
            a5,
        )),
        SYS_listxattr => as_ret(crate::services::fs::xattr::listxattr_syscall(
            a0,
            a1,
            a2 as usize,
            a4,
        )),
        SYS_removexattr => as_ret(crate::services::fs::xattr::removexattr_syscall(a0, a1, a4)),

        // 快照
        QX_SNAPSHOT_CREATE => as_ret(crate::services::fs::snapshot::snapshot_create_syscall(a0)),
        QX_SNAPSHOT_DESTROY => as_ret(crate::services::fs::snapshot::snapshot_destroy_syscall(a0)),
        QX_SNAPSHOT_ROLLBACK => {
            as_ret(crate::services::fs::snapshot::snapshot_rollback_syscall(a0))
        }
        QX_SNAPSHOT_CLONE => as_ret(crate::services::fs::snapshot::snapshot_clone_syscall(
            a0, a1,
        )),

        _ => return None,
    })
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
/// 进程相关系统调用
fn dispatch_proc(num: u64, args: [u64; 6]) -> Option<i64> {
    use crate::services::syscall::types::{
        SYS_adjtimex, SYS_arch_prctl, SYS_clock_nanosleep, SYS_clone, SYS_clone3, SYS_execve,
        SYS_execveat, SYS_exit, SYS_exit_group, SYS_fork, SYS_get_robust_list, SYS_getpgid,
        SYS_getpid, SYS_getppid, SYS_getpriority, SYS_getrlimit, SYS_getrusage, SYS_getsid,
        SYS_gettid, SYS_gettimeofday, SYS_kill, SYS_memfd_create, SYS_nanosleep, SYS_nice,
        SYS_pidfd_getfd, SYS_pidfd_open, SYS_pidfd_send_signal, SYS_prctl, SYS_process_vm_readv,
        SYS_process_vm_writev, SYS_reboot, SYS_rt_sigaction, SYS_rt_sigprocmask,
        SYS_sched_getaffinity, SYS_sched_setaffinity, SYS_sched_yield, SYS_seccomp,
        SYS_set_robust_list, SYS_setdomainname, SYS_sethostname, SYS_setns, SYS_setpgid,
        SYS_setpriority, SYS_setrlimit, SYS_setsid, SYS_settimeofday, SYS_sigaltstack, SYS_sysinfo,
        SYS_tcgetpgrp, SYS_tcsetpgrp, SYS_tgkill, SYS_uname, SYS_unshare, SYS_wait4, SYS_waitid,
    };
    let [a0, a1, a2, a3, a4, a5] = args;

    Some(match num {
        // 进程信息
        SYS_getpid => crate::services::proc::info::getpid_syscall() as i64,
        SYS_getppid => crate::services::proc::info::getppid_syscall() as i64,
        SYS_getpgid => as_ret(crate::services::proc::info::getpgid_syscall(a0 as i32)),
        SYS_gettid => crate::services::proc::info::gettid_syscall() as i64,
        SYS_setsid => crate::services::proc::session::proc_setsid(),
        SYS_getsid => crate::services::proc::session::proc_getsid(a0 as i32),
        SYS_setpgid => crate::services::proc::session::proc_setpgid(a0 as i32, a1 as i32),
        SYS_tcgetpgrp => crate::services::proc::session::tcgetpgrp_syscall(a0 as i32),
        SYS_tcsetpgrp => crate::services::proc::session::tcsetpgrp_syscall(a0 as i32, a1 as i32),

        // seccomp / prctl (T2 批 2, syscall-followup)
        SYS_seccomp => crate::services::proc::seccomp::seccomp_syscall(a0 as u32, a1 as u32, a2),
        SYS_prctl => crate::services::proc::seccomp::prctl_syscall(a0 as i64, a1, a2, a3, a4),

        // namespace (T2 批 4, syscall-followup)
        SYS_unshare => crate::services::proc::namespace::unshare_syscall(a0),
        SYS_setns => crate::services::proc::namespace::setns_syscall(a0, a1),

        // 信号
        SYS_rt_sigaction => as_ret(crate::services::proc::signal::rt_sigaction_syscall(
            a0 as i32, a1, a2,
        )),
        SYS_rt_sigprocmask => as_ret(crate::services::proc::signal::rt_sigprocmask_syscall(
            a0 as i32, a1, a2,
        )),
        SYS_kill => as_ret(crate::services::proc::signal::kill_syscall(
            a0 as i32, a1 as i32,
        )),
        SYS_tgkill => as_ret(crate::services::proc::signal::tgkill_syscall(
            a0 as i32, a1 as i32, a2 as i32,
        )),
        // sigaltstack (分册 9 批次 3): 替代栈注册/查询, 委托 framework TCB
        SYS_sigaltstack => as_ret(crate::services::proc::signal::sigaltstack_syscall(a0, a1)),

        // 线程本地存储 (分册 9 批次 3)
        SYS_arch_prctl => as_ret(crate::services::proc::clone::arch_prctl_syscall(a0, a1)),

        // 进程优先级
        SYS_nice => crate::services::proc::priority::nice_syscall(a0 as i32),
        SYS_getpriority => {
            crate::services::proc::priority::getpriority_syscall(a0 as i32, a1 as u32)
        }
        SYS_setpriority => {
            crate::services::proc::priority::setpriority_syscall(a0 as i32, a1 as u32, a2 as i32)
        }

        // CPU 亲和性
        SYS_sched_setaffinity => {
            crate::services::proc::affinity::sched_setaffinity_syscall(a0 as i32, a1 as u32, a2)
        }
        SYS_sched_getaffinity => {
            crate::services::proc::affinity::sched_getaffinity_syscall(a0 as i32, a1 as u32, a2)
        }

        // 进程生命周期
        SYS_fork => crate::services::proc::lifecycle::fork_syscall(),
        // execve: 进程替换 (path/argv 为用户指针, envp 当前忽略)
        SYS_execve => as_ret(crate::services::proc::exec::execve_syscall(a0, a1, a2)),
        // execveat (T1 G7): execve 的目录 fd 相对版本 (仅 AT_FDCWD)
        SYS_execveat => as_ret(crate::services::proc::exec::execveat_syscall(
            a0 as i32, a1, a2, a3, a4 as i32,
        )),
        SYS_exit => crate::services::proc::lifecycle::exit_syscall(a0 as i32),
        // SIMPLIFIED: exit_group 暂等同 exit (B05-43 返工登记); 影响面: 线程组未实现
        // 组级终止, 仅结束当前进程; 何时需扩展: 引入 tgid/线程组基础结构后遍历组内
        // 全部线程终止 (审查 DECISION-071 关联).
        SYS_exit_group => crate::services::proc::lifecycle::exit_syscall(a0 as i32),
        SYS_sched_yield => crate::services::proc::lifecycle::sched_yield_syscall(),

        // 系统信息
        SYS_getrusage => crate::services::proc::sysinfo::getrusage_syscall(a0 as i32, a1),
        SYS_sysinfo => crate::services::proc::sysinfo::sysinfo_syscall(a0),
        SYS_getrlimit => crate::services::proc::sysinfo::getrlimit_syscall(a0 as i32, a1),
        SYS_setrlimit => crate::services::proc::sysinfo::setrlimit_syscall(a0 as i32, a1),
        SYS_uname => as_ret(crate::services::proc::info::uname_syscall(a0)),
        SYS_gettimeofday => as_ret(crate::services::timer::clock::gettimeofday_syscall(a0)),
        // T1 G6: 时间组 — 墙钟设置 / 时钟调整 / 带时钟源的睡眠
        SYS_settimeofday => as_ret(crate::services::timer::clock::settimeofday_syscall(a0, a1)),
        SYS_adjtimex => crate::services::timer::clock::adjtimex_syscall(a0),
        SYS_clock_nanosleep => as_ret(crate::services::timer::clock::clock_nanosleep_syscall(
            a0 as i32, a1 as i32, a2, a3,
        )),

        // 定时器
        SYS_nanosleep => as_ret(crate::services::timer::sleep::nanosleep_syscall(a0, a1)),

        // 进程创建/等待
        SYS_clone => as_ret(crate::services::proc::clone::clone_syscall(
            a0, a1, a2, a3, a4,
        )),
        SYS_clone3 => {
            // clone3(2): 首个参数指向用户空间 `struct clone_args`.
            // SIMPLIFIED: 仅提取 flags/stack/parent_tid/child_tid/tls 五个字段委托
            // `clone_syscall`, 忽略 pidfd/set_tid/cgroup/exit_signal 等高级字段;
            // 影响面: 使用这些高级字段的调用方 (如线程库 clone3 路径) 语义不完整;
            // 何时需扩展: 完整实现 clone3 (独立 sys_clone3 机制, 支持全部字段) 后替换.
            #[repr(C)]
            #[derive(Copy, Clone)]
            struct CloneArgs {
                flags: u64,
                pidfd: u64,
                child_tid: u64,
                parent_tid: u64,
                exit_signal: u64,
                stack: u64,
                stack_size: u64,
                tls: u64,
            }
            let mut args = CloneArgs {
                flags: 0,
                pidfd: 0,
                child_tid: 0,
                parent_tid: 0,
                exit_signal: 0,
                stack: 0,
                stack_size: 0,
                tls: 0,
            };
            if !crate::framework::syscall::api::read_struct_from_user(a0, &mut args) {
                return Some(Errno::EFAULT.as_ret());
            }
            as_ret(crate::services::proc::clone::clone_syscall(
                args.flags,
                args.stack,
                args.parent_tid,
                args.child_tid,
                args.tls,
            ))
        }
        SYS_wait4 => as_ret(crate::services::proc::wait4::wait4_syscall(
            a0 as i32, a1, a2 as i32,
        )),
        SYS_waitid => as_ret(crate::services::proc::wait4::waitid_syscall(
            a0 as i32, a1, a2, a3 as i32,
        )),
        SYS_set_robust_list => as_ret(crate::services::proc::clone::set_robust_list_syscall(
            a0, a1,
        )),
        SYS_get_robust_list => as_ret(crate::services::proc::clone::get_robust_list_syscall(
            a0 as i32, a1, a2,
        )),

        // 系统信息
        SYS_reboot => crate::services::proc::sysinfo::reboot_syscall(a0 as i32),
        SYS_sethostname => crate::services::proc::sysinfo::sethostname_syscall(a0, a1),
        // setdomainname (T1 G7): 与 sethostname 同构, 写入当前进程 UTS namespace
        SYS_setdomainname => crate::services::proc::sysinfo::setdomainname_syscall(a0, a1),

        // memfd
        SYS_memfd_create => as_ret(crate::services::proc::memfd::memfd_create_syscall(
            a0, a1 as u32,
        )),

        // process_vm (T1 G2): 跨进程用户内存向量读写
        SYS_process_vm_readv => {
            as_ret(crate::services::proc::process_vm::process_vm_readv_syscall(
                a0 as i32, a1, a2, a3, a4, a5,
            ))
        }
        SYS_process_vm_writev => as_ret(
            crate::services::proc::process_vm::process_vm_writev_syscall(
                a0 as i32, a1, a2, a3, a4, a5,
            ),
        ),

        // pidfd
        SYS_pidfd_open => as_ret(crate::services::proc::pidfd::pidfd_open(
            a0 as u32, a1 as u32,
        )),
        SYS_pidfd_send_signal => as_ret(crate::services::proc::pidfd::pidfd_send_signal(
            a0 as u32, a1 as i32, a2, a3 as u32,
        )),
        SYS_pidfd_getfd => as_ret(crate::services::proc::pidfd::pidfd_getfd(
            a0 as u32, a1 as u32, a2 as u32,
        )),

        _ => return None,
    })
}

/// 网络相关系统调用
fn dispatch_net(num: u64, args: [u64; 6]) -> Option<i64> {
    use crate::services::syscall::types::{
        SYS_accept, SYS_bind, SYS_connect, SYS_getpeername, SYS_getsockname, SYS_getsockopt,
        SYS_listen, SYS_recvfrom, SYS_recvmmsg, SYS_recvmsg, SYS_sendmmsg, SYS_sendmsg, SYS_sendto,
        SYS_setsockopt, SYS_shutdown, SYS_socket, SYS_socketpair,
    };
    let [a0, a1, a2, a3, a4, a5] = args;

    Some(match num {
        SYS_socket => as_ret(crate::services::net::syscall::socket_syscall(
            a0 as i32, a1 as i32, a2 as i32,
        )),
        SYS_connect => as_ret(crate::services::net::syscall::connect_syscall(
            a0 as i32, a1, a2 as u32,
        )),
        SYS_accept => as_ret(crate::services::net::syscall::accept_syscall(
            a0 as i32, a1, a2,
        )),
        SYS_sendto => as_ret(crate::services::net::syscall::sendto_syscall(
            a0 as i32, a1, a2 as u32, a3 as i32, a4, a5 as u32,
        )),
        SYS_recvfrom => as_ret(crate::services::net::syscall::recvfrom_syscall(
            a0 as i32, a1, a2 as u32, a3 as i32, a4, a5,
        )),
        SYS_shutdown => as_ret(crate::services::net::syscall::shutdown_syscall(
            a0 as i32, a1 as i32,
        )),
        SYS_bind => as_ret(crate::services::net::syscall::bind_syscall(
            a0 as i32, a1, a2 as u32,
        )),
        SYS_listen => as_ret(crate::services::net::syscall::listen_syscall(
            a0 as i32, a1 as i32,
        )),
        SYS_sendmsg => as_ret(crate::services::net::syscall::sendmsg_syscall(
            a0 as i32, a1, a2 as i32,
        )),
        SYS_recvmsg => as_ret(crate::services::net::syscall::recvmsg_syscall(
            a0 as i32, a1, a2 as i32,
        )),
        // recvmmsg/sendmmsg/socketpair (T1 G3 实装): UDS 分流在 services 层
        SYS_recvmmsg => as_ret(crate::services::net::syscall::recvmmsg_syscall(
            a0 as i32, a1, a2 as u32, a3 as u32, a4,
        )),
        SYS_sendmmsg => as_ret(crate::services::net::syscall::sendmmsg_syscall(
            a0 as i32, a1, a2 as u32, a3 as u32,
        )),
        SYS_socketpair => as_ret(crate::services::net::syscall::socketpair_syscall(
            a0 as i32, a1 as i32, a2 as i32, a3,
        )),
        SYS_setsockopt => as_ret(crate::services::net::syscall::setsockopt_syscall(
            a0 as i32, a1 as i32, a2 as i32, a3, a4 as u32,
        )),
        SYS_getsockopt => as_ret(crate::services::net::syscall::getsockopt_syscall(
            a0 as i32, a1 as i32, a2 as i32, a3, a4,
        )),
        SYS_getsockname => as_ret(crate::services::net::syscall::getsockname_syscall(
            a0 as i32, a1, a2,
        )),
        SYS_getpeername => as_ret(crate::services::net::syscall::getpeername_syscall(
            a0 as i32, a1, a2,
        )),

        _ => return None,
    })
}

/// 内存管理相关系统调用
fn dispatch_mm(num: u64, args: [u64; 6]) -> Option<i64> {
    use crate::services::syscall::types::{
        SYS_brk, SYS_get_mempolicy, SYS_getcpu, SYS_madvise, SYS_mbind, SYS_migrate_pages,
        SYS_mincore, SYS_mlock, SYS_mlockall, SYS_mmap, SYS_mprotect, SYS_mremap, SYS_munlock,
        SYS_munlockall, SYS_munmap, SYS_set_mempolicy, SYS_userfaultfd,
    };
    let [a0, a1, a2, a3, a4, a5] = args;

    Some(match num {
        // 基础内存管理
        SYS_mprotect => as_ret(crate::services::mm::mprotect::mprotect_syscall(
            a0, a1, a2 as i32,
        )),
        SYS_brk => as_ret(crate::services::mm::brk::brk_syscall(a0)),

        // mmap 系列
        SYS_mmap => crate::services::mm::mmap::mmap_syscall_entry(
            a0, a1, a2 as i32, a3 as i32, a4 as i32, a5,
        ),
        SYS_munmap => crate::services::mm::mmap::munmap_syscall_entry(a0, a1),
        SYS_mremap => {
            // mremap (DECISION-J 第十八批: 自 framework dispatch 迁入, 策略主体本就在 services)
            use crate::framework::mm::vma_get_current_mm;
            match vma_get_current_mm() {
                Some(mm) => match i32::try_from(a3) {
                    Ok(flags) => {
                        match crate::services::mm::mremap::mremap_syscall(mm, a0, a1, a2, flags) {
                            Ok(addr) => addr as i64,
                            Err(e) => e.as_ret(),
                        }
                    }
                    Err(_) => Errno::EINVAL.as_ret(),
                },
                None => -1,
            }
        }

        // 内存建议与锁定
        SYS_madvise => crate::services::mm::madvise_mlock::sys_madvise(a0, a1, a2),
        SYS_mlock => crate::services::mm::madvise_mlock::sys_mlock(a0, a1),
        SYS_munlock => crate::services::mm::madvise_mlock::sys_munlock(a0, a1),
        SYS_mlockall => crate::services::mm::madvise_mlock::sys_mlockall(a0),
        SYS_munlockall => crate::services::mm::madvise_mlock::sys_munlockall(),
        SYS_mincore => crate::services::mm::madvise_mlock::sys_mincore(a0, a1, a2),

        // NUMA
        // mbind (T1 G4 实装): 地址范围级 NUMA 策略 (VMA 级落地)
        SYS_mbind => crate::services::mm::numa::mbind_syscall(a0, a1, a2 as u32, a3, a4, a5 as u32),
        SYS_get_mempolicy => crate::services::mm::numa::sys_get_mempolicy(a0, a1),
        SYS_set_mempolicy => crate::services::mm::numa::sys_set_mempolicy(a0, a1),
        SYS_migrate_pages => crate::services::mm::numa::sys_migrate_pages(a0),
        SYS_getcpu => crate::services::mm::numa::sys_getcpu(),

        // userfaultfd (T1 G4 实装): 用户态缺页处理 fd (ioctl/read 由 fd 路由分发)
        SYS_userfaultfd => crate::services::mm::uffd::userfaultfd_syscall(a0 as u32),

        _ => return None,
    })
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
/// 同步原语相关系统调用
fn dispatch_sync(num: u64, args: [u64; 6]) -> Option<i64> {
    use crate::services::syscall::types::{
        SYS_epoll_create, SYS_epoll_create1, SYS_epoll_ctl, SYS_epoll_pwait, SYS_epoll_wait,
        SYS_eventfd, SYS_eventfd2, SYS_futex, SYS_signalfd, SYS_signalfd4, SYS_timerfd_create,
        SYS_timerfd_gettime, SYS_timerfd_settime,
    };
    let [a0, a1, a2, a3, a4, a5] = args;

    Some(match num {
        // futex
        SYS_futex => {
            match crate::services::sync::futex::futex_syscall(
                a0, a1 as i32, a2 as i32, a3, a4 as u32,
            ) {
                Ok(crate::services::sync::futex::FutexResult::Woken) => 0,
                Ok(crate::services::sync::futex::FutexResult::WokenCount(n)) => i64::from(n),
                Ok(crate::services::sync::futex::FutexResult::Requeued { woken, .. }) => {
                    i64::from(woken)
                }
                Ok(crate::services::sync::futex::FutexResult::Pending) => 0,
                Err(e) => e.as_ret(),
            }
        }

        // epoll
        SYS_epoll_create => as_ret(crate::services::sync::epoll::epoll_create_syscall(
            a0 as i32,
        )),
        SYS_epoll_create1 => as_ret(crate::services::sync::epoll::epoll_create_syscall(
            a0 as i32,
        )),
        SYS_epoll_ctl => as_ret(crate::services::sync::epoll::epoll_ctl_syscall(
            a0 as i64, a1 as i32, a2 as i32, a3,
        )),
        SYS_epoll_wait => as_ret(crate::services::sync::epoll::epoll_wait_syscall(
            a0 as i64, a1, a2 as i32, a3 as i32,
        )),
        // epoll_pwait (T1 G5 实装): epoll_wait + 临时信号屏蔽字 (a4/a5)
        SYS_epoll_pwait => as_ret(crate::services::sync::epoll::epoll_pwait_syscall(
            a0 as i64, a1, a2 as i32, a3 as i32, a4, a5,
        )),

        // eventfd
        SYS_eventfd => as_ret(crate::services::sync::eventfd::eventfd_syscall(
            a0, a1 as i32,
        )),
        SYS_eventfd2 => as_ret(crate::services::sync::eventfd::eventfd_syscall(
            a0, a1 as i32,
        )),

        // signalfd
        SYS_signalfd => as_ret(crate::services::sync::signalfd::signalfd_syscall(
            a0 as i32, a1, a2 as i32,
        )),
        SYS_signalfd4 => as_ret(crate::services::sync::signalfd::signalfd_syscall(
            a0 as i32, a1, a2 as i32,
        )),

        // timerfd
        SYS_timerfd_create => as_ret(crate::services::timer::timerfd::timerfd_create_syscall(
            a0 as i32, a1 as i32,
        )),
        SYS_timerfd_settime => as_ret(crate::services::timer::timerfd::timerfd_settime_syscall(
            a0 as i32, a1 as i32, a2, a3,
        )),
        SYS_timerfd_gettime => as_ret(crate::services::timer::timerfd::timerfd_gettime_syscall(
            a0 as i32, a1,
        )),

        _ => return None,
    })
}

/// Credo 私有系统调用
fn dispatch_credo(num: u64, args: [u64; 6]) -> Option<i64> {
    use crate::services::syscall::types::{
        SYS_CREDO_BOOT_CHECK, SYS_CREDO_CHANGE_PASSWORD, SYS_CREDO_CHECK_CAP,
        SYS_CREDO_CREATE_FIRST, SYS_CREDO_CREATE_IDENTITY, SYS_CREDO_DELETE_IDENTITY,
        SYS_CREDO_DISK_FORMAT, SYS_CREDO_DISK_INFO, SYS_CREDO_DISK_LIST, SYS_CREDO_DISK_PARTITION,
        SYS_CREDO_FAT_FORMAT, SYS_CREDO_GET_CAPS, SYS_CREDO_GET_DOMAIN_FLAGS, SYS_CREDO_GET_PWM,
        SYS_CREDO_GETHOSTNAME, SYS_CREDO_GRANT, SYS_CREDO_HOTPLUG_STATUS, SYS_CREDO_IDENTITY_INFO,
        SYS_CREDO_LOGIN, SYS_CREDO_LOGOUT, SYS_CREDO_PROC_CPUTIME, SYS_CREDO_PROC_LIST,
        SYS_CREDO_PROC_SETPRI, SYS_CREDO_PROC_SLEEP, SYS_CREDO_REBOOT, SYS_CREDO_REVOKE,
        SYS_CREDO_SET_DOMAIN_FLAGS, SYS_CREDO_SET_PWM, SYS_CREDO_SETHOSTNAME,
        SYS_CREDO_VERIFY_PASSWORD, SYS_capget, SYS_capset, SYS_getegid, SYS_geteuid, SYS_getgid,
        SYS_getuid, SYS_setegid, SYS_seteuid, SYS_setgid, SYS_setregid, SYS_setreuid, SYS_setuid,
    };
    // SYS_CREDO_DISK_INSTALL 仅 x86_64 (非 kernel_test) 或 kernel_test 模式使用
    // (aarch64 生产构建走 `_ =>` 兜底 ENOSYS, 与迁移前 framework cfg 语义一致)
    #[cfg(any(feature = "kernel_test", target_arch = "x86_64"))]
    use crate::services::syscall::types::SYS_CREDO_DISK_INSTALL;
    let [a0, a1, a2, a3, _a4, _a5] = args;

    Some(match num {
        // 凭证 - UID/GID
        SYS_getuid => as_ret(crate::services::credo::uid::getuid_syscall()),
        SYS_getgid => as_ret(crate::services::credo::uid::getgid_syscall()),
        SYS_setuid => as_ret(crate::services::credo::uid::setuid_syscall(a0 as u32)),
        SYS_setgid => as_ret(crate::services::credo::uid::setgid_syscall(a0 as u32)),
        SYS_geteuid => as_ret(crate::services::credo::uid::geteuid_syscall()),
        SYS_getegid => as_ret(crate::services::credo::uid::getegid_syscall()),
        SYS_seteuid => as_ret(crate::services::credo::uid::seteuid_syscall(a0 as u32)),
        SYS_setegid => as_ret(crate::services::credo::uid::setegid_syscall(a0 as u32)),
        SYS_setreuid => as_ret(crate::services::credo::uid::setreuid_syscall(
            a0 as u32, a1 as u32,
        )),
        SYS_setregid => as_ret(crate::services::credo::uid::setregid_syscall(
            a0 as u32, a1 as u32,
        )),

        // Credo 认证
        SYS_CREDO_LOGIN => crate::services::credo::auth::auth_login_syscall(a0, a1),
        SYS_CREDO_LOGOUT => crate::services::credo::auth::auth_logout_syscall(),
        SYS_CREDO_CREATE_IDENTITY => {
            crate::services::credo::auth::auth_create_syscall(a0, a1, a2 as u8)
        }
        SYS_CREDO_DELETE_IDENTITY => crate::services::credo::auth::auth_delete_syscall(a0),
        SYS_CREDO_IDENTITY_INFO => crate::services::credo::auth::auth_info_syscall(a0),
        SYS_CREDO_CHANGE_PASSWORD => crate::services::credo::auth::auth_changepw_syscall(a0, a1),
        SYS_CREDO_VERIFY_PASSWORD => crate::services::credo::auth::auth_verify_syscall(a0),
        SYS_CREDO_CREATE_FIRST => crate::services::credo::auth::auth_create_first_syscall(a0),
        SYS_CREDO_GRANT => crate::services::credo::auth::auth_grant_syscall(a0, a1, a2 as u16, a3),
        SYS_CREDO_REVOKE => {
            crate::services::credo::auth::auth_revoke_syscall(a0, a1, a2 as u16, a3)
        }
        SYS_CREDO_CHECK_CAP => {
            crate::services::credo::auth::auth_check_cap_syscall(a0, a1 as u16, a2)
        }
        SYS_CREDO_GET_CAPS => crate::services::credo::auth::auth_get_caps_syscall(a0, a1 as u16),
        SYS_CREDO_GET_PWM => crate::services::credo::auth::pwm_get_syscall(),
        SYS_CREDO_SET_PWM => crate::services::credo::auth::pwm_set_syscall(a0),

        // 分册 9 批次 4: 域级行为门控 (DomainFlags) — 查询/设置当前进程
        SYS_CREDO_GET_DOMAIN_FLAGS => crate::services::credo::domain::domain_flags_get_syscall(),
        SYS_CREDO_SET_DOMAIN_FLAGS => crate::services::credo::domain::domain_flags_set_syscall(a0),

        // Linux capability ABI 映射 (分册 9 批次 3): 导出/写回 SYSTEM 域能力
        SYS_capget => crate::services::credo::auth::capget_syscall(a0, a1),
        SYS_capset => crate::services::credo::auth::capset_syscall(a0, a1),

        // Credo 系统信息
        SYS_CREDO_GETHOSTNAME => crate::services::proc::sysinfo::gethostname_syscall(a0, a1),
        SYS_CREDO_SETHOSTNAME => crate::services::proc::sysinfo::sethostname_syscall(a0, a1),
        SYS_CREDO_BOOT_CHECK => crate::services::proc::sysinfo::boot_check_syscall(a0 as i32),
        SYS_CREDO_PROC_LIST => crate::services::proc::proc_mgmt::proc_list_syscall(a0, a1 as u32),
        SYS_CREDO_PROC_SETPRI => {
            crate::services::proc::proc_mgmt::proc_setpri_syscall(a0 as u32, a1 as u32)
        }
        SYS_CREDO_PROC_CPUTIME => {
            crate::services::proc::proc_mgmt::credo_proc_cputime_syscall(a0 as u32)
        }
        SYS_CREDO_PROC_SLEEP => {
            // 单位约定: 输入为毫秒 (Credo 策略), 底层 nanosleep 为纳秒.
            const MS_TO_NS: u64 = 1_000_000;
            let ns = a0 * MS_TO_NS;
            as_ret(crate::services::timer::sleep::nanosleep_syscall(ns, a1))
        }
        SYS_CREDO_REBOOT => crate::services::proc::sysinfo::reboot_syscall(a0 as i32),

        // 存储设备
        SYS_CREDO_DISK_LIST => as_ret(
            crate::services::credo::storage::disk::disk_list(a0, a1 as u32).map(|n| n as usize),
        ),
        SYS_CREDO_DISK_INFO => {
            match crate::services::credo::storage::disk::disk_info(a0 as u32, a1) {
                Ok(()) => 0,
                Err(e) => e.as_ret(),
            }
        }
        SYS_CREDO_DISK_FORMAT => {
            match crate::services::credo::storage::disk::disk_format(a0 as u32, a1) {
                Ok(()) => 0,
                Err(e) => e.as_ret(),
            }
        }
        SYS_CREDO_DISK_PARTITION => {
            match crate::services::credo::storage::disk::disk_partition(a0 as u32, a1) {
                Ok(()) => 0,
                Err(e) => e.as_ret(),
            }
        }
        // T2 批 5: 引导安装 / 热插拔状态 自 framework 回退层迁移
        // (委托 framework 机制 sys_boot_install / sys_hotplug_status)
        #[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
        SYS_CREDO_DISK_INSTALL => {
            crate::services::credo::storage::disk::boot_install_syscall(a0 as u32)
        }
        #[cfg(feature = "kernel_test")]
        SYS_CREDO_DISK_INSTALL => Errno::ENOSYS.as_ret(),
        SYS_CREDO_HOTPLUG_STATUS => {
            crate::services::credo::storage::disk::hotplug_status_syscall(a0, a1 as u32)
        }
        SYS_CREDO_FAT_FORMAT => {
            match crate::services::credo::storage::disk::fat_format(a0 as u32) {
                Ok(()) => 0,
                Err(e) => e.as_ret(),
            }
        }

        _ => return None,
    })
}

/// 其他系统调用 (POSIX Timer, 熵源等)
fn dispatch_other(num: u64, args: [u64; 6]) -> Option<i64> {
    use crate::services::syscall::types::{
        QX_FTRACE_DISABLE, QX_FTRACE_ENABLE, QX_FTRACE_READ, QX_FTRACE_STAT, QX_FW_DETACH,
        QX_FW_GET, QX_FW_GET_INFO, QX_FW_LOAD, QX_GET_CANARY, QX_KGDB_ENTER, SYS_FB_MMAP,
        SYS_FB_OPEN, SYS_FB_RELEASE, SYS_bpf, SYS_clock_getres, SYS_getrandom, SYS_io_uring_enter,
        SYS_io_uring_setup, SYS_kexec_load, SYS_timer_create, SYS_timer_delete,
        SYS_timer_getoverrun, SYS_timer_gettime, SYS_timer_settime,
    };
    let [a0, a1, a2, a3, _a4, _a5] = args;

    Some(match num {
        // POSIX Timer (从 framework 回退迁移, §6.1 下沉 services/syscall/posix_timer)
        SYS_timer_create => crate::services::syscall::posix_timer::sys_timer_create(a0, a1, a2),
        SYS_timer_settime => {
            crate::services::syscall::posix_timer::sys_timer_settime(a0, a1, a2, a3)
        }
        SYS_timer_gettime => crate::services::syscall::posix_timer::sys_timer_gettime(a0, a1),
        SYS_timer_delete => crate::services::syscall::posix_timer::sys_timer_delete(a0),
        SYS_timer_getoverrun => crate::services::syscall::posix_timer::sys_timer_getoverrun(a0),
        SYS_clock_getres => crate::services::syscall::posix_timer::sys_clock_getres(a0, a1),

        // io_uring 异步 I/O (T2 批 3, syscall-followup): 委托 framework 机制
        // (IoUring 实例表), services 仅参数转换 + 错误码映射
        SYS_io_uring_setup => crate::services::io::iouring::io_uring_setup_syscall(a0),
        SYS_io_uring_enter => crate::services::io::iouring::io_uring_enter_syscall(a0, a1, a2),

        // eBPF / kexec (T2 批 4, syscall-followup): 既有安全代理接线,
        // 委托 framework 机制 (debug::sys_bpf / driver::sys_kexec)
        SYS_bpf => crate::services::debug::ebpf::bpf_syscall(a0, a1, a2),
        SYS_kexec_load => crate::services::driver::kexec::kexec_syscall(a0, a1, a2, a3),

        // 帧缓冲 (T2 批 5, syscall-followup): 委托 framework 机制
        // (机制函数: sys_fb_open / sys_fb_mmap / sys_fb_release)
        SYS_FB_OPEN => crate::services::driver::fb::fb_open_syscall(a0, a1),
        SYS_FB_MMAP => crate::services::driver::fb::fb_mmap_syscall(a0, a1, a2),
        SYS_FB_RELEASE => crate::services::driver::fb::fb_release_syscall(a0),

        // 熵源 / Stack Canary (§6.1 下沉 services/syscall/canary)
        SYS_getrandom => crate::services::syscall::canary::sys_getrandom(a0, a1, a2),
        QX_GET_CANARY => crate::services::syscall::canary::sys_get_canary(a0, a1),

        // 设备固件加载 (§6.2 下沉 services/syscall/firmware)
        QX_FW_LOAD => crate::services::syscall::firmware::sys_fw_load(a0, a1, a2, a3),
        QX_FW_GET => crate::services::syscall::firmware::sys_fw_get(a0, a1, a2, a3),
        QX_FW_GET_INFO => crate::services::syscall::firmware::sys_fw_get_info(a0, a1),
        QX_FW_DETACH => crate::services::syscall::firmware::sys_fw_detach(a0),

        // 内核调试 / 跟踪 (§6.2 下沉 services/syscall/ftrace)
        QX_FTRACE_ENABLE => crate::services::syscall::ftrace::sys_ftrace_enable(),
        QX_FTRACE_DISABLE => crate::services::syscall::ftrace::sys_ftrace_disable(),
        QX_FTRACE_READ => crate::services::syscall::ftrace::sys_ftrace_read(a0),
        QX_FTRACE_STAT => crate::services::syscall::ftrace::sys_ftrace_stat(a0),
        QX_KGDB_ENTER => crate::services::syscall::ftrace::sys_kgdb_enter(),

        _ => return None,
    })
}

// ============================================================================
// 注册
// ============================================================================

/// 注册 services 层分发策略到 framework
///
/// # Errors
///
/// 当分发策略已被注册时返回 `Err(())`.
pub fn register_services_dispatch() -> Result<(), ()> {
    static POLICY: ServicesSyscallDispatch = ServicesSyscallDispatch;
    let r = register_syscall_dispatch(&POLICY);
    crate::framework::klog::log_info(
        crate::framework::klog::LogCategory::Boot,
        format_args!(
            "[SYSCALL] register_services_dispatch result={}",
            if r.is_ok() { "OK" } else { "ERR" }
        ),
    );
    r.map_err(|_| ())
}
