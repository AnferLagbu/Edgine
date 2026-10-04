//! # 全局统一 FD 分配器 (TD-02) — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原代码于 T6-5 (2026-06-16) 迁至 `functions::proc::fd_alloc`, 本文件仅 re-export。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：全局 FD 编号是**用户态
//! 可见的内核机制** (集中基址规划 + 位图分配/释放/反查), 被 privileged 4 处
//! (sm_fi/eventfd/signalfd/timerfd) + functions (inotify/pidfd) 消费 — 属机制项,
//! 迁回。依赖闭包为空 (纯 core 原子 + 编译期 const 规划)。
//!
//! functions 侧改 `pub use crate::privileged::proc::fd_alloc::*` 保持 API 兼容。
//! 本文件 0 unsafe.
//!
//! ## 背景
//!
//! 历史 7 个独立 fd 分配器 (VFS/UNKFS/UDS/smoltcp/EVENTFD/SIGNALFD/INOTIFY) 分散在 privileged 与 functions, 同一进程内可能拿到相同 fd 编号, 进程级 `read/write` 无法可靠分发.
//! 2026-06-12 I-51 修复了 UDS 重叠; 2026-06-12 TD-01 修复了 EFD/SFD/INOTIFY 重叠. 本模块在基址层修复之上, 提供**单一入口**与**集中基址规划**.
//!
//! ## 范围
//!
//! 集中管理**用户态可见的全局 FD 编号**:
//!
//! | 子系统 | 起点 | 上限 | 容量 | 来源 |
//! |--------|------|------|------|------|
//! | Smoltcp | 0 | MAX_SM_FD | 256 | `privileged/net/init.rs` |
//! | Uds     | 1000 | UDS 范围 | 16 | `privileged/net/unix.rs` |
//! | EventFd | 1100 | EFD 范围 | 16 | `privileged/syscall/eventfd.rs` |
//! | SignalFd| 1120 | SFD 范围 | 16 | `privileged/syscall/signalfd.rs` |
//! | Inotify | 1140 | INOTIFY 范围 | 16 | `functions/fs/inotify.rs` |
//! | TimerFd | 1160 | `TIMER_FD` 范围 | 16 | `privileged/syscall/timerfd.rs` |
//! | PidFd   | 1180 | `PID_FD` 范围 | 16 | `functions/proc/pidfd.rs` |
//! | UserFaultFd | 1200 | `USERFAULT_FD` 范围 | 16 | `privileged/mm/uffd.rs` |
//!
//! **不包含** (这些是内部抽象, 不暴露给用户态, 不存在重叠问题):
//! - `VfsManager::alloc_fd()` — VFS 内部 slot 索引
//! - `Unkfs::alloc_fd()` — UNKFS 内部 slot 索引
//! - `FdTable::alloc_fd()` — per-process 视图映射
//!
//! ## 架构
//!
//! ```text
//! 用户态可见的全局 FD 编号
//!   ↓
//! FdSubsystem enum (5 个变体)  ← 用户必须显式声明
//!   ↓
//! FdPlan::range_for(sub)  ← 集中基址规划, 编译期 const
//!   ↓
//! alloc_fd / free_fd       ← 集中分配/释放, 静态契约测试守护
//! ```
//!
//! ## SAFETY
//!
//! - 全局 `static mut` 槽位表, 单线程启动期 (Boot 阶段) 初始化后只读
//! - 运行时 `alloc` / `free` 走 `Atomic` 原子位, 无锁
//! - 调用方在中断上下文禁止使用 (会与进程上下文并发)
//!
//! ## 演进
//!
//! - V1 (当前): 仅规划基址, alloc/free 接口预留, 现有子系统未强制改用
//! - V2 (完成): 所有子系统已迁移到 `alloc_fd(FdSubsystem::X)` (含 Smoltcp)
//!
//! V2 位图分配器支持槽位回收, 替代 V1 单调计数器.
//! `subsystem_of` 保留供 FD 分发使用 (测试要求暴露).

// ============================================================================
// 子系统枚举
// ============================================================================

/// 全局 FD 子系统标识
///
/// 与 `FdPlan::range_for` 一一对应. 新增子系统必须更新 `FdPlan::range_for` 与
/// `count_subsystems` 计数.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FdSubsystem {
    /// smoltcp TCP/UDP socket
    Smoltcp = 0,
    /// `AF_UNIX` 域套接字
    Uds = 1,
    /// eventfd
    EventFd = 2,
    /// signalfd
    SignalFd = 3,
    /// inotify
    Inotify = 4,
    /// timerfd
    TimerFd = 5,
    /// pidfd (pidfd_open)
    PidFd = 6,
    /// userfaultfd (userfaultfd)
    UserFaultFd = 7,
}

impl FdSubsystem {
    /// 子系统数量 (用于范围表边界)
    pub const COUNT: usize = 8;

    /// 通过下标获取子系统 (用于 `for i in 0..COUNT { ... }`)
    pub fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(Self::Smoltcp),
            1 => Some(Self::Uds),
            2 => Some(Self::EventFd),
            3 => Some(Self::SignalFd),
            4 => Some(Self::Inotify),
            5 => Some(Self::TimerFd),
            6 => Some(Self::PidFd),
            7 => Some(Self::UserFaultFd),
            _ => None,
        }
    }
}

// ============================================================================
// 集中基址规划
// ============================================================================

/// 子系统 FD 范围 (基址 + 容量)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdRange {
    pub base: i32,
    pub capacity: u16,
}

impl FdRange {
    pub const fn new(base: i32, capacity: u16) -> Self {
        Self { base, capacity }
    }

    pub const fn end_exclusive(self) -> i32 {
        self.base + self.capacity as i32
    }

    pub const fn contains(self, fd: i32) -> bool {
        fd >= self.base && fd < self.end_exclusive()
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.base < other.end_exclusive() && other.base < self.end_exclusive()
    }
}

/// 集中 FD 基址规划 — 单一来源 (Single Source of Truth)
///
/// 各子系统的 `*_FD_BASE` 常量在编译期引用本规划, 禁止分散定义.
/// 验收: 任意两个 `FdRange` 不重叠; 全部不与 smoltcp [0, 256) 重叠 (除 Smoltcp 自身).
pub struct FdPlan;

impl FdPlan {
    /// Smoltcp FD 空间 (TD-06: 容量从 `cfg_smoltcp_cap()` 派生, 当前默认 256.
    /// 用户可手动修改 `cfg_smoltcp_cap` 至 1024 / 4096, 同步 `privileged/net/init.rs` 的
    /// `MAX_SOCKETS` 与 buf 静态表尺寸 (`TCP_RX_BUFS` / `TCP_TX_BUFS` / UDP_*_BUFS)).
    pub const SMOLTCP: FdRange = FdRange::new(0, 256);

    /// UDS FD 空间 (TD-01: 历史 100 → 1000, 跳出 smoltcp)
    pub const UDS: FdRange = FdRange::new(1000, 16);

    /// `EventFd` FD 空间 (TD-01: 历史 200 → 1100)
    pub const EVENT_FD: FdRange = FdRange::new(1100, 16);

    /// `SignalFd` FD 空间 (TD-01: 历史 220 → 1120)
    pub const SIGNAL_FD: FdRange = FdRange::new(1120, 16);

    /// Inotify FD 空间 (TD-01: 历史 260 → 1140)
    pub const INOTIFY: FdRange = FdRange::new(1140, 16);

    /// `TimerFd` FD 空间 (TD-15: 历史 240 → 1160, 跳出 smoltcp [0, 256))
    pub const TIMER_FD: FdRange = FdRange::new(1160, 16);

    /// `PidFd` FD 空间 (pidfd_open, 2026: 新增)
    pub const PID_FD: FdRange = FdRange::new(1180, 16);

    /// `UserFaultFd` FD 空间 (userfaultfd, 2026: 新增)
    pub const USERFAULT_FD: FdRange = FdRange::new(1200, 16);

    /// 获取指定子系统的 FD 范围
    pub const fn range_for(sub: FdSubsystem) -> FdRange {
        match sub {
            FdSubsystem::Smoltcp => Self::SMOLTCP,
            FdSubsystem::Uds => Self::UDS,
            FdSubsystem::EventFd => Self::EVENT_FD,
            FdSubsystem::SignalFd => Self::SIGNAL_FD,
            FdSubsystem::Inotify => Self::INOTIFY,
            FdSubsystem::TimerFd => Self::TIMER_FD,
            FdSubsystem::PidFd => Self::PID_FD,
            FdSubsystem::UserFaultFd => Self::USERFAULT_FD,
        }
    }

    /// 全部 FD 范围 (用于启动期不变量校验)
    pub const ALL: &'static [FdRange] = &[
        Self::SMOLTCP,
        Self::UDS,
        Self::EVENT_FD,
        Self::SIGNAL_FD,
        Self::INOTIFY,
        Self::TIMER_FD,
        Self::PID_FD,
        Self::USERFAULT_FD,
    ];

    /// 启动期不变量: 任意两个范围不重叠
    ///
    /// 编译期 const fn, 可在测试或启动代码中调用.
    pub const fn ranges_non_overlapping() -> bool {
        let all = Self::ALL;
        let mut i = 0;
        while i < all.len() {
            let mut j = i + 1;
            while j < all.len() {
                if all[i].overlaps(all[j]) {
                    // Smoltcp 与自身比较时 base == end_exclusive, 不会重叠
                    // 但 Smoltcp 与其他 4 个范围不重叠 (其他 4 个 base ≥ 1000 ≥ 256)
                    return false;
                }
                j += 1;
            }
            i += 1;
        }
        true
    }
}

// ============================================================================
// V2 接口: alloc_fd / free_fd (支持槽位回收)
// ============================================================================
//
// 使用位图跟踪每个子系统的槽位占用状态, 支持 alloc/free 循环.

use core::sync::atomic::{AtomicU8, Ordering};

/// 每个子系统的位图: 16 字节 = 128 位, 足够覆盖最大容量 256
const BITMAP_SIZE: usize = 32;

/// 全局槽位占用位图 (每个子系统一个)
static FD_BITMAPS: [AtomicU8; FdSubsystem::COUNT * BITMAP_SIZE] =
    [const { AtomicU8::new(0) }; FdSubsystem::COUNT * BITMAP_SIZE];

/// 给定子系统分配一个全局 FD 编号
///
/// V2: 使用位图跟踪槽位占用, 支持 free 后回收.
pub fn alloc_fd(sub: FdSubsystem) -> Option<i32> {
    let range = FdPlan::range_for(sub);
    let base = range.base as usize;
    let capacity = range.capacity as usize;
    let bitmap_offset = sub as usize * BITMAP_SIZE;

    // 扫描位图寻找空闲槽位
    for i in 0..capacity {
        let byte_idx = bitmap_offset + i / 8;
        let bit_idx = i % 8;
        let byte = FD_BITMAPS[byte_idx].load(Ordering::Acquire);
        if byte & (1 << bit_idx) == 0 {
            // 找到空闲槽位, 标记为占用
            FD_BITMAPS[byte_idx].store(byte | (1 << bit_idx), Ordering::Release);
            return Some((base + i) as i32);
        }
    }
    None
}

/// 释放一个 FD 编号
///
/// V2: 清除位图中的占用标记, 支持槽位回收.
pub fn free_fd(sub: FdSubsystem, fd: i32) -> bool {
    let range = FdPlan::range_for(sub);
    if !range.contains(fd) {
        return false;
    }
    let slot = (fd - range.base) as usize;
    let bitmap_offset = sub as usize * BITMAP_SIZE;
    let byte_idx = bitmap_offset + slot / 8;
    let bit_idx = slot % 8;
    let byte = FD_BITMAPS[byte_idx].load(Ordering::Acquire);
    FD_BITMAPS[byte_idx].store(byte & !(1 << bit_idx), Ordering::Release);
    true
}

/// 通过 FD 编号反查所属子系统
///
/// 用于 `sys_read/write` 分发: 给定进程可见的 fd, 找到对应子系统.
pub fn subsystem_of(fd: i32) -> Option<FdSubsystem> {
    let mut i = 0;
    while i < FdSubsystem::COUNT {
        let sub = FdSubsystem::from_index(i)?;
        if FdPlan::range_for(sub).contains(fd) {
            return Some(sub);
        }
        i += 1;
    }
    None
}

// ============================================================================
// 启动期校验
// ============================================================================

// ============================================================================
// 启动期校验
// ============================================================================

/// 启动时调用, 校验 `FdPlan` 不变量. 不满足则 panic (不变量违反是编译/规划错误)
///
/// # Panics
///
/// 当 `FdPlan` 的 FD 范围发生重叠、违反 TD-02 不变量时 panic.
pub fn verify_plan() {
    assert!(
        FdPlan::ranges_non_overlapping(),
        "FD 范围重叠违反 TD-02 不变量"
    );
}

// ============================================================================
// V3 接口: fd_at / max_slots
// ============================================================================

/// 给定子系统 + slot 索引, 计算对应 FD 编号
///
/// V3 替代各子系统的 `*_FD_BASE + i as i32` 模式, 集中表达式, 避免分散的 `base + idx`.
/// `const fn`, 可在静态表初始化与 `panic!` 等不可变上下文中使用.
#[inline]
pub const fn fd_at(sub: FdSubsystem, slot: usize) -> i32 {
    FdPlan::range_for(sub).base + slot as i32
}

/// 给定子系统, 返回最大 slot 数 (用于 `for i in 0..max_slots(sub)` 替代硬编码 `MAX_xxx`)
///
/// V3 替代各子系统的 `EFD_MAX_SLOTS` / `SFD_MAX_SLOTS` / `MAX_UDS_FD` / `INOTIFY_MAX_INSTANCES` /
/// smoltcp `MAX_SM_FD` 等容量常量, 集中表达式, 避免分散字面量.
#[inline]
pub const fn max_slots(sub: FdSubsystem) -> usize {
    FdPlan::range_for(sub).capacity as usize
}

// ============================================================================
// V4 接口: idx_of
// ============================================================================

/// 给定 FD 编号, 反查所属子系统与 slot 索引
///
/// V4 替代各子系统的本地 `fd_to_idx` 函数 (`eventfd` / `signalfd` / `timerfd` /
/// `unix`), 集中表达式. 子系统本地不再持有 `*_FD_BASE` 字面量 + 减法边界检查.
///
/// 返回 `Some((sub, slot))` 当 `fd ∈ FdPlan::range_for(sub)` 时, 否则 `None`.
///
/// 验收: 任意 `fd ∈ [base, base + capacity)` 都唯一映射到 `(sub, slot)`.
#[inline]
pub fn idx_of(fd: i32) -> Option<(FdSubsystem, usize)> {
    let mut i = 0;
    while i < FdSubsystem::COUNT {
        let sub = FdSubsystem::from_index(i)?;
        let range = FdPlan::range_for(sub);
        if range.contains(fd) {
            return Some((sub, (fd - range.base) as usize));
        }
        i += 1;
    }
    None
}
