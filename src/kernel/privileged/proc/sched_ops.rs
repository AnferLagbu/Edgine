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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_init() {
    super::scheduler::init();
    SCHEDULER_EX.init();

    // 注册 tick 查询回调, 解耦 freg→proc::scheduler 依赖
    // SAFETY: get_tick 是 'static 函数指针, 在内核运行期间始终有效.
    unsafe {
        crate::privileged::tick_query::register_tick_query(crate::privileged::proc::get_tick);
    }
    // D2: 初始化 cgroup 子系统
    super::cgroup::cgroup_init();
    // D3: 初始化 NUMA 拓扑 (UMA 回退, 后续接入 ACPI SRAT)
    crate::privileged::mm::numa_init(
        crate::privileged::mm::pmm_get_total_pages() * crate::privileged::mm::PAGE_SIZE,
        crate::privileged::config::MAX_CPUS as u32,
    );
    // D4/T4-3 (eBPF init + 标准验证器注册) 已反转至 functions::debug::ebpf::init
    // 注册契约 (第二十五批): privileged proc 不再反向调用 functions verifier,
    // 由 lib.rs kernel_init 统一接入.
    // D5: 初始化电源管理子系统
    crate::privileged::driver::pm_init(crate::privileged::config::MAX_CPUS as u32);
    // D6: 初始化安全启动 + TPM (移至 sgeg_init, 消除 proc→sgeg 依赖)
    crate::privileged::sgeg::sgeg_init();
    // D7: 初始化 CET (Shadow Stack)
    crate::privileged::arch::cet_init();
    // D8: 初始化 Tickless (NO_HZ)
    crate::privileged::timer::tickless_init(crate::privileged::config::MAX_CPUS as u32);
    // D9: 初始化 NTP/PTP 时钟同步
    crate::privileged::timer::timesync_init();
    // D10: 初始化 kexec
    crate::privileged::driver::kexec_init();
    // D11: 初始化 UEFI (0 = 无 UEFI 固件, 实际由 bootloader 传入)
    crate::privileged::driver::uefi_init(0);
}

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
