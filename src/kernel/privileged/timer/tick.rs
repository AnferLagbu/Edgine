//! 全局 Tick 计数器和时间管理
//!
//! 提供系统级时间追踪功能：
//! - **Tick 计数器**: 自启动以来的中断次数
//! - **时间转换**: Tick ↔ 毫秒/微秒/纳秒
//! - **运行时间**: 系统 uptime 统计
//!
//! ## 设计理念
//!
//! 采用分层时间模型：
//! ```text
//! Layer 3: 高精度 (TSC)     ← 性能测量、微基准测试
//! Layer 2: 中精度 (Tick+PIT) ← 调度、超时、sleep
//! Layer 1: 低精度 (Tick)     ← 简单计时、日志时间戳
//! ```
//!
//! # Safety
//! 所有公共接口都是线程安全的，使用原子操作。

use super::pit::DEFAULT_INTERRUPT_FREQ_HZ;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

// ============================================================================
// 全局状态
// ============================================================================

/// 自启动以来的 tick 总数 (每次 IRQ0 +1)
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// 系统启动时的 TSC 值 (用于计算 uptime)
static BOOT_TSC: AtomicU64 = AtomicU64::new(0);

/// 上次更新 TSC 的时间戳 (用于增量计算)
static LAST_TSC: AtomicU64 = AtomicU64::new(0);

/// 当前配置的定时器频率 (Hz)
static TIMER_FREQ_HZ: AtomicU32 = AtomicU32::new(DEFAULT_INTERRUPT_FREQ_HZ);

/// Timer 子系统初始化标志
static TIMER_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// 每个 tick 对应的纳秒数 (预计算，避免重复除法)
static NS_PER_TICK: AtomicU64 = AtomicU64::new(0);

/// 每个 tick 对应的微秒数
static US_PER_TICK: AtomicU64 = AtomicU64::new(0);

/// 每个 tick 对应的毫秒数
static MS_PER_TICK: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// 初始化和配置
// ============================================================================

/// 初始化 Timer 子系统
///
/// 必须在内核早期调用，设置基础时间参数。
///
/// # Arguments
/// * `frequency_hz` - 定时器中断频率 (Hz), 推荐 1000 Hz
///
/// # Returns
/// * `Ok(u32)` - 实际配置的频率
/// * `Err(&str)` - 错误描述
///
/// # Errors
/// 当 `frequency_hz` 为 0 时返回 `Err("Timer frequency must be > 0")`;
/// 在 `x86_64` 上 PIT 初始化失败时返回 `Err` (错误信息来自 `pit_init`, 如
/// 频率超限或分频值越界).
pub fn timer_init(frequency_hz: u32) -> Result<u32, &'static str> {
    if frequency_hz == 0 {
        return Err("Timer frequency must be > 0");
    }

    // 1. 初始化定时器硬件
    #[cfg(target_arch = "x86_64")]
    let actual_freq = super::pit::pit_init(frequency_hz)?;
    #[cfg(target_arch = "aarch64")]
    let actual_freq = {
        // ARM 通用定时器已由 arch/aarch64/timer::init() 完成初始化.
        // 此处仅用请求频率作 tick 簿记.
        frequency_hz
    };

    // 2. 记录启动时间戳
    #[cfg(target_arch = "x86_64")]
    {
        let boot_tsc = crate::privileged::cpu::read_tsc();
        BOOT_TSC.store(boot_tsc, Ordering::Relaxed);
        LAST_TSC.store(boot_tsc, Ordering::Relaxed);
    }
    #[cfg(target_arch = "aarch64")]
    {
        let boot_cnt = crate::arch!(timestamp());
        BOOT_TSC.store(boot_cnt, Ordering::Relaxed);
        LAST_TSC.store(boot_cnt, Ordering::Relaxed);
    }

    // 3. 更新频率和时间转换常量
    TIMER_FREQ_HZ.store(actual_freq, Ordering::Relaxed);

    let ns_per_tick = 1_000_000_000u64 / u64::from(actual_freq);
    let us_per_tick = 1_000_000u64 / u64::from(actual_freq);
    let ms_per_tick = 1_000u64 / u64::from(actual_freq);

    NS_PER_TICK.store(ns_per_tick, Ordering::Relaxed);
    US_PER_TICK.store(us_per_tick, Ordering::Relaxed);
    MS_PER_TICK.store(ms_per_tick, Ordering::Relaxed);

    // 4. 重置 tick 计数器
    TICK_COUNT.store(0, Ordering::Release);

    // 5. 标记为已初始化
    TIMER_INITIALIZED.store(true, Ordering::Release);

    // 6. 初始化高精度定时器框架
    super::hrtimer::hrtimer_init();

    // 7. 注册 Timer softirq 处理程序 (预留, 当前 hrtimer 在 hardirq 中直接处理)
    crate::privileged::irq::open_softirq(
        crate::privileged::irq::SoftirqVec::Timer,
        timer_softirq_handler,
    );

    Ok(actual_freq)
}

/// Timer softirq 处理程序 — 预留: 待将 hrtimer 处理从 hardirq 迁移到 softirq 时启用
fn timer_softirq_handler() {
    // 当前 hrtimer_run_queues() 在 on_timer_interrupt() (hardirq) 中已调用.
    // 此 handler 为后续优化预留: 将 hrtimer 处理移到 softirq 可减少中断禁用时间.
}

/// 检查 Timer 是否已初始化
pub fn is_initialized() -> bool {
    TIMER_INITIALIZED.load(Ordering::Acquire)
}

// ============================================================================
// Tick 管理
// ============================================================================

/// 在 IRQ0 中断处理程序中调用此函数
///
/// 递增全局 tick 计数器并更新内部状态。
///
/// I-50: 内部统一触发 `hrtimer_run_queues()`, 调用方无需再单独调,
///       避免遗忘导致 hrtimer 不被处理. 旧调用方 (`timer_irq0_handler` /
///       `irq_handler_el1`) 仍显式调用一次以保持向后兼容, 重复处理由
///       hrtimer 自身的 "已 Running/Inactive 跳过" 逻辑避免副作用.
///
/// # Safety
/// 只能从中断上下文调用
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn on_timer_interrupt() {
    // B03-12: tick 计数器 fetch_add 从 Relaxed 改为 AcqRel。
    // AcqRel 保证 (a) 本 CPU 之前写入对其他 CPU 可见 (Release),
    //          (b) 读到其他 CPU 的最新 tick 值 (Acquire)。
    // 之前 Relaxed 写 + Acquire 读 不能跨 CPU 严格同步 (Acquire 仅与 Release 配对),
    // 多核下其他 CPU 读到的 tick 可能滞后于实际中断计数。
    TICK_COUNT.fetch_add(1, Ordering::AcqRel);

    // 更新 PIT 内部状态 (x86_64 only)
    #[cfg(target_arch = "x86_64")]
    super::pit::pit_on_interrupt();

    // 更新时间戳 (用于高精度测量)
    #[cfg(target_arch = "x86_64")]
    {
        let current_tsc = crate::privileged::cpu::read_tsc();
        LAST_TSC.store(current_tsc, Ordering::Relaxed);
    }
    #[cfg(target_arch = "aarch64")]
    {
        let current_cnt = crate::arch!(timestamp());
        LAST_TSC.store(current_cnt, Ordering::Relaxed);
    }

    // I-50: 统一入口 — tick 与 hrtimer 合并处理, 调用方不再需要单独调 hrtimer_run_queues.
    super::hrtimer::hrtimer_run_queues();

    // D9: 时钟渐进调整 — 消耗 adjtimex(ADJ_OFFSET) 登记的待调整偏移 (斜率推进).
    // 仅原子操作, 无锁/无分配, 因此可在 hardirq 上下文安全调用.
    super::time_sync::timesync_subsystem().tick_adjust();
}

/// 获取当前 tick 数
///
/// 返回自启动以来的总 tick 数。
///
/// # Returns
/// * `u64` - 总 tick 数 (单调递增)
///
/// # Example
/// ```rust,no_run
/// let start_ticks = get_ticks();
/// // ... 执行一些工作 ...
/// let elapsed = get_ticks() - start_ticks;
/// println!("耗时 {} ticks", elapsed);
/// ```
#[inline(always)]
pub fn get_ticks() -> u64 {
    TICK_COUNT.load(Ordering::Acquire)
}

/// 获取当前定时器频率
///
/// # Returns
/// * `u32` - 当前频率 (Hz)
#[inline]
pub fn get_frequency() -> u32 {
    TIMER_FREQ_HZ.load(Ordering::Acquire)
}

/// 重置 tick 计数器 (仅用于测试或特殊场景)
///
/// # Warning
/// 此操作会影响所有依赖时间的功能！
pub fn reset_ticks() {
    TICK_COUNT.store(0, Ordering::Release);
}

// ============================================================================
// 时间转换函数
// ============================================================================

/// 将 ticks 转换为毫秒
///
/// # Arguments
/// * `ticks` - tick 数
///
/// # Returns
/// * `u64` - 毫秒数
#[inline]
pub fn ticks_to_ms(ticks: u64) -> u64 {
    let freq = u64::from(get_frequency());
    if freq == 0 {
        return 0;
    }
    // saturating_mul: `ticks * 1000` 在 ticks 接近 u64::MAX 时溢出
    // (debug 构建直接 panic) — 本函数的契约是"不 panic"
    // (2026-09-24 UT-06 实测修复, 见 privileged/timer/mod.rs::test_error_handling).
    ticks.saturating_mul(1000) / freq
}

/// 将 ticks 转换为微秒
#[inline]
pub fn ticks_to_us(ticks: u64) -> u64 {
    let freq = u64::from(get_frequency());
    if freq == 0 {
        return 0;
    }
    // 同 ticks_to_ms: 防溢出 (UT-06 实测, 同一根因).
    ticks.saturating_mul(1_000_000) / freq
}

/// 将 ticks 转换为纳秒
#[inline]
pub fn ticks_to_ns(ticks: u64) -> u64 {
    let freq = u64::from(get_frequency());
    if freq == 0 {
        return 0;
    }
    // 同 ticks_to_ms: 防溢出 (UT-06 实测, 同一根因).
    ticks.saturating_mul(1_000_000_000) / freq
}

/// 将毫秒转换为 ticks (向上取整)
#[inline]
pub fn ms_to_ticks(ms: u64) -> u64 {
    let freq = u64::from(get_frequency());
    if freq == 0 {
        return ms;
    }
    // saturating_mul: `ms * freq` 在 ms 接近 u64::MAX 时溢出 (UT-06 实测修复).
    ms.saturating_mul(freq).div_ceil(1000) // 向上取整
}

/// 将微秒转换为 ticks (向上取整)
#[inline]
pub fn us_to_ticks(us: u64) -> u64 {
    let freq = u64::from(get_frequency());
    if freq == 0 {
        return us;
    }
    // 同 ms_to_ticks: 防溢出 (UT-06 实测, 同一根因).
    us.saturating_mul(freq).div_ceil(1_000_000)
}

/// 使用预计算的常量进行快速转换 (性能关键路径)
#[inline]
pub fn ticks_to_ms_fast(ticks: u64) -> u64 {
    ticks * MS_PER_TICK.load(Ordering::Relaxed)
}

/// 使用预计算常量进行快速微秒转换 (性能关键路径)
#[inline]
pub fn ticks_to_us_fast(ticks: u64) -> u64 {
    ticks * US_PER_TICK.load(Ordering::Relaxed)
}

// ============================================================================
// 系统运行时间 (Uptime)
// ============================================================================

/// 获取系统运行时间 (毫秒)
///
/// 基于 tick 计数器计算，精度取决于定时器频率。
///
/// # Returns
/// * `u64` - 自启动以来的毫秒数
#[inline]
pub fn get_uptime_ms() -> u64 {
    ticks_to_ms(get_ticks())
}

/// 获取系统运行时间 (秒)
///
/// # Returns
/// * `u64` - 自启动以来的秒数 (截断)
#[inline]
pub fn get_uptime_s() -> u64 {
    get_uptime_ms() / 1000
}

/// 获取高精度运行时间 (基于 TSC)
///
/// 使用 TSC 提供亚 tick 粒度的时间测量。
/// 注意：需要先校准 TSC 频率才能转换为实际时间单位。
///
/// # Returns
/// * `(u64, u64)` - (TSC 周期数, 从上次 tick 以来的周期数)
pub fn get_uptime_tsc() -> (u64, u64) {
    let boot_tsc = BOOT_TSC.load(Ordering::Acquire);
    let last_tsc = LAST_TSC.load(Ordering::Acquire);
    let current_tsc = crate::privileged::cpu::read_tsc();

    let total_cycles = current_tsc.saturating_sub(boot_tsc);
    let since_last_tick = current_tsc.saturating_sub(last_tsc);

    (total_cycles, since_last_tick)
}

// ============================================================================
// 辅助工具
// ============================================================================

/// 格式化时间为人类可读字符串
///
/// # Arguments
/// * `ms` - 毫秒数
///
/// # Returns
/// 格式化字符串 (如 "1h23m45s678ms")
///
/// # Note
/// 需要分配内存，仅在非关键路径使用
#[cfg(feature = "alloc")]
pub fn format_duration(ms: u64) -> alloc::string::String {
    use alloc::format;

    let seconds = ms / 1000;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    let mut result = alloc::string::String::new();

    if days > 0 {
        result.push_str(&format!("{}d", days));
    }
    if hours % 24 > 0 || !result.is_empty() {
        result.push_str(&format!("{}h", hours % 24));
    }
    if minutes % 60 > 0 || !result.is_empty() {
        result.push_str(&format!("{}m", minutes % 60));
    }

    let secs = seconds % 60;
    let millis = ms % 1000;

    if millis > 0 {
        result.push_str(&format!("{}s{}ms", secs, millis));
    } else if secs > 0 || result.is_empty() {
        result.push_str(&format!("{}s", secs));
    } else {
        result.push_str("0ms");
    }

    result
}

/// 获取详细的系统时间信息 (用于调试)
///
/// # Returns
/// 包含各种时间指标的元组
pub fn get_time_info() -> (u64, u32, u64, u64, u64) {
    (
        get_ticks(),                         // 总 tick 数
        get_frequency(),                     // 定时器频率
        get_uptime_ms(),                     // 运行时间 (ms)
        NS_PER_TICK.load(Ordering::Relaxed), // 每 tick 纳秒数
        US_PER_TICK.load(Ordering::Relaxed), // 每 tick 微秒数
    )
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 频率相关用例的互斥锁.
    ///
    /// `zero_frequency_handling` 需临时把全局频率置 0 才能覆盖除零保护分支,
    /// 而 `initial_state` 断言频率为初值; 并行 test runner 下两者必须互斥,
    /// 否则偶发失败 (2026-09-24 UT-06).
    static FREQ_TEST_LOCK: crate::privileged::sync::IrqSpinLock<()> =
        crate::privileged::sync::IrqSpinLock::new(());

    /// 把全局频率置 0 的 RAII 守卫 (断言 panic 时也能恢复).
    struct FreqZeroGuard(u32);

    impl FreqZeroGuard {
        fn new() -> Self {
            let old = TIMER_FREQ_HZ.load(Ordering::Relaxed);
            TIMER_FREQ_HZ.store(0, Ordering::Relaxed);
            Self(old)
        }
    }

    impl Drop for FreqZeroGuard {
        fn drop(&mut self) {
            TIMER_FREQ_HZ.store(self.0, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_initial_state() {
        let _lock = FREQ_TEST_LOCK.lock();
        // 未初始化时应该返回安全值
        assert_eq!(get_ticks(), 0);
        assert_eq!(get_frequency(), DEFAULT_INTERRUPT_FREQ_HZ);
        assert!(!is_initialized());
    }

    #[test]
    fn test_time_conversions_at_1khz() {
        // 假设 1000 Hz (1ms per tick)
        let freq: u32 = 1000;

        // 测试 tick → time
        assert_eq!((1000 * 1000) / freq as u64, 1000); // 1000 ticks = 1000ms @ 1kHz
        assert_eq!((1 * 1_000_000) / freq as u64, 1000); // 1 tick = 1000μs @ 1kHz

        // 测试 time → tick
        assert_eq!((1000 * freq as u64 + 999) / 1000, 1000); // 1000ms ≈ 1000 ticks
        assert_eq!((1 * freq as u64 + 999_999) / 1_000_000, 1); // 1ms ≈ 1 tick
    }

    #[test]
    fn test_time_conversions_at_100hz() {
        let freq: u32 = 100;

        // 100 Hz = 10ms per tick
        assert_eq!((100 * 1000) / freq as u64, 1000); // 100 ticks = 1000ms
        assert_eq!((10 * 1_000_000) / freq as u64, 100000); // 10 ticks = 100000μs
    }

    #[test]
    fn test_zero_frequency_handling() {
        // 频率为 0 时应该返回 0 或原值（避免除零）
        // 全局频率初值是 DEFAULT_INTERRUPT_FREQ_HZ (非 0), 故必须显式置 0
        // 才能覆盖该分支 (2026-09-24 UT-06 实测修正).
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = FreqZeroGuard::new();
        assert_eq!(get_frequency(), 0);
        assert_eq!(ticks_to_ms(1000), 0);
        assert_eq!(ticks_to_us(1000), 0);
        assert_eq!(ms_to_ticks(1000), 1000); // 无法转换时返回原值
    }

    #[test]
    fn test_monotonicity() {
        // tick 应该是单调递增的
        let t1 = get_ticks();
        let t2 = get_ticks();
        assert!(t2 >= t1);
    }

    #[test]
    fn test_uptime_calculation() {
        // u64 恒 >= 0, 原恒真断言删除; 保留调用以冒烟验证
        // ticks_to_ms 在频率未初始化时不 panic.
        let _ = get_uptime_ms();
        let _ = get_uptime_s();
    }

    #[test]
    fn test_get_time_info() {
        let (_ticks, freq, _uptime, ns_per_tick, us_per_tick) = get_time_info();

        // ticks / uptime 为 u64, 恒 >= 0 的断言已删 (无信息量).
        assert!(freq > 0);

        // 如果已初始化，ns_per_tick 应该 > 0
        if is_initialized() {
            assert!(ns_per_tick > 0);
            assert!(us_per_tick > 0);
        }
    }
}

/// 注册定时器 tick 子系统测试用例 (kernel_test)
#[cfg(feature = "kernel_test")]
// J-01 (2026-09-08): items_after_statements — 测试注册函数内嵌套测试 fn 为内核测试惯用模式
#[expect(
    clippy::items_after_statements,
    reason = "items_after_statements: 测试注册函数内嵌套测试 fn 为内核测试惯用模式; 当前优先 expect"
)]
pub fn register_timer_tick_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};
    let r = runner();

    fn initial_state() -> TestResult {
        crate::assert_eq_test!(get_ticks(), 0, "ticks");
        crate::assert_eq_test!(get_frequency(), DEFAULT_INTERRUPT_FREQ_HZ, "freq");
        crate::check!(!is_initialized(), "not initialized");
        TestResult::Pass
    }

    fn zero_frequency_handling() -> TestResult {
        crate::assert_eq_test!(ticks_to_ms(1000), 1000, "ticks_to_ms");
        crate::assert_eq_test!(ticks_to_us(1000), 1000000, "ticks_to_us");
        crate::assert_eq_test!(ms_to_ticks(1000), 1000, "ms_to_ticks");
        TestResult::Pass
    }

    fn monotonicity() -> TestResult {
        let t1 = get_ticks();
        let t2 = get_ticks();
        crate::check!(t2 >= t1, "monotonic");
        TestResult::Pass
    }

    r.register("timer::tick", "initial_state", initial_state as TestFn);
    r.register(
        "timer::tick",
        "zero_frequency_handling",
        zero_frequency_handling as TestFn,
    );
    r.register("timer::tick", "monotonicity", monotonicity as TestFn);
}
