#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯 re-export 兼容层。
//! 调度器常量 — functions 侧 SCHED_*/EEVDF_* 兼容层
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原 EEVDF_*/SCHED_* 常量于 T6-9 (2026-06-16) 迁至此处。`SCHED_*` 被 privileged
//! proc 调度机制消费, 按"机制持有的数据结构/常量归 privileged"判据迁回
//! `privileged/config/sched.rs`。
//!
//! ## DECISION-O ② 收敛单向记录 (2026-09-13)
//!
//! `EEVDF_*` 原留在此处的前提是"仅被 functions sched_policy 策略消费"; DECISION-J
//! 将 EevdfRunQueue/DlRunQueue 反转迁回 privileged/proc/eevdf.rs 后, privileged 机制
//! 直接消费 `EEVDF_*` — 前提失效, 权威迁回 `privileged/config/sched.rs`, 本文件改
//! 纯 re-export 保持 functions 侧 API 兼容 (functions→privileged 合法方向),
//! eevdf 依赖收敛单向。

pub use crate::privileged::config::{
    DL_MAX_UTILIZATION_PCT, DL_MIN_PERIOD, DL_MIN_RUNTIME, EEVDF_MIN_GRANULARITY,
    EEVDF_NICE0_WEIGHT, EEVDF_TARGET_LATENCY, SCHED_BOOST_INTERVAL, SCHED_LEVEL_0_QUANTUM,
    SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM, SCHED_LEVEL_3_QUANTUM, SCHED_RT_WATCHDOG_TICKS,
};
