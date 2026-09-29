#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 framework API。
//! 文件锁 (flock + POSIX record locks) — framework 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 本文件实现曾于 E6 系列迁至 `services::fs::flock`, framework 侧仅
//! re-export。按"机制持有的数据结构/常量归 framework"统一判据反转：
//! flock/POSIX 锁表被 framework VFS 机制内联消费 (vfs/path.rs inode 释放
//! 路径调用 `posix_lock_release_inode`), 锁表是 VFS 机制状态的一部分 —
//! 属机制项, 迁回。依赖闭包仅 framework (IrqSpinLock + core 原子), 0 unsafe。
//!
//! services 侧改 `pub use crate::services::fs::flock::*`
//! 保持 API 兼容 (services→framework 合法方向)。
//!
//! ## 架构
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │  FlockTable (全局)                           │
//! │  ┌─────────────────────────────────────────┐ │
//! │  │ FlockEntry { ino, owner_pid, owner_fd,  │ │
//! │  │              type, count }              │ │
//! │  └─────────────────────────────────────────┘ │
//! │  同一 inode 上: 多个 SH 可共存, EX 独占       │
//! └─────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────┐
//! │  PosixLockTable (全局)                       │
//! │  ┌─────────────────────────────────────────┐ │
//! │  │ PosixLock { ino, pid, start, len,       │ │
//! │  │            type }                       │ │
//! │  └─────────────────────────────────────────┘ │
//! │  字节范围冲突检测: 重叠区间 + 互斥类型判断     │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! ## 设计决策
//!
//! - **固定大小数组**: `no_std` 无堆, 预分配槽位
//! - **flock 关联 (pid, fd)**: close(fd) 或进程退出时自动释放
//! - **POSIX lock 关联 (pid, inode)**: 同一 pid 对同一 inode 的新锁替换旧锁
//! - **`LOCK_NB` 非阻塞**: 立即返回 EAGAIN 而非阻塞等待
//! - **与 Linux 的差异**:
//!   - Linux flock 和 POSIX lock 互不影响; `QueenX` 同样保持独立
//!   - Linux POSIX lock 关联到 (pid, file); `QueenX` 简化为 (pid, inode, range)
//!   - 不实现 `F_SETLKW` 阻塞等待 (v1), 返回 EAGAIN
//!
//! ## 安全契约
//!
//! - 全局状态由 `IrqSpinLock` 守护
//! - close(fd) 时必须调用 `flock_release_fd` 释放该 fd 持有的所有锁
//! - 进程退出时必须调用 `flock_release_pid` 释放该进程所有锁

use core::sync::atomic::{AtomicU64, Ordering};

use crate::framework::sync::IrqSpinLock as Mutex;

// ============================================================================
// 常量
// ============================================================================

/// flock 锁表最大条目数
const FLOCK_TABLE_SIZE: usize = 64;
/// POSIX 锁表最大条目数
const POSIX_LOCK_TABLE_SIZE: usize = 64;

// flock 操作类型
/// 共享锁 (读锁)
pub const LOCK_SH: i32 = 1;
/// 排他锁 (写锁)
pub const LOCK_EX: i32 = 2;
/// 解锁
pub const LOCK_UN: i32 = 8;
/// 非阻塞标志
pub const LOCK_NB: i32 = 4;

// POSIX lock 类型
/// 读锁
pub const F_RDLCK: i32 = 0;
/// 写锁
pub const F_WRLCK: i32 = 1;
/// 解锁
pub const F_UNLCK: i32 = 2;

// fcntl 命令 (扩展)
/// 获取锁 (非阻塞)
pub const F_SETLK: i32 = 6;
/// 获取锁 (阻塞) — v1 返回 EAGAIN
pub const F_SETLKW: i32 = 7;
/// 测试锁
pub const F_GETLK: i32 = 5;

// 特殊值
/// POSIX lock 中 `l_len` = 0 表示锁到文件末尾
pub const POSIX_LOCK_TO_EOF: u64 = 0;

// ============================================================================
// flock 条目
// ============================================================================

/// flock 锁条目
#[derive(Clone, Copy)]
struct FlockEntry {
    /// 被锁定的 inode 号
    ino: u32,
    /// 持有锁的进程 PID
    owner_pid: u32,
    /// 持有锁的 fd (同一进程可多次 open 同一文件)
    owner_fd: i32,
    /// 锁类型: `LOCK_SH` 或 `LOCK_EX`
    lock_type: i32,
    /// 引用计数 (同一 pid+fd 可重复 lock)
    count: u32,
    /// 有效标志
    valid: bool,
}

impl Default for FlockEntry {
    fn default() -> Self {
        Self {
            ino: 0,
            owner_pid: 0,
            owner_fd: -1,
            lock_type: 0,
            count: 0,
            valid: false,
        }
    }
}

// ============================================================================
// POSIX record lock 条目
// ============================================================================

/// POSIX 字节范围锁条目
#[derive(Clone, Copy)]
struct PosixLockEntry {
    /// 被锁定的 inode 号
    ino: u32,
    /// 持有锁的进程 PID
    owner_pid: u32,
    /// 锁起始偏移 (字节)
    start: u64,
    /// 锁长度 (字节), 0 = 到文件末尾
    len: u64,
    /// 锁类型: `F_RDLCK` 或 `F_WRLCK`
    lock_type: i32,
    /// 有效标志
    valid: bool,
}

impl Default for PosixLockEntry {
    fn default() -> Self {
        Self {
            ino: 0,
            owner_pid: 0,
            start: 0,
            len: 0,
            lock_type: 0,
            valid: false,
        }
    }
}

// ============================================================================
// 全局锁表
// ============================================================================

/// 全局 flock 表
static FLOCK_TABLE: Mutex<FlockTable> = Mutex::new(FlockTable::new());
/// 全局 POSIX lock 表
static POSIX_LOCK_TABLE: Mutex<PosixLockTable> = Mutex::new(PosixLockTable::new());

/// 统计: flock 操作次数
static FLOCK_OPS: AtomicU64 = AtomicU64::new(0);
/// 统计: POSIX lock 操作次数
static POSIX_LOCK_OPS: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// FlockTable 实现
// ============================================================================

struct FlockTable {
    entries: [FlockEntry; FLOCK_TABLE_SIZE],
    count: usize,
}

impl FlockTable {
    const fn new() -> Self {
        Self {
            entries: [FlockEntry {
                ino: 0,
                owner_pid: 0,
                owner_fd: -1,
                lock_type: 0,
                count: 0,
                valid: false,
            }; FLOCK_TABLE_SIZE],
            count: 0,
        }
    }

    /// 查找指定 inode 上是否存在冲突的排他锁
    fn find_conflict_shared(&self, ino: u32, exclude_pid: u32) -> Option<u32> {
        for entry in &self.entries {
            if entry.valid
                && entry.ino == ino
                && entry.owner_pid != exclude_pid
                && entry.lock_type == LOCK_EX
            {
                return Some(entry.owner_pid);
            }
        }
        None
    }

    /// 查找指定 inode 上是否存在冲突的锁 (任何类型, 排除自己)
    fn find_conflict_exclusive(&self, ino: u32, exclude_pid: u32) -> Option<u32> {
        for entry in &self.entries {
            if entry.valid && entry.ino == ino && entry.owner_pid != exclude_pid {
                return Some(entry.owner_pid);
            }
        }
        None
    }

    /// 查找指定 (pid, fd) 在指定 inode 上的条目索引
    fn find_entry(&self, ino: u32, pid: u32, fd: i32) -> Option<usize> {
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.valid && entry.ino == ino && entry.owner_pid == pid && entry.owner_fd == fd {
                return Some(i);
            }
        }
        None
    }

    /// 查找空闲槽位
    fn find_free(&self) -> Option<usize> {
        for (i, entry) in self.entries.iter().enumerate() {
            if !entry.valid {
                return Some(i);
            }
        }
        None
    }

    /// 获取指定 inode 上的共享锁计数 (排除指定 pid)
    fn shared_count(&self, ino: u32, exclude_pid: u32) -> usize {
        self.entries
            .iter()
            .filter(|e| {
                e.valid && e.ino == ino && e.owner_pid != exclude_pid && e.lock_type == LOCK_SH
            })
            .count()
    }
}

// ============================================================================
// PosixLockTable 实现
// ============================================================================

struct PosixLockTable {
    entries: [PosixLockEntry; POSIX_LOCK_TABLE_SIZE],
    count: usize,
}

impl PosixLockTable {
    const fn new() -> Self {
        Self {
            entries: [PosixLockEntry {
                ino: 0,
                owner_pid: 0,
                start: 0,
                len: 0,
                lock_type: 0,
                valid: false,
            }; POSIX_LOCK_TABLE_SIZE],
            count: 0,
        }
    }

    /// 检查两个范围是否重叠
    fn ranges_overlap(a_start: u64, a_len: u64, b_start: u64, b_len: u64) -> bool {
        let a_end = if a_len == 0 {
            u64::MAX
        } else {
            a_start.saturating_add(a_len)
        };
        let b_end = if b_len == 0 {
            u64::MAX
        } else {
            b_start.saturating_add(b_len)
        };
        a_start < b_end && b_start < a_end
    }

    /// 查找与指定范围冲突的 POSIX 锁
    fn find_conflict(
        &self,
        ino: u32,
        pid: u32,
        start: u64,
        len: u64,
        lock_type: i32,
    ) -> Option<(u32, i32)> {
        for entry in &self.entries {
            if !entry.valid || entry.ino != ino || entry.owner_pid == pid {
                continue;
            }
            if !Self::ranges_overlap(start, len, entry.start, entry.len) {
                continue;
            }
            if lock_type == F_RDLCK && entry.lock_type == F_RDLCK {
                continue;
            }
            return Some((entry.owner_pid, entry.lock_type));
        }
        None
    }

    /// 查找指定 (pid, ino) 上与指定范围重叠的条目索引
    fn find_overlapping(&self, ino: u32, pid: u32, start: u64, len: u64) -> Option<usize> {
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.valid
                && entry.ino == ino
                && entry.owner_pid == pid
                && Self::ranges_overlap(start, len, entry.start, entry.len)
            {
                return Some(i);
            }
        }
        None
    }

    /// 查找空闲槽位
    fn find_free(&self) -> Option<usize> {
        for (i, entry) in self.entries.iter().enumerate() {
            if !entry.valid {
                return Some(i);
            }
        }
        None
    }
}

// ============================================================================
// 公开 API: flock
// ============================================================================

/// flock 操作结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlockResult {
    /// 成功
    Ok,
    /// 非阻塞模式下锁被占用
    WouldBlock,
    /// 无效参数
    Invalid,
    /// 锁表已满
    NoSpace,
    /// 该 fd 未持有锁 (`LOCK_UN` 时)
    NotHeld,
}

/// flock 系统调用实现
pub fn sys_flock(fd: i32, operation: i32, pid: u32, ino: u32) -> FlockResult {
    FLOCK_OPS.fetch_add(1, Ordering::Relaxed);

    if ino == 0 {
        return FlockResult::Invalid;
    }

    let nonblock = (operation & LOCK_NB) != 0;
    let op = operation & !LOCK_NB;

    match op {
        LOCK_SH => flock_lock(fd, pid, ino, LOCK_SH, nonblock),
        LOCK_EX => flock_lock(fd, pid, ino, LOCK_EX, nonblock),
        LOCK_UN => flock_unlock(fd, pid, ino),
        _ => FlockResult::Invalid,
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn flock_lock(fd: i32, pid: u32, ino: u32, lock_type: i32, nonblock: bool) -> FlockResult {
    let mut table = FLOCK_TABLE.lock();

    // 检查该 (pid, fd) 是否已持有该 inode 的锁
    if let Some(idx) = table.find_entry(ino, pid, fd) {
        let existing_type = table.entries[idx].lock_type;

        // 已持有同类型锁: 增加引用计数
        if existing_type == lock_type {
            table.entries[idx].count = table.entries[idx].count.saturating_add(1);
            return FlockResult::Ok;
        }

        // 升级: SH → EX, 需要检查是否与其他进程持有 SH 锁
        if existing_type == LOCK_SH && lock_type == LOCK_EX {
            let other_shared = table.shared_count(ino, pid);
            if other_shared > 0 {
                if nonblock {
                    return FlockResult::WouldBlock;
                }
                return FlockResult::WouldBlock;
            }
            table.entries[idx].lock_type = LOCK_EX;
            return FlockResult::Ok;
        }

        // 降级: EX → SH, 总是成功
        if existing_type == LOCK_EX && lock_type == LOCK_SH {
            table.entries[idx].lock_type = LOCK_SH;
            return FlockResult::Ok;
        }

        return FlockResult::Ok;
    }

    // 新锁: 检查冲突
    let conflict = match lock_type {
        LOCK_SH => table.find_conflict_shared(ino, pid),
        LOCK_EX => table.find_conflict_exclusive(ino, pid),
        _ => return FlockResult::Invalid,
    };

    if let Some(_conflict_pid) = conflict {
        if nonblock {
            return FlockResult::WouldBlock;
        }
        return FlockResult::WouldBlock;
    }

    // 无冲突, 分配新条目
    let idx = match table.find_free() {
        Some(i) => i,
        None => return FlockResult::NoSpace,
    };

    table.entries[idx] = FlockEntry {
        ino,
        owner_pid: pid,
        owner_fd: fd,
        lock_type,
        count: 1,
        valid: true,
    };
    table.count += 1;

    FlockResult::Ok
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn flock_unlock(fd: i32, pid: u32, ino: u32) -> FlockResult {
    let mut table = FLOCK_TABLE.lock();

    let idx = match table.find_entry(ino, pid, fd) {
        Some(i) => i,
        None => return FlockResult::NotHeld,
    };

    let count = table.entries[idx].count;
    if count > 1 {
        table.entries[idx].count -= 1;
    } else {
        table.entries[idx] = FlockEntry::default();
        table.count -= 1;
    }

    FlockResult::Ok
}

/// 释放指定 fd 持有的所有 flock 锁
pub fn flock_release_fd(pid: u32, fd: i32) {
    let mut table = FLOCK_TABLE.lock();
    let mut released = 0u32;
    for entry in &mut table.entries {
        if entry.valid && entry.owner_pid == pid && entry.owner_fd == fd {
            *entry = FlockEntry::default();
            released += 1;
        }
    }
    table.count -= released as usize;
}

/// 释放指定进程持有的所有 flock 锁
pub fn flock_release_pid(pid: u32) {
    let mut table = FLOCK_TABLE.lock();
    let mut released = 0u32;
    for entry in &mut table.entries {
        if entry.valid && entry.owner_pid == pid {
            *entry = FlockEntry::default();
            released += 1;
        }
    }
    table.count -= released as usize;
}

// ============================================================================
// 公开 API: POSIX record locks
// ============================================================================

/// POSIX lock 操作结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosixLockResult {
    /// 非阻塞模式下锁被占用, 或 `F_GETLK` 发现冲突锁
    WouldBlock,
    /// 无效参数
    Invalid,
    /// 锁表已满
    NoSpace,
}

/// POSIX 锁查询结果 (`F_GETLK` 返回)
#[derive(Debug, Clone, Copy)]
pub struct PosixLockConflict {
    /// 冲突锁的持有者 PID
    pub pid: u32,
    /// 冲突锁类型
    pub lock_type: i32,
    /// 冲突锁起始偏移
    pub start: u64,
    /// 冲突锁长度
    pub len: u64,
}

/// fcntl `F_SETLK` / `F_GETLK` 实现
///
/// # Errors
/// 当 `ino` 为 0 或 `cmd` 不是 `F_GETLK`/`F_SETLK`/`F_SETLKW` 时返回 `Invalid`;
/// 其余错误由底层 `posix_getlk`/`posix_setlk` 传播.
pub fn sys_posix_lock(
    pid: u32,
    ino: u32,
    cmd: i32,
    lock_type: i32,
    start: u64,
    len: u64,
) -> Result<Option<PosixLockConflict>, PosixLockResult> {
    POSIX_LOCK_OPS.fetch_add(1, Ordering::Relaxed);

    if ino == 0 {
        return Err(PosixLockResult::Invalid);
    }

    match cmd {
        F_GETLK => posix_getlk(pid, ino, lock_type, start, len),
        F_SETLK | F_SETLKW => posix_setlk(pid, ino, lock_type, start, len, cmd == F_SETLKW),
        _ => Err(PosixLockResult::Invalid),
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
fn posix_getlk(
    pid: u32,
    ino: u32,
    lock_type: i32,
    start: u64,
    len: u64,
) -> Result<Option<PosixLockConflict>, PosixLockResult> {
    if lock_type == F_UNLCK {
        return Ok(None);
    }

    let table = POSIX_LOCK_TABLE.lock();
    match table.find_conflict(ino, pid, start, len, lock_type) {
        Some((conflict_pid, conflict_type)) => {
            for entry in &table.entries {
                if entry.valid
                    && entry.owner_pid == conflict_pid
                    && PosixLockTable::ranges_overlap(start, len, entry.start, entry.len)
                {
                    return Ok(Some(PosixLockConflict {
                        pid: conflict_pid,
                        lock_type: conflict_type,
                        start: entry.start,
                        len: entry.len,
                    }));
                }
            }
            Ok(Some(PosixLockConflict {
                pid: conflict_pid,
                lock_type: conflict_type,
                start: 0,
                len: 0,
            }))
        }
        None => Ok(None),
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
fn posix_setlk(
    pid: u32,
    ino: u32,
    lock_type: i32,
    start: u64,
    len: u64,
    _wait: bool,
) -> Result<Option<PosixLockConflict>, PosixLockResult> {
    let mut table = POSIX_LOCK_TABLE.lock();

    match lock_type {
        F_UNLCK => {
            let mut released = 0u32;
            for i in 0..POSIX_LOCK_TABLE_SIZE {
                let entry = &table.entries[i];
                let should_remove = entry.valid
                    && entry.ino == ino
                    && entry.owner_pid == pid
                    && PosixLockTable::ranges_overlap(start, len, entry.start, entry.len);
                if should_remove {
                    table.entries[i] = PosixLockEntry::default();
                    released += 1;
                }
            }
            table.count -= released as usize;
            Ok(None)
        }
        F_RDLCK | F_WRLCK => {
            if let Some((conflict_pid, conflict_type)) =
                table.find_conflict(ino, pid, start, len, lock_type)
            {
                return Ok(Some(PosixLockConflict {
                    pid: conflict_pid,
                    lock_type: conflict_type,
                    start: 0,
                    len: 0,
                }));
            }

            if let Some(idx) = table.find_overlapping(ino, pid, start, len) {
                table.entries[idx].lock_type = lock_type;
                table.entries[idx].start = start;
                table.entries[idx].len = len;
                return Ok(None);
            }

            let idx = match table.find_free() {
                Some(i) => i,
                None => return Err(PosixLockResult::NoSpace),
            };

            table.entries[idx] = PosixLockEntry {
                ino,
                owner_pid: pid,
                start,
                len,
                lock_type,
                valid: true,
            };
            table.count += 1;

            Ok(None)
        }
        _ => Err(PosixLockResult::Invalid),
    }
}

/// 释放指定进程持有的所有 POSIX 锁
pub fn posix_lock_release_pid(pid: u32) {
    let mut table = POSIX_LOCK_TABLE.lock();
    let mut released = 0u32;
    for entry in &mut table.entries {
        if entry.valid && entry.owner_pid == pid {
            *entry = PosixLockEntry::default();
            released += 1;
        }
    }
    table.count -= released as usize;
}

/// 释放指定 inode 上的所有 POSIX 锁
pub fn posix_lock_release_inode(ino: u32) {
    let mut table = POSIX_LOCK_TABLE.lock();
    let mut released = 0u32;
    for entry in &mut table.entries {
        if entry.valid && entry.ino == ino {
            *entry = PosixLockEntry::default();
            released += 1;
        }
    }
    table.count -= released as usize;
}

// ============================================================================
// 统计
// ============================================================================

/// flock 操作次数
pub fn flock_ops() -> u64 {
    FLOCK_OPS.load(Ordering::Relaxed)
}

/// POSIX lock 操作次数
pub fn posix_lock_ops() -> u64 {
    POSIX_LOCK_OPS.load(Ordering::Relaxed)
}

/// flock 条目数
pub fn flock_count() -> usize {
    FLOCK_TABLE.lock().count
}

/// POSIX lock 条目数
pub fn posix_lock_count() -> usize {
    POSIX_LOCK_TABLE.lock().count
}

/// 重置统计计数器
pub fn reset_stats() {
    FLOCK_OPS.store(0, Ordering::Relaxed);
    POSIX_LOCK_OPS.store(0, Ordering::Relaxed);
}
