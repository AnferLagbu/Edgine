//! I-48: execve 后信号状态重置
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `FakeProcess` / `reset` 平行实现, 改引内核真实源码
//! `edgine::kernel::privileged::proc::signal::reset_signal_state_on_exec`
//! (全局 PROCESS_TABLE 查找 + 清零 pending/sigaction/blocked, 无效 PID 静默 no-op).
//! 通过真实 `Process::new` + `PROCESS_TABLE.insert` 构造宿主进程, Drop 时回收.
//!
//! 内核 `Process` 字段 `pending_signals` / `blocked_mask` (AtomicU64) 与
//! `sigaction_table` (Mutex<[u64; 64]>) 均为 pub, 测试可直接经 `&Process` 读写
//! 验证 reset 语义.

use std::sync::atomic::Ordering;

use edgine::kernel::privileged::proc::process::{PROCESS_TABLE, Process};
use edgine::kernel::privileged::proc::signal::reset_signal_state_on_exec;

/// 宿主进程句柄: 构造 + 插入全局 PROCESS_TABLE, Drop 时回收
struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new() -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, "execve-signal-test", None));
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Self { pid }
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    /// 对测试进程执行只读/可变访问 (经 PROCESS_TABLE 取权威引用)
    fn with<R>(&self, f: impl FnOnce(&Process) -> R) -> R {
        let ptr = PROCESS_TABLE.get(self.pid).expect("测试进程应在表中");
        // SAFETY: PROCESS_TABLE.get() 返回有效指针, 测试期间进程不会被释放
        let proc = unsafe { &*ptr };
        f(proc)
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
    }
}

#[test]
fn fresh_process_all_zero() {
    let p = TestProc::new();
    p.with(|proc| {
        assert_eq!(proc.pending_signals.load(Ordering::Acquire), 0);
        assert_eq!(proc.blocked_mask.load(Ordering::Acquire), 0);
        let table = proc.sigaction_table.lock();
        for (i, &entry) in table.iter().enumerate() {
            assert_eq!(entry, 0, "sigaction[{}] should be 0", i);
        }
    });
}

#[test]
fn reset_clears_pending() {
    let p = TestProc::new();
    p.with(|proc| {
        proc.pending_signals.store(0x2_0200, Ordering::Release);
        assert_ne!(proc.pending_signals.load(Ordering::Acquire), 0);
    });
    reset_signal_state_on_exec(p.pid());
    p.with(|proc| {
        assert_eq!(proc.pending_signals.load(Ordering::Acquire), 0);
    });
}

#[test]
fn reset_clears_sigaction_table() {
    let p = TestProc::new();
    p.with(|proc| {
        proc.sigaction_table.lock()[0] = 0xDEAD_BEEF;
        proc.sigaction_table.lock()[8] = 0xCAFE_BABE;
    });
    reset_signal_state_on_exec(p.pid());
    p.with(|proc| {
        let table = proc.sigaction_table.lock();
        for (i, &entry) in table.iter().enumerate() {
            assert_eq!(entry, 0, "sigaction[{}] not reset", i);
        }
    });
}

#[test]
fn reset_clears_blocked_mask() {
    let p = TestProc::new();
    p.with(|proc| {
        proc.blocked_mask.store(0xFFFF_FFFF, Ordering::Release);
    });
    reset_signal_state_on_exec(p.pid());
    p.with(|proc| {
        assert_eq!(proc.blocked_mask.load(Ordering::Acquire), 0);
    });
}

#[test]
fn reset_idempotent() {
    let p = TestProc::new();
    p.with(|proc| {
        proc.pending_signals.store(0x1234, Ordering::Release);
        proc.blocked_mask.store(0x1234, Ordering::Release);
        proc.sigaction_table.lock()[5] = 0xABCD;
    });
    reset_signal_state_on_exec(p.pid());
    reset_signal_state_on_exec(p.pid());
    reset_signal_state_on_exec(p.pid());
    p.with(|proc| {
        assert_eq!(proc.pending_signals.load(Ordering::Acquire), 0);
        assert_eq!(proc.blocked_mask.load(Ordering::Acquire), 0);
        let table = proc.sigaction_table.lock();
        for entry in table.iter() {
            assert_eq!(*entry, 0);
        }
    });
}

#[test]
fn reset_does_not_affect_other_processes() {
    // execve 路径只重置目标进程, 不影响其他进程
    let target = TestProc::new();
    let other = TestProc::new();
    target.with(|proc| {
        proc.pending_signals.store(0x200, Ordering::Release);
        proc.sigaction_table.lock()[0] = 0x1234;
    });
    other.with(|proc| {
        proc.pending_signals.store(0x8000, Ordering::Release);
        proc.sigaction_table.lock()[0] = 0x1234;
    });
    reset_signal_state_on_exec(target.pid());
    // target 已清零
    target.with(|proc| {
        assert_eq!(proc.pending_signals.load(Ordering::Acquire), 0);
        assert_eq!(proc.sigaction_table.lock()[0], 0);
    });
    // other 不受影响
    other.with(|proc| {
        assert_eq!(proc.pending_signals.load(Ordering::Acquire), 0x8000);
        assert_eq!(proc.sigaction_table.lock()[0], 0x1234);
    });
}

#[test]
fn reset_missing_pid_is_noop() {
    // 无效 PID 静默 no-op (不 panic, 不改任何状态)
    let p = TestProc::new();
    p.with(|proc| {
        proc.pending_signals.store(0x100, Ordering::Release);
    });
    reset_signal_state_on_exec(u32::MAX - 1);
    // 现有进程不受影响 (函数在查表失败时直接返回)
    p.with(|proc| {
        assert_eq!(proc.pending_signals.load(Ordering::Acquire), 0x100);
    });
}

#[test]
fn test_linux_execve_signal_pendings_documented() {
    // 文档化回归: Linux execve(2) 行为 (man page):
    // 1. SA_RESETHAND 标志的 handler → SIG_DFL
    // 2. 挂起标准信号保留
    // 3. 挂起实时信号保留
    // Edgine 简化: 全新进程, 无保留. 此处记录差异, 不在运行时检查.
    const DOC_LINUX_BEHAVIOR: &str = "Linux: SA_RESETHAND resets; pendings preserved";
    const DOC_EDGINE_BEHAVIOR: &str =
        "Edgine: fresh process via transactional replace, no carry-over";
    assert!(!DOC_LINUX_BEHAVIOR.is_empty());
    assert!(!DOC_EDGINE_BEHAVIOR.is_empty());
}
