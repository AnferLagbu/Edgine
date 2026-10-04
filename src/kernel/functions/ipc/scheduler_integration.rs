#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯策略实现。
//! IPC 调度器集成 (阻塞/唤醒机制) — functions 层策略主体
//!
//! ## T6-9 迁移记录
//!
//! 原属 privileged/ipc/scheduler_integration.rs, 2026-06-16 提取到 functions.
//! 纯策略代码 (阻塞/唤醒/超时等待), 0 unsafe.
//! privileged 仅保留 re-export.
//!
//! 提供与内核调度器的桥接功能：
//! - 阻塞当前线程到等待队列
//! - 唤醒等待队列中的线程
//! - 支持超时等待

use super::types::{WaitQueue, WaitQueueItem};
use crate::privileged::proc::{
    process_get_current_pid, scheduler_block, scheduler_unblock, scheduler_yield_ex,
};
use crate::privileged::sync::in_irq_context;
use crate::privileged::timer::hrtimer::hrtimer_sleep;

/// B07-15: 中断上下文唤醒守卫 — 中断/softirq 上下文不得直接调度或阻塞.
fn in_interrupt_context() -> bool {
    in_irq_context()
}

/// 阻塞当前进程到指定等待队列
///
/// 将当前进程加入等待队列并让出 CPU，
/// 直到被其他进程唤醒或超时。
///
/// # Arguments
/// * `wait_queue` - 目标等待队列
/// * `timeout_ms` - 超时时间 (毫秒), 0 表示无限等待
///
/// # Returns
/// * Ok(()) - 成功被唤醒
/// * Err(i32) - 错误码 (-1: 队列已满/超时, -2: 无进程上下文/中断上下文)
///
/// # Errors
/// 当无法获取当前进程 (`process_get_current_pid` 返回 0) 或处于中断上下文时
/// 返回 `Err(-2)`; 等待队列已满时返回 `Err(-1)` (调用方须回退忙等, 避免
/// 等待项丢失导致永久睡眠).
#[inline]
pub fn block_current_thread(wait_queue: &mut WaitQueue, _timeout_ms: u64) -> Result<(), i32> {
    if in_interrupt_context() {
        // 中断上下文禁止阻塞/调度 (持锁期间睡眠会死锁).
        return Err(-2);
    }
    // B07-15 修复①: 入队/唤醒统一以 pid 为准 — `scheduler_block`/`scheduler_unblock`
    // 均按 pid 操作进程表, 入队项必须存 pid (对齐 epoll 范式 `tid: current_pid`).
    // 原实现误用 `thread_get_current()` (线程 tid, 独立 id 空间), 唤醒时
    // `scheduler_unblock(tid)` 按 pid 查找导致唤醒错位 (死锁或唤醒错误进程).
    let pid = process_get_current_pid();
    if pid == 0 {
        return Err(-2); // idle/无进程上下文, 无法真实阻塞
    }
    // B07-15 修复④: 队列满时禁止阻塞 — 若入队失败仍 `scheduler_block`,
    // 该进程不在任何队列中, 无人能唤醒 (永久睡眠).
    if !wait_queue.add(WaitQueueItem { pid }) {
        return Err(-1);
    }

    // 标记进程为 Blocked 并让出 CPU.
    scheduler_block(crate::privileged::proc::BlockReason::WaitingForIo);
    scheduler_yield_ex();

    // 被唤醒后返回
    Ok(())
}

/// 唤醒等待队列中的一个进程
///
/// 从等待队列头部取出一个进程并标记为可运行。
///
/// # Arguments
/// * `wait_queue` - 目标等待队列
///
/// # Returns
/// * true - 成功唤醒一个进程 (或已在中断上下文登记唤醒)
/// * false - 队列为空
#[inline]
pub fn wake_one_thread(wait_queue: &mut WaitQueue) -> bool {
    // B07-15 修复③: 中断上下文不可直接调度唤醒, 仅登记 pending,
    // 由后续进程上下文路径补唤醒 (原实现仅清标志, 唤醒被静默丢弃).
    if in_interrupt_context() {
        wait_queue.request_wake();
        return true;
    }
    // 进程上下文: 先补唤醒中断上下文遗留的 pending 请求.
    for item in wait_queue.drain_pending() {
        scheduler_unblock(item.pid);
    }
    if let Some(item) = wait_queue.wake_one() {
        scheduler_unblock(item.pid);
        true
    } else {
        false
    }
}

/// 唤醒等待队列中的所有进程
///
/// 标记队列中所有进程为可运行状态。
///
/// # Arguments
/// * `wait_queue` - 目标等待队列
#[inline]
pub fn wake_all_threads(wait_queue: &mut WaitQueue) {
    // B07-15 修复③: 中断上下文仅登记 pending, 由进程上下文补唤醒.
    if in_interrupt_context() {
        wait_queue.request_wake();
        return;
    }
    // B07-15 修复②: 先补 pending, 再逐个出队并调度唤醒.
    // 原实现仅 `wake_all()` 清空队列、从不调用 `scheduler_unblock`,
    // 阻塞进程永不被唤醒 (死锁).
    for item in wait_queue.drain_pending() {
        scheduler_unblock(item.pid);
    }
    while let Some(item) = wait_queue.wake_one() {
        scheduler_unblock(item.pid);
    }
}

/// 带超时的阻塞等待
///
/// 结合定时器和等待队列实现精确的超时控制。
///
/// # Arguments
/// * `wait_queue` - 目标等待队列
/// * `timeout_ms` - 超时时间 (毫秒)
///
/// # Returns
/// * Ok(()) - 在超时前被唤醒
/// * Err(-1) - 超时
///
/// # Errors
/// 当等待超时仍未满足条件时返回 `Err(-1)`; 其余错误与 [`block_current_thread`] 相同.
pub fn block_with_timeout(wait_queue: &mut WaitQueue, timeout_ms: u64) -> Result<(), i32> {
    if timeout_ms == 0 {
        // 无限等待
        return block_current_thread(wait_queue, 0);
    }

    // B07-15: 用 hrtimer 睡眠替代忙等待 (TRACK-8C5FFB).
    // 忙等待在单核下会 starve 唤醒方 (无抢占 + 不可重入调度器).
    let timeout_nanos = timeout_ms.saturating_mul(1_000_000);
    // 周期性检查等待队列是否仍需要阻塞 (被外部唤醒则退出).
    // 简化: 有限次重试 + hrtimer 睡眠, 期间被唤醒则立即返回.
    let deadline =
        crate::privileged::timer::hrtimer::hrtimer_clock_read().saturating_add(timeout_nanos);
    loop {
        if wait_queue.count() == 0 {
            // 已无等待者 (被唤醒或条件满足)
            return Ok(());
        }
        if in_interrupt_context() {
            return Err(-2);
        }
        if crate::privileged::timer::hrtimer::hrtimer_clock_read() >= deadline {
            return Err(-1); // 超时
        }
        // 睡眠一个短时隙后重查 (让出 CPU, 非忙等).
        hrtimer_sleep(timeout_nanos.min(1_000_000)).map_err(|()| -1)?;
        if wait_queue.count() == 0 {
            return Ok(());
        }
    }
}
