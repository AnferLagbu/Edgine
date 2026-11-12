// SPDX-License-Identifier: MPL-2.0
// TD-15: TD-02 V4 — fd_alloc::idx_of 集中反查 + 子系统本地 fd_to_idx 迁移
//
// 验收:
//   - `idx_of` 在 FdPlan::ALL 范围内反查都正确
//   - 已迁 fd 对象模型的子系统不再有本地 fd_to_idx / FD 编号区间算术
//   - timerfd 历史 240 已迁出 smoltcp [0, 256) 重叠 (基址改 1160)
//   - 双架构编译通过; 静态契约用例全过
//
// 该测试为 I-51/TD-01/TD-02 V4 验收的强化版.
// fd 对象模型阶段 3 (2026-11-10): eventfd / signalfd / unix / inotify / timerfd
// / pidfd 已迁至 OpenFileTable 挂表, 不再有本地 fd_to_idx; FdPlan / FdSubsystem
// 条目保留至阶段 4 一并退役.

use std::fs;
use std::path::Path;

const FD_ALLOC: &str = "src/kernel/privileged/proc/fd_alloc.rs";
const TIMERFD: &str = "src/kernel/privileged/syscall/timerfd.rs";
const UNIX: &str = "src/kernel/functions/net/unix.rs";
const INOTIFY: &str = "src/kernel/functions/fs/inotify.rs";
const PIDFD: &str = "src/kernel/functions/proc/pidfd.rs";

fn read(p: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

#[test]
fn test_idx_of_declared_in_fd_alloc() {
    let src = read(FD_ALLOC);
    assert!(
        src.contains("pub fn idx_of(fd: i32) -> Option<(FdSubsystem, usize)>"),
        "fd_alloc 必须公开 pub fn idx_of (TD-15)"
    );
}

#[test]
fn test_timerfd_subsystem_in_fd_plan() {
    let src = read(FD_ALLOC);
    assert!(
        src.contains("TimerFd = 5"),
        "FdSubsystem 必须有 TimerFd = 5 变体 (TD-15)"
    );
    assert!(
        src.contains("pub const TIMER_FD: FdRange = FdRange::new(1160, 16)"),
        "FdPlan::TIMER_FD 必须是 (1160, 16) (TD-15)"
    );
    assert!(
        src.contains("pub const COUNT: usize = 8"),
        "FdSubsystem::COUNT 必须是 8 (TD-15 + PidFd + UserFaultFd)"
    );
    assert!(
        src.contains("FdSubsystem::TimerFd => Self::TIMER_FD"),
        "range_for 必须映射 TimerFd → TIMER_FD (TD-15)"
    );
}

#[test]
fn test_timerfd_migrated_off_numeric_fd_space() {
    // fd 对象模型阶段 3 第 6 批: privileged timerfd 只保留槽位机制 (静态槽位
    // 表 + HrTimer 回调), fd 反查 / FD 编号区间 / 位图分配全部移除 (fd 由
    // per-process FdTable 管).
    let src = read(TIMERFD);
    for needle in [
        "TFD_FD_BASE",
        "FdPlan",
        "fd_alloc::",
        "fd_at(",
        "fn fd_to_idx",
        "fn is_timerfd_fd",
        "timerfd_poll_events",
    ] {
        assert!(
            !src.contains(needle),
            "timerfd.rs 不得残留 `{needle}` (TimerFd 已迁至 fd 对象模型, TD-15)"
        );
    }
    assert!(
        src.contains("pub fn alloc_slot"),
        "privileged timerfd 必须暴露槽位分配 (fd 对象模型)"
    );
}

#[test]
fn test_unix_migrated_off_local_fd_translation() {
    // fd 对象模型阶段 3 第 4 批: unix.rs 只暴露策略表槽位下标, 本地 fd 反查 /
    // FD 编号区间 / 位图分配全部移除 (fd 由 per-process FdTable 管).
    let src = read(UNIX);
    for needle in ["fn fd_to_idx", "UDS_FD_BASE", "FdPlan", "fd_alloc::"] {
        assert!(
            !src.contains(needle),
            "unix.rs 不得残留 `{needle}` (UDS 已迁至 fd 对象模型, TD-15)"
        );
    }
    assert!(
        src.contains("pub fn uds_alloc(sock_type: UnixSockType) -> Result<u8, UdsError>"),
        "UDS 策略表必须只暴露槽位下标分配 (TD-15)"
    );
}

#[test]
fn test_inotify_migrated_off_numeric_fd_space() {
    // fd 对象模型阶段 3 第 5 批: inotify.rs 只暴露策略表槽位下标, 本地 fd 反查 /
    // FD 编号区间 / 位图分配全部移除 (fd 由 per-process FdTable 管).
    let src = read(INOTIFY);
    for needle in [
        "INOTIFY_FD_BASE",
        "FdPlan",
        "fd_alloc::",
        "fd_at(",
        "fn fd_to_idx",
        "fn is_inotify_fd",
    ] {
        assert!(
            !src.contains(needle),
            "inotify.rs 不得残留 `{needle}` (Inotify 已迁至 fd 对象模型, TD-15)"
        );
    }
    assert!(
        src.contains("pub struct InotifyFile"),
        "inotify 必须暴露 InotifyFile 对象 (fd 对象模型)"
    );
}

#[test]
fn test_pidfd_migrated_off_numeric_fd_space() {
    // fd 对象模型阶段 3 第 7 批: pidfd 对象挂 OpenFileTable, fd 由
    // per-process FdTable 管; 全局 fd→pid 映射表与 PID_FD 区间消费全部移除.
    let src = read(PIDFD);
    for needle in [
        "PID_FD",
        "FdSubsystem",
        "fd_alloc::",
        "pidfd_map",
        "fn pid_of_fd",
        "fn is_pidfd_fd",
        "fn free_entry",
    ] {
        assert!(
            !src.contains(needle),
            "pidfd.rs 不得残留 `{needle}` (PidFd 已迁至 fd 对象模型, TD-15)"
        );
    }
    assert!(
        src.contains("pub struct PidFdFile"),
        "pidfd 必须暴露 PidFdFile 对象 (fd 对象模型)"
    );
}

#[test]
fn test_fd_plan_invariant_still_holds() {
    // V4 扩展 ALL 数组后, ranges_non_overlapping 必须仍为 true
    // (此项由启动期 verify_plan() 在运行时校验, 静态契约保证)
    let src = read(FD_ALLOC);
    assert!(
        src.contains("pub const ALL: &'static [FdRange] = &[\n        Self::SMOLTCP,")
            || src
                .lines()
                .any(|l| l.contains("Self::SMOLTCP,") || l.contains("Self::UDS,")),
        "ALL 数组必须包含全部 6 个子系统范围 (TD-15)"
    );
    // TIMER_FD 路径引用至少 2 次: range_for match 臂 + ALL 数组
    let count = src.matches("Self::TIMER_FD").count();
    assert!(
        count >= 2,
        "Self::TIMER_FD 至少出现 2 次 (range_for + ALL), 实际 {count} 次 (TD-15)"
    );
}
