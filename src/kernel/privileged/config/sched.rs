//! 调度器常量 (SCHED_\*/CFS_\*) — privileged 机制常量
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原 SCHED_\* 常量于 T6-9 (2026-06-16) 迁至 `functions::config::sched`, 本文件仅
//! re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：`SCHED_*`
//! 被 privileged proc 调度机制 (user_proc/scheduler_ex) 直接消费 — 属机制常量,
//! 迁回。
//!
//! ## DECISION-O ② 收敛单向记录 (2026-09-13)
//!
//! `CFS_*` 于 T6-9 与 SCHED_* 一同迁 functions, 当时前提是"仅被 functions
//! sched_policy 策略消费"。DECISION-J 将 CfsRunQueue/DlRunQueue 反转迁回
//! privileged/proc/cfs.rs 后, privileged 机制 (cfs.rs/scheduler.rs/process.rs)
//! 直接消费 `CFS_*` — 前提失效, 按同一判据迁回本文件; functions 侧改 re-export
//! (functions→privileged 合法方向), cfs 依赖收敛单向。
//!
//! functions 侧 re-export SCHED_*/CFS_* 保持 API 兼容。
//! 本文件 0 unsafe (纯常量).

// ============================================================================
// 通用调度器常量
// ============================================================================

/// 调度级别 0 量子 (最高优先级, 实时).
pub const SCHED_LEVEL_0_QUANTUM: u32 = 80;

/// 调度级别 1 量子.
pub const SCHED_LEVEL_1_QUANTUM: u32 = 60;

/// 调度级别 2 量子.
pub const SCHED_LEVEL_2_QUANTUM: u32 = 40;

/// 调度级别 3 量子 (最低优先级, idle).
pub const SCHED_LEVEL_3_QUANTUM: u32 = 20;

/// 调度器提升检查间隔 (tick 数).
pub const SCHED_BOOST_INTERVAL: u64 = 1000;

/// 实时调度器看门狗超时 (tick 数).
pub const SCHED_RT_WATCHDOG_TICKS: u64 = 500;

// ============================================================================
// CFS (Completely Fair Scheduler) 常量
// ============================================================================

/// CFS 目标延迟 (调度器 tick 数).
pub const CFS_TARGET_LATENCY: u64 = 60;

/// CFS 最小粒度 (调度器 tick 数).
pub const CFS_MIN_GRANULARITY: u64 = 8;

/// CFS 提升检查间隔 (调度器 tick 数).
pub const CFS_BOOST_INTERVAL: u64 = 1000;

/// CFS 默认 nice=0 任务的权重.
pub const CFS_NICE0_WEIGHT: u64 = 1024;

// ============================================================================
// 截止期调度 (EDF + CBS)
// ============================================================================

/// Deadline 调度器最小运行时间 (tick 数).
pub const CFS_DL_MIN_RUNTIME: u64 = 1;

/// Deadline 调度器最小周期 (tick 数).
pub const CFS_DL_MIN_PERIOD: u64 = 10;

/// Deadline 调度器最大利用率 (百分比).
pub const CFS_DL_MAX_UTILIZATION_PCT: u64 = 95;
