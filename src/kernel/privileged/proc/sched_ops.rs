//! 调度器操作子模块 — 调度器查询/控制/初始化
//!
//! 从 `api.rs` 拆分而来, 包含所有 `scheduler_*` 函数、`sched_*` 内部函数
//! 以及调度器相关的 `proc_*` 包装函数。
//!
//! ## 依赖
//! - `process::PROCESS_TABLE` — 进程表
//! - `scheduler::SCHEDULER` — 主调度器
//!
//! ## FFI 面收敛（死 C-ABI 层专项）
//!
//! 本文件原有 25 个 `#[unsafe(no_mangle)] extern "C"` 导出，经全仓引用普查
//! （`.rs` + `.c/.h/.S/.asm/.ld` + `scripts/` `tools/`，剔除注释与 `pub use`）
//! 确认 24 个零调用者，已按「三合一 + 三档」判据删除：活路径的等价入口分别由
//! `scheduler_*` 安全 Rust API（per-CPU 进程级）与 `proc_ops::*` 承担。
//! 保留的 `SCHEDULER` 侧方法本体（RT / quota / limit / `should_reschedule` 面）
//! 登记为「机制已实现、入口待 syscall 接线」，见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §6。

#![deny(missing_docs)]

use super::process::PROCESS_TABLE;
use super::scheduler::Scheduler;
use super::types::{BlockReason, Pid, ProcessPriority};

// ============================================================================
// 调度器查询与控制
// ============================================================================

/// 获取当前调度进程的 CPU 时间 (tick 计数).
#[must_use]
pub fn scheduler_current_cputime() -> u64 {
    Scheduler::current()
        .and_then(|pid| {
            PROCESS_TABLE.with_process(pid, |p| {
                p.user_time.load(core::sync::atomic::Ordering::Acquire)
                    + p.sys_time.load(core::sync::atomic::Ordering::Acquire)
            })
        })
        .unwrap_or(0)
}

/// 解除进程阻塞 (加入就绪队列).
pub fn scheduler_unblock(pid: u32) {
    Scheduler::unblock(pid);
}

/// 阻塞当前进程并让出 CPU.
pub fn scheduler_block(reason: BlockReason) {
    Scheduler::block(reason);
}

/// 将进程加入就绪队列.
pub fn scheduler_add_to_run_queue(pid: u32) {
    Scheduler::add_to_run_queue(pid);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 让出当前 CPU 并触发重新调度 (扩展版)
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_yield_ex() {
    Scheduler::yield_current();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 让出当前 CPU 并触发重新调度
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_yield() {
    Scheduler::yield_current();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 选择下一个可运行进程, 返回其 PID (无则返回 0)
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_schedule() -> Pid {
    Scheduler::schedule().unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 定时器中断驱动的调度器 tick (记账/抢占/负载均衡)
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_tick() {
    // D6: 定时器中断统一驱动**进程级**调度器 tick.
    //
    // 线程级 `SCHEDULER_EX` 并行实现已移除 (P-29 route C): 进程级 CFS 记账/抢占判定/
    // 睡眠唤醒/僵尸回收/周期负载均衡均由本 `Scheduler::tick` per-CPU 路径单点承担,
    // 其中 user/sys 时间记账直接调 `proc_account_tick`.
    Scheduler::tick(crate::privileged::smp::get_current_cpu() as usize);
}

// 初始化职责已移出本文件 (P-8 init 编排收口): 原 `#[unsafe(no_mangle)]
// pub extern "C" fn scheduler_init()` 是一个无生产调用者的 FFI 孤儿入口, 内部挂着
// 10 项子系统 init (cgroup / numa / pm / sgeg / CET / tickless / timesync / kexec /
// uefi / tick_query 注册) —— 生产环境**从未执行**. 现由 crate root
// `lib.rs::kernel_init` 的「延迟子系统编排」块单点接线 (调度器本体 init 见其步骤 8).

// ============================================================================
// 调度器配置
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 返回当前进程的 PWM (比例权重) 值, 无当前进程则返回 0
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_get_current_pwm() -> u64 {
    Scheduler::current()
        .and_then(|pid| PROCESS_TABLE.with_process(pid, super::process::Process::get_pwm))
        .unwrap_or(0)
}

// ============================================================================
// 调度器内部函数 (供 C/汇编调用)
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// C ABI 导出: 设置指定进程的调度优先级 (成功 0 / 进程不存在 -1)
#[unsafe(no_mangle)]
pub extern "C" fn proc_set_priority(pid: Pid, priority: u32) -> i32 {
    if PROCESS_TABLE
        .with_process(pid, |p| {
            p.set_priority(ProcessPriority::from_u32(priority));
        })
        .is_some()
    {
        0
    } else {
        -1
    }
}
