//! I-52: Zombie 进程信号投递边界检查
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `FakeProcess` / `ProcessState` 镜像 / `do_signal_send` /
//! `do_signal_send_inner` 平行实现, 改引内核真实源码:
//! - `edgine::kernel::privileged::proc::signal::do_signal_send` (全局 PROCESS_TABLE)
//! - `edgine::kernel::privileged::proc::ProcessState` (functions::proc::types 权威)
//! - 通过真实 `Process::new` + `PROCESS_TABLE.insert` 构造宿主进程, 测试后
//!   `remove_and_free` 清理 (PID 经 `allocate_pid` 唯一分配, 避免并行互踩).
//!
//! ## 因内核 host 不可测已移除
//! - `do_signal_send_inner` 为内核**私有**函数 (`fn`, 非 pub), host 无法直接调用,
//!   对应两条 inner 测试移除.
//!
//! ## 与镜像的差异 (以内核为权威)
//! - 内核 `do_signal_send` 信号有效域为 `1..=63` (实时信号 32..63, 注释:
//!   "sig 上限 63 = bit 63, 避开 1u64<<64 UB"), 原镜像误作 `1..=31`.
//!   故 sig=32 内核视为合法 (不返回 -1), 测试断言已同步.
//! - 内核 `ProcessState` 判别值: Created=0/Ready=1/Running=2/Blocked=3/Zombie=4/
//!   Terminated=5/Frozen=6; 原镜像 (Ready=0..Zombie=3) 已删除.
//! - 内核 `signal_pending_set(sig)` 置 `1<<sig` 位 (非镜像的 `1<<(sig-1)`),
//!   测试统一经内核 `signal_pending_get()` 读取, 不硬编码位号.

use std::sync::atomic::Ordering;

use edgine::kernel::privileged::proc::ProcessState;
use edgine::kernel::privileged::proc::process::{PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::signal::do_signal_send;

/// 宿主进程句柄: 构造 + 插入全局 PROCESS_TABLE, Drop 时回收
struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new(state: ProcessState) -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, "zombie-signal-test", None));
        proc.state.store(state as u32, Ordering::SeqCst);
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Self { pid }
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    /// 经内核 getter 读取 pending 位图
    fn pending(&self) -> u64 {
        let ptr = PROCESS_TABLE.get(self.pid).expect("测试进程应在表中");
        // SAFETY: PROCESS_TABLE.get() 返回有效指针, 测试期间进程不会被释放
        unsafe { &*ptr }.signal_pending_get()
    }

    fn state(&self) -> ProcessState {
        let ptr = PROCESS_TABLE.get(self.pid).expect("测试进程应在表中");
        // SAFETY: PROCESS_TABLE.get() 返回有效指针, 测试期间进程不会被释放
        unsafe { &*ptr }.get_state()
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

#[test]
fn kernel_process_state_discriminants() {
    // 内核 ProcessState (functions::proc::types 权威) 判别值
    assert_eq!(ProcessState::Created as u32, 0);
    assert_eq!(ProcessState::Ready as u32, 1);
    assert_eq!(ProcessState::Running as u32, 2);
    assert_eq!(ProcessState::Blocked as u32, 3);
    assert_eq!(ProcessState::Zombie as u32, 4);
    assert_eq!(ProcessState::Terminated as u32, 5);
    assert_eq!(ProcessState::Frozen as u32, 6);
    assert_eq!(ProcessState::from_u8(4), ProcessState::Zombie);
    assert_eq!(ProcessState::Zombie.name(), "Zombie");
}

#[test]
fn zombie_blocks_signal_send() {
    let proc = TestProc::new(ProcessState::Zombie);
    let res = do_signal_send(proc.pid(), 9);
    assert_eq!(res, Err(-3), "Zombie 投递应返回 -3 (ESRCH)");
    // pending 不变
    assert_eq!(proc.pending(), 0);
    assert_eq!(proc.state(), ProcessState::Zombie);
}

#[test]
fn ready_state_delivers() {
    let proc = TestProc::new(ProcessState::Ready);
    let res = do_signal_send(proc.pid(), 9);
    assert!(res.is_ok());
    assert!(proc.pending() != 0, "SIGKILL 应设置 pending 位");
}

#[test]
fn blocked_state_delivers_and_wakes() {
    let proc = TestProc::new(ProcessState::Blocked);
    let res = do_signal_send(proc.pid(), 9);
    assert!(res.is_ok());
    // 内核: Blocked 投递后状态转为 Ready
    assert_eq!(
        proc.state(),
        ProcessState::Ready,
        "Blocked 投递后应唤醒为 Ready"
    );
    assert!(proc.pending() != 0);
}

#[test]
fn running_state_delivers() {
    let proc = TestProc::new(ProcessState::Running);
    let res = do_signal_send(proc.pid(), 9);
    assert!(res.is_ok());
    assert!(proc.pending() != 0);
}

#[test]
fn sig_zero_missing_returns_noent() {
    // sig=0 仅检查存在性; 超大 pid 必然不在表内 (>= MAX_PROCESSES)
    let res = do_signal_send(u32::MAX - 1, 0);
    assert_eq!(res, Err(-2), "sig=0 且进程不存在应返回 -2");
}

#[test]
fn sig_zero_existing_zombie_returns_ok() {
    let proc = TestProc::new(ProcessState::Zombie);
    // sig=0 仅检查存在, Zombie 也返回 Ok; 不投递
    let res = do_signal_send(proc.pid(), 0);
    assert!(res.is_ok());
    assert_eq!(proc.pending(), 0);
}

#[test]
fn sig_63_valid_realtime_signal() {
    // 内核信号域 1..=63; 63 (实时信号) 对非 Zombie 进程合法
    let proc = TestProc::new(ProcessState::Ready);
    let res = do_signal_send(proc.pid(), 63);
    assert!(res.is_ok(), "sig=63 在内核 1..=63 域内应合法");
    assert!(proc.pending() != 0);
}

#[test]
fn sig_255_rejected() {
    let proc = TestProc::new(ProcessState::Ready);
    let res = do_signal_send(proc.pid(), 255);
    assert_eq!(res, Err(-1), "sig=255 超出 1..=63 应返回 -1");
}

#[test]
fn sig_64_rejected() {
    // 内核上限 63 (避开 1u64<<64 UB), 64 应拒绝
    let proc = TestProc::new(ProcessState::Ready);
    let res = do_signal_send(proc.pid(), 64);
    assert_eq!(res, Err(-1), "sig=64 超出内核上限 63 应返回 -1");
}

#[test]
fn zombie_recovery_not_via_signal() {
    // Zombie 状态不被任何信号清除; 全程 pending=0, 状态保持 Zombie
    let proc = TestProc::new(ProcessState::Zombie);
    for sig in 1..=63u8 {
        let res = do_signal_send(proc.pid(), sig);
        assert_eq!(res, Err(-3), "Zombie 对 sig={} 应返回 -3", sig);
    }
    assert_eq!(proc.state(), ProcessState::Zombie);
    assert_eq!(proc.pending(), 0);
}
