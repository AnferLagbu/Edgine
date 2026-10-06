#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! 会话 / 进程组 / 控制终端 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 策略代码 (会话管理 + 进程组 + 控制终端规则) 于 T1-6 (2026-06-16) 迁至
//! `functions::proc::session`, 本文件仅 re-export。按统一判据反转：
//! 会话/进程组是进程关系机制状态, sys_tcgetpgrp/sys_tcsetpgrp 被 privileged
//! syscall dispatch 直接消费 + proc 顶层导出 — 属机制项, 迁回。依赖闭包
//! 仅 privileged。
//!
//! functions 侧改 `pub use crate::privileged::proc::session::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。
//! 日志使用 `privileged::klog::serial_write_bytes` (safe API).
//! 进程表访问使用 privileged 的安全 API (`PROCESS_TABLE`, `process_get_current_pid`).
//!
//! ## POSIX 语义
//!
//! - **会话 (session)**: 一组进程组的集合, 由 `setsid()` 创建, SID = 创建者 PID
//! - **进程组 (process group)**: 一组进程的集合, 用于信号广播
//! - **控制终端 (controlling terminal)**: 每个会话最多一个, 前台进程组接收终端信号

use crate::privileged::config::MAX_SESSIONS;
use crate::privileged::klog::serial_write_bytes;
use crate::privileged::proc::PROCESS_TABLE;
use crate::privileged::proc::process_get_current_pid;
use crate::privileged::sync::IrqSpinLock as Mutex;
use core::sync::atomic::Ordering;

fn log(s: &str) {
    serial_write_bytes(s.as_bytes());
}

fn log_num(n: u64) {
    if n == 0 {
        log("0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut num = n;
    let mut i = 19;
    while num > 0 {
        buf[i] = (num % 10) as u8 + b'0';
        num /= 10;
        i -= 1;
    }
    let s = core::str::from_utf8(&buf[i + 1..]).unwrap_or("?");
    log(s);
}

// ============================================================================
// Session 数据结构
// ============================================================================

/// 会话状态 — `Active` 表示会话有效, `Zombie` 表示会话待回收或槽位空闲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionState {
    Active = 0,
    Zombie = 1,
}

/// 会话结构体
#[repr(C)]
pub struct Session {
    pub session_id: core::sync::atomic::AtomicU64,
    pub pwm: core::sync::atomic::AtomicU64,
    pub parent_sid: core::sync::atomic::AtomicU64,
    /// 控制终端设备号 (0 = 无)
    pub terminal: core::sync::atomic::AtomicU64,
    /// 前台进程组 ID
    pub foreground_pgid: core::sync::atomic::AtomicU32,
    pub create_time: core::sync::atomic::AtomicU64,
    pub state: core::sync::atomic::AtomicU32,
    pub process_list: core::sync::atomic::AtomicU64,
    pub process_count: core::sync::atomic::AtomicU32,
    pub next: core::sync::atomic::AtomicU64,
}

impl Session {
    pub const fn new() -> Self {
        Self {
            session_id: core::sync::atomic::AtomicU64::new(0),
            pwm: core::sync::atomic::AtomicU64::new(0),
            parent_sid: core::sync::atomic::AtomicU64::new(0),
            terminal: core::sync::atomic::AtomicU64::new(0),
            foreground_pgid: core::sync::atomic::AtomicU32::new(0),
            create_time: core::sync::atomic::AtomicU64::new(0),
            state: core::sync::atomic::AtomicU32::new(SessionState::Zombie as u32),
            process_list: core::sync::atomic::AtomicU64::new(0),
            process_count: core::sync::atomic::AtomicU32::new(0),
            next: core::sync::atomic::AtomicU64::new(0),
        }
    }
}

// ============================================================================
// SessionManager
// ============================================================================

/// 会话管理器 — 以固定容量数组维护会话表, 并自增分配会话 ID。
pub struct SessionManager {
    session_table: Mutex<[Session; MAX_SESSIONS]>,
    next_session_id: core::sync::atomic::AtomicU64,
}

impl SessionManager {
    pub const fn new() -> Self {
        Self {
            session_table: Mutex::new([const { Session::new() }; MAX_SESSIONS]),
            next_session_id: core::sync::atomic::AtomicU64::new(1),
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn init(&self) {
        log("Session manager initialized\n");
    }

    fn alloc_session(&self) -> Option<usize> {
        let table = self.session_table.lock();
        for i in 0..MAX_SESSIONS {
            if table[i].session_id.load(Ordering::SeqCst) == 0 {
                return Some(i);
            }
        }
        None
    }

    /// 创建新会话, 返回 `session_id`
    pub fn create(&self, pwm: u64) -> Option<u64> {
        let idx = self.alloc_session()?;
        let sid = self.next_session_id.fetch_add(1, Ordering::SeqCst);

        let mut table = self.session_table.lock();
        let session = &mut table[idx];

        session.session_id.store(sid, Ordering::SeqCst);
        session.pwm.store(pwm, Ordering::SeqCst);
        session.parent_sid.store(0, Ordering::SeqCst);
        session.terminal.store(0, Ordering::SeqCst);
        session.foreground_pgid.store(0, Ordering::SeqCst);
        session.create_time.store(0, Ordering::SeqCst);
        session
            .state
            .store(SessionState::Active as u32, Ordering::SeqCst);
        session.process_list.store(0, Ordering::SeqCst);
        session.process_count.store(0, Ordering::SeqCst);
        session.next.store(0, Ordering::SeqCst);

        log("Session created: SID=");
        log_num(sid);
        log("\n");

        Some(sid)
    }

    /// 创建会话, SID = `leader_pid` (setsid 语义)
    pub fn create_with_sid(&self, leader_pid: u32, pwm: u64) -> Option<u64> {
        let idx = self.alloc_session()?;
        let sid = u64::from(leader_pid);

        let mut table = self.session_table.lock();
        let session = &mut table[idx];

        session.session_id.store(sid, Ordering::SeqCst);
        session.pwm.store(pwm, Ordering::SeqCst);
        session.parent_sid.store(0, Ordering::SeqCst);
        session.terminal.store(0, Ordering::SeqCst);
        session.foreground_pgid.store(leader_pid, Ordering::SeqCst);
        session.create_time.store(0, Ordering::SeqCst);
        session
            .state
            .store(SessionState::Active as u32, Ordering::SeqCst);
        session.process_list.store(0, Ordering::SeqCst);
        session.process_count.store(0, Ordering::SeqCst);
        session.next.store(0, Ordering::SeqCst);

        Some(sid)
    }

    pub fn destroy(&self, session_id: u64) {
        if let Some(idx) = self.find_session(session_id) {
            let mut table = self.session_table.lock();
            let session = &mut table[idx];

            session.session_id.store(0, Ordering::SeqCst);
            session
                .state
                .store(SessionState::Zombie as u32, Ordering::SeqCst);
            session.process_list.store(0, Ordering::SeqCst);
            session.process_count.store(0, Ordering::SeqCst);
            session.terminal.store(0, Ordering::SeqCst);
            session.foreground_pgid.store(0, Ordering::SeqCst);

            log("Session destroyed: SID=");
            log_num(session_id);
            log("\n");
        }
    }

    fn find_session(&self, session_id: u64) -> Option<usize> {
        let table = self.session_table.lock();
        for i in 0..MAX_SESSIONS {
            if table[i].session_id.load(Ordering::SeqCst) == session_id {
                return Some(i);
            }
        }
        None
    }

    pub fn get_session(&self, session_id: u64) -> Option<usize> {
        self.find_session(session_id)
    }

    /// 设置会话的控制终端
    pub fn set_controlling_terminal(&self, session_id: u64, dev: u64) -> bool {
        if let Some(idx) = self.find_session(session_id) {
            let table = self.session_table.lock();
            let session = &table[idx];
            if session.terminal.load(Ordering::SeqCst) == 0 {
                session.terminal.store(dev, Ordering::SeqCst);
                return true;
            }
        }
        false
    }

    /// 获取会话的控制终端
    pub fn get_controlling_terminal(&self, session_id: u64) -> u64 {
        self.find_session(session_id).map_or(0, |idx| {
            let table = self.session_table.lock();
            table[idx].terminal.load(Ordering::SeqCst)
        })
    }

    /// 释放会话的控制终端
    pub fn release_controlling_terminal(&self, session_id: u64) {
        if let Some(idx) = self.find_session(session_id) {
            let table = self.session_table.lock();
            table[idx].terminal.store(0, Ordering::SeqCst);
            table[idx].foreground_pgid.store(0, Ordering::SeqCst);
        }
    }

    /// 设置前台进程组
    pub fn set_foreground_pgid(&self, session_id: u64, pgid: u32) -> bool {
        if let Some(idx) = self.find_session(session_id) {
            let table = self.session_table.lock();
            table[idx].foreground_pgid.store(pgid, Ordering::SeqCst);
            return true;
        }
        false
    }

    /// 获取前台进程组
    pub fn get_foreground_pgid(&self, session_id: u64) -> u32 {
        self.find_session(session_id).map_or(0, |idx| {
            let table = self.session_table.lock();
            table[idx].foreground_pgid.load(Ordering::SeqCst)
        })
    }
}

pub static SESSION_MANAGER: SessionManager = SessionManager::new();

/// 会话子系统初始化入口 — 初始化全局 [`SESSION_MANAGER`]。
pub fn init() {
    SESSION_MANAGER.init();
}

// ============================================================================
// 系统调用策略
// ============================================================================

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
/// setsid — 创建新会话
pub fn proc_setsid() -> i64 {
    let pid = process_get_current_pid();
    if pid == 0 {
        return -1;
    }

    let is_group_leader = PROCESS_TABLE
        .with_process(pid, |p| {
            let pgid = p.pgid.load(Ordering::SeqCst);
            let effective_pgid = if pgid == 0 { p.pid.0 } else { pgid };
            effective_pgid == pid
        })
        .unwrap_or(false);

    if is_group_leader {
        return -1;
    }

    SESSION_MANAGER.create_with_sid(pid, 0).map_or(-1, |sid| {
        PROCESS_TABLE.with_process(pid, |p| {
            p.session_id.store(sid, Ordering::SeqCst);
            p.pgid.store(pid, Ordering::SeqCst);
        });
        sid as i64
    })
}

/// getsid(pid) — 取会话 ID
pub fn proc_getsid(pid: i32) -> i64 {
    if pid < 0 {
        return -22;
    }

    let target_pid = if pid == 0 {
        process_get_current_pid()
    } else {
        pid as u32
    };

    if target_pid == 0 {
        return -3;
    }

    PROCESS_TABLE
        .with_process(target_pid, |p| {
            let sid = p.session_id.load(Ordering::SeqCst);
            if sid == 0 {
                i64::from(p.pid.0)
            } else {
                sid as i64
            }
        })
        .unwrap_or(-3)
}

#[expect(
    clippy::similar_names,
    reason = "similar_names: 变量名相似表达同族概念; 当前优先 expect"
)]
/// setpgid(pid, pgid) — 设置进程组
pub fn proc_setpgid(pid: i32, pgid: i32) -> i64 {
    if pid < 0 || pgid < 0 {
        return -22;
    }

    let current_pid = process_get_current_pid();
    let target_pid = if pid == 0 { current_pid } else { pid as u32 };
    let new_pgid = if pgid == 0 { target_pid } else { pgid as u32 };

    if target_pid == 0 {
        return -3;
    }

    let current_sid = PROCESS_TABLE
        .with_process(current_pid, |p| p.session_id.load(Ordering::SeqCst))
        .unwrap_or(0);

    // 阶段 1: 单次加锁校验 target 的会话与关系.
    let Some((target_sid, is_self, is_child)) = PROCESS_TABLE.with_process(target_pid, |p| {
        (
            p.session_id.load(Ordering::SeqCst),
            p.pid.0 == current_pid,
            p.parent.is_some_and(|ppid| ppid.0 == current_pid),
        )
    }) else {
        return -3;
    };

    if current_sid != target_sid {
        return -1;
    }
    if !is_self && !is_child {
        return -1;
    }

    // 阶段 2: 校验目标进程组归属 (独立加锁; 不得在 `processes` 锁内嵌套
    // `with_process` / `for_each`, 否则不可重入锁同核自死锁).
    if new_pgid != target_pid && new_pgid != 0 {
        let group_in_session = PROCESS_TABLE
            .with_process(new_pgid, |leader| {
                leader.session_id.load(Ordering::SeqCst) == current_sid
            })
            .unwrap_or(false);

        if !group_in_session {
            let mut found = false;
            PROCESS_TABLE.for_each(|proc| {
                let pg = proc.pgid.load(Ordering::SeqCst);
                let sid = proc.session_id.load(Ordering::SeqCst);
                if pg == new_pgid && sid == current_sid {
                    found = true;
                }
                true
            });
            if !found {
                return -22;
            }
        }
    }

    // 阶段 3: 写入新 pgid.
    match PROCESS_TABLE.with_process(target_pid, |p| {
        p.pgid.store(new_pgid, Ordering::SeqCst);
    }) {
        Some(()) => 0,
        None => -3,
    }
}

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
/// getpgid(pid) — 取进程组 ID
pub fn proc_getpgid(pid: i32) -> i64 {
    if pid < 0 {
        return -22;
    }
    let target_pid = if pid == 0 {
        process_get_current_pid()
    } else {
        pid as u32
    };
    if target_pid == 0 {
        return -3;
    }
    PROCESS_TABLE
        .with_process(target_pid, |proc| {
            let pgid = proc.pgid.load(Ordering::SeqCst);
            if pgid == 0 {
                i64::from(proc.pid.0)
            } else {
                i64::from(pgid)
            }
        })
        .unwrap_or(-3)
}

/// 初始化新创建进程的 pgid
pub fn proc_init_pgid(pid: u32) {
    PROCESS_TABLE.with_process(pid, |proc| {
        let cur = proc.pgid.load(Ordering::SeqCst);
        if cur == 0 {
            proc.pgid.store(pid, Ordering::SeqCst);
        }
    });
}

// ============================================================================
// 控制终端辅助
// ============================================================================

/// TIOCSCTTY — 设置控制终端
pub fn sys_tiocsctty(fd: i32) -> i64 {
    let pid = process_get_current_pid();
    if pid == 0 {
        return -1;
    }

    let sid = PROCESS_TABLE
        .with_process(pid, |p| p.session_id.load(Ordering::SeqCst))
        .unwrap_or(0);

    if sid == 0 {
        return -1;
    }

    if sid != u64::from(pid) {
        return -1;
    }

    let dev = fd as u64;

    if SESSION_MANAGER.set_controlling_terminal(sid, dev) {
        0
    } else {
        -1
    }
}

/// 获取当前进程的控制终端设备号
pub fn get_controlling_terminal() -> u64 {
    let pid = process_get_current_pid();
    if pid == 0 {
        return 0;
    }
    let sid = PROCESS_TABLE
        .with_process(pid, |p| p.session_id.load(Ordering::SeqCst))
        .unwrap_or(0);
    if sid == 0 {
        return 0;
    }
    SESSION_MANAGER.get_controlling_terminal(sid)
}

/// 获取当前会话的前台进程组
pub fn get_foreground_pgid() -> u32 {
    let pid = process_get_current_pid();
    if pid == 0 {
        return 0;
    }
    let sid = PROCESS_TABLE
        .with_process(pid, |p| p.session_id.load(Ordering::SeqCst))
        .unwrap_or(0);
    if sid == 0 {
        return 0;
    }
    SESSION_MANAGER.get_foreground_pgid(sid)
}

/// tcsetpgrp / tcgetpgrp syscall 策略已迁至 functions
/// (functions::proc::session::tcsetpgrp_syscall / tcgetpgrp_syscall, T2 批 2,
/// syscall-followup) — 本文件仅保留机制 (SessionManager / 会话进程组查询辅助).

/// 向前台进程组发送信号
pub fn signal_foreground_pgid(sig: u8) {
    let pgid = get_foreground_pgid();
    if pgid == 0 {
        return;
    }
    crate::privileged::proc::do_signal_send_extended(-(pgid as i32), sig).ok();
}

/// 会话 leader 退出时释放控制终端
pub fn session_leader_exit(pid: u32) {
    let sid = PROCESS_TABLE
        .with_process(pid, |p| p.session_id.load(Ordering::SeqCst))
        .unwrap_or(0);

    if sid == 0 || sid != u64::from(pid) {
        return;
    }

    let fg_pgid = SESSION_MANAGER.get_foreground_pgid(sid);
    if fg_pgid != 0 {
        crate::privileged::proc::do_signal_send_extended(-(fg_pgid as i32), 1).ok();
        crate::privileged::proc::do_signal_send_extended(-(fg_pgid as i32), 18).ok();
    }

    SESSION_MANAGER.release_controlling_terminal(sid);
}
