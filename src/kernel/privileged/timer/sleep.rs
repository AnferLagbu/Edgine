//! Sleep 和延时功能
//!
//! 提供多种精度的睡眠和等待机制：
//! - **忙等待 (Busy-wait)**: 极短延时，精确但浪费 CPU
//! - **调度器阻塞**: 长延时，高效利用 CPU
//! - **TSC 高精度**: 微秒/纳秒级延时
//! - **自适应策略**: 已下沉 functions (`functions::timer::sleep::adaptive_sleep`)
//!
//! ## 使用指南
//!
//! ```text
//! 延时长度          推荐方法              适用场景
//! ──────────────────────────────────────────────────
//! < 1 μs           busy_wait_ns()        中断禁用、硬件初始化
//! 1 μs - 1 ms      busy_wait_us()       短暂轮询
//! 1 ms - 10 ms      busy_wait_ms()       快速响应需求
//! > 10 ms          timer_sleep()        普通应用、用户态
//! ```
//!
//! # Performance
//! 关键路径函数已标记 `#[inline(always)]` 以优化性能。

use super::tick::{get_ticks, is_initialized, ms_to_ticks};
use crate::privileged::cpu::read_tsc;

/// 每毫秒的纳秒数 (睡眠机制内 ns↔ms 换算系数)
const NS_PER_MS: u64 = 1_000_000;

// ============================================================================
// 忙等待实现 (Busy-wait)
// ============================================================================

/// 使用硬件计数器进行纳秒级忙等待
///
/// 最精确的延时方式，但不释放 CPU。
/// 适用于中断上下文或需要微秒级精度的场景。
///
/// 周期↔时长 的折算一律取单一时基登记的频率 (`calibration::counter_hz_for_wait`),
/// 本模块不另存频率副本.
///
/// # Arguments
/// * `ns` - 等待时间 (纳秒)
///
/// # Example
/// ```rust,no_run
/// // 等待 500 纳秒 (0.5 微秒)
/// busy_wait_ns(500);
/// ```
#[inline(always)]
pub fn busy_wait_ns(ns: u64) {
    if ns == 0 {
        return;
    }

    let start = read_tsc();

    // 单一时基: 频率取 `timer::calibration` 的登记值 (旧形态经 FFI 绕道
    // `cpu_get_tsc_frequency` 取的是 `CpuInfo` 里的厂商硬编码估计值, 与真值可差
    // 1.4×); 未登记时按合理性上限折算 ⇒ 只会多等不会少等 (旧 `ns * 2` 假设 2 GHz,
    // 真频更高时等待不足, 驱动依赖的硬件时序下限失效).
    let freq_hz = super::calibration::counter_hz_for_wait();
    // mul_div_saturating: 128 位中间值, 长延时 (如 busy_wait_ms(u64::MAX/1000))
    // 不再因 `ns * freq_hz` 溢出而 panic.
    let target_cycles = super::tick::mul_div_saturating(ns, freq_hz, 1_000_000_000);

    loop {
        let elapsed = read_tsc().saturating_sub(start);
        if elapsed >= target_cycles {
            break;
        }

        // 提示 CPU 我们在自旋循环
        core::hint::spin_loop();
    }
}

/// 使用 TSC 进行微秒级忙等待
///
/// # Arguments
/// * `us` - 等待时间 (微秒)
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn busy_wait_us(us: u64) {
    if us == 0 {
        return;
    }

    // 转换为纳秒后调用
    busy_wait_ns(us * 1000);
}

/// 使用 TSC 进行毫秒级忙等待
///
/// # Arguments
/// * `ms` - 等待时间 (毫秒)
#[inline(always)]
pub fn busy_wait_ms(ms: u64) {
    if ms == 0 {
        return;
    }

    // 转换为微秒后调用
    busy_wait_us(ms * 1000);
}

// ============================================================================
// 调度器阻塞 Sleep
// ============================================================================

/// 阻塞当前线程指定毫秒数
///
/// 将当前线程置为 `Sleeping` 阻塞态并让出 CPU，
/// 由调度器 tick 的睡眠唤醒扫描在其 `sleep_until` 到期后唤醒。
/// 这是**最高效**的长延时方式 (睡眠期间不占 CPU)。
///
/// # Arguments
/// * `ms` - 睡眠时间 (毫秒), 0 表示立即返回
///
/// # Returns
/// * `Ok(())` - 正常唤醒
/// * `Err(i32)` - 错误码 (-1: 被信号中断)
///
/// # Example
/// ```rust,no_run
/// // 睡眠 100 毫秒 (高效，不浪费 CPU)
/// timer_sleep(100).unwrap();
/// ```
///
/// # Errors
/// 当前实现的所有代码路径均返回 `Ok(())` (定时器未初始化时回退到忙等待或
/// yield 循环), 不会返回 `Err`; `Err(-1)` 为预留的"被信号中断"错误码.
pub fn timer_sleep(ms: u64) -> Result<(), i32> {
    if ms == 0 {
        return Ok(());
    }

    if !is_initialized() {
        // Timer 未初始化时回退到忙等待
        busy_wait_ms(ms);
        return Ok(());
    }

    let pid = crate::privileged::proc::process_get_current_pid();
    if pid == 0 {
        // idle/内核线程回退到 yield 循环
        return timer_sleep_yield(ms);
    }

    // 单一唤醒机制: 写入 `sleep_until` (tick 时基), 由调度器 tick 的睡眠唤醒
    // 扫描 (见 `Scheduler::tick`) 在到期后 `unblock` —— 本路径是全内核唯一的
    // 毫秒级可阻塞睡眠入口 (原 `extern "C"` 包装层已随死 FFI 一并删除),
    // 不再并行维护 hrtimer 回调唤醒路径 (该路径回调从未触发, 且栈上 HrTimer 在
    // 进程退出后会留下悬垂队列项 = UAF 隐患).
    let wake_at = get_ticks() + ms_to_ticks(ms);
    crate::privileged::proc::PROCESS_TABLE.with_process(pid, |proc| {
        proc.sleep_until
            .store(wake_at, core::sync::atomic::Ordering::SeqCst);
    });

    // `block()` 只登记挂起状态与 need_reschedule, 不切换上下文; 必须同步
    // `schedule()` 才能真正挂起调用者 —— 否则调用者会带着 Blocked 状态继续执行.
    crate::privileged::proc::scheduler_block(crate::privileged::proc::BlockReason::Sleeping);
    crate::privileged::proc::scheduler_schedule();

    Ok(())
}

/// 纳秒级睡眠机制 (tsleep 类 syscall 的共享权威实现)
///
/// - `< 1ms`: hrtimer 时钟源忙等待 (高精度, 不依赖调度器可阻塞性)
/// - `>= 1ms`: 委托 [`timer_sleep`] (hrtimer 到期回调唤醒 + 调度器阻塞)
///
/// 毫秒换算向上取整, 保证实际睡眠时长不低于请求时长 (POSIX 语义).
///
/// 调用方: `privileged::syscall::sys_nanosleep` (SYS_nanosleep) 与
/// `functions::timer::clock::clock_nanosleep_syscall` (SYS_clock_nanosleep);
/// 两者共用本函数以避免睡眠策略出现内核内部并行实现.
///
/// # Arguments
/// * `total_ns` - 睡眠时长 (纳秒); 0 表示立即返回
pub fn sleep_ns(total_ns: u64) {
    if total_ns == 0 {
        return;
    }

    if total_ns < NS_PER_MS {
        let start = crate::privileged::timer::hrtimer_clock_read();
        let target = start + total_ns;
        while crate::privileged::timer::hrtimer_clock_read() < target {
            core::hint::spin_loop();
        }
    } else {
        let sleep_ms = total_ns.div_ceil(NS_PER_MS);
        let _ = timer_sleep(sleep_ms);
    }
}

/// yield 循环回退实现 (idle/内核线程或定时器不可用时)
fn timer_sleep_yield(ms: u64) -> Result<(), i32> {
    // SAFETY: get_ticks() 读取全局原子计数器, scheduler_yield_ex() 是
    // 正常的调度器让出函数; 均可在进程上下文安全调用.
    unsafe {
        // SAFETY: scheduler_yield_ex 由框架调度器提供, 符号与 C ABI 声明一致
        unsafe extern "C" {
            fn scheduler_yield_ex();
        }
        let start_tick = get_ticks();
        let target_ticks = ms_to_ticks(ms);

        loop {
            let elapsed = get_ticks().saturating_sub(start_tick);
            if elapsed >= target_ticks {
                return Ok(());
            }
            scheduler_yield_ex();
        }
    }
}

/// 带超时的条件等待
///
/// 循环检查条件直到满足或超时。
/// 结合了条件变量和超时机制。
///
/// # Type Parameters
/// * `F` - 条件检查闭包
///
/// # Arguments
/// * `condition` - 返回 true 表示条件满足
/// * `timeout_ms` - 超时时间 (毫秒), 0 表示无限等待
///
/// # Returns
/// * `Ok(())` - 条件满足
/// * `Err(-1)` - 超时
///
/// # Errors
/// 当在 `timeout_ms` 内条件始终未满足时返回 `Err(-1)` (超时).
pub fn wait_with_timeout<F>(condition: F, timeout_ms: u64) -> Result<(), i32>
where
    F: Fn() -> bool,
{
    if condition() {
        return Ok(()); // 条件立即满足
    }

    if timeout_ms == 0 {
        // 无限等待
        while !condition() {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                // SAFETY: scheduler_yield_ex 由框架调度器提供, 进程上下文安全调用
                unsafe extern "C" {
                    fn scheduler_yield_ex();
                }
                scheduler_yield_ex();
            }
        }
        return Ok(());
    }

    // 带超时等待
    let start_tick = get_ticks();
    let target_ticks = ms_to_ticks(timeout_ms);

    loop {
        if condition() {
            return Ok(()); // 条件满足
        }

        let elapsed = get_ticks().saturating_sub(start_tick);
        if elapsed >= target_ticks {
            return Err(-1); // 超时
        }

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // SAFETY: scheduler_yield_ex 由框架调度器提供, 进程上下文安全调用
            unsafe extern "C" {
                fn scheduler_yield_ex();
            }
            scheduler_yield_ex();
        }
    }
}

// ============================================================================
// 兼容 C 接口
// ============================================================================

/// 兼容 C 接口的忙等待 sleep 函数
///
/// 与原始 `timer_sleep_busy()` C 函数签名兼容。
///
/// # Arguments
/// * `ms` - 忙等待时间 (毫秒)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn timer_sleep_busy_compat(ms: u64) {
    busy_wait_ms(ms);
}

// ============================================================================
// 辅助工具
// ============================================================================

/// 测量代码块执行时间 (基于单一时基的纳秒视图)
///
/// 读数与日志前缀 / hrtimer / 网络时钟共用同一时基锚点: 硬件时基建立后具备计数器粒度,
/// 未建立时退化为 tick 粒度 (不拿假频率反推).
///
/// # Type Parameters
/// * `F` - 要测量的代码块
///
/// # Arguments
/// * `func` - 要执行的闭包
///
/// # Returns
/// * `(T, u64)` - (返回值, 执行时间 `[纳秒]`)
///
/// # Example
/// ```rust,no_run
/// let (result, duration_ns) = measure_time(|| {
///     some_expensive_operation()
/// });
/// println!("耗时 {} ns", duration_ns);
/// ```
pub fn measure_time<T, F>(func: F) -> (T, u64)
where
    F: FnOnce() -> T,
{
    let start = super::tick::get_uptime_ns();
    let result = func();
    let end = super::tick::get_uptime_ns();

    (result, end.saturating_sub(start))
}

/// 测量代码块执行时间 (基于 ticks)
///
/// 更适合测量较长的操作 (>1ms)。
///
/// # Type Parameters
/// * `F` - 要测量的代码块
///
/// # Returns
/// * `(T, u64)` - (返回值, 执行时间 `[ticks]`)
pub fn measure_time_ticks<T, F>(func: F) -> (T, u64)
where
    F: FnOnce() -> T,
{
    let start = get_ticks();
    let result = func();
    let end = get_ticks();

    (result, end.saturating_sub(start))
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_busy_wait_zero_duration() {
        // 零延时应该立即返回
        let start = read_tsc();
        busy_wait_ns(0);
        busy_wait_us(0);
        busy_wait_ms(0);
        let end = read_tsc();

        // 应该几乎不消耗时间 (< 1000 cycles)
        assert!(end.saturating_sub(start) < 1000);
    }

    #[test]
    fn test_busy_wait_positive_duration() {
        // 正延时应该消耗合理的时间.
        // 时基互斥: 折算频率是全局登记值, 与 calibration 用例并行会互相污染.
        let _lock = crate::privileged::timer::tick::FREQ_TEST_LOCK.lock();

        let start = read_tsc();
        busy_wait_us(100); // 100 微秒
        let end = read_tsc();

        // 期望周期数由同一个登记频率反算 (host 下未登记 ⇒ 取合理性上限),
        // 断言只锁"确实走到周期下界"与"没有数量级失控" — 不再假设主机 CPU 频率.
        let freq = crate::privileged::timer::calibration::counter_hz_for_wait();
        let expected = (100_000u64 * freq) / 1_000_000_000;
        let elapsed_cycles = end.saturating_sub(start);
        assert!(
            elapsed_cycles >= expected - expected / 10,
            "忙等待未走到目标周期数"
        );
        assert!(elapsed_cycles < expected * 2, "忙等待时长数量级失控");
    }

    #[test]
    fn test_timer_sleep_zero() {
        // 零延时应该成功
        assert!(timer_sleep(0).is_ok());
    }

    #[test]
    fn test_measure_time_basic() {
        let (result, _duration_ns) = measure_time(|| {
            42 // 简单计算
        });

        assert_eq!(result, 42);
        // duration_ns 为 u64, 恒 >= 0 (恒真断言已删).
    }

    #[test]
    fn test_measure_time_ticks_basic() {
        let (result, _duration_ticks) = measure_time_ticks(|| {
            "hello".to_string() // 分配操作
        });

        assert_eq!(result, "hello");
        // duration_ticks 为 u64, 恒 >= 0 (恒真断言已删).
    }

    #[test]
    fn test_wait_with_timeout_immediate() {
        // 条件立即满足
        let result = wait_with_timeout(|| true, 1000);
        assert!(result.is_ok());
    }
}

/// 注册忙等待 (sleep) 相关测试用例 (kernel_test)
#[cfg(feature = "kernel_test")]
pub fn register_timer_sleep_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};

    fn busy_wait_zero_duration() -> TestResult {
        let start = read_tsc();
        busy_wait_ns(0);
        busy_wait_us(0);
        busy_wait_ms(0);
        let end = read_tsc();
        crate::check!(end.saturating_sub(start) < 100_000, "zero wait fast");
        TestResult::Pass
    }

    fn timer_sleep_zero() -> TestResult {
        crate::check!(timer_sleep(0).is_ok(), "sleep 0 ok");
        TestResult::Pass
    }

    fn measure_time_basic() -> TestResult {
        let (result, duration_ns) = measure_time(|| 42u64);
        crate::assert_eq_test!(result, 42u64, "result");
        let _ = duration_ns;
        TestResult::Pass
    }

    fn measure_time_ticks_basic() -> TestResult {
        let (result, duration_ticks) = measure_time_ticks(|| 123u64);
        crate::assert_eq_test!(result, 123u64, "result");
        let _ = duration_ticks;
        TestResult::Pass
    }

    fn wait_with_timeout_immediate() -> TestResult {
        let result = wait_with_timeout(|| true, 1000);
        crate::check!(result.is_ok(), "immediate condition ok");
        TestResult::Pass
    }

    let r = runner();
    r.register(
        "timer::sleep",
        "busy_wait_zero",
        busy_wait_zero_duration as TestFn,
    );
    r.register(
        "timer::sleep",
        "timer_sleep_zero",
        timer_sleep_zero as TestFn,
    );
    r.register("timer::sleep", "measure_time", measure_time_basic as TestFn);
    r.register(
        "timer::sleep",
        "measure_time_ticks",
        measure_time_ticks_basic as TestFn,
    );
    r.register(
        "timer::sleep",
        "wait_with_timeout_immediate",
        wait_with_timeout_immediate as TestFn,
    );
}
