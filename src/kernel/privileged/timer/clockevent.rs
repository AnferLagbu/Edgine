//! 每核周期 tick 设备 (clockevent) —— `x86_64` LAPIC 定时器装配
//!
//! ## 职责与分层
//!
//! 把"单一硬件时基"(`timer::tick`) 换算成本核 LAPIC 定时器的到期编程, 并在 tick
//! ISR 尾部续期。分层判据: [`crate::privileged::arch::x86_64::apic`] 只提供寄存器
//! 原语 (写 LVT/DCR/ICR/`IA32_TSC_DEADLINE`), 模式选择、频率换算、续期策略都在本
//! 模块 —— 与 Asterinas/Linux 的 clockevent 划分一致 (机制在 arch, 策略在 timer)。
//!
//! ## 两种工作模式 (deadline 优先 + count 回退)
//!
//! - **TSC-Deadline**: 到期值 = 时基计数器读数 + n × 每 tick 计数增量。截止期直接
//!   落在 tick 时基 (TSC) 域, 与 `get_ticks()` / hrtimer 同一坐标系, **零标定、
//!   无累积漂移**。选定前须 CPUID 位与 LVT 回读双重确认 (见
//!   [`apic::timer_deadline_supported`])。
//! - **count 回退**: 无 deadline 能力时, 以**已登记的硬件时基**为窗口实测 APIC
//!   总线频率, 装填值 = `增量 × 总线频率 ÷ 时基频率`。实测一次, 全生命周期复用;
//!   不再像旧实现那样借 PIT 门 + `timer_sleep_busy` FFI 自拷贝一份标定逻辑。
//!
//! ## 为什么 PIT 不再是 tick 源
//!
//! PIT 通道 0 是全局单例: 只有一个周期、只在 BSP 投递、经 8259 ExtINT。多核下
//! 次级核没有周期 tick (旧代码注释自证), idle 也就无法停 tick。LAPIC 定时器是
//! 每核私有设备, 既解决 SMP 记账, 也是 tickless (E-III) 的前置。PIT 通道 0 仍
//! 保留作启动期频率测定的参考门 (纯 I/O 读, 不依赖中断投递)。
//!
//! ## SAFETY
//!
//! 本模块属 privileged/TCB, 允许 unsafe: 全部硬件访问委托给 `apic` 原语, 本模块
//! 自身不直接触碰 MMIO/MSR。

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::privileged::arch::x86_64::apic;
use crate::privileged::config::MAX_CPUS;
use crate::privileged::idt::TICK_VECTOR;

use super::{calibration, tick};

// ============================================================================
// 常量
// ============================================================================

/// count 回退模式的定时器分频器 (÷16)
///
/// 1 ms tick 在 ÷16 下约数千至数十万计数, 既避开 32 位装填值的上下边界, 也让
/// 10 ms 实测窗口的差分不受单计数噪声影响。
const TIMER_DIVISOR: u32 = 16;

/// count 回退模式实测总线频率的窗口 (ms)
///
/// 与 [`calibration`] 的 PIT 参考门同量级 (10 ms), 使启动期总实测时间可预期。
const MEASURE_WINDOW_MS: u64 = 10;

/// tick 源尚未装配
const MODE_UNSET: u32 = 0;
/// TSC-Deadline 模式 (绝对截止期落在时基计数器域)
const MODE_DEADLINE: u32 = 1;
/// 递减计数模式 (按实测总线频率换算装填值)
const MODE_COUNT: u32 = 2;

// ============================================================================
// 全局与每核状态
// ============================================================================

/// 当前工作模式 (`MODE_*`); 非 `MODE_UNSET` 即视为已装配
///
/// 宽度取 u32 而非 u8: 与仓内其余全局原子 (`AtomicU32` / `AtomicU64`) 同口径.
static MODE: AtomicU32 = AtomicU32::new(MODE_UNSET);

/// 硬件自由计数器频率 (Hz), 装配时从 [`calibration`] 取一次并缓存
///
/// 缓存而非每次现取: 装配后该值是常量, 而每次 tick 都要用它做单位换算。
static COUNTER_HZ: AtomicU64 = AtomicU64::new(0);

/// count 模式: APIC 总线经分频后的有效计数频率 (Hz); deadline 模式恒 0
static POST_DIV_HZ: AtomicU64 = AtomicU64::new(0);

/// 每核"本次到期之后是否继续按周期 tick"
///
/// 这是 clockevent **自身的装填状态** (硬件接下来会怎样), 不是 tickless 的策略
/// 副本 (后者记 "本核为什么停 tick" 与时长统计)。二者由 enter/exit 路径单向驱动
/// 保持一致, 不在 ISR 里反查策略状态 —— 续期处在 hardirq, 少一次跨模块加锁就
/// 少一条锁序边 (见 docs/explain/ref-lock-order.md)。
static AUTO_CONTINUE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// 每核上一次装填的绝对截止期 (时基计数器域), 续期由此推进而非 "now + period"
///
/// 以 `last + period` 起算使 tick 不与中断进入延迟相互累积 (每次重新 `now +
/// period` 会把 ISR 延迟摊进周期, 长期看 tick 越来越慢)。
static EXPIRY: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// CPU 逻辑索引 → 本模块每核数组槽位
#[inline]
fn local_slot() -> usize {
    (crate::privileged::smp::current_cpu_index() as usize) % MAX_CPUS
}

/// tick 源是否已装配 (启动完成判据与 tickless 的前置查询)
#[inline]
pub fn is_ready() -> bool {
    MODE.load(Ordering::Acquire) != MODE_UNSET
}

// ============================================================================
// 装配
// ============================================================================

/// CPU 是否声明支持 TSC-Deadline (CPUID Leaf 1 ECX bit 24)
fn cpu_supports_deadline() -> bool {
    crate::privileged::cpu::get_cpu_info().is_some_and(|info| {
        info.features
            .contains(crate::privileged::cpu::CpuFeatures::TSCDEADLINE)
    })
}

/// 选定本内核的 tick 源模式并登记换算参数 (BSP 单次, 幂等)
///
/// # Panics
/// tick 源是内核的硬依赖, 无可用模式时**必须**停机而不是静默降级: 没有周期 tick
/// 的表现为"调度器永不抢占 / hrtimer 永不触发", 属事后极难归因的挂死, 远不如一条
/// 明确的启动失败日志可诊断。PIT 已退役为 tick 源, 故此处不设 PIT 回退路径。
pub fn setup() {
    if is_ready() {
        return;
    }

    if !apic::is_initialized() {
        crate::klog_err!(
            Timer,
            "clockevent: no local APIC — x86_64 periodic tick requires LAPIC (PIT retired)"
        );
        panic!("clockevent setup failed: local APIC unavailable");
    }

    let counter_hz = match calibration::hw_counter_frequency_hz() {
        Some(hz) if hz != 0 => hz,
        _ => {
            crate::klog_err!(
                Timer,
                "clockevent: hardware timebase not registered (step 7.5 establish_boot_timebase failed)"
            );
            panic!("clockevent setup failed: no hardware timebase");
        }
    };

    if tick::counters_per_tick() == 0 {
        crate::klog_err!(
            Timer,
            "clockevent: timebase is not hardware-derived, tick period unknown"
        );
        panic!("clockevent setup failed: counters_per_tick == 0");
    }

    COUNTER_HZ.store(counter_hz, Ordering::Release);

    // deadline 优先: CPUID 声明 + 本核 LAPIC 回读双确认, 两者齐备才用.
    // 拦住的究竟是哪一支必须记下来 —— 真机 bring-up 时这是区分"CPU 根本没报"
    // 与"CPU 报了但本核 LAPIC 忽略 bit18"的唯一线索.
    let cpuid_declares = cpu_supports_deadline();
    let lapic_accepts = cpuid_declares && apic::timer_deadline_supported();
    if cpuid_declares && !lapic_accepts {
        crate::klog_warn!(
            Timer,
            "clockevent: CPUID declares TSC-deadline but LAPIC ignores bit18 — count mode"
        );
    }
    if lapic_accepts {
        MODE.store(MODE_DEADLINE, Ordering::Release);
        crate::klog_boot_info!(
            "Clockevent: LAPIC TSC-deadline (counter {} Hz, zero calibration)",
            counter_hz
        );
    } else {
        if !cpuid_declares {
            crate::klog_boot_info!("Clockevent: TSC-deadline not declared — measuring APIC bus");
        }
        let bus_hz = match measure_bus_hz(counter_hz) {
            Ok(hz) => hz,
            Err(reason) => {
                crate::klog_err!(Timer, "clockevent: APIC bus measurement failed: {}", reason);
                panic!("clockevent setup failed: APIC bus frequency unmeasurable");
            }
        };
        // 分频后有效计数频率; TIMER_DIVISOR 非 0 常量, 整除必不为 0 (bus_hz 已校验).
        POST_DIV_HZ.store(bus_hz / u64::from(TIMER_DIVISOR), Ordering::Release);
        apic::set_timer_divisor(TIMER_DIVISOR);
        MODE.store(MODE_COUNT, Ordering::Release);
        crate::klog_boot_info!(
            "Clockevent: LAPIC count mode (bus {} Hz, div {})",
            bus_hz,
            TIMER_DIVISOR
        );
    }

    arm_local();
    verify_expiry_observed();
}

/// 装填后的闭环自检: 本核 tick 设备确实到期了 (deadline / count 两支共用)
///
/// 为何不能只靠写后回读: deadline 模式的正确性还取决于 `IA32_TSC_DEADLINE` 写入
/// 真被 LAPIC 采纳且到期会维内中断 —— 本机 (AMD Ryzen) 无该能力, 这一支只能在
/// Intel 真机由本闭环取证, 故它不是可选的加强项而是该分支的唯一防线.
///
/// # 判据取 IRR 挂起位而不是"等投递"
///
/// 启动全程 `IF=0` (`boot.asm` 起即 `cli`, 直到进 Ring 3 才由 iretq 置位), 到这里
/// 中断不可能被递进核心。曾试过在此开一个有界 `sti` 窗口等真投递 —— 行不通:
/// 那会把 tick handler (含 `poll_network` 与其它锁) 强行插进"设计上单线程无中断"
/// 的初始化临界区, e2e 由 3/3 通过退化为 3/3 失败。关中断不是可绕的开销, 而是
/// 启动期的并发前提。IRR 则不然: LAPIC 在到期的那一刻就把向量记到本核 IRR,
/// 不依赖 IF, 因此无需破坏该前提就能证明"定时器真在跑"。
///
/// 同时接受 [`rearm_local`] 的递增计数作备选证据 (本函数现在只在 AP 上线前跑,
/// 若将来被 IF=1 上下文复用, 投递已发生则 IRR 已清).
///
/// # Panics
/// 宽限窗口内到期痕迹为零即停机 (理由同 [`setup`]: 静默降级换来的是上线后才
/// 暴露的"永不抢占").
fn verify_expiry_observed() {
    /// 宽限窗口 (以 tick 周期计): 兼顾 QEMU TCG 抖动与装填到到期的硬件延迟.
    const GRACE_TICKS: u64 = 10;

    let horizon =
        tick::counter_now().saturating_add(tick::counters_per_tick().saturating_mul(GRACE_TICKS));
    loop {
        if apic::apic_is_in_irr(TICK_VECTOR) {
            // 正面信号在此 (启动上下文, IF=0) 而不是 tick ISR 里打: deadline 分支在
            // AMD 主机上不可达, Intel 真机就靠本行 + IRR 判据证明"到期链已闭合".
            crate::klog_boot_info!(
                "Clockevent: tick expiry observed in IRR (mode {})",
                MODE.load(Ordering::Acquire)
            );
            return;
        }
        if TICK_DELIVERED.load(Ordering::Acquire) > 0 {
            return;
        }
        if tick::counter_now() >= horizon {
            break;
        }
        core::hint::spin_loop();
    }
    crate::klog_err!(
        Timer,
        "clockevent: tick source never expired within {} periods (mode {}, LVT {:#x})",
        GRACE_TICKS,
        MODE.load(Ordering::Acquire),
        apic::read_timer_lvt()
    );
    panic!("clockevent setup failed: tick source silent");
}

/// 以已登记的硬件时基为窗口实测 APIC 总线频率 (count 回退模式)
///
/// 定时器全程屏蔽 ⇒ 不产生中断, 只让递减计数器跑; 窗口结束读剩余值差分。
/// 满量程初值使 10 ms 窗口内不可能绕回 32 位计数器 (÷16 下最多约 6.5e6 计数)。
fn measure_bus_hz(counter_hz: u64) -> Result<u64, &'static str> {
    apic::mask_timer();
    apic::set_timer_divisor(TIMER_DIVISOR);
    apic::set_timer_count(u32::MAX);

    let window = tick::mul_div_saturating(counter_hz, MEASURE_WINDOW_MS, 1_000).max(1);
    let start = tick::counter_now();
    while tick::counter_now().saturating_sub(start) < window {
        core::hint::spin_loop();
    }

    let elapsed = u64::from(u32::MAX.wrapping_sub(apic::get_timer_count()));
    if elapsed == 0 {
        return Err("APIC timer did not count during measurement window");
    }

    // bus_hz = elapsed × divisor × counter_hz / window (window 是计数器增量,
    // 除以 counter_hz 即秒); 128 位中间值由 mul_div_saturating 保证不溢出.
    let hz = tick::mul_div_saturating(
        elapsed,
        counter_hz.saturating_mul(u64::from(TIMER_DIVISOR)),
        window,
    );
    if hz == 0 {
        return Err("measured APIC bus frequency rounds to 0");
    }
    Ok(hz)
}

// ============================================================================
// 每核装填与续期
// ============================================================================

/// 把绝对截止期换算成本核硬件装填值并写入
// 有意窄化: 硬件装填为 32 位; 超界即饱和到最大值 (对应远超一个 tick 的截止期,
// 由 tickless 的长睡眠产生, 截断只会提前唤醒, 不会延后).
fn write_expiry(deadline: u64) {
    match MODE.load(Ordering::Acquire) {
        MODE_DEADLINE => apic::set_deadline(deadline),
        MODE_COUNT => {
            let counter_hz = COUNTER_HZ.load(Ordering::Acquire);
            let post_div = POST_DIV_HZ.load(Ordering::Acquire);
            let delta = deadline.saturating_sub(tick::counter_now());
            // 至少装 1 个计数: 换算向下取整为 0 时会让定时器立即重发 (中断风暴).
            let counts = tick::mul_div_saturating(delta, post_div, counter_hz).max(1);
            apic::set_timer_count(crate::privileged::glue::low_u32_from_u64(
                counts.min(u64::from(u32::MAX)),
            ));
        }
        // 未装配 (setup 前的 tick 不可能到达: LVT 在 apic::init 里默认屏蔽)
        _ => {}
    }
}

/// 装配本核 tick 设备并按全局 tick 周期装填首个到期
///
/// BSP 由 [`setup`] 调用; 次级核在 `ap_entry` 内自调用 (LAPIC 是每核私有寄存器,
/// 只能本核写)。调用前 [`setup`] 必须已在 BSP 完成 —— 启动编排把次级核上线排在
/// 模式选定之后, 故此处不再判定模式缺失。
pub fn arm_local() {
    if !is_ready() {
        return;
    }
    let slot = local_slot();
    let next = tick::counter_now().saturating_add(tick::counters_per_tick());
    EXPIRY[slot].store(next, Ordering::Release);
    AUTO_CONTINUE[slot].store(true, Ordering::Release);
    apic::program_timer(hardware_mode(), TICK_VECTOR);
    write_expiry(next);
}

/// 已投递的 tick 总数 (全部核): 由 tick ISR 尾部的 [`rearm_local`] 递增
///
/// 消费者为 [`verify_expiry_observed`]: 本函数现在只在 AP 上线前跑, 若将来被
/// IF=1 上下文复用, 投递已发生则 IRR 已清, 本计数是唯一痕迹.
static TICK_DELIVERED: AtomicU64 = AtomicU64::new(0);

/// 本核模式枚举值 → `apic::TimerMode`
fn hardware_mode() -> apic::TimerMode {
    match MODE.load(Ordering::Acquire) {
        MODE_DEADLINE => apic::TimerMode::TscDeadline,
        _ => apic::TimerMode::OneShot,
    }
}

/// tick ISR 尾部续期: 推进本核下一个到期
///
/// `EXPIRY + period` 起算; 若因长时间关中断 / tickless 唤醒而落后于当前读数,
/// 重新以 now 起算 (宁可丢几格记账, 也不追发一串补 tick —— 记账已由硬件时基
/// 在 `get_ticks()` 里派生补齐, 不依赖中断次数)。
///
/// **本路径不得打日志**: 这里是 hardirq, 格式化与串口发射既拉长中断时长
/// (P-3 插桩取证实测一条 60+ 字符日志 ≈ 5 ms IRQ-off 自扰动), 又把 klog 的两级
/// 栈缓冲区 (~600 B) 压在被中断现场的栈上. 取证信号留在启动上下文
/// (`verify_expiry_observed`) 打, 本函数只累加计数器.
pub fn rearm_local() {
    TICK_DELIVERED.fetch_add(1, Ordering::AcqRel);
    let slot = local_slot();
    if !AUTO_CONTINUE[slot].load(Ordering::Acquire) {
        // 本核处于单次装填 (tickless) 或已停 tick: 到期后不再自动续周期。
        return;
    }
    let period = tick::counters_per_tick();
    let now = tick::counter_now();
    let mut next = EXPIRY[slot].load(Ordering::Acquire).saturating_add(period);
    if next <= now {
        next = now.saturating_add(period);
    }
    EXPIRY[slot].store(next, Ordering::Release);
    write_expiry(next);
}

/// 把本核下一个到期改为 `now + delta_ns` 的单次装填 (tickless 进入)
///
/// 装填后 AUTO_CONTINUE 置否: 该到期触发后不再自动续周期 tick, 直到退出
/// tickless 时由 [`arm_local`] 重新起算。
pub fn program_oneshot_ns(delta_ns: u64) {
    if !is_ready() {
        return;
    }
    let counter_hz = COUNTER_HZ.load(Ordering::Acquire);
    let delta = tick::mul_div_saturating(delta_ns, counter_hz, 1_000_000_000);
    let slot = local_slot();
    let next = tick::counter_now().saturating_add(delta);
    EXPIRY[slot].store(next, Ordering::Release);
    AUTO_CONTINUE[slot].store(false, Ordering::Release);
    write_expiry(next);
}

/// 按 `hz` 恢复本核周期 tick (tickless 退出 / 频率调整)
///
/// `hz` 必须等于 tick 簿记频率: 硬件到达率与 `counters_per_tick` 脱钩会让
/// Deadline 记账与时间片粒度按名义值错位 (aarch64 曾以 10 ms 周期簿记 1000 Hz,
/// 见 E-II-a)。不一致时拒绝装填而不是"照做", 由调用方决定如何回报。
///
/// # Errors
/// 当 `hz` 与 tick 簿记频率 (`tick::get_frequency()`) 不一致时返回 `Err` ——
/// 属配置错误而非运行时故障; 错误文案单源, 由调用方 (tickless 恢复路径) 记日志。
pub fn arm_periodic(hz: u32) -> Result<(), &'static str> {
    let bookkeeping = tick::get_frequency();
    if hz != bookkeeping {
        return Err("tick rate must equal timebase bookkeeping frequency");
    }
    arm_local();
    Ok(())
}
