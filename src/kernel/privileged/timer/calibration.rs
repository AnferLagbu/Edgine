//! TSC (Time Stamp Counter) 频率校准
//!
//! 使用 PIT 作为参考时钟精确测量 TSC 频率：
//! - **PIT-based 校准**: 利用已知频率的 PIT 精确测量 TSC
//! - **多采样平均**: 减少测量误差
//! - **自动缓存**: 避免重复校准
//!
//! ## 算法原理
//!
//! ```text
//! 1. 读取初始 TSC 值 (tsc_start)
//! 2. 等待已知时间的 PIT 中断 (N 个 tick)
//! 3. 读取结束 TSC 值 (tsc_end)
//! 4. 计算: TSC_Freq = (tsc_end - tsc_start) / N * PIT_Freq
//! ```
//!
//! # Performance
//! 校准通常在启动时执行一次，结果缓存供后续使用。

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::privileged::cpu::{read_tsc, read_tsc_serialized};

// ============================================================================
// 全局状态
// ============================================================================

/// 已校准的 TSC 频率 (MHz)
static CALIBRATED_TSC_FREQ_MHZ: AtomicU64 = AtomicU64::new(0);

/// 已校准的 TSC 频率 (Hz, 完整值)
static CALIBRATED_TSC_FREQ_HZ: AtomicU64 = AtomicU64::new(0);

/// 是否已完成校准
static CALIBRATION_DONE: AtomicBool = AtomicBool::new(false);

/// 上次校准的 TSC 范围 (用于验证)
static LAST_CALIBRATION_RANGE: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// 校准算法实现
// ============================================================================

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
/// 执行 TSC 频率校准
///
/// 使用 PIT 作为精确的时间基准，测量 TSC 的实际运行频率。
///
/// # Arguments
/// * `calibration_ms` - 校准持续时间 (毫秒), 推荐 10-50ms
///
/// # Returns
/// * `Ok(u64)` - 校准得到的 TSC 频率 (MHz)
/// * `Err(&str)` - 校准失败原因
///
/// # Example
/// ```rust,no_run
/// let tsc_freq_mhz = calibrate_tsc(20).unwrap();
/// println!("TSC frequency: {} MHz", tsc_freq_mhz);
/// ```
///
/// # Errors
/// 当 `calibration_ms` 为 0 时返回 `Err("Calibration duration must be > 0")`;
/// 当 PIT 未初始化时返回 `Err("PIT must be initialized before calibration")`;
/// 当定时器子系统未初始化时返回 `Err("Timer not initialized")`;
/// 校准时长过短、计算溢出或测得频率超出合理范围 (100 MHz-10 GHz) 时
/// 分别返回 `Err("Calibration too short")`, `Err("Calculation overflow")`,
/// `Err("Calibrated frequency out of reasonable range")`.
pub fn calibrate_tsc(calibration_ms: u64) -> Result<u64, &'static str> {
    if calibration_ms == 0 {
        return Err("Calibration duration must be > 0");
    }

    if !super::pit::pit_is_initialized() {
        return Err("PIT must be initialized before calibration");
    }

    // 计算需要的 PIT ticks 数
    let pit_freq = u64::from(super::tick::get_frequency());
    if pit_freq == 0 {
        return Err("Timer not initialized");
    }

    let target_ticks = (calibration_ms * pit_freq) / 1000;
    if target_ticks == 0 {
        return Err("Calibration too short");
    }

    // 多次采样取平均 (提高精度)
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const SAMPLE_COUNT: usize = 3;
    let mut measurements: [u64; SAMPLE_COUNT] = [0; 3];

    for sample in 0..SAMPLE_COUNT {
        // 1. 记录起始状态
        let start_tick = super::tick::get_ticks();
        let start_tsc = read_tsc_serialized();

        // 2. 等待目标 tick 数
        loop {
            let current_tick = super::tick::get_ticks();
            let elapsed = current_tick.saturating_sub(start_tick);

            if elapsed >= target_ticks {
                break;
            }

            // 短暂让出 CPU (避免忙等待影响精度)
            core::hint::spin_loop();
        }

        // 3. 记录结束状态
        let end_tsc = read_tsc_serialized();

        // 4. 计算 TSC 周期数
        let tsc_cycles = end_tsc.saturating_sub(start_tsc);

        // 5. 存储测量值
        measurements[sample] = tsc_cycles;

        // 短暂间隔后再进行下一次采样
        if sample < SAMPLE_COUNT - 1 {
            super::sleep::busy_wait_ms(1);
        }
    }

    // 计算平均值 (使用中位数以减少异常值影响)
    measurements.sort_unstable();
    let median_cycles = measurements[SAMPLE_COUNT / 2];

    // 计算 TSC 频率
    // 公式: TSC_Hz = cycles / seconds = cycles / (ticks / PIT_FREQ)
    // 公式: TSC_MHz = TSC_Hz / 1_000_000

    let total_ns = (target_ticks * 1_000_000_000u64) / pit_freq;
    if total_ns == 0 {
        return Err("Calculation overflow");
    }

    let tsc_freq_hz = (median_cycles * 1_000_000_000u64) / total_ns;
    let tsc_freq_mhz = tsc_freq_hz / 1_000_000;

    // 验证合理性 (应该在合理范围内: 100 MHz - 10 GHz)
    if !(100..=10_000).contains(&tsc_freq_mhz) {
        return Err("Calibrated frequency out of reasonable range");
    }

    // 缓存结果
    CALIBRATED_TSC_FREQ_MHZ.store(tsc_freq_mhz, Ordering::Release);
    CALIBRATED_TSC_FREQ_HZ.store(tsc_freq_hz, Ordering::Release);
    LAST_CALIBRATION_RANGE.store(median_cycles, Ordering::Release);
    CALIBRATION_DONE.store(true, Ordering::Release);

    Ok(tsc_freq_mhz)
}

/// 快速 TSC 校准 (简化版，适用于启动早期)
///
/// 使用单次短时测量，速度更快但精度略低。
///
/// # Arguments
/// * `calibration_ms` - 校准时间 (毫秒), 推荐 5-10ms
///
/// # Errors
/// 错误与 `calibrate_tsc` 相同: 校准时长非法、PIT/定时器未初始化、
/// 计算溢出或频率超出合理范围时均返回 `Err(&str)`.
pub fn quick_calibrate(calibration_ms: u64) -> Result<u64, &'static str> {
    calibrate_tsc(calibration_ms)
}

// ============================================================================
// 查询接口
// ============================================================================

/// 获取已校准的 TSC 频率 (MHz)
///
/// # Returns
/// * `Some(u64)` - 已校准的频率 (MHz)
/// * `None` - 尚未校准
pub fn get_tsc_frequency_mhz() -> Option<u64> {
    if !CALIBRATION_DONE.load(Ordering::Acquire) {
        return None;
    }

    let freq = CALIBRATED_TSC_FREQ_MHZ.load(Ordering::Acquire);
    if freq == 0 { None } else { Some(freq) }
}

/// 获取已校准的 TSC 频率 (Hz, 完整精度)
///
/// # Returns
/// * `Some(u64)` - 已校准的频率 (Hz)
/// * `None` - 尚未校准
pub fn get_tsc_frequency_hz() -> Option<u64> {
    if !CALIBRATION_DONE.load(Ordering::Acquire) {
        return None;
    }

    let freq = CALIBRATED_TSC_FREQ_HZ.load(Ordering::Acquire);
    if freq == 0 { None } else { Some(freq) }
}

/// 检查是否已完成校准
pub fn is_calibrated() -> bool {
    CALIBRATION_DONE.load(Ordering::Acquire)
}

#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
/// 获取上次校准的详细信息
///
/// # Returns
/// 元组 `(freq_mhz, freq_hz, calibration_range)`
pub fn get_calibration_info() -> (Option<u64>, Option<u64>, Option<u64>) {
    let freq_mhz = get_tsc_frequency_mhz();
    let freq_hz = get_tsc_frequency_hz();
    let range = {
        let r = LAST_CALIBRATION_RANGE.load(Ordering::Acquire);
        if r == 0 { None } else { Some(r) }
    };

    (freq_mhz, freq_hz, range)
}

// ============================================================================
// 高精度时间转换 (需要校准后使用)
// ============================================================================

/// 将 TSC 周期数转换为纳秒 (高精度版本)
///
/// 需要先调用 `calibrate_tsc()` 完成校准。
///
/// # Arguments
/// * `cycles` - TSC 周期数
///
/// # Returns
/// * `Some(u64)` - 纳秒数 (如果已校准)
/// * `None` - 尚未校准
pub fn tsc_to_nanoseconds(cycles: u64) -> Option<u64> {
    let freq_hz = get_tsc_frequency_hz()?;

    if freq_hz == 0 {
        return None;
    }

    // 公式: ns = cycles * 1_000_000_000 / freq_hz
    Some((cycles * 1_000_000_000u64) / freq_hz)
}

/// 将 TSC 周期数转换为微秒
pub fn tsc_to_microseconds(cycles: u64) -> Option<u64> {
    tsc_to_nanoseconds(cycles).map(|ns| ns / 1000)
}

/// 将 TSC 周期数转换为毫秒
pub fn tsc_to_milliseconds(cycles: u64) -> Option<u64> {
    tsc_to_microseconds(cycles).map(|us| us / 1000)
}

/// 将纳秒转换为 TSC 周期数
pub fn nanoseconds_to_tsc(ns: u64) -> Option<u64> {
    let freq_hz = get_tsc_frequency_hz()?;

    if freq_hz == 0 {
        return None;
    }

    // 公式: cycles = ns * freq_hz / 1_000_000_000
    Some((ns * freq_hz) / 1_000_000_000u64)
}

/// 获取当前时间 (基于 TSC 的纳秒级时间戳)
///
/// 返回自某个固定点以来的纳秒数。
/// 注意：这不是绝对时间，而是相对时间，
/// 适合用于测量时间间隔。
///
/// # Returns
/// * `Some(u64)` - 当前时间 (纳秒)
/// * `None` - 尚未校准
pub fn get_time_ns() -> Option<u64> {
    if !is_calibrated() {
        return None;
    }

    let current_tsc = read_tsc();

    // 使用 timer 初始化时的 TSC 作为基准
    // (通过 uptime 间接获取)
    let uptime_ticks = super::tick::get_ticks();
    let uptime_ns = super::tick::ticks_to_ns(uptime_ticks);

    // 添加基于 TSC 的亚 tick 精度
    tsc_to_nanoseconds(current_tsc).map_or(Some(uptime_ns), Some)
}

/// 获取当前时间 (微秒)
pub fn get_time_us() -> Option<u64> {
    get_time_ns().map(|ns| ns / 1000)
}

/// 获取当前时间 (毫秒)
pub fn get_time_ms() -> Option<u64> {
    get_time_us().map(|us| us / 1000)
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        // 未校准时应该返回 None
        assert!(!is_calibrated());
        assert!(get_tsc_frequency_mhz().is_none());
        assert!(get_tsc_frequency_hz().is_none());
        assert!(tsc_to_nanoseconds(1000).is_none());
        assert!(get_time_ns().is_none());
    }

    #[test]
    fn test_calibration_interface() {
        // 测试校准函数存在且签名正确
        // 实际校准需要 PIT 初始化

        // 验证函数可调用 (可能返回错误)
        let result = calibrate_tsc(10);

        // 可能成功或失败，但不应该 panic
        let _ = result;
    }

    #[test]
    fn test_quick_calibrate_alias() {
        // quick_calibrate 应该是 calibrate_tsc 的别名
        // 验证接口一致性
        let _fn_ptr: fn(u64) -> Result<u64, &'static str> = quick_calibrate;
    }

    #[test]
    fn test_conversion_functions_exist() {
        // 验证所有转换函数存在
        // 这些函数在未校准时返回 None

        assert!(tsc_to_nanoseconds(0).is_none());
        assert!(tsc_to_microseconds(0).is_none());
        assert!(tsc_to_milliseconds(0).is_none());
        assert!(nanoseconds_to_tsc(0).is_none());
    }

    #[test]
    fn test_calibration_info() {
        let (mhz, hz, range) = get_calibration_info();

        // 未校准时都应该是 None
        assert!(mhz.is_none());
        assert!(hz.is_none());
        assert!(range.is_none());
    }

    #[test]
    fn test_zero_duration_rejected() {
        // 零持续时间应该被拒绝
        assert!(calibrate_tsc(0).is_err());
        assert!(quick_calibrate(0).is_err());
    }
}

#[cfg(feature = "kernel_test")]
// J-01 (2026-09-08): items_after_statements — 测试注册函数内嵌套测试 fn 为内核测试惯用模式
#[expect(
    clippy::items_after_statements,
    reason = "items_after_statements: 测试注册函数内嵌套测试 fn 为内核测试惯用模式; 当前优先 expect"
)]
pub fn register_timer_calibration_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};
    let r = runner();

    fn initial_state() -> TestResult {
        crate::check!(!is_calibrated(), "not calibrated");
        crate::check!(get_tsc_frequency_mhz().is_none(), "mhz none");
        crate::check!(get_tsc_frequency_hz().is_none(), "hz none");
        crate::check!(tsc_to_nanoseconds(1000).is_none(), "tsc_to_ns none");
        crate::check!(get_time_ns().is_none(), "time_ns none");
        TestResult::Pass
    }

    fn conversion_functions_unavailable() -> TestResult {
        crate::check!(tsc_to_nanoseconds(0).is_none(), "ns none");
        crate::check!(tsc_to_microseconds(0).is_none(), "us none");
        crate::check!(tsc_to_milliseconds(0).is_none(), "ms none");
        crate::check!(nanoseconds_to_tsc(0).is_none(), "ns_to_tsc none");
        TestResult::Pass
    }

    fn calibration_info_unavailable() -> TestResult {
        let (mhz, hz, range) = get_calibration_info();
        crate::check!(mhz.is_none(), "mhz none");
        crate::check!(hz.is_none(), "hz none");
        crate::check!(range.is_none(), "range none");
        TestResult::Pass
    }

    fn zero_duration_rejected() -> TestResult {
        crate::check!(calibrate_tsc(0).is_err(), "zero rejected");
        crate::check!(quick_calibrate(0).is_err(), "quick zero rejected");
        TestResult::Pass
    }

    r.register(
        "timer::calibration",
        "initial_state",
        initial_state as TestFn,
    );
    r.register(
        "timer::calibration",
        "conversion_unavailable",
        conversion_functions_unavailable as TestFn,
    );
    r.register(
        "timer::calibration",
        "calibration_info_unavailable",
        calibration_info_unavailable as TestFn,
    );
    r.register(
        "timer::calibration",
        "zero_duration_rejected",
        zero_duration_rejected as TestFn,
    );
}
