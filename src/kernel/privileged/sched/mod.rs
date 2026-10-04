//! 调度器 (TCB) — Scheduler trait / Task 抽象
//!
//! 策略注入点: functions 层通过 Scheduler trait 管理任务，
//! 通过 Task 句柄安全访问进程属性。
//!
//! ## Task 抽象实装状态
//!
//! [sched_trait.rs:30-117](self/sched_trait.rs) 已完成 `Task` 抽象:
//! - `Task { proc_ptr: *const Process, pid: Pid }` 安全句柄
//! - 10 个属性方法 (pid/name/state/priority/is_kernel/pwm/exit_code/
//!   cpu_time_ticks/cr3/pending_signals) 通过 `Process` Atomic 字段
//! - `unsafe fn from_raw` + `Send`/`Sync` 显式 unsafe impl
//! - `Scheduler` trait + `EdgineScheduler` 委托给 `proc::SCHEDULER`
//!
//! 历史: 2026-08-04 复核, 计划文档 (REVIEW-FINDING-030) 注释
//! "task 抽象在 Phase 1.4.2 计划中但尚未实现" 已过期, 实装早于
//! 计划文档更新. functions/proc 通过 `pub mod sched;` (mod.rs:28)
//! 暴露 `Scheduler` trait + 策略注册入口.

pub mod sched_trait;
