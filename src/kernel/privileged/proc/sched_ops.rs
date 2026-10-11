//! 调度器操作子模块 — 调度器查询/控制/初始化
//!
//! 从 `api.rs` 拆分而来, 包含所有 `scheduler_*` 函数、`sched_*` 内部函数
//! 以及调度器相关的 `proc_*` 包装函数。
//!
//! ## 依赖
//! - `process::PROCESS_TABLE` — 进程表
//! - `scheduler::SCHEDULER` — 主调度器
//! - `scheduler_ex::SCHEDULER_EX` — 扩展调度器
//!
//! ## FFI 面收敛（死 C-ABI 层专项）
//!
//! 本文件原有 25 个 `#[unsafe(no_mangle)] extern "C"` 导出，经全仓引用普查
//! （`.rs` + `.c/.h/.S/.asm/.ld` + `scripts/` `tools/`，剔除注释与 `pub use`）
//! 确认 24 个零调用者，已按「三合一 + 三档」判据删除：活路径的等价入口分别由
//! `sched_trait` 策略接口、`scheduler_*` 安全 Rust API 与 `proc_ops::*` 承担。
//! 保留的 `SCHEDULER` 侧方法本体（RT / quota / limit / should_reschedule 面）
//! 登记为「机制已实现、入口待 syscall 接线」，见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §6。

use super::process::PROCESS_TABLE;
use super::scheduler::SCHEDULER;
use super::scheduler_ex::SCHEDULER_EX;
use super::types::{BlockReason, Pid, ProcessPriority};

// ============================================================================
// 调度器查询与控制
// ============================================================================

/// 获取当前调度线程的 CPU 时间 (纳秒).
pub fn scheduler_current_cputime() -> u64 {
    let current = SCHEDULER_EX
        .current
        .load(core::sync::atomic::Ordering::Acquire)
        as *mut super::thread::Thread;
    if current.is_null() {
        return 0;
    }
    // SAFETY: current 是调度器维护的有效线程指针
    unsafe {
        (*current)
            .cpu_time
            .load(core::sync::atomic::Ordering::Acquire)
    }
}

/// 解除进程阻塞 (加入就绪队列).
pub fn scheduler_unblock(pid: u32) {
    SCHEDULER.unblock(pid);
}

/// 阻塞当前进程并让出 CPU.
pub fn scheduler_block(reason: BlockReason) {
    SCHEDULER.block(reason);
}

/// 将进程加入就绪队列.
pub fn scheduler_add_to_run_queue(pid: u32) {
    SCHEDULER.add_to_run_queue(pid);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_yield_ex() {
    SCHEDULER_EX.yield_current();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_yield() {
    SCHEDULER.yield_current();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_schedule() -> Pid {
    SCHEDULER.schedule().unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_tick() {
    // D6: 定时器中断统一驱动**进程级**调度器 tick.
    //
    // 原实现只驱动线程级 `SCHEDULER_EX.tick()`; 而本项目 `ThreadManager::create_thread`
    // 无调用者 (进程无对应 `Thread`), 线程级调度实际空转, 导致进程级 CFS 记账/
    // 抢占判定/睡眠唤醒/zombie 回收/周期负载均衡全部不推进 —— aarch64 双核下表现为
    // 任务被重新入队后本核永久停在 idle (ISSUE-RT-004).
    //
    // 线程级记账不丢失: `Scheduler::tick` 内部会调用 `SCHEDULER_EX.tick_accounting()`,
    // 故此处只保留单一 tick 入口, 不重复记账.
    SCHEDULER.tick(crate::privileged::smp::get_current_cpu() as usize);
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
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_get_current_pwm() -> u64 {
    SCHEDULER
        .current()
        .and_then(|pid| PROCESS_TABLE.with_process(pid, super::process::Process::get_pwm))
        .unwrap_or(0)
}

// ============================================================================
// 调度器内部函数 (供 C/汇编调用)
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
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
