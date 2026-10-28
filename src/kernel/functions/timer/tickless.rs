#![deny(unsafe_code)]
//! Tickless (NO_HZ) — 动态时钟中断管理
//!
//! ## 设计
//!
//! 传统内核使用固定频率的周期时钟中断 (periodic tick), 即使 CPU 空闲也持续触发.
//! Tickless 模式在 CPU 空闲时停掉周期 tick, 仅在下一个有意义的事件时唤醒,
//! 从而省电并减少不必要的上下文切换.
//!
//! ### 模式
//!
//! 1. **NO_HZ_IDLE**: 空闲 CPU 停止周期 tick (默认)
//!    - 进入 idle 时: 计算下一个定时器到期时间, 编程 one-shot
//!    - 退出 idle 时: 恢复周期 tick
//!
//! 2. **NO_HZ_FULL**: 仅有任务的 CPU 也停止 tick (进阶)
//!    - 适合实时/低延迟场景
//!    - 需要通过 sched_tick 等机制补充记账
//!
//! ### 与 Linux 的差异
//!
//! 1. **仅实现 NO_HZ_IDLE**: NO_HZ_FULL 需要更复杂的记账
//! 2. **无 adaptive tick**: 不根据 CPU 数量自动调整
//! 3. **无 full dynticks**: 不支持完全无 tick 模式
//!
//! ## 分层位置
//!
//! 本模块是**策略层** (functions, 0 unsafe): per-CPU 模式状态机、空闲时长记账与
//! 统计在此实现. 定时器编程属机制, 仍由 privileged 侧安全入口承担:
//! 时钟读数与最早到期时间取自 `privileged::timer::{hrtimer_clock_read,
//! hrtimer_next_expiry}` (与 hrtimer 同源), 本核单次/周期装填取自
//! `privileged::timer::clockevent` (LAPIC MSR 写入留在 TCB).

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::privileged::sync::IrqSpinLock;
use crate::privileged::timer::{get_frequency, hrtimer_clock_read, hrtimer_next_expiry};
use alloc::vec::Vec;

// ============================================================================
// 常量
// ============================================================================

/// 默认 tick 频率 (Hz)
pub const DEFAULT_HZ: u32 = 1000;
/// 最大可配置 HZ
pub const MAX_HZ: u32 = 10000;
/// 最小可配置 HZ
pub const MIN_HZ: u32 = 100;

// ============================================================================
// Tickless 模式
// ============================================================================

/// Tickless 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TicklessMode {
    /// 周期 tick (传统模式)
    Periodic = 0,
    /// `NO_HZ_IDLE`: 空闲时停止 tick
    NoHzIdle = 1,
    /// `NO_HZ_FULL`: 始终无 tick (仅 `sched_tick`)
    NoHzFull = 2,
}

impl TicklessMode {
    /// 由 syscall 传入的裸值还原模式 (未知值退化为 `Periodic`)
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::NoHzIdle,
            2 => Self::NoHzFull,
            _ => Self::Periodic,
        }
    }
}

// ============================================================================
// Per-CPU Tickless 状态
// ============================================================================

/// Per-CPU Tickless 状态
#[derive(Debug)]
pub struct TicklessCpuState {
    /// 当前模式
    pub mode: AtomicU32,
    /// 是否处于 tickless 状态 (tick 已停止)
    pub tick_stopped: AtomicBool,
    /// 进入 tickless 的时间 (ns)
    pub idle_entry_time: AtomicU64,
    /// 累计 tickless 时间 (ns)
    pub idle_tickless_time: AtomicU64,
    /// 下一个定时器到期时间 (ns, 0=无)
    pub next_timer_expiry: AtomicU64,
    /// 周期 tick 频率 (Hz)
    pub hz: AtomicU32,
    /// tickless 进入次数
    pub tickless_enter_count: AtomicU64,
    /// tickless 退出次数
    pub tickless_exit_count: AtomicU64,
}

impl TicklessCpuState {
    /// 构造默认周期模式的 per-CPU 状态
    pub fn new() -> Self {
        Self {
            mode: AtomicU32::new(TicklessMode::Periodic as u32),
            tick_stopped: AtomicBool::new(false),
            idle_entry_time: AtomicU64::new(0),
            idle_tickless_time: AtomicU64::new(0),
            next_timer_expiry: AtomicU64::new(0),
            hz: AtomicU32::new(DEFAULT_HZ),
            tickless_enter_count: AtomicU64::new(0),
            tickless_exit_count: AtomicU64::new(0),
        }
    }
}

// ============================================================================
// Tickless 子系统
// ============================================================================

/// Tickless 子系统
pub struct TicklessSubsystem {
    /// Per-CPU 状态
    per_cpu: IrqSpinLock<Vec<TicklessCpuState>>,
    /// 全局默认模式
    global_mode: AtomicU32,
    /// 是否已初始化
    initialized: AtomicBool,
}

impl TicklessSubsystem {
    /// 构造空子系统 (per-CPU 表由 [`Self::init`] 按核数填充)
    pub const fn new() -> Self {
        Self {
            per_cpu: IrqSpinLock::new(Vec::new()),
            global_mode: AtomicU32::new(TicklessMode::NoHzIdle as u32),
            initialized: AtomicBool::new(false),
        }
    }

    /// 初始化
    pub fn init(&self, num_cpus: u32) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        let mut per_cpu = self.per_cpu.lock();
        for _ in 0..num_cpus {
            let state = TicklessCpuState::new();
            // 默认使用全局模式
            state
                .mode
                .store(self.global_mode.load(Ordering::Acquire), Ordering::Release);
            per_cpu.push(state);
        }
        self.initialized.store(true, Ordering::Release);
        crate::privileged::klog_ffi!(
            klog_ffi_info,
            "[Tickless] initialized: {} CPUs, mode=NO_HZ_IDLE",
            num_cpus
        );
    }

    /// CPU 进入 idle 前调用: 停止周期 tick, 编程 one-shot
    ///
    /// 返回: 下一个唤醒时间 (ns), 0 表示无定时器
    pub fn enter_tickless(&self, cpu_id: u32) -> u64 {
        let per_cpu = self.per_cpu.lock();
        if (cpu_id as usize) >= per_cpu.len() {
            return 0;
        }
        let state = &per_cpu[cpu_id as usize];
        let mode = TicklessMode::from_u32(state.mode.load(Ordering::Acquire));

        if mode == TicklessMode::Periodic {
            // 周期模式: 不停 tick
            return 0;
        }

        // 标记 tick 已停止
        state.tick_stopped.store(true, Ordering::Release);
        state.tickless_enter_count.fetch_add(1, Ordering::Relaxed);

        // 记录进入时间
        let now_ns = Self::read_clock_ns();
        state.idle_entry_time.store(now_ns, Ordering::Release);

        // 计算下一个定时器到期时间
        let next_expiry = self.get_next_timer_expiry(cpu_id);
        state
            .next_timer_expiry
            .store(next_expiry, Ordering::Release);

        if next_expiry > 0 && next_expiry > now_ns {
            // 编程 one-shot 定时器
            let delta_ns = next_expiry - now_ns;
            Self::program_oneshot(delta_ns);
        }
        // 如果没有待处理定时器, 则无限期停止 tick
        // 直到下一个中断 (外部/设备) 唤醒

        next_expiry
    }

    /// CPU 退出 idle 后调用: 恢复周期 tick
    pub fn exit_tickless(&self, cpu_id: u32) {
        let per_cpu = self.per_cpu.lock();
        if (cpu_id as usize) >= per_cpu.len() {
            return;
        }
        let state = &per_cpu[cpu_id as usize];

        if !state.tick_stopped.load(Ordering::Acquire) {
            return; // 本来就没停
        }

        // 计算空闲时间
        let now_ns = Self::read_clock_ns();
        let entry_ns = state.idle_entry_time.load(Ordering::Acquire);
        if now_ns > entry_ns {
            let idle_ns = now_ns - entry_ns;
            state
                .idle_tickless_time
                .fetch_add(idle_ns, Ordering::Relaxed);
        }

        // 恢复周期 tick
        state.tick_stopped.store(false, Ordering::Release);
        state.tickless_exit_count.fetch_add(1, Ordering::Relaxed);

        // 恢复周期定时器
        let hz = state.hz.load(Ordering::Acquire);
        Self::program_periodic(hz);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 获取下一个定时器到期时间 (绝对纳秒, 单调时钟)
    ///
    /// 委托 hrtimer 子系统查询最早到期定时器; `0` 表示无待触发定时器
    /// (进入 tickless 后无限期停 tick, 等外部/设备中断唤醒)。
    fn get_next_timer_expiry(&self, _cpu_id: u32) -> u64 {
        hrtimer_next_expiry().unwrap_or(0)
    }

    /// 设置 Per-CPU 模式
    pub fn set_cpu_mode(&self, cpu_id: u32, mode: TicklessMode) -> bool {
        let per_cpu = self.per_cpu.lock();
        if (cpu_id as usize) >= per_cpu.len() {
            return false;
        }
        per_cpu[cpu_id as usize]
            .mode
            .store(mode as u32, Ordering::Release);
        true
    }

    /// 设置全局模式
    pub fn set_global_mode(&self, mode: TicklessMode) {
        self.global_mode.store(mode as u32, Ordering::Release);
        let per_cpu = self.per_cpu.lock();
        for state in per_cpu.iter() {
            state.mode.store(mode as u32, Ordering::Release);
        }
    }

    /// 获取全局模式
    pub fn get_global_mode(&self) -> TicklessMode {
        TicklessMode::from_u32(self.global_mode.load(Ordering::Acquire))
    }

    /// 获取 Per-CPU 统计 `(进入次数, 退出次数, 累计 tickless 时间 ns)`
    pub fn get_cpu_stats(&self, cpu_id: u32) -> Option<(u64, u64, u64)> {
        let per_cpu = self.per_cpu.lock();
        per_cpu.get(cpu_id as usize).map(|s| {
            (
                s.tickless_enter_count.load(Ordering::Acquire),
                s.tickless_exit_count.load(Ordering::Acquire),
                s.idle_tickless_time.load(Ordering::Acquire),
            )
        })
    }

    /// 设置 tick 频率
    pub fn set_hz(&self, cpu_id: u32, hz: u32) -> bool {
        if !(MIN_HZ..=MAX_HZ).contains(&hz) {
            return false;
        }
        // tick 到达率必须等于时基簿记频率: 二者脱钩会使 Deadline 记账与时间片
        // 粒度按名义值错位 (硬件装填值由 `tick::counters_per_tick()` 单一来源算出,
        // clockevent 对此强约束). 先校验再落库, 不留下"名义 hz 与实际不一致"的状态.
        if hz != get_frequency() {
            return false;
        }
        let per_cpu = self.per_cpu.lock();
        if (cpu_id as usize) >= per_cpu.len() {
            return false;
        }
        per_cpu[cpu_id as usize].hz.store(hz, Ordering::Release);
        // 如果当前是周期模式, 重新编程
        if !per_cpu[cpu_id as usize]
            .tick_stopped
            .load(Ordering::Acquire)
        {
            Self::program_periodic(hz);
        }
        true
    }

    /// 是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    // ========================================================================
    // 定时器编程 (机制在 privileged, 本层只调用其安全入口)
    // ========================================================================

    /// 读取当前时钟 (ns, 与 hrtimer 同源)
    ///
    /// hrtimer 到期时间为校准时钟 (TSC/CNTPCT) 上的绝对纳秒; tickless 的
    /// 到期判断 (`next_expiry > now_ns`) 必须与 hrtimer 用同一时钟, 否则
    /// 粗粒度 tick 时钟会误判到期/未到期。
    fn read_clock_ns() -> u64 {
        hrtimer_clock_read()
    }

    /// 编程 one-shot 定时器: 把本核下一个到期改到 `now + delta_ns`
    ///
    /// 装填后本核不再自动续周期 tick (由 clockevent 的单次装填状态承担),
    /// 直到退出 tickless 时 [`Self::program_periodic`] 重新起算。
    fn program_oneshot(delta_ns: u64) {
        #[cfg(target_arch = "x86_64")]
        crate::privileged::timer::clockevent::program_oneshot_ns(delta_ns);
        #[cfg(target_arch = "aarch64")]
        {
            // SIMPLIFIED: aarch64 未向本核 Generic Timer 装填单次到期, 停 tick 在本架构
            // 上是纯记账 (软件标记), 硬件仍按周期发中断; 影响面 = NO_HZ_IDLE 在 aarch64
            // 无省电收益且 tick 仍会到达; 待 arch::aarch64::timer 提供每核 one-shot 装填
            // 入口 (CNTV_TVAL + CTL.ENABLE=0/1 语义) 后接回同一抽象。
            let _ = delta_ns;
        }
    }

    /// 恢复本核周期 tick
    ///
    /// 返回装填是否受理: `hz` 与时基簿记频率不一致 (或硬件尚未装配) 时返回
    /// `false` 并记录, 不静默假装成功。
    fn program_periodic(hz: u32) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            match crate::privileged::timer::clockevent::arm_periodic(hz) {
                Ok(()) => true,
                Err(reason) => {
                    crate::privileged::klog_warn!(
                        Timer,
                        "tickless: resume periodic tick rejected: {}",
                        reason
                    );
                    false
                }
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SIMPLIFIED: 同 `program_oneshot` — aarch64 的周期装填由 arch 侧
            // `timer::reload` 在 ISR 内完成, tickless 退出路径不重算周期.
            let _ = hz;
            true
        }
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 Tickless 子系统
static TICKLESS: TicklessSubsystem = TicklessSubsystem::new();

/// 初始化 Tickless
pub fn init(num_cpus: u32) {
    TICKLESS.init(num_cpus);
}

/// 获取全局 Tickless 子系统
pub fn subsystem() -> &'static TicklessSubsystem {
    &TICKLESS
}

/// Tickless 是否已初始化
pub fn is_initialized() -> bool {
    TICKLESS.is_initialized()
}

// ============================================================================
// 系统调用
// ============================================================================

/// `EG_TICKLESS` — Tickless 系统调用 (functions 层唯一实现, 无 privileged 侧直调)
///
/// `a0`: cmd
///   0 = `set_global_mode(mode`: a1)
///   1 = `get_global_mode()` → mode
///   2 = `set_cpu_mode(cpu`: a1, mode: a2)
///   3 = `get_cpu_stats(cpu`: a1) → (enter 位于高 32 位 | exit 位于低 32 位) 或 `idle_time`
///   4 = `set_hz(cpu`: a1, hz: a2)
///   5 = `is_initialized()` → bool (是否已初始化)
pub fn tickless_syscall(cmd: u64, a1: u64, a2: u64) -> i64 {
    if !is_initialized() && cmd != 5 {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // set_global_mode
            let mode = TicklessMode::from_u32(crate::functions::intcast::low_u32_from_u64(a1));
            subsystem().set_global_mode(mode);
            0
        }
        1 => {
            // get_global_mode
            subsystem().get_global_mode() as i64
        }
        2 => {
            // set_cpu_mode
            let mode = TicklessMode::from_u32(crate::functions::intcast::low_u32_from_u64(a2));
            if subsystem().set_cpu_mode(crate::functions::intcast::low_u32_from_u64(a1), mode) {
                0
            } else {
                -(22i64)
            }
        }
        3 => {
            // get_cpu_stats
            match subsystem().get_cpu_stats(crate::functions::intcast::low_u32_from_u64(a1)) {
                Some((enter, exit, idle_ns)) => {
                    // 返回: 高32位=enter_count, 低32位=exit_count
                    // idle_ns 通过第二次调用返回 (简化)
                    let _ = idle_ns;
                    (enter.cast_signed() << 32) | (exit.cast_signed() & 0xFFFF_FFFF)
                }
                None => -(22i64),
            }
        }
        4 => {
            // set_hz
            if subsystem().set_hz(
                crate::functions::intcast::low_u32_from_u64(a1),
                crate::functions::intcast::low_u32_from_u64(a2),
            ) {
                0
            } else {
                -(22i64)
            }
        }
        5 => {
            // is_initialized
            i64::from(is_initialized())
        }
        _ => -(38i64), // ENOSYS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tickless 状态机 + hrtimer 接线 (host 可测的稳定态部分)。
    ///
    /// 正面路径 (有定时器时返回其到期时间) 依赖全局 hrtimer 队列, 与
    /// `hrtimer::tests` 的 `hrtimer_init()` 清理存在跨模块并发竞争, 故本测试
    /// 只覆盖稳定态: 空队列 enter 返回 0 / tick 停复 / 统计计数。
    /// hrtimer 到期查询的正面路径由 `hrtimer::tests::test_queue_operations`
    /// (kernel_test/QEMU) 覆盖。
    #[test]
    fn tickless_state_machine_and_hrtimer_wiring() {
        crate::privileged::timer::hrtimer::hrtimer_init();
        let tl = subsystem();
        tl.init(1);

        // 默认 NO_HZ_IDLE (全局模式)
        assert_eq!(tl.get_global_mode(), TicklessMode::NoHzIdle);

        // 空队列 (hrtimer_init 后无定时器): enter 返回 0, 进入 tickless
        let exp = tl.enter_tickless(0);
        assert_eq!(exp, 0, "空队列应返回 0 (无限期停 tick)");
        let (enter1, exit1, _) = tl.get_cpu_stats(0).expect("cpu0 统计");
        assert_eq!(enter1, 1, "enter 计数 +1");

        // exit: 恢复周期 tick, 计数 +1, 空闲时间累计
        tl.exit_tickless(0);
        let (enter2, exit2, _idle_ns) = tl.get_cpu_stats(0).expect("cpu0 统计");
        assert_eq!(enter2, 1, "enter 计数不变");
        assert_eq!(exit2, exit1 + 1, "exit 计数 +1");

        // 再次进入/退出: 状态机可重复
        tl.enter_tickless(0);
        tl.exit_tickless(0);
        let (enter3, exit3, _) = tl.get_cpu_stats(0).expect("cpu0 统计");
        assert_eq!(enter3, 2, "再次进入计数 +1");
        assert_eq!(exit3, exit2 + 1, "再次退出计数 +1");

        // 清理: 队列保持空 (避免污染其他 hrtimer 测试)
        crate::privileged::timer::hrtimer::hrtimer_init();
    }
}
