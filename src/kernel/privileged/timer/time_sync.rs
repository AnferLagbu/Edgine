//! NTP/PTP 时钟同步 — 时钟调整状态承接 (TCB 状态面)
//!
//! NTP/PTP 类型、算法、调整策略与 `timesync` syscall 分发已下沉
//! `functions::timer::time_sync` (0 unsafe, 见 docs/plan §6.3)。本模块仅保留
//! tick 中断直接消费的状态与机制:
//!
//! - [`ClockAdjState`]: 时钟调整状态 (原子字段), 供 hardirq 消耗, 并由 functions
//!   策略层经安全原子访问面读写;
//! - [`ClockAdjState::tick_adjust`]: 每个 tick 消耗一步渐进偏移 (仅原子操作,
//!   hardirq 上下文安全);
//! - [`ADJ_RATE_NS`]: 渐进调整步长 (tick 机制参数).
//!
//! ## SAFETY
//!
//! 本模块属于 privileged/TCB, 允许 unsafe; 当前实现无 unsafe 块.

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// 渐进调整速率 (每次 tick 调整的最大 ns)
pub const ADJ_RATE_NS: i64 = 1000; // 1us/tick

/// 时钟调整状态 (TCB 状态: tick 中断直接消费)
#[derive(Debug)]
pub struct ClockAdjState {
    /// 当前频率调整 (ppb, 十亿分之一)
    freq_adj_ppb: AtomicI64,
    /// 待调整偏移 (ns), 渐进消耗
    offset_remaining: AtomicI64,
    /// 累计偏移调整 (ns)
    total_offset_adj: AtomicI64,
    /// 累计频率调整 (ppb)
    total_freq_adj: AtomicI64,
    /// 上次 NTP 同步时间 (ns)
    last_sync_time: AtomicU64,
    /// 同步次数
    sync_count: AtomicU64,
    /// 时钟是否同步
    synced: AtomicBool,
}

impl ClockAdjState {
    /// 创建零初始化的调整状态
    pub const fn new() -> Self {
        Self {
            freq_adj_ppb: AtomicI64::new(0),
            offset_remaining: AtomicI64::new(0),
            total_offset_adj: AtomicI64::new(0),
            total_freq_adj: AtomicI64::new(0),
            last_sync_time: AtomicU64::new(0),
            sync_count: AtomicU64::new(0),
            synced: AtomicBool::new(false),
        }
    }

    /// 当前频率调整 (ppb) 的原子访问面
    pub fn freq_adj_ppb(&self) -> &AtomicI64 {
        &self.freq_adj_ppb
    }

    /// 待消耗渐进偏移 (ns) 的原子访问面
    pub fn offset_remaining(&self) -> &AtomicI64 {
        &self.offset_remaining
    }

    /// 累计偏移调整 (ns) 的原子访问面
    pub fn total_offset_adj(&self) -> &AtomicI64 {
        &self.total_offset_adj
    }

    /// 累计频率调整 (ppb) 的原子访问面
    pub fn total_freq_adj(&self) -> &AtomicI64 {
        &self.total_freq_adj
    }

    /// 上次同步时刻 (ns) 的原子访问面
    pub fn last_sync_time(&self) -> &AtomicU64 {
        &self.last_sync_time
    }

    /// 同步次数的原子访问面
    pub fn sync_count(&self) -> &AtomicU64 {
        &self.sync_count
    }

    /// 同步标志的原子访问面
    pub fn synced(&self) -> &AtomicBool {
        &self.synced
    }

    /// 每个 tick 调用: 消耗一步渐进偏移
    ///
    /// 返回本次调整的 ns 偏移. 仅原子操作, 可在 hardirq 上下文安全调用.
    pub fn tick_adjust(&self) -> i64 {
        let remaining = self.offset_remaining.load(Ordering::Acquire);
        if remaining == 0 {
            return 0;
        }

        let adj = if remaining.abs() <= ADJ_RATE_NS {
            let r = remaining;
            self.offset_remaining.store(0, Ordering::Release);
            r
        } else if remaining > 0 {
            self.offset_remaining
                .fetch_sub(ADJ_RATE_NS, Ordering::Relaxed);
            ADJ_RATE_NS
        } else {
            self.offset_remaining
                .fetch_add(ADJ_RATE_NS, Ordering::Relaxed);
            -ADJ_RATE_NS
        };

        self.total_offset_adj.fetch_add(adj, Ordering::Relaxed);
        adj
    }
}

/// 全局时钟调整状态
static CLOCK_ADJ: ClockAdjState = ClockAdjState::new();

/// 获取全局时钟调整状态
///
/// functions 策略层经此取得安全原子访问面, 完成 NTP 调整/墙钟换算策略.
pub fn clock_adj_state() -> &'static ClockAdjState {
    &CLOCK_ADJ
}
