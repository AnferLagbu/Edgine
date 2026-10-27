//! 硬件自由计数器频率测定与时基建立
//!
//! 内核只有**一个时间源**: `timer::tick` 的锚点 + 架构自由计数器. 本模块负责
//! 回答"那个计数器每秒跳多少下" —— 它是频率的**唯一写入点**, 其余模块只读
//! (`hw_counter_frequency_hz()`), 不再各存一份估计值.
//!
//! ## 频率来源 (按优先级)
//!
//! | 来源 | 架构 | 性质 |
//! |---|---|---|
//! | CPUID Leaf 0x15 标称值 | x86_64 | CPU 直接报告, 无需等待, 最精确 |
//! | PIT 通道 0 实测 | x86_64 | 以 1.193182 MHz 为参考时钟量窗口 |
//! | `CNTFRQ_EL0` | aarch64 | ARM Generic Timer 实现定义频率 |
//!
//! 三者均不可得 ⇒ 不登记频率、不建硬件时基 (`tick::timebase_is_hardware()`
//! 保持 false), tick 读数留在中断计数视图。**宁可没有高精度时基, 也不写死一个
//! 猜测值** —— 旧实现按厂商硬编码 2.5/3.0/1.0 GHz, 与真值可差 1.4× (旧记录见
//! `docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-3/P-7).
//!
//! ## 为什么参考时钟是 PIT 而不是 tick 计数
//!
//! ```text
//! 频率 = 计数器增量 / 时间窗口
//! ```
//!
//! 若时间窗口由"每次 tick 中断 +1"的计数给出, 而那个计数又按本模块测得的频率
//! 派生, 即构成循环论证 (读数会被自己的输出锚定); 且它要求中断已使能 —— 启动
//! 阶段 IF 尚未打开时会永久自旋. 现直接读 PIT 倒计数器的回绕量: 参考值由硬件
//! 给出, 与中断状态无关, 启动早期即可测定.
//!
//! # Performance
//! 测定只在启动时跑一次 (三个短窗口样本), 结果登记后不再重复.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(target_arch = "x86_64")]
use crate::privileged::cpu::read_tsc_serialized;

// ============================================================================
// 全局状态
// ============================================================================

/// 已登记的硬件自由计数器频率 (Hz, 完整精度)
static COUNTER_FREQ_HZ: AtomicU64 = AtomicU64::new(0);

/// 已登记的硬件自由计数器频率 (MHz, 截断值, 供只关心量级的展示方使用)
static COUNTER_FREQ_MHZ: AtomicU64 = AtomicU64::new(0);

/// 频率是否已登记
static FREQUENCY_RECORDED: AtomicBool = AtomicBool::new(false);

/// 上次测定所用的 PIT 参考窗口 (周期数, 诊断用)
static LAST_REFERENCE_CYCLES: AtomicU64 = AtomicU64::new(0);

/// 硬件计数器频率的合理性下限 (Hz)
///
/// 下限取 1 MHz 而非 100 MHz: ARM Generic Timer 常见 24 MHz / QEMU virt 默认
/// 62.5 MHz, 把它们当"离谱"拦掉会让 aarch64 永远没有硬件时基.
const COUNTER_HZ_MIN: u64 = 1_000_000;

/// 硬件计数器频率的合理性上限 (Hz)
const COUNTER_HZ_MAX: u64 = 10_000_000_000;

/// 默认测定窗口 (毫秒)
///
/// 单样本 10 ms × 3 样本取中位数: 窗口再短则 PIT 周期数太少 (1.193182 MHz 下
/// 10 ms = 11932 个参考周期, 相对误差量级 1e-4), 再长则拖慢启动.
const DEFAULT_CALIBRATION_WINDOW_MS: u64 = 10;

/// 每样本轮询 PIT 的看门狗上限 (次)
///
/// 正常窗口 (10 ms) 只需万次量级轮询; 取 2 亿次作上限 (~数秒), 远超任何合理
/// 机器. 触发即判定参考时钟不推进 (硬件异常或虚拟化层未实现 latch), 直接报错
/// 而不是无限自旋把启动卡死.
#[cfg(target_arch = "x86_64")]
const PIT_WAIT_GUARD: u64 = 200_000_000;

// ============================================================================
// 时基来源
// ============================================================================

/// 硬件计数器频率的来源
///
/// 由 [`establish_boot_timebase`] 返回, 供启动日志标明时基到底从哪里来
/// (出问题时第一个要看的就是这一行).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimebaseSource {
    /// CPUID Leaf 0x15 报告的标称频率 (`x86_64`)
    CpuidNominal,
    /// 以 PIT 通道 0 为参考时钟实测得到 (`x86_64`)
    PitMeasured,
    /// ARM Generic Timer `CNTFRQ_EL0` 实现定义频率 (aarch64)
    GenericTimer,
}

impl TimebaseSource {
    /// 人类可读标签 (启动日志用)
    pub const fn label(self) -> &'static str {
        match self {
            Self::CpuidNominal => "CPUID 15H",
            Self::PitMeasured => "PIT measured",
            Self::GenericTimer => "CNTFRQ_EL0",
        }
    }
}

// ============================================================================
// 频率登记 (唯一写入点)
// ============================================================================

/// 登记硬件计数器频率 (Hz) — 全内核唯一写入点
///
/// 登记前过合理性范围: 该值一旦落定就成为 tick / hrtimer / 网络时钟的换算依据,
/// 离谱值会把整个时基带偏 (CPUID 字段与 PIT 实测都可能因虚拟化层而无意义).
///
/// # Errors
/// 频率不在 [`COUNTER_HZ_MIN`], [`COUNTER_HZ_MAX`] 区间内时返回 `Err`.
fn record_counter_hz(hz: u64) -> Result<u64, &'static str> {
    if !(COUNTER_HZ_MIN..=COUNTER_HZ_MAX).contains(&hz) {
        return Err("counter frequency outside plausible range");
    }

    COUNTER_FREQ_HZ.store(hz, Ordering::Release);
    COUNTER_FREQ_MHZ.store(hz / 1_000_000, Ordering::Relaxed);
    FREQUENCY_RECORDED.store(true, Ordering::Release);
    Ok(hz)
}

// ============================================================================
// 频率测定
// ============================================================================

/// 测定硬件自由计数器频率 (Hz) 并登记
///
/// - `x86_64`: 以 PIT 通道 0 (1.193182 MHz) 为参考时钟, 取
///   `DEFAULT_CALIBRATION_WINDOW_MS` 的 `window_ms` 个三个样本的中位数.
///   参考量由硬件倒计数器给出, **不依赖定时器中断**, 因此启动早期 (IF 未开)
///   也能测定.
/// - aarch64: 通用定时器频率由 `CNTFRQ_EL0` 直接读出, 不需实测.
///
/// # Arguments
/// * `window_ms` - 单样本窗口长度 (毫秒), 必须 > 0
///
/// # Errors
/// 窗口非法 / PIT 未初始化 / 参考计数器不推进 / 测得值超出合理性范围时返回 `Err`.
pub fn calibrate_counter_hz(window_ms: u64) -> Result<u64, &'static str> {
    if window_ms == 0 {
        return Err("Calibration window must be > 0");
    }

    #[cfg(target_arch = "x86_64")]
    {
        // 样本数: 声明置于块首, 避免出现"语句之后才有 item"
        const SAMPLE_COUNT: usize = 3;

        if !super::pit::pit_is_initialized() {
            return Err("PIT must be initialized before calibration");
        }

        let divisor = u64::from(super::pit::pit_current_divisor());
        if divisor == 0 {
            return Err("PIT divisor not configured");
        }

        // 单样本目标窗口对应的 PIT 周期数
        let target_cycles = window_ms.saturating_mul(super::pit::PIT_BASE_FREQUENCY) / 1_000;
        if target_cycles == 0 {
            return Err("Calibration window too short");
        }

        let mut samples: [u64; SAMPLE_COUNT] = [0; SAMPLE_COUNT];
        for sample in &mut samples {
            *sample = measure_one_sample_hz(target_cycles, divisor)?;
        }
        // 中位数: 抑制偶发干扰 (SMI / 虚拟机被偷走的时片) 带来的单样本偏差
        samples.sort_unstable();
        let hz = samples[SAMPLE_COUNT / 2];
        record_counter_hz(hz)
    }

    #[cfg(target_arch = "aarch64")]
    {
        // 通用定时器频率是实现定义值, 直接读即可; `window_ms` 在此不使用.
        let hz = crate::privileged::arch::aarch64::timer::read_frequency();
        record_counter_hz(hz).map_err(|_| "CNTFRQ_EL0 value outside plausible range")
    }
}

/// 单次 PIT 参考测定: 跑到 PIT 走过 `target_cycles` 个周期, 返回计数器频率
///
/// # Errors
/// PIT 计数读不出 / 参考计数器不推进 / 两端的计数器读数退化 (增量为 0) 时返回 `Err`.
#[cfg(target_arch = "x86_64")]
fn measure_one_sample_hz(target_cycles: u64, divisor: u64) -> Result<u64, &'static str> {
    // PIT 倒计数器从 divisor 递减到 1 后重装; 相邻两次轮询间隔远小于一个周期
    // (inb 量级为百纳秒, 周期为微秒级), 故此处假设最多跳过 1 次回绕.
    let mut prev = u64::from(super::pit::pit_read_count().ok_or("Failed to read PIT count")?);
    let start = read_tsc_serialized();
    let mut reference_cycles: u64 = 0;
    let mut guard: u64 = 0;

    loop {
        let current = u64::from(super::pit::pit_read_count().ok_or("Failed to read PIT count")?);
        reference_cycles += if current <= prev {
            prev - current
        } else {
            // 发生一次回绕: prev 跑到重装值再降到 current
            prev + divisor - current
        };
        prev = current;

        if reference_cycles >= target_cycles {
            break;
        }
        guard += 1;
        if guard > PIT_WAIT_GUARD {
            return Err("PIT reference counter not advancing");
        }
        core::hint::spin_loop();
    }

    let elapsed = read_tsc_serialized().saturating_sub(start);
    if elapsed == 0 {
        return Err("Counter did not advance during calibration window");
    }
    LAST_REFERENCE_CYCLES.store(reference_cycles, Ordering::Relaxed);

    Ok(super::tick::mul_div_saturating(
        elapsed,
        super::pit::PIT_BASE_FREQUENCY,
        reference_cycles,
    ))
}

/// 启动期建立单一时基: 取频率 → 登记 → 向 `tick` 建锚
///
/// 由 `kernel_init` 在 `timer_init` 之后调用 (见 `src/kernel/lib.rs` 步骤 7.5).
/// 失败时时基保持中断计数视图, tickless 会被 `tick::timebase_is_hardware()` 拦住.
///
/// # Returns
/// * `Ok((hz, 来源))` - 已登记的计数器频率与其来源
///
/// # Errors
/// 所有可用频率来源均不可得, 或计数器分辨率低于一个 tick 时返回 `Err`.
pub fn establish_boot_timebase() -> Result<(u64, TimebaseSource), &'static str> {
    #[cfg(target_arch = "x86_64")]
    {
        // 优先级 1: CPU 自己报的标称频率 — 不需等待, 也不受虚拟化计时干扰
        if let Some(hz) = crate::privileged::cpu::tsc_nominal_hz() {
            if record_counter_hz(hz).is_ok() {
                super::tick::establish_timebase(hz)?;
                return Ok((hz, TimebaseSource::CpuidNominal));
            }
        }

        // 优先级 2: 以 PIT 为参考时钟实测
        let hz = calibrate_counter_hz(DEFAULT_CALIBRATION_WINDOW_MS)?;
        super::tick::establish_timebase(hz)?;
        Ok((hz, TimebaseSource::PitMeasured))
    }

    #[cfg(target_arch = "aarch64")]
    {
        let hz = calibrate_counter_hz(DEFAULT_CALIBRATION_WINDOW_MS)?;
        super::tick::establish_timebase(hz)?;
        Ok((hz, TimebaseSource::GenericTimer))
    }
}

// ============================================================================
// 查询接口
// ============================================================================

/// 获取已登记的硬件计数器频率 (MHz)
///
/// # Returns
/// * `Some(u64)` - 已登记 (截断值)
/// * `None` - 尚未登记
pub fn hw_counter_frequency_mhz() -> Option<u64> {
    if !is_calibrated() {
        return None;
    }

    let freq = COUNTER_FREQ_MHZ.load(Ordering::Acquire);
    if freq == 0 { None } else { Some(freq) }
}

/// 获取已登记的硬件计数器频率 (Hz, 完整精度)
///
/// 全内核时基频率的唯一读取入口: `x86_64` 为 TSC, aarch64 为 `CNTPCT_EL0`
/// 计数器频率 —— 两者只有一个含义: “`tick::counter_read()` 每秒跳多少下”.
///
/// # Returns
/// * `Some(u64)` - 已登记
/// * `None` - 尚未登记
pub fn hw_counter_frequency_hz() -> Option<u64> {
    if !is_calibrated() {
        return None;
    }

    let freq = COUNTER_FREQ_HZ.load(Ordering::Acquire);
    if freq == 0 { None } else { Some(freq) }
}

/// 检查频率是否已登记
pub fn is_calibrated() -> bool {
    FREQUENCY_RECORDED.load(Ordering::Acquire)
}

/// 硬件延时折算用的计数器频率 (Hz)
///
/// 与 [`hw_counter_frequency_hz`] 的区别只在**未登记时**: 本入口返回合理性上限.
/// 频率未知时无法把纳秒折成周期数, 按上限折算算出的周期数只会偏多 —— 硬件忙等待
/// 的安全方向是"不少于请求时长" (旧实现 `ns * 2` 假设 2 GHz, 真频更高时**少等**,
/// 使驱动依赖的时序下限失效).
pub fn counter_hz_for_wait() -> u64 {
    hw_counter_frequency_hz().unwrap_or(COUNTER_HZ_MAX)
}

/// 获取上次登记的详细信息
///
/// # Returns
/// 元组 `(freq_mhz, freq_hz, 上次测定用的参考周期数)`
pub fn get_calibration_info() -> (Option<u64>, Option<u64>, Option<u64>) {
    let range = {
        let r = LAST_REFERENCE_CYCLES.load(Ordering::Acquire);
        if r == 0 { None } else { Some(r) }
    };

    (hw_counter_frequency_mhz(), hw_counter_frequency_hz(), range)
}

// ============================================================================
// 高精度时间转换 (需要频率已登记)
// ============================================================================

/// 将计数器周期数转换为纳秒
///
/// 需先由 [`establish_boot_timebase`] 登记频率.
///
/// # Arguments
/// * `cycles` - 计数器周期数
///
/// # Returns
/// * `Some(u64)` - 纳秒数 (如果频率已登记)
/// * `None` - 尚未登记
pub fn tsc_to_nanoseconds(cycles: u64) -> Option<u64> {
    let freq_hz = hw_counter_frequency_hz()?;
    Some(super::tick::mul_div_saturating(
        cycles,
        1_000_000_000,
        freq_hz,
    ))
}

/// 将 TSC 周期数转换为微秒
pub fn tsc_to_microseconds(cycles: u64) -> Option<u64> {
    tsc_to_nanoseconds(cycles).map(|ns| ns / 1000)
}

/// 将 TSC 周期数转换为毫秒
pub fn tsc_to_milliseconds(cycles: u64) -> Option<u64> {
    tsc_to_microseconds(cycles).map(|us| us / 1000)
}

/// 将纳秒转换为计数器周期数
pub fn nanoseconds_to_tsc(ns: u64) -> Option<u64> {
    let freq_hz = hw_counter_frequency_hz()?;
    Some(super::tick::mul_div_saturating(ns, freq_hz, 1_000_000_000))
}

/// 获取当前时间 (自启动以来的纳秒, 单一时基派生)
///
/// 直接取 `tick` 的纳秒视图 (与 `get_ticks()` / hrtimer / 网络时钟同一锚点),
/// 不另开一条换算路径.
///
/// # Returns
/// * `Some(u64)` - 自启动以来的纳秒数
/// * `None` - 频率尚未登记
pub fn get_time_ns() -> Option<u64> {
    if !is_calibrated() {
        return None;
    }

    Some(super::tick::get_uptime_ns())
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
    use crate::privileged::timer::tick::FREQ_TEST_LOCK;

    /// 频率登记静态量的快照守卫: 进入"未登记"隔离态, Drop 时恢复.
    ///
    /// 登记值是进程级全局状态, 且 `sleep` 的忙等待折算会读它 ⇒ 并行 test runner
    /// 下必须与 `tick` / `sleep` 的用例互斥 (共用 `FREQ_TEST_LOCK`) 并自动还原.
    struct RecordedGuard {
        hz: u64,
        mhz: u64,
        recorded: bool,
        reference: u64,
    }

    impl RecordedGuard {
        fn isolated() -> Self {
            let guard = Self {
                hz: COUNTER_FREQ_HZ.load(Ordering::Relaxed),
                mhz: COUNTER_FREQ_MHZ.load(Ordering::Relaxed),
                recorded: FREQUENCY_RECORDED.load(Ordering::Relaxed),
                reference: LAST_REFERENCE_CYCLES.load(Ordering::Relaxed),
            };
            COUNTER_FREQ_HZ.store(0, Ordering::Relaxed);
            COUNTER_FREQ_MHZ.store(0, Ordering::Relaxed);
            FREQUENCY_RECORDED.store(false, Ordering::Relaxed);
            guard
        }
    }

    impl Drop for RecordedGuard {
        fn drop(&mut self) {
            // Drop 路径恢复: 断言 panic 也不给其它用例留脏频率.
            COUNTER_FREQ_HZ.store(self.hz, Ordering::Relaxed);
            COUNTER_FREQ_MHZ.store(self.mhz, Ordering::Relaxed);
            FREQUENCY_RECORDED.store(self.recorded, Ordering::Relaxed);
            LAST_REFERENCE_CYCLES.store(self.reference, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_initial_state() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = RecordedGuard::isolated();
        // 尚未登记时全部读数应报 None (而不是一个编造的频率)
        assert!(!is_calibrated());
        assert!(hw_counter_frequency_mhz().is_none());
        assert!(hw_counter_frequency_hz().is_none());
        assert!(tsc_to_nanoseconds(1000).is_none());
        assert!(get_time_ns().is_none());
    }

    #[test]
    fn test_calibrate_requires_initialized_pit() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = RecordedGuard::isolated();
        // host 下 PIT 未初始化 ⇒ 必须走错误出口, 不得 panic 也不得编造频率
        assert!(calibrate_counter_hz(10).is_err());
        assert!(!is_calibrated(), "失败的测定不得登记频率");
    }

    #[test]
    fn test_zero_window_rejected() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = RecordedGuard::isolated();
        // 零窗口应该被拒绝 (先于 PIT 检查, 两架构行为一致)
        assert!(calibrate_counter_hz(0).is_err());
    }

    #[test]
    fn test_record_enforces_plausible_range() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = RecordedGuard::isolated();
        // 登记是全内核唯一频率写入点: 范围外的值 (0 / 低于 1 MHz / 高于 10 GHz)
        // 必须被拒, 否则一个编造值会带偏整个时基 (旧实现的厂商硬编码即此族).
        assert!(record_counter_hz(0).is_err());
        assert!(record_counter_hz(COUNTER_HZ_MIN - 1).is_err());
        assert!(record_counter_hz(COUNTER_HZ_MAX + 1).is_err());
        assert!(!is_calibrated(), "越界登记不得置位已登记标志");

        // 范围内可以登记 (含 ARM Generic Timer 的 24 / 62.5 MHz)
        assert_eq!(record_counter_hz(24_000_000), Ok(24_000_000));
        assert!(is_calibrated());
        assert_eq!(hw_counter_frequency_hz(), Some(24_000_000));
        assert_eq!(hw_counter_frequency_mhz(), Some(24));
        assert_eq!(tsc_to_nanoseconds(24), Some(1_000));
        // 反向换算与正向互为逆运算: 24 周期 @ 24 MHz = 1_000 ns
        assert_eq!(nanoseconds_to_tsc(1_000), Some(24));
    }

    /// E-I: 忙等待折算入口 — 已登记取真值, 未登记取合理性上限 (只会多等不会少等).
    #[test]
    fn test_counter_hz_for_wait_never_fabricates_low() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = RecordedGuard::isolated();

        assert_eq!(
            counter_hz_for_wait(),
            COUNTER_HZ_MAX,
            "未登记时必须按上限折算, 不得给出偏小的周期数"
        );
        assert!(record_counter_hz(2_000_000_000).is_ok());
        assert_eq!(
            counter_hz_for_wait(),
            2_000_000_000,
            "已登记后必须取登记值 (不得继续用兼容值)"
        );
    }

    #[test]
    fn test_conversion_functions_exist() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = RecordedGuard::isolated();
        // 验证所有转换函数存在 (未登记时返回 None)
        assert!(tsc_to_microseconds(0).is_none());
        assert!(tsc_to_milliseconds(0).is_none());
    }

    #[test]
    fn test_calibration_info_shape() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = RecordedGuard::isolated();
        let (mhz, hz, range) = get_calibration_info();

        // 元组三项要么全 None (未登记), 要么频率两项均为 Some
        assert_eq!(mhz.is_some(), hz.is_some());
        assert_eq!(hz.is_some(), is_calibrated());
        let _ = range;
    }

    /// E-I 反循环论证防线: 测定路径不得拿 `tick` 的计数读数当时间窗口
    /// (那个读数由本模块测得的频率派生; 旧实现还依赖中断已使能).
    #[test]
    fn test_measurement_does_not_use_tick_count() {
        let src = include_str!("calibration.rs");
        let flat: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        // needle 运行时拼接: 源码文本里不能出现这个字串, 否则本用例自身会误报
        let needle: String = ["super::tick::get_", "ticks()"].concat();
        assert!(
            !flat.contains(&needle),
            "测定路径重新以 tick 计数作参考时钟 (循环论证 / 启动早期会死自旋)"
        );
    }
}

/// 注册硬件计数器频率测定相关测试用例 (kernel_test)
#[cfg(feature = "kernel_test")]
pub fn register_timer_calibration_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};

    /// E-I 接线验证: 启动编排已登记频率, 且读数可用
    fn boot_timebase_recorded() -> TestResult {
        crate::check!(is_calibrated(), "counter frequency recorded at boot");
        // unwrap_or(0): 未登记时让下一条 check 以同一语义失败 (内核测试禁 unwrap)
        let hz = hw_counter_frequency_hz().unwrap_or(0);
        crate::check!(hz >= 1_000_000, "hz within plausible range");
        crate::check!(get_time_ns().is_some(), "time_ns available");
        TestResult::Pass
    }

    /// E-I 单一时基: 转换函数在登记后给出有限值, 且纳秒读数单调
    fn conversions_after_record() -> TestResult {
        let hz = hw_counter_frequency_hz().unwrap_or(0);
        crate::check!(hz > 0, "frequency recorded before conversion");
        // 1 秒的周期数折算回 1e9 纳秒 (截断容差 1 ns)
        let ns = tsc_to_nanoseconds(hz).unwrap_or(0);
        crate::check!(ns.abs_diff(1_000_000_000) <= 1, "roundtrip within 1ns");
        crate::check!(get_time_ns().is_some(), "time_ns available");
        let t1 = get_time_ns().unwrap_or(0);
        let t2 = get_time_ns().unwrap_or(t1);
        crate::check!(t2 >= t1, "time_ns monotonic");
        TestResult::Pass
    }

    /// 零窗口必须在任何硬件访问前被拒 (不重测、不覆写已登记频率)
    fn zero_window_rejected() -> TestResult {
        crate::check!(calibrate_counter_hz(0).is_err(), "zero window rejected");
        crate::check!(is_calibrated(), "rejected call did not clear registration");
        TestResult::Pass
    }

    let r = runner();
    r.register(
        "timer::calibration",
        "boot_timebase_recorded",
        boot_timebase_recorded as TestFn,
    );
    r.register(
        "timer::calibration",
        "conversions_after_record",
        conversions_after_record as TestFn,
    );
    r.register(
        "timer::calibration",
        "zero_window_rejected",
        zero_window_rejected as TestFn,
    );
}
