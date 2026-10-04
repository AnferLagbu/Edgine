//! 调度决策 trait — 策略-机制分离接口
//!
//! T-01: 调度决策 (哪个线程先跑、何时提升、时间片多长) 由 functions 实现,
//! privileged 仅保留上下文切换、RunQueue 操作等机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型 `ThreadPriority`)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackMlfqPolicy`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_sched_decision()` 注册自己的策略实现
//!
//! ## 命名
//!
//! 命名为 `SchedDecision` 而非 `SchedPolicy`, 因为 `SchedPolicy` 已被
//! `privileged::proc::scheduler` 中的 enum (Normal/Fifo/Rr/Idle/Deadline) 占用.

pub use super::types::ThreadPriority;
use crate::privileged::sync::OnceLock;

/// 调度决策接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait SchedDecision: Send + Sync {
    /// 从就绪队列中选择下一个运行的优先级索引
    ///
    /// `queue_lengths[i]` 为优先级 i 的就绪线程数.
    /// 返回 `None` 表示无就绪线程 (idle).
    fn pick_next_priority(&self, queue_lengths: [u32; 5]) -> Option<usize>;

    /// 是否应触发优先级提升
    fn should_boost(&self, tick_count: u64, last_boost: u64) -> bool;

    /// 优先级提升后的目标优先级
    fn boost_target(&self) -> ThreadPriority;

    /// 计算指定优先级的时间片
    fn time_slice_for(&self, priority: ThreadPriority) -> u32;

    /// 时间片耗尽判定 — 返回 true 表示应重新调度
    fn should_reschedule(&self, time_slice_remaining: u32) -> bool;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略
///
/// 在 functions 注册策略之前, 调度器使用此策略.
/// 逻辑与原硬编码行为一致.
pub struct FallbackPolicy;

impl SchedDecision for FallbackPolicy {
    fn pick_next_priority(&self, queue_lengths: [u32; 5]) -> Option<usize> {
        for prio in (0..5).rev() {
            if queue_lengths[prio] > 0 {
                return Some(prio);
            }
        }
        None
    }

    fn should_boost(&self, tick_count: u64, last_boost: u64) -> bool {
        tick_count.saturating_sub(last_boost) >= super::types::SCHED_BOOST_INTERVAL as u64
    }

    fn boost_target(&self) -> ThreadPriority {
        ThreadPriority::High
    }

    fn time_slice_for(&self, priority: ThreadPriority) -> u32 {
        use super::types::{
            SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM,
            SCHED_LEVEL_3_QUANTUM, ThreadPriority,
        };
        match priority {
            ThreadPriority::Realtime => SCHED_LEVEL_0_QUANTUM,
            ThreadPriority::High => SCHED_LEVEL_1_QUANTUM,
            ThreadPriority::Normal => SCHED_LEVEL_2_QUANTUM,
            ThreadPriority::Low => SCHED_LEVEL_3_QUANTUM,
            // DECISION-072: u32::MAX = "永不过期"语义; 仅当无其他优先级任务时被调度
            ThreadPriority::Idle => u32::MAX,
        }
    }

    fn should_reschedule(&self, time_slice_remaining: u32) -> bool {
        time_slice_remaining <= 1
    }
}

static FALLBACK_POLICY: FallbackPolicy = FallbackPolicy;

/// 全局策略注册表 — functions 通过 `register_sched_decision` 注册
static SCHED_DECISION: OnceLock<&'static dyn SchedDecision> = OnceLock::new();

/// 注册调度决策策略 (由 `functions::proc::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_sched_decision(
    policy: &'static dyn SchedDecision,
) -> Result<(), &'static dyn SchedDecision> {
    match SCHED_DECISION.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的调度决策策略 (未注册时返回内建回退)
#[inline]
pub fn current_sched_decision() -> &'static dyn SchedDecision {
    match SCHED_DECISION.get() {
        Some(&p) => p,
        None => &FALLBACK_POLICY,
    }
}
