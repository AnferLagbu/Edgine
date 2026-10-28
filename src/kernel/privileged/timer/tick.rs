//! 全局 Tick 计数器和时间管理
//!
//! 提供系统级时间追踪功能：
//! - **Tick 计数器**: 自启动以来的中断次数
//! - **时间转换**: Tick ↔ 毫秒/微秒/纳秒
//! - **运行时间**: 系统 uptime 统计
//!
//! ## 设计理念
//!
//! **单一时基**: tick 序号与纳秒读数取自同一个锚点 + 同一个硬件自由计数器
//! (x86_64: TSC; aarch64: CNTPCT_EL0), 不再各自取数 — 历史形态下 hrtimer 走裸
//! 计数器、tick 走中断计数, 一旦停 tick (tickless) 两者即背离.
//! 计数器频率由 `timer::calibration` 单点测定后登记; 测定失败时所有读数回落到
//! 中断计数视图, tickless 由 `timebase_is_hardware()` 硬前置拦住 (停 tick 会让
//! 回落视图冻结).
//!
//! ```text
//! Layer 2: 纳秒 (get_uptime_ns) ← hrtimer / 网络时钟 / tickless 时间补偿
//! Layer 1: tick   (get_ticks)   ← 调度、超时、uptime、日志时间戳
//! 锚点以下: 中断计数 TICK_IRQ_COUNT ← 校准参考量 + 时基未建立时的回落视图
//! ```
//!
//! # Safety
//! 所有公共接口都是线程安全的，使用原子操作。

use super::pit::DEFAULT_INTERRUPT_FREQ_HZ;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

// ============================================================================
// 全局状态
// ============================================================================

/// 自启动以来的定时器中断次数 (每次 IRQ0 +1)
///
/// 硬件时基建立后不再直接参与 `get_ticks()` 求值, 仅保留作校准参考量与观测计数.
static TICK_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

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

// ---- 硬件派生时基 (锚点 + 自由计数器) --------------------------------------

/// 建锚时刻的 tick 序号 (锚点以下的历史由中断计数确定)
static ANCHOR_TICK: AtomicU64 = AtomicU64::new(0);

/// 建锚时刻的纳秒数 (与 `ANCHOR_TICK` 同一时刻, 供纳秒视图起算)
static ANCHOR_NS: AtomicU64 = AtomicU64::new(0);

/// 建锚时刻的硬件自由计数器读数
static ANCHOR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 硬件自由计数器频率 (Hz); 0 = 时基仍由中断计数派生
static COUNTER_HZ: AtomicU64 = AtomicU64::new(0);

/// 一个 tick 对应的计数器增量 (整除截断; 0 = 硬件时基未建立)
///
/// 该值是 `get_ticks()` 的开关: 非 0 即表示时基已由硬件计数器派生.
static COUNTERS_PER_TICK: AtomicU64 = AtomicU64::new(0);

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

    // 4. 重置时基视图 (中断计数归零 + 锚点重建)
    //    此时 COUNTERS_PER_TICK 仍为 0 (校准在 timer_init 之后), 故 get_ticks()
    //    从 0 起按中断计数走; 锚点读数仅为万一重复 init 时保持一致性.
    TICK_IRQ_COUNT.store(0, Ordering::Release);
    ANCHOR_TICK.store(0, Ordering::Relaxed);
    ANCHOR_NS.store(0, Ordering::Relaxed);
    ANCHOR_COUNTER.store(counter_read(), Ordering::Relaxed);

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
///       避免遗忘导致 hrtimer 不被处理. 旧调用方 (`timer_tick_handler` /
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
    TICK_IRQ_COUNT.fetch_add(1, Ordering::AcqRel);

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
    super::time_sync::clock_adj_state().tick_adjust();
}

/// 安全的 64 位乘除: `(value * mul) / div`, 128 位中间值不溢出
///
/// 时基换算的共用底档 (tick ↔ 计数器 ↔ 纳秒), 由本模块单点持有而非各模块自拷一份
/// (旧形态: `hrtimer::mul_u64_div` 与本行同一算法重写了两遍).
///
/// `div == 0` 时返回 0 (调用方的未初始化防护); 仅在**最终结果**超 `u64::MAX`
/// 时饱和截断 (物理上不可达的防御).
#[inline]
pub(super) fn mul_div_saturating(value: u64, mul: u64, div: u64) -> u64 {
    if div == 0 {
        return 0;
    }
    let product = u128::from(value) * u128::from(mul);
    crate::privileged::lib::low_u64_from_u128((product / u128::from(div)).min(u128::from(u64::MAX)))
}

/// 读当前架构的硬件自由计数器 (不做序列化屏障)
///
/// x86_64 = TSC (`rdtsc`), aarch64 = `CNTPCT_EL0`; 两者都是复位后单调递增的
/// 自由跑计数器, 不依赖中断是否到达 —— 这是 tickless 期间时基仍能推进的根本原因.
#[inline]
fn counter_read() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        crate::privileged::cpu::read_tsc()
    }
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch!(timestamp())
    }
}

/// 建立硬件派生时基: 以当前时基视图为锚点, 此后读数由自由计数器推进
///
/// 调用方 (`timer::calibration::establish_boot_timebase`) 必须先取得可靠的
/// 计数器频率; 本函数只装配锚点, 不校验频率真伪.
///
/// 幂等且可重复调用: 再次建锚以当时的派生读数为新锚点, 读数不会回退.
///
/// # Arguments
/// * `counter_hz` - 硬件自由计数器频率 (Hz), 必须 > 0
///
/// # Returns
/// * `Ok(())` - 硬件时基已建立 (`timebase_is_hardware()` 转为 true)
///
/// # Errors
/// 计数器频率为 0 / tick 频率未配置 (`timer_init` 未先跑) / 计数器每 tick
/// 增量不足 1 (分辨率低于 tick) 时分别返回 `Err`.
///
/// # Note
/// 本函数不测定频率, 仅装配锚点; 失败时时基保持中断计数视图 (调用方须记日志).
pub fn establish_timebase(counter_hz: u64) -> Result<(), &'static str> {
    if counter_hz == 0 {
        return Err("hardware counter frequency is 0");
    }

    let tick_freq = u64::from(get_frequency());
    if tick_freq == 0 {
        return Err("tick frequency not configured (timer_init must run first)");
    }

    // 一个 tick 占多少个计数器增量; 整除截断使 tick 视图最多超前 1 tick,
    // 且不随时间累积 (每次都由锚点重新派生, 不是增量累加).
    let counters_per_tick = counter_hz / tick_freq;
    if counters_per_tick == 0 {
        return Err("hardware counter advances slower than one tick");
    }

    // 关中断装配锚点: 五个静态量必须属于同一时刻的快照, 否则并发到达的 IRQ
    // 会在中途给 TICK_IRQ_COUNT +1, 使锚点 tick 与锚点计数器读数错位.
    let flags = crate::privileged::sync::disable_interrupts();
    // 先读旧视图 (此时 COUNTERS_PER_TICK 可能为 0 ⇒ 中断计数, 也可能非 0
    // ⇒ 旧硬件时基的派生值), 再落锚, 最后才切换 COUNTERS_PER_TICK.
    // 两个视图均取“当时的读数”作锚: 重新建锚时 tick 与纳秒都保持连续,
    // 不会因整除截断而回跳 (纳秒若改由 ticks_to_ns(anchor_tick) 反推, 重锚
    // 就会丢掉不足一个 tick 的那段增量, 使纳秒读数回退).
    let anchor_tick = get_ticks();
    let anchor_ns = get_uptime_ns();
    let anchor_counter = counter_read();
    ANCHOR_TICK.store(anchor_tick, Ordering::Relaxed);
    ANCHOR_NS.store(anchor_ns, Ordering::Relaxed);
    ANCHOR_COUNTER.store(anchor_counter, Ordering::Relaxed);
    COUNTER_HZ.store(counter_hz, Ordering::Relaxed);
    COUNTERS_PER_TICK.store(counters_per_tick, Ordering::Release);
    crate::privileged::sync::restore_interrupts(&flags);

    Ok(())
}

/// 时基是否已由硬件自由计数器派生
///
/// tickless (停 tick) 的**硬前置**: 停 tick 后中断计数不再推进, 只有硬件派生
/// 时基才能保证 `get_ticks()` / uptime / hrtimer 继续前进.
#[inline]
pub fn timebase_is_hardware() -> bool {
    COUNTERS_PER_TICK.load(Ordering::Acquire) != 0
}

/// 一个 tick 占多少硬件计数器增量 (`0` = 硬件时基未建立)
///
/// tick 源 (clockevent) 装填周期与 tickless 换算截止期的唯一依据 —— 由本模块
/// 持有 (建锚时算出), 不在调用方重写 `counter_hz / tick_hz` (否则两处整除口径
/// 可各自漂移, 而 tick 到达率与簿记率脱钩正是 E-II-a 修掉的缺陷).
#[inline]
pub fn counters_per_tick() -> u64 {
    COUNTERS_PER_TICK.load(Ordering::Acquire)
}

/// 读硬件自由计数器当前值 (时基域)
///
/// 与 `get_ticks()` / `ANCHOR_COUNTER` 同一坐标系: tick 源的绝对截止期必须落在
/// 该域, 否则到期判断与时基读数不可比 (x86_64 = TSC, aarch64 = `CNTPCT_EL0`).
#[inline]
pub fn counter_now() -> u64 {
    counter_read()
}

/// 获取当前 tick 数
///
/// 硬件时基已建立时由自由计数器从锚点派生 (停 tick 期间仍推进, 且单调不回退);
/// 未建立时回落为定时器中断计数.
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
#[inline]
pub fn get_ticks() -> u64 {
    let per_tick = COUNTERS_PER_TICK.load(Ordering::Acquire);
    if per_tick == 0 {
        // 回落视图: 时基未建立 (校准未完成或失败) ⇒ 仍由中断计数给出 tick.
        return TICK_IRQ_COUNT.load(Ordering::Acquire);
    }

    let anchor_counter = ANCHOR_COUNTER.load(Ordering::Relaxed);
    // saturating_sub: 多核下计数器读数与锚点无因果关系, 若锚点由另一核稍后
    // 写入 (重复建锚的竞态窗口) 则本核读数可能小于锚点; 饱和至 0 即回锚点值,
    // 保证读数单调 (时基的第一契约).
    let delta = counter_read().saturating_sub(anchor_counter);
    ANCHOR_TICK.load(Ordering::Relaxed) + delta / per_tick
}

/// 获取当前定时器频率
///
/// # Returns
/// * `u32` - 当前频率 (Hz)
#[inline]
pub fn get_frequency() -> u32 {
    TIMER_FREQ_HZ.load(Ordering::Acquire)
}

/// 重置时基视图 (仅用于测试或特殊场景)
///
/// # Warning
/// 此操作会影响所有依赖时间的功能！
/// 硬件时基已建立时保留派生机制, 只把锚点搬到当前计数器读数并使 tick 归零
/// (否则中断计数归零而派生读数依旧, 两个视图会背离).
pub fn reset_ticks() {
    let flags = crate::privileged::sync::disable_interrupts();
    TICK_IRQ_COUNT.store(0, Ordering::Release);
    ANCHOR_TICK.store(0, Ordering::Relaxed);
    ANCHOR_NS.store(0, Ordering::Relaxed);
    ANCHOR_COUNTER.store(counter_read(), Ordering::Relaxed);
    crate::privileged::sync::restore_interrupts(&flags);
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

/// 获取系统运行时间 (纳秒)
///
/// 与 `get_ticks()` 同一锚点、同一计数器 (单一时基): 硬件时基建立后具备计数器
/// 粒度 (亚 tick), 且停 tick 期间仍推进; 未建立时退化为 tick 粒度.
///
/// hrtimer / 网络时钟 / tickless 时间补偿均以此为准.
#[inline]
pub fn get_uptime_ns() -> u64 {
    let per_tick = COUNTERS_PER_TICK.load(Ordering::Acquire);
    if per_tick == 0 {
        return ticks_to_ns(get_ticks());
    }

    let anchor_counter = ANCHOR_COUNTER.load(Ordering::Relaxed);
    let delta = counter_read().saturating_sub(anchor_counter);
    let anchor_ns = ANCHOR_NS.load(Ordering::Relaxed);
    // max(1): COUNTER_HZ 非 0 已由 establish_timebase 保证, 此处只为防未初始化除零.
    let hz = COUNTER_HZ.load(Ordering::Relaxed).max(1);
    anchor_ns.saturating_add(mul_div_saturating(delta, 1_000_000_000, hz))
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

/// 时基全局静态量的 host 测试互斥锁.
///
/// `tick` / `calibration` / `sleep` 三个子模块的用例都会读写时基全局状态
/// (频率登记 / 锚点 / 频率置 0 / 忙等待折算), 并行 test runner 下必须共用一把锁,
/// 否则偶发失败 (2026-09-24 UT-06 同源问题).
// P-71: `pub(in …)` 路径走不了 crate 内的 `privileged` 自别名 (E0578), 直接用真实模块路径.
#[cfg(test)]
pub(in crate::timer) static FREQ_TEST_LOCK: crate::privileged::sync::IrqSpinLock<()> =
    crate::privileged::sync::IrqSpinLock::new(());

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 时基静态量的快照守卫.
    ///
    /// 新用例需直接摆弄频率/锚点/开关量 (不走 `timer_init`, 避免 host 下碰 PIT I/O),
    /// 并行 test runner 下必须与频率相关用例互斥 ⇒ 均持 `FREQ_TEST_LOCK`.
    struct TimebaseGuard {
        freq: u32,
        irq: u64,
        anchor_tick: u64,
        anchor_ns: u64,
        anchor_counter: u64,
        counter_hz: u64,
        per_tick: u64,
    }

    impl TimebaseGuard {
        /// 快照当前全局时基状态, 并进入“频率 = `freq_hz`、硬件时基未建立”的隔离态.
        fn isolated(freq_hz: u32) -> Self {
            let guard = Self {
                freq: TIMER_FREQ_HZ.load(Ordering::Relaxed),
                irq: TICK_IRQ_COUNT.load(Ordering::Relaxed),
                anchor_tick: ANCHOR_TICK.load(Ordering::Relaxed),
                anchor_ns: ANCHOR_NS.load(Ordering::Relaxed),
                anchor_counter: ANCHOR_COUNTER.load(Ordering::Relaxed),
                counter_hz: COUNTER_HZ.load(Ordering::Relaxed),
                per_tick: COUNTERS_PER_TICK.load(Ordering::Relaxed),
            };
            TIMER_FREQ_HZ.store(freq_hz, Ordering::Relaxed);
            TICK_IRQ_COUNT.store(0, Ordering::Relaxed);
            ANCHOR_TICK.store(0, Ordering::Relaxed);
            ANCHOR_NS.store(0, Ordering::Relaxed);
            ANCHOR_COUNTER.store(0, Ordering::Relaxed);
            COUNTER_HZ.store(0, Ordering::Relaxed);
            COUNTERS_PER_TICK.store(0, Ordering::Relaxed);
            guard
        }
    }

    impl Drop for TimebaseGuard {
        fn drop(&mut self) {
            // Drop 路径上恢复: 断言 panic 也不给其它用例留脏全局状态.
            TIMER_FREQ_HZ.store(self.freq, Ordering::Relaxed);
            TICK_IRQ_COUNT.store(self.irq, Ordering::Relaxed);
            ANCHOR_TICK.store(self.anchor_tick, Ordering::Relaxed);
            ANCHOR_NS.store(self.anchor_ns, Ordering::Relaxed);
            ANCHOR_COUNTER.store(self.anchor_counter, Ordering::Relaxed);
            COUNTER_HZ.store(self.counter_hz, Ordering::Relaxed);
            COUNTERS_PER_TICK.store(self.per_tick, Ordering::Relaxed);
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

    /// E-I: 建锚入参必须合法 — 非法频率/零 tick 频率/低分辨率计数器均要拒绝
    /// (一旦接受将给出永远不变的 tick 视图, 比“没有硬件时基”更糟).
    #[test]
    fn test_establish_timebase_rejects_invalid_input() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = TimebaseGuard::isolated(1000);

        assert!(establish_timebase(0).is_err(), "计数器频率 0 必须拒绝");
        // 500 Hz 计数器 / 1000 Hz tick ⇒ 每 tick 增量不足 1, 派生读数会卡死
        assert!(
            establish_timebase(500).is_err(),
            "分辨率低于 tick 的计数器必须拒绝"
        );
        {
            let _zero = FreqZeroGuard::new();
            assert!(
                establish_timebase(1_000_000).is_err(),
                "tick 频率未配置 (timer_init 未先跑) 必须拒绝"
            );
        }
        assert!(!timebase_is_hardware(), "失败的建锚不得切换时基");
    }

    /// E-I: 时基开关与派生视图 — 未建立时走中断计数, 建立后以锚点为起点且单调.
    #[test]
    fn test_get_ticks_switches_to_derived_view() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = TimebaseGuard::isolated(1000);

        assert!(!timebase_is_hardware());
        TICK_IRQ_COUNT.store(7, Ordering::Relaxed);
        assert_eq!(get_ticks(), 7, "回落视图必须等于中断计数");

        assert!(establish_timebase(2_000_000_000).is_ok());
        assert!(timebase_is_hardware());

        let t1 = get_ticks();
        assert!(t1 >= 7, "建锚后读数不得回退到锚点以下");
        assert!(get_ticks() >= t1, "硬件时基读数必须单调");
        assert!(
            get_uptime_ns() >= 7_000_000,
            "纳秒视图与 tick 视图共用锚点 (7 tick @ 1kHz = 7ms)"
        );
    }

    /// E-I: 重复建锚 (重校准场景) 不回退读数 — 新锚点取当时派生值.
    #[test]
    fn test_reanchor_does_not_rewind_timebase() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = TimebaseGuard::isolated(1000);

        assert!(establish_timebase(1_000_000_000).is_ok());
        let before = get_ticks();
        let before_ns = get_uptime_ns();

        assert!(establish_timebase(3_000_000_000).is_ok());
        assert!(get_ticks() >= before, "重新建锚后 tick 读数不回退");
        assert!(get_uptime_ns() >= before_ns, "重新建锚后纳秒读数不回退");
    }

    /// E-I: 重置只搬锚点, 不拆掉硬件时基 (否则两个视图会背离).
    #[test]
    fn test_reset_ticks_keeps_hardware_timebase() {
        let _lock = FREQ_TEST_LOCK.lock();
        let _guard = TimebaseGuard::isolated(1000);

        assert!(establish_timebase(1_000_000_000).is_ok());
        reset_ticks();

        assert_eq!(TICK_IRQ_COUNT.load(Ordering::Relaxed), 0);
        assert_eq!(ANCHOR_TICK.load(Ordering::Relaxed), 0);
        assert!(timebase_is_hardware(), "重置不得拆掉已建立的硬件时基");
        assert!(
            get_ticks() < 1_000_000,
            "重置后读数应回到起点附近 (而不保留旧累计值)"
        );
    }

    /// E-I: 换算底档 — 128 位中间值不溢出, 除零与饱和路径均不 panic.
    #[test]
    fn test_mul_div_saturating() {
        // 1e14 个计数 × 1e9 远超 u64::MAX, 先乘后除不得溢出
        assert_eq!(
            mul_div_saturating(100_000_000_000_000, 1_000_000_000, 2_500_000_000),
            40_000_000_000_000
        );
        // 除零 = 未初始化防护, 返回 0
        assert_eq!(mul_div_saturating(10, 10, 0), 0);
        // 最终结果确实超 u64::MAX 时饱和, 不 wrap
        assert_eq!(mul_div_saturating(u64::MAX, 2, 1), u64::MAX);
    }
}

/// 注册定时器 tick 子系统测试用例 (kernel_test)
#[cfg(feature = "kernel_test")]
pub fn register_timer_tick_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};

    fn initial_state() -> TestResult {
        crate::assert_eq_test!(get_ticks(), 0, "ticks");
        crate::assert_eq_test!(get_frequency(), DEFAULT_INTERRUPT_FREQ_HZ, "freq");
        crate::check!(!is_initialized(), "not initialized");
        TestResult::Pass
    }

    fn zero_frequency_handling() -> TestResult {
        crate::assert_eq_test!(ticks_to_ms(1000), 1000, "ticks_to_ms");
        crate::assert_eq_test!(ticks_to_us(1000), 1_000_000, "ticks_to_us");
        crate::assert_eq_test!(ms_to_ticks(1000), 1000, "ms_to_ticks");
        TestResult::Pass
    }

    fn monotonicity() -> TestResult {
        let t1 = get_ticks();
        let t2 = get_ticks();
        crate::check!(t2 >= t1, "monotonic");
        TestResult::Pass
    }

    /// E-I 接线验证: 启动编排已完成时基建立 (否则 tickless 必须被拦住)
    fn timebase_established_at_boot() -> TestResult {
        crate::check!(timebase_is_hardware(), "hardware timebase established");
        crate::check!(get_frequency() > 0, "tick frequency configured");
        TestResult::Pass
    }

    /// E-I 单一时基: tick 与纳秒视图同向推进且互一致 (容差 2 tick)
    fn uptime_ns_and_ticks_agree() -> TestResult {
        let ns = get_uptime_ns();
        let ms = get_uptime_ms();
        // tick 视图用整数 `COUNTERS_PER_TICK` (整除截断), 纳秒视图用精确频率,
        // 两者在一个 tick 内可能偏差 (两读数也取于不同瞬间) ⇒ 容差 2ms.
        crate::check!(
            ns.abs_diff(ms.saturating_mul(1_000_000)) <= 2_000_000,
            "ns/ms divergence within 2 ticks"
        );
        crate::check!(get_uptime_ns() >= ns, "ns monotonic");
        crate::check!(
            get_ticks() >= ANCHOR_TICK.load(Ordering::Relaxed),
            "tick at or after anchor"
        );
        TestResult::Pass
    }

    let r = runner();
    r.register("timer::tick", "initial_state", initial_state as TestFn);
    r.register(
        "timer::tick",
        "zero_frequency_handling",
        zero_frequency_handling as TestFn,
    );
    r.register("timer::tick", "monotonicity", monotonicity as TestFn);
    r.register(
        "timer::tick",
        "timebase_established_at_boot",
        timebase_established_at_boot as TestFn,
    );
    r.register(
        "timer::tick",
        "uptime_ns_and_ticks_agree",
        uptime_ns_and_ticks_agree as TestFn,
    );
}
