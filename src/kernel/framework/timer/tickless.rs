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
//! ## SAFETY
//!
//! 本模块属于 framework/TCB, 允许 unsafe.
//! 定时器编程涉及 LAPIC/HPET MSR 写入.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::framework::sync::IrqSpinLock;
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
        crate::klog_ffi!(
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
        super::hrtimer::hrtimer_next_expiry().unwrap_or(0)
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

    /// 获取 Per-CPU 统计
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
    // 定时器编程 (架构相关)
    // ========================================================================

    /// 读取当前时钟 (ns, 与 hrtimer 同源)
    ///
    /// hrtimer 到期时间为校准时钟 (TSC/CNTPCT) 上的绝对纳秒; tickless 的
    /// 到期判断 (`next_expiry > now_ns`) 必须与 hrtimer 用同一时钟, 否则
    /// 粗粒度 tick 时钟会误判到期/未到期。
    fn read_clock_ns() -> u64 {
        super::hrtimer::hrtimer_clock_read()
    }

    /// 编程 one-shot 定时器
    fn program_oneshot(delta_ns: u64) {
        #[cfg(target_arch = "x86_64")]
        {
            // 使用 LAPIC Timer one-shot 模式
            // SAFETY: LAPIC timer 编程是标准操作
            let timer_hz = crate::framework::arch::apic::get_timer_hz();
            if timer_hz == 0 {
                return;
            }
            let count = ((delta_ns * timer_hz) / 1_000_000_000) as u32;
            if count > 0 {
                crate::framework::arch::apic::init_timer(
                    0x20,  // IRQ0 vector
                    false, // one-shot
                    16,    // divisor
                );
                crate::framework::arch::apic::set_timer_count(count);
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // 使用 ARM Generic Timer one-shot
            let _ = delta_ns;
        }
    }

    /// 恢复周期定时器
    fn program_periodic(hz: u32) {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: LAPIC timer 周期模式编程
            let timer_hz = crate::framework::arch::apic::get_timer_hz();
            if timer_hz == 0 {
                return;
            }
            let count = (timer_hz / u64::from(hz)) as u32;
            crate::framework::arch::apic::init_timer(
                0x20, true, // periodic
                16,
            );
            crate::framework::arch::apic::set_timer_count(count);
        }
        #[cfg(target_arch = "aarch64")]
        {
            let _ = hz;
        }
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局 Tickless 子系统
static TICKLESS: TicklessSubsystem = TicklessSubsystem::new();

/// 初始化 Tickless
pub fn tickless_init(num_cpus: u32) {
    TICKLESS.init(num_cpus);
}

/// 获取全局 Tickless 子系统
pub fn tickless_subsystem() -> &'static TicklessSubsystem {
    &TICKLESS
}

/// Tickless 是否已初始化
pub fn tickless_is_initialized() -> bool {
    TICKLESS.is_initialized()
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_tickless` — Tickless 系统调用
///
/// `a0`: cmd
///   0 = `set_global_mode(mode`: a1)
///   1 = `get_global_mode()` → mode
///   2 = `set_cpu_mode(cpu`: a1, mode: a2)
///   3 = `get_cpu_stats(cpu`: a1) → (enter 位于高 32 位 | exit 位于低 32 位) 或 `idle_time`
///   4 = `set_hz(cpu`: a1, hz: a2)
///   5 = `is_initialized()` → bool (是否已初始化)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub extern "C" fn sys_tickless(cmd: u64, a1: u64, a2: u64) -> i64 {
    if !tickless_is_initialized() && cmd != 5 {
        return -(11i64); // EAGAIN
    }

    match cmd {
        0 => {
            // set_global_mode
            let mode = TicklessMode::from_u32(a1 as u32);
            tickless_subsystem().set_global_mode(mode);
            0
        }
        1 => {
            // get_global_mode
            tickless_subsystem().get_global_mode() as i64
        }
        2 => {
            // set_cpu_mode
            let mode = TicklessMode::from_u32(a2 as u32);
            if tickless_subsystem().set_cpu_mode(a1 as u32, mode) {
                0
            } else {
                -(22i64)
            }
        }
        3 => {
            // get_cpu_stats
            match tickless_subsystem().get_cpu_stats(a1 as u32) {
                Some((enter, exit, idle_ns)) => {
                    // 返回: 高32位=enter_count, 低32位=exit_count
                    // idle_ns 通过第二次调用返回 (简化)
                    let _ = idle_ns;
                    ((enter as i64) << 32) | (exit as i64 & 0xFFFFFFFF)
                }
                None => -(22i64),
            }
        }
        4 => {
            // set_hz
            if tickless_subsystem().set_hz(a1 as u32, a2 as u32) {
                0
            } else {
                -(22i64)
            }
        }
        5 => {
            // is_initialized
            i64::from(tickless_is_initialized())
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
        super::super::hrtimer::hrtimer_init();
        let tl = tickless_subsystem();
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
        super::super::hrtimer::hrtimer_init();
    }
}
