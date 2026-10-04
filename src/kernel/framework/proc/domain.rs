#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯判定逻辑 + 原子状态读写。
//! SGEG 域级行为门控 (`DomainFlags`) — framework 机制实现
//!
//! ## 归属记录
//!
//! `DomainFlags` 状态挂在 `Process` 结构体 (`framework` 进程机制状态), 门控
//! 入口 `domain_gate_check` 被 `framework` syscall 分发路径消费 — 属机制项,
//! 与 `seccomp_check` 同构 (见 `framework/proc/seccomp.rs` 归属说明).
//! 依赖闭包仅 framework (`proc` / `sgeg::types` / `syscall`)。
//!
//! services 侧策略 (`services::sgeg::domain`) 消费本文件的机制 API,
//! 方向合法 (services→framework)。
//!
//! ## 门控语义
//!
//! 8 位标志分两类:
//! - **门控位** (`NO_FORK` / `NO_EXEC` / `NO_NET` / `NO_DEVICE` / `SANDBOX` /
//!   `READONLY`): 在 syscall 咽喉点拒绝对应类别调用;
//! - **元数据位** (`TEMP` / `SYSTEM`): 不参与门控, 仅供审计/生命周期标记,
//!   本文件不产生任何拒绝行为.

use crate::framework::proc::PROCESS_TABLE;
use crate::framework::proc::process_get_current_pid;
use crate::framework::sgeg::types::DomainFlags;
use crate::framework::syscall::Errno;
use crate::framework::syscall::types::{
    SYS_accept, SYS_accept4, SYS_bind, SYS_chmod, SYS_clone, SYS_clone3, SYS_connect, SYS_execve,
    SYS_execveat, SYS_exit, SYS_exit_group, SYS_fchmod, SYS_fork, SYS_ftruncate, SYS_ioctl,
    SYS_listen, SYS_mkdir, SYS_mount, SYS_open, SYS_openat, SYS_pwrite64, SYS_read, SYS_recvfrom,
    SYS_recvmsg, SYS_rename, SYS_renameat, SYS_rt_sigreturn, SYS_sendmsg, SYS_sendto, SYS_socket,
    SYS_socketpair, SYS_truncate, SYS_umount2, SYS_unlink, SYS_unlinkat, SYS_write, SYS_writev,
};
use core::sync::atomic::Ordering;

// ============================================================================
// 拒绝分类表
// ============================================================================

/// `NO_FORK` 拒绝集: 进程创建族.
///
/// 本内核未定义 `SYS_vfork`, 故不含该项.
const FORK_SYSCALLS: &[u64] = &[SYS_clone, SYS_fork, SYS_clone3];

/// `NO_EXEC` 拒绝集: 程序装载族.
const EXEC_SYSCALLS: &[u64] = &[SYS_execve, SYS_execveat];

/// `NO_NET` 拒绝集: 套接字建立/连接/收发族.
const NET_SYSCALLS: &[u64] = &[
    SYS_socket,
    SYS_socketpair,
    SYS_connect,
    SYS_bind,
    SYS_listen,
    SYS_accept,
    SYS_accept4,
    SYS_sendto,
    SYS_recvfrom,
    SYS_sendmsg,
    SYS_recvmsg,
];

/// `NO_DEVICE` 拒绝集: 设备控制与挂载族.
const DEVICE_SYSCALLS: &[u64] = &[SYS_ioctl, SYS_mount, SYS_umount2];

/// `READONLY` 拒绝集: 路径/描述符写操作族.
const WRITE_SYSCALLS: &[u64] = &[
    SYS_write,
    SYS_pwrite64,
    SYS_writev,
    SYS_truncate,
    SYS_ftruncate,
    SYS_rename,
    SYS_mkdir,
    SYS_unlink,
    SYS_chmod,
    SYS_fchmod,
    SYS_unlinkat,
    SYS_renameat,
];

/// `SANDBOX` 白名单: 复合位, 仅放行下列调用, 其余一律拒绝.
const SANDBOX_ALLOWED: &[u64] = &[
    SYS_read,         // read
    SYS_write,        // write
    SYS_exit,         // exit
    SYS_exit_group,   // exit_group
    SYS_rt_sigreturn, // rt_sigreturn
];

/// `open`/`openat` 的写意图标志掩码 (Linux ABI).
///
/// 成分: `O_WRONLY(0o1) | O_RDWR(0o2) | O_CREAT(0o100) | O_TRUNC(0o1000) |
/// O_APPEND(0o2000)`. `O_RDONLY == 0` 天然落在掩码外。
///
/// framework 层不可依赖 `services::fs::open` 的 `O_*` 常量 (依赖方向违规),
/// 故此处以 Linux ABI 数值直接表达; 两处数值同源, 变更需同步。
const OPEN_WRITE_MASK: u64 = 0o1 | 0o2 | 0o100 | 0o1000 | 0o2000;

// ============================================================================
// 判定
// ============================================================================

/// `open`/`openat` 的标志位是否含写意图.
///
/// `SYS_open(path, flags, mode)` 的 flags 在 `args[1]`;
/// `SYS_openat(dirfd, path, flags, mode)` 的 flags 在 `args[2]`.
fn open_flags_are_write(num: u64, args: &[u64; 6]) -> bool {
    let flags = if num == SYS_open { args[1] } else { args[2] };
    flags & OPEN_WRITE_MASK != 0
}

/// 判定给定域标志下, 该 syscall 是否被拒绝 (纯函数, 可单测).
///
/// `SANDBOX` 为复合位: 命中时以严格白名单裁决, 不再叠加其余位判定
/// (白名单已是最严策略)。
pub(crate) fn denied_by(flags: DomainFlags, num: u64, args: &[u64; 6]) -> bool {
    if flags.contains(DomainFlags::SANDBOX) {
        return !SANDBOX_ALLOWED.contains(&num);
    }
    if flags.contains(DomainFlags::NO_FORK) && FORK_SYSCALLS.contains(&num) {
        return true;
    }
    if flags.contains(DomainFlags::NO_EXEC) && EXEC_SYSCALLS.contains(&num) {
        return true;
    }
    if flags.contains(DomainFlags::NO_NET) && NET_SYSCALLS.contains(&num) {
        return true;
    }
    if flags.contains(DomainFlags::NO_DEVICE) && DEVICE_SYSCALLS.contains(&num) {
        return true;
    }
    if flags.contains(DomainFlags::READONLY) {
        if WRITE_SYSCALLS.contains(&num) {
            return true;
        }
        if (num == SYS_open || num == SYS_openat) && open_flags_are_write(num, args) {
            return true;
        }
    }
    false
}

// ============================================================================
// 门控入口 (syscall 咽喉点消费)
// ============================================================================

/// 域级行为门控检查 — 在 seccomp 检查之后、策略分发之前调用.
///
/// 返回 `Some(-EPERM)` 表示当前进程的域标志拒绝了该调用;
/// `None` 表示放行 (标志为 0 时恒放行, 即默认无门控).
#[inline(never)]
pub fn domain_gate_check(num: u64, args: &[u64; 6]) -> Option<i64> {
    let pid = process_get_current_pid();
    let flags = PROCESS_TABLE
        .with_process(pid, |p| {
            DomainFlags::from_bits_truncate(p.domain_flags.load(Ordering::Acquire))
        })
        .unwrap_or(DomainFlags::NONE);
    if denied_by(flags, num, args) {
        Some(Errno::EPERM.as_ret())
    } else {
        None
    }
}

// ============================================================================
// 状态读写 (services 策略消费)
// ============================================================================

/// 读取指定进程的域级行为门控标志; 进程不存在返回 `None`.
///
/// 未定义位被 `from_bits_truncate` 丢弃 (未知位视为未设置).
pub fn domain_flags_get(pid: u32) -> Option<DomainFlags> {
    PROCESS_TABLE.with_process(pid, |p| {
        DomainFlags::from_bits_truncate(p.domain_flags.load(Ordering::Acquire))
    })
}

/// 写入指定进程的域级行为门控标志; 进程不存在返回 `false`.
///
/// 未定义位被 `from_bits_truncate` 丢弃 (fail-safe: 未知位不得生效).
pub fn domain_flags_set(pid: u32, flags: DomainFlags) -> bool {
    PROCESS_TABLE
        .with_process(pid, |p| {
            p.domain_flags.store(flags.bits(), Ordering::Release);
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    // 仅测试断言使用, 不在产线判定表中出现 (避免非测试构建的未使用导入)
    use crate::framework::syscall::types::SYS_readv;

    /// 无标志 ⇒ 任何调用均放行
    #[test]
    fn test_domain_gate_none_allows_all() {
        let args = [0u64; 6];
        assert!(!denied_by(DomainFlags::NONE, SYS_clone, &args));
        assert!(!denied_by(DomainFlags::NONE, SYS_execve, &args));
        assert!(!denied_by(DomainFlags::NONE, SYS_socket, &args));
        assert!(!denied_by(DomainFlags::NONE, SYS_write, &args));
    }

    /// NO_FORK: 仅拒进程创建族, 其余类别不受影响
    ///
    /// 断言使用显式 syscall 编号 (不遍历被测表), 使表内容漂移可被检出.
    #[test]
    fn test_domain_gate_no_fork() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_FORK, SYS_clone, &args));
        assert!(denied_by(DomainFlags::NO_FORK, SYS_fork, &args));
        assert!(denied_by(DomainFlags::NO_FORK, SYS_clone3, &args));
        assert!(!denied_by(DomainFlags::NO_FORK, SYS_execve, &args));
        assert!(!denied_by(DomainFlags::NO_FORK, SYS_read, &args));
    }

    /// NO_EXEC: 仅拒装载族
    #[test]
    fn test_domain_gate_no_exec() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_EXEC, SYS_execve, &args));
        assert!(denied_by(DomainFlags::NO_EXEC, SYS_execveat, &args));
        assert!(!denied_by(DomainFlags::NO_EXEC, SYS_clone, &args));
    }

    /// NO_NET: 仅拒网络族
    #[test]
    fn test_domain_gate_no_net() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_NET, SYS_socket, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_socketpair, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_connect, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_bind, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_listen, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_accept, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_accept4, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_sendto, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_recvfrom, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_sendmsg, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_recvmsg, &args));
        assert!(!denied_by(DomainFlags::NO_NET, SYS_read, &args));
        assert!(!denied_by(DomainFlags::NO_NET, SYS_write, &args));
    }

    /// NO_DEVICE: 仅拒设备控制与挂载族
    #[test]
    fn test_domain_gate_no_device() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_DEVICE, SYS_ioctl, &args));
        assert!(denied_by(DomainFlags::NO_DEVICE, SYS_mount, &args));
        assert!(denied_by(DomainFlags::NO_DEVICE, SYS_umount2, &args));
        assert!(!denied_by(DomainFlags::NO_DEVICE, SYS_read, &args));
    }

    /// READONLY: 写类 syscall 与带写意图的 open/openat 被拒, 只读 open 放行
    #[test]
    fn test_domain_gate_readonly() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::READONLY, SYS_write, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_pwrite64, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_writev, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_truncate, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_ftruncate, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_rename, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_mkdir, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_unlink, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_chmod, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_fchmod, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_unlinkat, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_renameat, &args));
        // 只读路径不受 READONLY 影响
        assert!(!denied_by(DomainFlags::READONLY, SYS_read, &args));
        assert!(!denied_by(DomainFlags::READONLY, SYS_readv, &args));
        // open(path, O_RDONLY, 0) ⇒ 放行
        assert!(!denied_by(
            DomainFlags::READONLY,
            SYS_open,
            &[0, 0, 0, 0, 0, 0]
        ));
        // open(path, O_WRONLY, 0) ⇒ 拒绝
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_open,
            &[0, 0o1, 0, 0, 0, 0]
        ));
        // openat(dirfd, path, O_RDONLY, 0) ⇒ 放行
        assert!(!denied_by(
            DomainFlags::READONLY,
            SYS_openat,
            &[0, 0, 0, 0, 0, 0]
        ));
        // openat(dirfd, path, O_RDWR, 0) ⇒ 拒绝
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_openat,
            &[0, 0, 0o2, 0, 0, 0]
        ));
        // openat(dirfd, path, O_RDONLY|O_CREAT, 0o644) ⇒ 拒绝 (创建隐含写)
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_openat,
            &[0, 0, 0o100, 0o644, 0, 0]
        ));
        // openat(dirfd, path, O_RDONLY|O_APPEND, 0) ⇒ 拒绝 (追加隐含写)
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_openat,
            &[0, 0, 0o2000, 0, 0, 0]
        ));
    }

    /// SANDBOX: 严格白名单 (复合位), 与其余位叠加不改变裁决
    #[test]
    fn test_domain_gate_sandbox_whitelist() {
        let args = [0u64; 6];
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_read, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_write, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_exit, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_exit_group, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_rt_sigreturn, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_open, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_clone, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_socket, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_ioctl, &args));
    }

    /// 元数据位 (TEMP / SYSTEM) 不参与门控
    #[test]
    fn test_domain_gate_metadata_bits_not_gating() {
        let args = [0u64; 6];
        assert!(!denied_by(DomainFlags::TEMP, SYS_clone, &args));
        assert!(!denied_by(DomainFlags::SYSTEM, SYS_ioctl, &args));
        assert!(!denied_by(
            DomainFlags::TEMP | DomainFlags::SYSTEM,
            SYS_socket,
            &args
        ));
    }

    /// 位常量数值与门控语义表一致 (防止位偏移漂移)
    #[test]
    fn test_domain_flags_bit_values() {
        assert_eq!(DomainFlags::NO_FORK.bits(), 0x01);
        assert_eq!(DomainFlags::NO_EXEC.bits(), 0x02);
        assert_eq!(DomainFlags::NO_NET.bits(), 0x04);
        assert_eq!(DomainFlags::NO_DEVICE.bits(), 0x08);
        assert_eq!(DomainFlags::SANDBOX.bits(), 0x10);
        assert_eq!(DomainFlags::READONLY.bits(), 0x20);
        assert_eq!(DomainFlags::TEMP.bits(), 0x40);
        assert_eq!(DomainFlags::SYSTEM.bits(), 0x80);
    }
}
