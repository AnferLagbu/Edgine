//! 调度器操作子模块 — 调度器查询/控制/初始化
//!
//! 从 `api.rs` 拆分而来, 包含所有 `scheduler_*` 函数、`sched_*` 内部函数
//! 以及调度器相关的 `proc_*` 包装函数。
//!
//! ## 依赖
//! - `process::PROCESS_TABLE` — 进程表
//! - `scheduler::SCHEDULER` — 主调度器
//! - `scheduler_ex::SCHEDULER_EX` — 扩展调度器
//! - `thread::THREAD_MANAGER` — 线程管理

use super::process::PROCESS_TABLE;
use super::scheduler::SCHEDULER;
use super::scheduler_ex::SCHEDULER_EX;
use super::thread::THREAD_MANAGER;
use super::types::{BlockReason, Pid, ProcessPriority, ProcessState};

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

/// 是否有可运行进程.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_has_runnable() -> i32 {
    i32::from(SCHEDULER.has_any_runnable())
}

/// 获取当前线程.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn thread_get_current() -> u64 {
    THREAD_MANAGER.get_current_thread().unwrap_or(0)
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
pub extern "C" fn scheduler_add(pid: Pid) {
    SCHEDULER.add(pid);
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
    SCHEDULER.tick(crate::framework::smp::get_current_cpu() as usize);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_init() {
    super::scheduler::init();
    SCHEDULER_EX.init();

    // 注册 tick 查询回调, 解耦 barrier→proc::scheduler 依赖
    // SAFETY: get_tick 是 'static 函数指针, 在内核运行期间始终有效.
    unsafe {
        crate::framework::tick_query::register_tick_query(crate::framework::proc::get_tick);
    }
    // D2: 初始化 cgroup 子系统
    super::cgroup::cgroup_init();
    // D3: 初始化 NUMA 拓扑 (UMA 回退, 后续接入 ACPI SRAT)
    crate::framework::mm::numa_init(
        crate::framework::mm::pmm_get_total_pages() * crate::framework::mm::PAGE_SIZE,
        crate::framework::config::MAX_CPUS as u32,
    );
    // D4/T4-3 (eBPF init + 标准验证器注册) 已反转至 services::debug::ebpf::init
    // 注册契约 (第二十五批): framework proc 不再反向调用 services verifier,
    // 由 lib.rs kernel_init 统一接入.
    // D5: 初始化电源管理子系统
    crate::framework::driver::pm_init(crate::framework::config::MAX_CPUS as u32);
    // D6: 初始化安全启动 + TPM (移至 credo_init, 消除 proc→credo 依赖)
    crate::framework::credo::credo_init();
    // D7: 初始化 CET (Shadow Stack)
    crate::framework::arch::cet_init();
    // D8: 初始化 Tickless (NO_HZ)
    crate::framework::timer::tickless_init(crate::framework::config::MAX_CPUS as u32);
    // D9: 初始化 NTP/PTP 时钟同步
    crate::framework::timer::timesync_init();
    // D10: 初始化 kexec
    crate::framework::driver::kexec_init();
    // D11: 初始化 UEFI (0 = 无 UEFI 固件, 实际由 bootloader 传入)
    crate::framework::driver::uefi_init(0);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_init() {}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn thread_init() {
    super::thread::init();
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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_set_quota(pwm: u64, max_runtime: u64, period: u64) {
    SCHEDULER.set_quota(pwm, max_runtime, period);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_remove_quota(pwm: u64) {
    SCHEDULER.remove_quota(pwm);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_set_proc_limit(pwm: u64, max_procs: u32) {
    SCHEDULER.set_limit(pwm, max_procs);
}

// ============================================================================
// 调度器内部函数 (供 C/汇编调用)
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_current_pid_internal() -> Pid {
    SCHEDULER.current().unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_yield_internal() {
    SCHEDULER.yield_current();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_block(reason: u32) {
    let block_reason = BlockReason::from_u8(reason as u8);
    SCHEDULER.block(block_reason);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_unblock(pid: Pid) {
    SCHEDULER.unblock(pid);
}

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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_state(pid: Pid) -> u32 {
    PROCESS_TABLE
        .with_process(pid, |p| p.get_state() as u32)
        .unwrap_or(ProcessState::Terminated as u32)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sched_init_internal() {
    SCHEDULER.init();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sched_add_internal(pid: Pid) {
    SCHEDULER.add(pid);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sched_schedule_internal() -> Pid {
    SCHEDULER.schedule().unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sched_should_reschedule() -> i32 {
    i32::from(SCHEDULER.should_reschedule())
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sched_set_current(pid: Pid) {
    SCHEDULER.set_current(pid);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sched_get_current() -> Pid {
    SCHEDULER.current().unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_exit_code(pid: Pid) -> i32 {
    PROCESS_TABLE
        .with_process(pid, |p| {
            p.exit_code.load(core::sync::atomic::Ordering::SeqCst) as i32
        })
        .unwrap_or(-1)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_is_initialized() -> i32 {
    i32::from(SCHEDULER.is_initialized())
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_add_rt_task(pid: Pid, rt_priority: u8, policy: u32) {
    use super::scheduler::SchedPolicy;
    SCHEDULER.add_rt_task(pid, rt_priority, SchedPolicy::from_u32(policy));
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_set_sched_policy(pid: Pid, policy: u32, rt_priority: u8) -> i32 {
    use super::scheduler::SchedPolicy;
    if SCHEDULER.set_sched_policy(pid, SchedPolicy::from_u32(policy), rt_priority) {
        0
    } else {
        -1
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn scheduler_get_rt_count() -> usize {
    SCHEDULER.get_rt_count()
}
