#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯判定逻辑 + 原子状态读写。
//! SGEG 域级行为门控 (`DomainFlags`) — privileged 机制实现
//!
//! ## 归属记录
//!
//! `DomainFlags` 状态挂在 `Process` 结构体 (`privileged` 进程机制状态), 门控
//! 入口 `domain_gate_check` 被 `privileged` syscall 分发路径消费 — 属机制项,
//! 与 `seccomp_check` 同构 (见 `privileged/proc/seccomp.rs` 归属说明).
//! 依赖闭包仅 privileged (`proc` / `sgeg::types` / `syscall`)。
//!
//! functions 侧策略 (`functions::sgeg::domain`) 消费本文件的机制 API,
//! 方向合法 (functions→privileged)。
//!
//! ## 门控语义
//!
//! 8 位标志分两类:
//! - **门控位** (`NO_FORK` / `NO_EXEC` / `NO_NET` / `NO_DEVICE` / `SANDBOX` /
//!   `READONLY`): 在 syscall 咽喉点拒绝对应类别调用;
//! - **元数据位** (`TEMP` / `SYSTEM`): 不参与门控, 仅供审计/生命周期标记,
//!   本文件不产生任何拒绝行为.

use crate::privileged::proc::PROCESS_TABLE;
use crate::privileged::proc::process_get_current_pid;
use crate::privileged::sgeg::types::DomainFlags;
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::types::{
    SYS_ACCEPT, SYS_ACCEPT4, SYS_BIND, SYS_CHMOD, SYS_CLONE, SYS_CLONE3, SYS_CONNECT, SYS_EXECVE,
    SYS_EXECVEAT, SYS_EXIT, SYS_EXIT_GROUP, SYS_FCHMOD, SYS_FORK, SYS_FTRUNCATE, SYS_IOCTL,
    SYS_LISTEN, SYS_MKDIR, SYS_MOUNT, SYS_OPEN, SYS_OPENAT, SYS_PWRITE64, SYS_READ, SYS_RECVFROM,
    SYS_RECVMSG, SYS_RENAME, SYS_RENAMEAT, SYS_RT_SIGRETURN, SYS_SENDMSG, SYS_SENDTO, SYS_SOCKET,
    SYS_SOCKETPAIR, SYS_TRUNCATE, SYS_UMOUNT2, SYS_UNLINK, SYS_UNLINKAT, SYS_WRITE, SYS_WRITEV,
};
use core::sync::atomic::Ordering;

// ============================================================================
// 拒绝分类表
// ============================================================================

/// `NO_FORK` 拒绝集: 进程创建族.
///
/// 本内核未定义 `SYS_vfork`, 故不含该项.
const FORK_SYSCALLS: &[u64] = &[SYS_CLONE, SYS_FORK, SYS_CLONE3];

/// `NO_EXEC` 拒绝集: 程序装载族.
const EXEC_SYSCALLS: &[u64] = &[SYS_EXECVE, SYS_EXECVEAT];

/// `NO_NET` 拒绝集: 套接字建立/连接/收发族.
const NET_SYSCALLS: &[u64] = &[
    SYS_SOCKET,
    SYS_SOCKETPAIR,
    SYS_CONNECT,
    SYS_BIND,
    SYS_LISTEN,
    SYS_ACCEPT,
    SYS_ACCEPT4,
    SYS_SENDTO,
    SYS_RECVFROM,
    SYS_SENDMSG,
    SYS_RECVMSG,
];

/// `NO_DEVICE` 拒绝集: 设备控制与挂载族.
const DEVICE_SYSCALLS: &[u64] = &[SYS_IOCTL, SYS_MOUNT, SYS_UMOUNT2];

/// `READONLY` 拒绝集: 路径/描述符写操作族.
const WRITE_SYSCALLS: &[u64] = &[
    SYS_WRITE,
    SYS_PWRITE64,
    SYS_WRITEV,
    SYS_TRUNCATE,
    SYS_FTRUNCATE,
    SYS_RENAME,
    SYS_MKDIR,
    SYS_UNLINK,
    SYS_CHMOD,
    SYS_FCHMOD,
    SYS_UNLINKAT,
    SYS_RENAMEAT,
];

/// `SANDBOX` 白名单: 复合位, 仅放行下列调用, 其余一律拒绝.
const SANDBOX_ALLOWED: &[u64] = &[
    SYS_READ,         // read
    SYS_WRITE,        // write
    SYS_EXIT,         // exit
    SYS_EXIT_GROUP,   // exit_group
    SYS_RT_SIGRETURN, // rt_sigreturn
];

/// `open`/`openat` 的写意图标志掩码 (Linux ABI).
///
/// 成分: `O_WRONLY(0o1) | O_RDWR(0o2) | O_CREAT(0o100) | O_TRUNC(0o1000) |
/// O_APPEND(0o2000)`. `O_RDONLY == 0` 天然落在掩码外。
///
/// privileged 层不可依赖 `functions::fs::open` 的 `O_*` 常量 (依赖方向违规),
/// 故此处以 Linux ABI 数值直接表达; 两处数值同源, 变更需同步。
const OPEN_WRITE_MASK: u64 = 0o1 | 0o2 | 0o100 | 0o1000 | 0o2000;

// ============================================================================
// 判定
// ============================================================================

/// `open`/`openat` 的标志位是否含写意图.
///
/// `SYS_OPEN(path, flags, mode)` 的 flags 在 `args[1]`;
/// `SYS_OPENAT(dirfd, path, flags, mode)` 的 flags 在 `args[2]`.
fn open_flags_are_write(num: u64, args: &[u64; 6]) -> bool {
    let flags = if num == SYS_OPEN { args[1] } else { args[2] };
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
        if (num == SYS_OPEN || num == SYS_OPENAT) && open_flags_are_write(num, args) {
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
        Some(Errno::Eperm.as_ret())
    } else {
        None
    }
}

// ============================================================================
// 状态读写 (functions 策略消费)
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
    use crate::privileged::syscall::types::SYS_READV;

    /// 无标志 ⇒ 任何调用均放行
    #[test]
    fn test_domain_gate_none_allows_all() {
        let args = [0u64; 6];
        assert!(!denied_by(DomainFlags::NONE, SYS_CLONE, &args));
        assert!(!denied_by(DomainFlags::NONE, SYS_EXECVE, &args));
        assert!(!denied_by(DomainFlags::NONE, SYS_SOCKET, &args));
        assert!(!denied_by(DomainFlags::NONE, SYS_WRITE, &args));
    }

    /// NO_FORK: 仅拒进程创建族, 其余类别不受影响
    ///
    /// 断言使用显式 syscall 编号 (不遍历被测表), 使表内容漂移可被检出.
    #[test]
    fn test_domain_gate_no_fork() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_FORK, SYS_CLONE, &args));
        assert!(denied_by(DomainFlags::NO_FORK, SYS_FORK, &args));
        assert!(denied_by(DomainFlags::NO_FORK, SYS_CLONE3, &args));
        assert!(!denied_by(DomainFlags::NO_FORK, SYS_EXECVE, &args));
        assert!(!denied_by(DomainFlags::NO_FORK, SYS_READ, &args));
    }

    /// NO_EXEC: 仅拒装载族
    #[test]
    fn test_domain_gate_no_exec() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_EXEC, SYS_EXECVE, &args));
        assert!(denied_by(DomainFlags::NO_EXEC, SYS_EXECVEAT, &args));
        assert!(!denied_by(DomainFlags::NO_EXEC, SYS_CLONE, &args));
    }

    /// NO_NET: 仅拒网络族
    #[test]
    fn test_domain_gate_no_net() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_NET, SYS_SOCKET, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_SOCKETPAIR, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_CONNECT, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_BIND, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_LISTEN, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_ACCEPT, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_ACCEPT4, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_SENDTO, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_RECVFROM, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_SENDMSG, &args));
        assert!(denied_by(DomainFlags::NO_NET, SYS_RECVMSG, &args));
        assert!(!denied_by(DomainFlags::NO_NET, SYS_READ, &args));
        assert!(!denied_by(DomainFlags::NO_NET, SYS_WRITE, &args));
    }

    /// NO_DEVICE: 仅拒设备控制与挂载族
    #[test]
    fn test_domain_gate_no_device() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::NO_DEVICE, SYS_IOCTL, &args));
        assert!(denied_by(DomainFlags::NO_DEVICE, SYS_MOUNT, &args));
        assert!(denied_by(DomainFlags::NO_DEVICE, SYS_UMOUNT2, &args));
        assert!(!denied_by(DomainFlags::NO_DEVICE, SYS_READ, &args));
    }

    /// READONLY: 写类 syscall 与带写意图的 open/openat 被拒, 只读 open 放行
    #[test]
    fn test_domain_gate_readonly() {
        let args = [0u64; 6];
        assert!(denied_by(DomainFlags::READONLY, SYS_WRITE, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_PWRITE64, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_WRITEV, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_TRUNCATE, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_FTRUNCATE, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_RENAME, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_MKDIR, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_UNLINK, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_CHMOD, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_FCHMOD, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_UNLINKAT, &args));
        assert!(denied_by(DomainFlags::READONLY, SYS_RENAMEAT, &args));
        // 只读路径不受 READONLY 影响
        assert!(!denied_by(DomainFlags::READONLY, SYS_READ, &args));
        assert!(!denied_by(DomainFlags::READONLY, SYS_READV, &args));
        // open(path, O_RDONLY, 0) ⇒ 放行
        assert!(!denied_by(
            DomainFlags::READONLY,
            SYS_OPEN,
            &[0, 0, 0, 0, 0, 0]
        ));
        // open(path, O_WRONLY, 0) ⇒ 拒绝
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_OPEN,
            &[0, 0o1, 0, 0, 0, 0]
        ));
        // openat(dirfd, path, O_RDONLY, 0) ⇒ 放行
        assert!(!denied_by(
            DomainFlags::READONLY,
            SYS_OPENAT,
            &[0, 0, 0, 0, 0, 0]
        ));
        // openat(dirfd, path, O_RDWR, 0) ⇒ 拒绝
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_OPENAT,
            &[0, 0, 0o2, 0, 0, 0]
        ));
        // openat(dirfd, path, O_RDONLY|O_CREAT, 0o644) ⇒ 拒绝 (创建隐含写)
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_OPENAT,
            &[0, 0, 0o100, 0o644, 0, 0]
        ));
        // openat(dirfd, path, O_RDONLY|O_APPEND, 0) ⇒ 拒绝 (追加隐含写)
        assert!(denied_by(
            DomainFlags::READONLY,
            SYS_OPENAT,
            &[0, 0, 0o2000, 0, 0, 0]
        ));
    }

    /// SANDBOX: 严格白名单 (复合位), 与其余位叠加不改变裁决
    #[test]
    fn test_domain_gate_sandbox_whitelist() {
        let args = [0u64; 6];
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_READ, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_WRITE, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_EXIT, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_EXIT_GROUP, &args));
        assert!(!denied_by(DomainFlags::SANDBOX, SYS_RT_SIGRETURN, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_OPEN, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_CLONE, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_SOCKET, &args));
        assert!(denied_by(DomainFlags::SANDBOX, SYS_IOCTL, &args));
    }

    /// 元数据位 (TEMP / SYSTEM) 不参与门控
    #[test]
    fn test_domain_gate_metadata_bits_not_gating() {
        let args = [0u64; 6];
        assert!(!denied_by(DomainFlags::TEMP, SYS_CLONE, &args));
        assert!(!denied_by(DomainFlags::SYSTEM, SYS_IOCTL, &args));
        assert!(!denied_by(
            DomainFlags::TEMP | DomainFlags::SYSTEM,
            SYS_SOCKET,
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
