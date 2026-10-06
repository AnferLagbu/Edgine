//! Sleep 和延时功能
//!
//! 提供多种精度的睡眠和等待机制：
//! - **忙等待 (Busy-wait)**: 极短延时，精确但浪费 CPU
//! - **调度器阻塞**: 长延时，高效利用 CPU
//! - **TSC 高精度**: 微秒/纳秒级延时
//! - **自适应策略**: 根据时长自动选择最佳方式
//!
//! ## 使用指南
//!
//! ```
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
use crate::privileged::cpu::{cycles_to_nanoseconds, read_tsc};

/// 每毫秒的纳秒数 (睡眠机制内 ns↔ms 换算系数)
const NS_PER_MS: u64 = 1_000_000;

// ============================================================================
// 忙等待实现 (Busy-wait)
// ============================================================================

/// 使用 TSC 进行纳秒级忙等待
///
/// 最精确的延时方式，但不释放 CPU。
/// 适用于中断上下文或需要微秒级精度的场景。
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

    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    unsafe extern "C" {
        fn cpu_get_tsc_frequency() -> u64;
    }
    // SAFETY: `cpu_get_tsc_frequency` 是有效的 C ABI 函数指针; 参数列表与声明一致
    let freq_hz = unsafe { cpu_get_tsc_frequency() };

    let target_cycles = if freq_hz > 0 {
        (ns * freq_hz) / 1_000_000_000
    } else {
        ns * 2
    };

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

/// 使用 PIT 计数器进行精确微秒级忙等待
///
/// 比 TSC 方式更可靠（不依赖 CPU 频率），
/// 但需要 PIT 已初始化。
///
/// # Arguments
/// * `us` - 等待时间 (微秒)
///
/// # Returns
/// * `Ok(())` - 成功完成
/// * `Err(&str)` - PIT 未初始化或其他错误
///
/// # Errors
/// 当 PIT 未初始化时返回 `Err("PIT not initialized")`; 当读取 PIT 计数值失败时返回
/// `Err("Failed to read PIT count")`.
pub fn pit_busy_wait_us(us: u64) -> Result<(), &'static str> {
    if us == 0 {
        return Ok(());
    }

    if !super::pit::pit_is_initialized() {
        return Err("PIT not initialized");
    }

    // 读取当前 PIT 计数值作为起始点
    let start_count = super::pit::pit_read_count().ok_or("Failed to read PIT count")?;

    // 计算目标计数值变化量
    // PIT 频率 = 1.193182 MHz → 每微秒 ≈ 1.193 个周期
    let cycles_needed = (us * super::pit::PIT_BASE_FREQUENCY) / 1_000_000;

    loop {
        let current_count = super::pit::pit_read_count().ok_or("Failed to read PIT count")?;

        // 计算已过去的周期数 (考虑倒计数特性)
        let elapsed = if current_count <= start_count {
            start_count - current_count
        } else {
            // 回绕处理
            start_count + (0xFFFFu16 - current_count) + 1
        };

        if u64::from(elapsed) >= cycles_needed {
            break;
        }

        core::hint::spin_loop();
    }

    Ok(())
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
    // 扫描 (见 `Scheduler::tick`) 在到期后 `unblock` —— 与 `proc_sleep_ms` 同源,
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
        let start_tick = get_ticks();
        let target_ticks = ms_to_ticks(ms);

        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        // SAFETY: scheduler_yield_ex 由框架调度器提供, 进程上下文安全调用
        unsafe extern "C" {
            fn scheduler_yield_ex();
        }

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
// 自适应 Sleep 策略
// ============================================================================

/// 自适应睡眠函数
///
/// 根据延时长度自动选择最优策略：
/// - **< 1 ms**: 忙等待 (避免调度开销)
/// - **≥ 1 ms**: 调度器阻塞 (节省 CPU)
///
/// # Arguments
/// * `ms` - 睡眠时间 (毫秒)
pub fn adaptive_sleep(ms: u64) {
    if ms == 0 {
        return;
    }

    // 阈值: 1 毫秒
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const BUSY_WAIT_THRESHOLD_MS: u64 = 1;

    if ms < BUSY_WAIT_THRESHOLD_MS {
        // 短延时: 忙等待
        busy_wait_ms(ms);
    } else {
        // 长延时: 调度器阻塞
        let _ = timer_sleep(ms);
    }
}

/// 兼容 C 接口的 sleep 函数
///
/// 与原始 `timer_sleep()` C 函数签名完全兼容，
/// 用于平滑迁移现有代码。
///
/// # Arguments
/// * `ms` - 睡眠时间 (毫秒)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn timer_sleep_compat(ms: u64) {
    adaptive_sleep(ms);
}

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

/// 测量代码块执行时间 (基于 TSC)
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
    let start = read_tsc();
    let result = func();
    let end = read_tsc();

    let cycles = end.saturating_sub(start);
    let ns = cycles_to_nanoseconds(cycles, 2000); // 假设 2GHz

    (result, ns)
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
        // 正延时应该消耗合理的时间
        let start = read_tsc();
        busy_wait_us(100); // 100 微秒
        let end = read_tsc();

        let elapsed_cycles = end.saturating_sub(start);
        // 在 2GHz CPU 上, 100μs ≈ 200,000 cycles
        // 给予 50% 的误差范围
        assert!(elapsed_cycles > 100_000); // 至少 50μs
        assert!(elapsed_cycles < 500_000); // 不超过 250μs
    }

    #[test]
    fn test_timer_sleep_zero() {
        // 零延时应该成功
        assert!(timer_sleep(0).is_ok());
    }

    #[test]
    fn test_adaptive_sleep_behavior() {
        // 测试自适应策略选择
        // 注意: 这些测试只验证不会 panic

        adaptive_sleep(0); // 应该立即返回
        adaptive_sleep(1); // 忙等待
        adaptive_sleep(100); // 调度器阻塞
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

    #[test]
    fn test_pit_busy_wait_uninitialized() {
        // PIT 未初始化时应该返回错误
        let result = pit_busy_wait_us(100);
        assert!(result.is_err());
    }
}

/// 注册忙等待 (sleep) 相关测试用例 (kernel_test)
#[cfg(feature = "kernel_test")]
// J-01 (2026-09-08): items_after_statements — 测试注册函数内嵌套测试 fn 为内核测试惯用模式
#[expect(
    clippy::items_after_statements,
    reason = "items_after_statements: 测试注册函数内嵌套测试 fn 为内核测试惯用模式; 当前优先 expect"
)]
pub fn register_timer_sleep_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};
    let r = runner();

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

    fn pit_busy_wait_uninitialized() -> TestResult {
        let result = pit_busy_wait_us(100);
        crate::check!(result.is_err(), "pit not initialized");
        TestResult::Pass
    }

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
    r.register(
        "timer::sleep",
        "pit_uninitialized",
        pit_busy_wait_uninitialized as TestFn,
    );
}
