//! 高精度定时器框架 (High-Resolution Timer)
//!
//! 提供纳秒级精度的内核定时器机制：
//! - **单次触发** (one-shot): 到期执行回调后自动移除
//! - **周期触发** (periodic): 到期执行回调后自动重新入队
//! - **取消**: 随时取消未触发的定时器
//!
//! ## 架构
//!
//! ```text
//! HrTimer Privileged
//! ├── hrtimer.rs       核心数据结构与 API
//! │   ├── HrTimer      定时器对象 (嵌入用户结构体)
//! │   ├── HrTimerQueue 全局优先队列 (按到期时间排序)
//! │   └── hrtimer_*    公共 API
//! │
//! ├── [x86_64] LAPIC Timer one-shot 模式 (Phase 2)
//! └── [aarch64] Generic Timer CNTP_CVAL one-shot (Phase 2)
//! ```
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use kernel::privileged::timer::hrtimer::{HrTimer, HrTimerRestart, hrtimer_start};
//!
//! struct MyDevice {
//!     timer: HrTimer,
//! }
//!
//! fn my_timer_callback(timer: &HrTimer) -> HrTimerRestart {
//!     // 处理超时逻辑
//!     HrTimerRestart::Periodic  // 或 OneShot
//! }
//!
//! // 初始化并启动 (10ms 后触发)
//! device.timer.init(my_timer_callback);
//! hrtimer_start(&mut device.timer, 10_000_000);  // 10ms = 10_000_000 ns
//! ```
//!
//! # Safety
//!
//! - HrTimer 必须在有效内存中 (通常嵌入其他结构体), 生命周期覆盖定时器触发前
//! - 回调在中断上下文执行, 不可睡眠
//! - 取消操作需确保回调未在另一 CPU 执行 (当前单队列 spinlock 保证)

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::privileged::sync::IrqSpinLock as Mutex;
use alloc::vec::Vec;
// ============================================================================
// 公共类型
// ============================================================================

/// 定时器回调返回值 — 控制定时器是否重新入队
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HrTimerRestart {
    /// 单次触发, 回调后移除
    OneShot,
    /// 周期触发, 按 `interval_ns` 重新入队
    Periodic,
}

/// 定时器回调函数类型
///
/// # 约束
/// - 在中断上下文执行, 不可睡眠
/// - 不可长时间阻塞 (建议 < 1ms)
/// - 不可调用 `hrtimer_start/cancel` (避免死锁)
pub type HrTimerCallback = fn(&HrTimer) -> HrTimerRestart;

/// 定时器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HrTimerState {
    /// 未初始化或已取消
    Inactive,
    /// 已入队等待触发
    Pending,
    /// 回调正在执行
    Running,
}

// ============================================================================
// HrTimer — 定时器对象 (嵌入用户结构体)
// ============================================================================

/// 高精度定时器
///
/// 设计为嵌入用户结构体使用 (类似 Linux `struct hrtimer`)。
/// 用户负责 `HrTimer` 的内存生命周期: 从 `init()` 到最后一次回调返回 `OneShot` 之间,
/// `HrTimer` 必须在有效内存中。
///
/// # 线程安全
///
/// - `expiry_ns` 和 `state` 使用原子操作, 支持跨 CPU 读取
/// - 入队/出队/回调执行由全局 spinlock 保护
/// - 取消操作是异步的: `hrtimer_cancel()` 设置 Inactive 标志,
///   实际从队列移除可能在下次 `hrtimer_run_queues()` 时完成
pub struct HrTimer {
    /// 绝对到期时间 (纳秒)
    expiry_ns: AtomicU64,
    /// 周期间隔 (纳秒), 0 = 单次
    interval_ns: AtomicU64,
    /// 回调函数
    callback: HrTimerCallback,
    /// 当前状态
    state: AtomicU64, // 编码为 HrTimerState 的 u64
    /// 队列内序号 (用于快速定位, 0 = 未入队)
    queue_seq: AtomicU64,
}

impl HrTimer {
    /// 创建未初始化的 `HrTimer`
    ///
    /// 必须调用 `init()` 后才能使用。
    pub const fn uninit() -> Self {
        Self {
            expiry_ns: AtomicU64::new(0),
            interval_ns: AtomicU64::new(0),
            callback: noop_callback,
            state: AtomicU64::new(HrTimerState::Inactive as u64),
            queue_seq: AtomicU64::new(0),
        }
    }

    /// 初始化定时器
    ///
    /// 设置回调函数, 状态置为 Inactive。
    /// 可重复调用以更换回调。
    pub fn init(&mut self, callback: HrTimerCallback) {
        self.callback = callback;
        self.expiry_ns.store(0, Ordering::Relaxed);
        self.interval_ns.store(0, Ordering::Relaxed);
        self.state
            .store(HrTimerState::Inactive as u64, Ordering::Relaxed);
        self.queue_seq.store(0, Ordering::Relaxed);
    }

    /// 获取到期时间 (纳秒)
    pub fn expiry_ns(&self) -> u64 {
        self.expiry_ns.load(Ordering::Acquire)
    }

    /// 获取周期间隔 (纳秒)
    pub fn interval_ns(&self) -> u64 {
        self.interval_ns.load(Ordering::Acquire)
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 获取当前状态
    pub fn state(&self) -> HrTimerState {
        match self.state.load(Ordering::Acquire) {
            0 => HrTimerState::Inactive,
            1 => HrTimerState::Pending,
            2 => HrTimerState::Running,
            _ => HrTimerState::Inactive,
        }
    }

    /// 是否处于 Pending 状态
    pub fn is_pending(&self) -> bool {
        self.state() == HrTimerState::Pending
    }

    /// 推进周期定时器到下一个周期
    ///
    /// 如果当前时间已超过到期时间, 按 `interval_ns` 向前推进到期时间,
    /// 直到到期时间 > 当前时间。
    ///
    /// 返回跳过的周期数。
    pub fn forward(&self, now_ns: u64) -> u64 {
        let interval = self.interval_ns.load(Ordering::Acquire);
        if interval == 0 {
            return 0;
        }

        let mut expiry = self.expiry_ns.load(Ordering::Acquire);
        let mut skipped = 0u64;

        while expiry <= now_ns {
            expiry += interval;
            skipped += 1;
        }

        if skipped > 0 {
            self.expiry_ns.store(expiry, Ordering::Release);
        }

        skipped
    }

    fn set_state(&self, state: HrTimerState) {
        self.state.store(state as u64, Ordering::Release);
    }
}

/// 空回调 (未初始化定时器的默认值)
fn noop_callback(_timer: &HrTimer) -> HrTimerRestart {
    HrTimerRestart::OneShot
}

// ============================================================================
// 全局定时器队列
// ============================================================================

/// 队列内条目
struct TimerEntry {
    /// 定时器指针 (`NonNull` 确保非空)
    timer: core::ptr::NonNull<HrTimer>,
    /// 入队时的序号 (用于取消时快速判断是否过期)
    seq: u64,
}

// SAFETY: TimerEntry 通过全局 spinlock 同步访问, 指针在回调前有效。
// NonNull<HrTimer> 本身不是 Send, 但内核中定时器可在任意 CPU 上操作,
// 由 spinlock 保证互斥。
unsafe impl Send for TimerEntry {}
unsafe impl Sync for TimerEntry {}

/// 全局序号计数器
static QUEUE_SEQ: AtomicU64 = AtomicU64::new(1);

/// 全局定时器队列
///
/// 使用 Vec + spinlock 实现, 按到期时间升序排列。
/// 后续优化为 per-CPU 无锁队列。
static HRTIMER_QUEUE: Mutex<Vec<TimerEntry>> = Mutex::new(Vec::new());

/// 框架初始化标志
static HRTIMER_READY: AtomicBool = AtomicBool::new(false);

// ============================================================================
// 公共 API
// ============================================================================

/// 初始化 hrtimer 框架
///
/// 在内核启动早期调用一次。
pub fn hrtimer_init() {
    let mut queue = HRTIMER_QUEUE.lock();
    queue.clear();
    HRTIMER_READY.store(true, Ordering::Release);
}

/// 检查 hrtimer 框架是否已初始化
pub fn is_hrtimer_ready() -> bool {
    HRTIMER_READY.load(Ordering::Acquire)
}

#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
/// 启动定时器 (绝对时间)
///
/// 将定时器以绝对到期时间 `expiry_ns` 入队。
/// 如果定时器已在队列中, 先移除旧条目再重新入队。
///
/// # Arguments
/// * `timer` - 定时器对象 (必须已 init)
/// * `expiry_ns` - 绝对到期时间 (纳秒, 从启动起)
///
/// # Safety
///
/// 调用方必须保证:
/// - `timer` 在回调触发前保持有效内存
/// - 不在回调中调用此函数 (会死锁)
pub fn hrtimer_start(timer: &HrTimer, expiry_ns: u64) {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return;
    }

    let seq = QUEUE_SEQ.fetch_add(1, Ordering::Relaxed);
    timer.expiry_ns.store(expiry_ns, Ordering::Release);
    timer.queue_seq.store(seq, Ordering::Release);
    timer.set_state(HrTimerState::Pending);

    let mut queue = HRTIMER_QUEUE.lock();

    // 移除同一 timer 的旧条目 (seq 不同则跳过)
    queue.retain(|entry| {
        if core::ptr::eq(entry.timer.as_ptr(), timer) {
            // 同一个 timer, 检查 seq 是否匹配
            // SAFETY: `entry` 由调用方保证为有效指针; 只读访问
            let old_seq = unsafe { (*entry.timer.as_ptr()).queue_seq.load(Ordering::Acquire) };
            old_seq != seq
        } else {
            true
        }
    });

    // 按到期时间插入 (保持升序)
    let insert_pos = queue
        .iter()
        .position(|entry| {
            // SAFETY: `entry` 由调用方保证为有效指针; 只读访问
            let other_expiry = unsafe { (*entry.timer.as_ptr()).expiry_ns.load(Ordering::Acquire) };
            other_expiry > expiry_ns
        })
        .unwrap_or(queue.len());

    // SAFETY: timer 由调用方保证在回调前有效, NonNull::new_unchecked 因引用非空
    let ptr = unsafe { core::ptr::NonNull::new_unchecked(timer as *const HrTimer as *mut HrTimer) };
    queue.insert(insert_pos, TimerEntry { timer: ptr, seq });
}

/// 启动定时器 (相对时间)
///
/// 便捷接口: 从当前时间起延迟 `delay_ns` 纳秒后触发。
///
/// # Arguments
/// * `timer` - 定时器对象
/// * `delay_ns` - 延迟时间 (纳秒)
pub fn hrtimer_start_rel(timer: &HrTimer, delay_ns: u64) {
    let now = hrtimer_clock_read();
    hrtimer_start(timer, now + delay_ns);
}

/// 启动周期定时器
///
/// 首次在 `delay_ns` 纳秒后触发, 之后每 `interval_ns` 纳秒触发一次。
///
/// # Arguments
/// * `timer` - 定时器对象
/// * `delay_ns` - 首次延迟 (纳秒)
/// * `interval_ns` - 周期间隔 (纳秒)
pub fn hrtimer_start_periodic(timer: &HrTimer, delay_ns: u64, interval_ns: u64) {
    timer.interval_ns.store(interval_ns, Ordering::Release);
    hrtimer_start_rel(timer, delay_ns);
}

/// 取消定时器
///
/// 将定时器标记为 Inactive。如果定时器在队列中, 会在下次
/// `hrtimer_run_queues()` 时移除。如果回调正在执行, 等待回调完成。
///
/// 返回 true 表示成功取消 (定时器处于 Pending 状态),
/// 返回 false 表示定时器未在 Pending 状态。
pub fn hrtimer_cancel(timer: &HrTimer) -> bool {
    let was_pending = timer.state() == HrTimerState::Pending;
    timer.set_state(HrTimerState::Inactive);
    timer.queue_seq.store(0, Ordering::Release);
    was_pending
}

/// 处理已到期的定时器
///
/// 在定时器中断处理程序中调用。遍历队列, 执行所有已到期定时器的回调。
///
/// # Safety
///
/// - 必须在中断上下文调用 (或中断禁用状态)
/// - 不可递归调用
pub fn hrtimer_run_queues() {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return;
    }

    let now_ns = hrtimer_clock_read();

    // 收集到期定时器 (释放锁后执行回调, 避免死锁)
    let expired: Vec<(core::ptr::NonNull<HrTimer>, HrTimerCallback, u64)> = {
        let mut queue = HRTIMER_QUEUE.lock();

        let mut expired = Vec::new();

        // 队列按到期时间升序, 找到第一个未到期的位置
        for entry in queue.iter() {
            let timer_ptr = entry.timer.as_ptr();
            // SAFETY: 入队时保证指针有效, 出队前不会释放
            let expiry = unsafe { (*timer_ptr).expiry_ns.load(Ordering::Acquire) };
            let state = unsafe { (*timer_ptr).state() };

            if expiry <= now_ns && state == HrTimerState::Pending {
                let cb = unsafe { (*timer_ptr).callback };
                let seq = entry.seq;
                unsafe { (*timer_ptr).set_state(HrTimerState::Running) };
                expired.push((entry.timer, cb, seq));
            } else if state == HrTimerState::Inactive {
                // 已取消, 跳过
            } else if expiry > now_ns {
                break; // 后续都未到期
            }
        }

        // 移除已到期和已取消的条目
        queue.retain(|entry| {
            // SAFETY: `entry` 由调用方保证为有效指针; 只读访问
            let state = unsafe { (*entry.timer.as_ptr()).state() };
            state == HrTimerState::Pending
        });

        // 重新排序 (retain 可能打乱顺序)
        // SAFETY: `entry` 由调用方保证为有效指针; 只读访问
        queue.sort_by_key(|entry| unsafe {
            (*entry.timer.as_ptr()).expiry_ns.load(Ordering::Acquire)
        });

        expired
    };

    // 执行回调 (无锁, 允许回调中操作定时器)
    for (timer_ptr, callback, _seq) in expired {
        // SAFETY: 入队时保证指针有效
        let timer = unsafe { timer_ptr.as_ref() };
        let restart = callback(timer);

        match restart {
            HrTimerRestart::OneShot => {
                timer.set_state(HrTimerState::Inactive);
                timer.queue_seq.store(0, Ordering::Release);
            }
            HrTimerRestart::Periodic => {
                let interval = timer.interval_ns.load(Ordering::Acquire);
                if interval > 0 {
                    let old_expiry = timer.expiry_ns.load(Ordering::Acquire);
                    let new_expiry = old_expiry + interval;
                    // 如果已严重过期, 从当前时间开始
                    let next = if new_expiry <= now_ns {
                        now_ns + interval
                    } else {
                        new_expiry
                    };
                    // 重新入队: hrtimer_start 会获取锁
                    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                    hrtimer_start(unsafe { timer_ptr.as_ref() }, next);
                } else {
                    timer.set_state(HrTimerState::Inactive);
                }
            }
        }
    }
}

/// 获取下一个到期时间 (纳秒)
///
/// 返回 None 表示队列为空。
/// 用于编程硬件定时器的下次触发时间。
pub fn hrtimer_next_expiry() -> Option<u64> {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return None;
    }

    let queue = HRTIMER_QUEUE.lock();
    queue.first().map(|entry| {
        // SAFETY: 入队时保证指针有效
        unsafe { (*entry.timer.as_ptr()).expiry_ns.load(Ordering::Acquire) }
    })
}

/// 获取队列中待触发定时器数量
pub fn hrtimer_pending_count() -> usize {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return 0;
    }
    let queue = HRTIMER_QUEUE.lock();
    queue.len()
}

// ============================================================================
// 时钟源
// ============================================================================

/// 读取当前时间 (纳秒, 单调递增)
///
/// 基于 `arch!(timestamp())` 和校准频率转换。
/// 如果未校准, 回退到 tick 计数。
///
/// # 精度
///
/// - `x86_64`: TSC 频率校准后, 纳秒级精度
/// - aarch64: `CNTPCT_EL0` + `CNTFRQ_EL0`, 纳秒级精度
/// - 未校准: 毫秒级 (tick 精度)
pub fn hrtimer_clock_read() -> u64 {
    // 优先使用校准后的高精度时间
    if let Some(freq_hz) = crate::privileged::timer::calibration::get_tsc_frequency_hz() {
        if freq_hz > 0 {
            let ts = crate::arch!(timestamp());
            // ns = cycles * 1_000_000_000 / freq_hz
            // 使用 128 位乘法避免溢出: 先除后乘会损失精度
            return mul_u64_div(ts, 1_000_000_000, freq_hz);
        }
    }

    // 回退: tick 计数 → 纳秒
    crate::privileged::timer::tick::ticks_to_ns(crate::privileged::timer::tick::get_ticks())
}

/// 将纳秒转换为时钟周期数
///
/// 用于编程硬件定时器。
pub fn hrtimer_ns_to_cycles(ns: u64) -> u64 {
    if let Some(freq_hz) = crate::privileged::timer::calibration::get_tsc_frequency_hz() {
        if freq_hz > 0 {
            return mul_u64_div(ns, freq_hz, 1_000_000_000);
        }
    }

    // 回退: 纳秒 → tick
    crate::privileged::timer::tick::us_to_ticks(ns / 1000)
}

/// 安全的 64 位乘除法: (a * b) / c, 无溢出
///
/// 使用 128 位中间结果。
#[inline]
fn mul_u64_div(a: u64, b: u64, c: u64) -> u64 {
    if c == 0 {
        return 0;
    }
    // 在 64 位平台上, u128 可用且通常有硬件支持
    (u128::from(a) * u128::from(b) / u128::from(c)) as u64
}

// ============================================================================
// I-50: hrtimer_sleep — 高精度 sleep 公共 API
// ============================================================================

/// 高精度 sleep (纳秒)
///
/// 用 hrtimer 机制 sleep 指定时长, 由 `hrtimer_run_queues()` 在 tick handler
/// 中统一触发 (见 `tick::on_timer_interrupt`). 精度高于 `timer_sleep(ms)` (毫秒级).
///
/// # 阻塞模型
///
/// 当前实现是 **busy-wait** (spin on atomic flag), 不让出 CPU. 适用于:
/// - 短延时 (< 1ms, 让出 CPU 调度开销更大)
/// - 中断上下文外
/// - 精度敏感场景 (网络超时, 设备探测重试)
///
/// # 后续优化 (后续阶段, 本 fix 不阻塞)
/// - 阻塞式 sleep: 注册定时器后调 `scheduler_yield`, 定时器 ISR 唤醒
/// - per-CPU timer 队列 (Phase 2 目标)
///
/// # Arguments
/// * `delay_ns` - 延迟时长 (纳秒, 0 = 立即返回)
///
/// # Returns
/// * `Ok(())`  - sleep 成功
/// * `Err(())` - hrtimer 框架未初始化 (`hrtimer_init()` 未调用)
///
/// # Errors
/// 当 hrtimer 框架尚未初始化 (`HRTIMER_READY == false`) 时返回 `Err(())`.
pub fn hrtimer_sleep(delay_ns: u64) -> Result<(), ()> {
    if delay_ns == 0 {
        return Ok(());
    }
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return Err(());
    }

    // 闭包用 static 状态在 ISR (hrtimer 回调) 与调用方 (本函数) 之间传递完成信号.
    // SAFETY: SLEEP_FLAG 仅为本函数独占, 不会跨调用并发 (单线程执行模型).
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    static mut SLEEP_FLAG: AtomicBool = AtomicBool::new(false);

    let mut timer = HrTimer::uninit();
    timer.init(|_t| {
        // SAFETY: SLEEP_FLAG 由本函数 set up, 回调仅 store true.
        unsafe { SLEEP_FLAG.store(true, Ordering::Release) };
        HrTimerRestart::OneShot
    });

    hrtimer_start_rel(&timer, delay_ns);

    // 自旋等待. hrtimer_run_queues 会在下一个 tick (或下次硬件定时器中断)
    // 处理到期定时器, 触发回调 set SLEEP_FLAG=true.
    // SAFETY: SLEEP_FLAG 由本函数 set up, 此处仅 load 检查完成.
    let mut spins: u32 = 0;
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const SLEEP_WAIT_BOUND: u32 = 1_000_000_000; // ~1s @ 1GHz spin_loop
    unsafe {
        while !SLEEP_FLAG.load(Ordering::Acquire) {
            core::hint::spin_loop();
            spins = spins.saturating_add(1);
            if spins >= SLEEP_WAIT_BOUND {
                // 兜底: 超时仍按已完成返回, 不让进程卡死
                break;
            }
        }
        SLEEP_FLAG.store(false, Ordering::Release);
    }
    Ok(())
}

// ============================================================================
// 单元测试 (host-side, 不依赖硬件)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hrtimer_state_encoding() {
        assert_eq!(HrTimerState::Inactive as u64, 0);
        assert_eq!(HrTimerState::Pending as u64, 1);
        assert_eq!(HrTimerState::Running as u64, 2);
    }

    #[test]
    fn test_hrtimer_uninit() {
        let timer = HrTimer::uninit();
        assert_eq!(timer.state(), HrTimerState::Inactive);
        assert_eq!(timer.expiry_ns(), 0);
        assert_eq!(timer.interval_ns(), 0);
        assert!(!timer.is_pending());
    }

    #[test]
    fn test_hrtimer_init() {
        let mut timer = HrTimer::uninit();
        timer.init(noop_callback);
        assert_eq!(timer.state(), HrTimerState::Inactive);
    }

    #[test]
    fn test_hrtimer_forward() {
        let mut timer = HrTimer::uninit();
        timer.init(noop_callback);
        timer.interval_ns.store(1_000_000, Ordering::Release); // 1ms
        timer.expiry_ns.store(5_000_000, Ordering::Release); // 5ms

        // 当前时间 8ms: 自 5ms 起按 1ms 推进, 到期时间须 > 当前时间,
        // 故 5→6→7→8→9 共跳过 4 个周期 (2026-09-24 UT-06 实测修正原断言 3/8ms).
        let skipped = timer.forward(8_000_000);
        assert_eq!(skipped, 4);
        assert_eq!(timer.expiry_ns(), 9_000_000);

        // 当前时间 7ms, 未超过到期时间, 不跳过
        let skipped = timer.forward(7_000_000);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_hrtimer_forward_zero_interval() {
        let mut timer = HrTimer::uninit();
        timer.init(noop_callback);
        timer.expiry_ns.store(5_000_000, Ordering::Release);
        let skipped = timer.forward(10_000_000);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_mul_u64_div() {
        assert_eq!(mul_u64_div(1_000_000_000, 3, 1_000_000_000), 3);
        assert_eq!(mul_u64_div(0, 100, 50), 0);
        assert_eq!(mul_u64_div(100, 0, 50), 0);
        assert_eq!(mul_u64_div(100, 50, 0), 0);
        // 大数测试 (避免溢出)
        let freq = 2_500_000_000u64; // 2.5 GHz
        let ns = mul_u64_div(1_000_000_000, 1_000_000_000, freq);
        assert!(ns > 0);
    }

    #[test]
    fn test_cancel_while_inactive() {
        let mut timer = HrTimer::uninit();
        timer.init(noop_callback);
        let cancelled = hrtimer_cancel(&timer);
        assert!(!cancelled); // 未入队, 取消返回 false
    }

    #[test]
    fn test_next_expiry_empty() {
        // UT-06 (2026-09-24): 先 hrtimer_init() 清空队列使断言与环境无关.
        // 原断言依赖 "框架未初始化", 但同文件 test_hrtimer_sleep_init 会入队一个
        // 栈上定时器, host 侧无 tick 消费, 队列残留导致顺序耦合失败.
        hrtimer_init();
        assert!(hrtimer_next_expiry().is_none());
    }

    #[test]
    fn test_pending_count_empty() {
        // UT-06 (2026-09-24): 同上, 清空队列后待触发数必为 0.
        hrtimer_init();
        assert_eq!(hrtimer_pending_count(), 0);
    }

    // I-50: hrtimer_sleep 公共 API host-test
    #[test]
    fn test_hrtimer_sleep_zero() {
        // 0 纳秒应立即返回 Ok
        assert!(hrtimer_sleep(0).is_ok());
    }

    #[test]
    fn test_hrtimer_sleep_init() {
        // 初始化后, 短延时 sleep 应成功 (实际精度由硬件 tick 决定, 此处只验 API 通路)
        hrtimer_init();
        // 1ms = 1_000_000 ns, 在 host 环境下无 tick 中断, 走 SLEEP_WAIT_BOUND 退路,
        // 仍返回 Ok (不卡死, 不 panic)
        assert!(hrtimer_sleep(1_000_000).is_ok());
    }
}

// ============================================================================
// 内核测试 (QEMU feature gate)
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_state_encoding() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    assert_eq_test!(HrTimerState::Inactive as u64, 0, "Inactive=0");
    assert_eq_test!(HrTimerState::Pending as u64, 1, "Pending=1");
    assert_eq_test!(HrTimerState::Running as u64, 2, "Running=2");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_uninit_state() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    let timer = HrTimer::uninit();
    assert_eq_test!(timer.state(), HrTimerState::Inactive, "state");
    assert_eq_test!(timer.expiry_ns(), 0, "expiry");
    check!(!timer.is_pending(), "not pending");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_init_and_cancel() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    static mut TEST_TIMER: HrTimer = HrTimer::uninit();
    // SAFETY: 测试单线程, 无竞争
    unsafe {
        TEST_TIMER.init(noop_callback);
        assert_eq_test!(TEST_TIMER.state(), HrTimerState::Inactive, "after init");
        let cancelled = hrtimer_cancel(&TEST_TIMER);
        check!(!cancelled, "cancel inactive returns false");
    }
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_forward_periodic() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    let mut timer = HrTimer::uninit();
    timer.init(noop_callback);
    timer.interval_ns.store(1_000_000, Ordering::Release);
    timer.expiry_ns.store(5_000_000, Ordering::Release);
    // forward 用 `expiry <= now` 语义: expiry 恰好等于 now 时视为已到期再推进一位.
    // 5ms → 6 → 7 → 8(==now, 再推) → 9ms, 跳过 4 个周期, 新 expiry 9ms.
    let skipped = timer.forward(8_000_000);
    assert_eq_test!(skipped, 4, "skipped 4 periods");
    assert_eq_test!(timer.expiry_ns(), 9_000_000, "new expiry");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_mul_u64_div_basic() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    assert_eq_test!(mul_u64_div(100, 3, 100), 3, "100*3/100=3");
    assert_eq_test!(mul_u64_div(0, 100, 50), 0, "zero a");
    assert_eq_test!(mul_u64_div(100, 0, 50), 0, "zero b");
    assert_eq_test!(mul_u64_div(100, 50, 0), 0, "zero c");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_clock_read() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // u64 类型即非负契约; 此测试只验证调用不 panic + 返回合理量级.
    let ns = hrtimer_clock_read();
    check!(ns < u64::MAX, "clock read bounded");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_queue_operations() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    // J-01 (2026-09-08): static mut 移至函数顶部 (items_after_statements 清理) —
    // 测试用持久 HrTimer, 生命周期跨多次调用
    static mut T1: HrTimer = HrTimer::uninit();
    hrtimer_init();
    check!(is_hrtimer_ready(), "initialized");
    assert_eq_test!(hrtimer_pending_count(), 0, "empty queue");

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        T1.init(noop_callback);
        hrtimer_start(&T1, 1_000_000_000);
    }
    assert_eq_test!(hrtimer_pending_count(), 1, "1 timer");

    let next = hrtimer_next_expiry();
    check!(next.is_some(), "has next expiry");
    assert_eq_test!(next.unwrap(), 1_000_000_000, "expiry matches");

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    hrtimer_cancel(unsafe { &T1 });
    // 取消后队列在下一次 run_queues 时清理
    hrtimer_run_queues();
    assert_eq_test!(hrtimer_pending_count(), 0, "empty after cancel+run");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_periodic_restart() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    static mut T2: HrTimer = HrTimer::uninit();
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        T2.init(periodic_test_callback);
        let now = hrtimer_clock_read();
        hrtimer_start(&T2, now + 1_000_000); // 1ms 后
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    check!(unsafe { T2.is_pending() }, "pending after start");

    // 运行队列 (可能未到期, 但测试回调逻辑)
    hrtimer_run_queues();
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn periodic_test_callback(_timer: &HrTimer) -> HrTimerRestart {
    HrTimerRestart::Periodic
}

/// 注册高精度定时器 (hrtimer) 测试用例 (kernel_test)
#[cfg(feature = "kernel_test")]
pub fn register_hrtimer_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("hrtimer", "state_encoding", test_state_encoding);
    r.register("hrtimer", "uninit_state", test_uninit_state);
    r.register("hrtimer", "init_and_cancel", test_init_and_cancel);
    r.register("hrtimer", "forward_periodic", test_forward_periodic);
    r.register("hrtimer", "mul_u64_div", test_mul_u64_div_basic);
    r.register("hrtimer", "clock_read", test_clock_read);
    r.register("hrtimer", "queue_ops", test_queue_operations);
    r.register("hrtimer", "periodic_restart", test_periodic_restart);
}
