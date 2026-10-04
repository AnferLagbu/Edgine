//! 系统容量常量 — privileged 机制常量
//!
//! ## DECISION-J 归属反转记录 (2026-09-12)
//!
//! 原常量定义 (MAX_CPUS/MAX_IRQS/MAX_PROCESSES 等) 于 T6-9 (2026-06-16) 迁至
//! `functions::config::capacity`, 本文件仅 re-export。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：`MAX_CPUS` 被
//! privileged smp/cpu_local/rcu/irq 的 `static [T; MAX_CPUS]` per-CPU 数组直接
//! 消费、`MAX_IRQS` 被 privileged irq 机制消费 — 属机制容量常量, 迁回。
//!
//! functions 侧改 `pub use crate::privileged::config::capacity::*` 保持 API 兼容。
//! 本文件 0 unsafe (纯常量).

// ============================================================================
// CPU / 中断容量
// ============================================================================

/// 内核支持的最大 CPU 数.
/// 用于 `static [T; MAX_CPUS]` 类 per-CPU 数组.
pub const MAX_CPUS: usize = 1024;

/// 支持的最大 IRQ 号.
pub const MAX_IRQS: usize = 256;

// ============================================================================
// 进程 / 线程容量
// ============================================================================

/// 全系统最大进程数.
///
/// 权威: `proc::process::ProcessTable` 的数组大小, 决定 PID 空间.
pub const MAX_PROCESSES: usize = 256;

/// 全系统最大线程数.
pub const MAX_THREADS: usize = 128;

/// 单个进程的最大线程数.
pub const MAX_THREADS_PER_PROCESS: usize = 16;

// ============================================================================
// 文件 / 会话容量
// ============================================================================

/// 单个进程的最大文件描述符数.
pub const MAX_OPEN_FILES: usize = 32;

/// 最大登录会话数.
pub const MAX_SESSIONS: usize = 16;
