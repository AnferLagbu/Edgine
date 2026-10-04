//! 电源管理 — privileged 机制实现
//!
//! ## DECISION-M 归属反转记录 (2026-09-13)
//!
//! 原策略代码于 T4-4 (2026-06-16) 迁至 `functions::driver::power`, 本文件仅 re-export。
//! 按"机制持有的数据结构/常量归 privileged"统一判据反转：`PM_SUBSYSTEM: PmSubsystem`
//! 全局实例由 privileged 机制持有 (`pm_init/pm_idle/pm_suspend/sys_pm` 直接操作),
//! governor/C-state 算法全部操作 PmSubsystem 内部字段 (per_cpu_stats/governor/
//! thresholds/per_cpu_freq_idx/notifiers), 无法脱离机制状态独立存在。
//! `sys_pm_dispatch` 为 cmd→方法映射 (空转转发, 被调方法全在本文件), 无独立策略业务
//! — 与 IpcStrategy 的关键区别: IpcStrategy 有真实业务算法 (pipe/shm/msgq) 需 trait
//! 注入; 此处 trait 注入=空转抽象。裁决: DECISION-M 方案 B — 15 类型 + dispatch
//! 迁回 privileged, functions 改 re-export 壳。
//!
//! functions 侧改 `pub use crate::privileged::driver::power::*` 保持 API 兼容。
//! 演进预留: 未来 governor 独立化再下沉, 不堵死。
//!
//! ## 职责
//!
//! - 全局电源管理子系统 (PM_SUBSYSTEM): C-state 空闲 + DVFS 频率 + Suspend/Resume
//! - `sys_pm` syscall 分发
//! - 架构相关硬件操作 (hlt/wfi/rdtsc/arch_shutdown)

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use alloc::vec;
use alloc::vec::Vec;

use crate::privileged::sync::IrqSpinLock;

// ============================================================================
// 常量
// ============================================================================

/// 最大 C-state 数量
pub const MAX_CSTATES: usize = 4;
/// 最大频率等级
pub const MAX_FREQ_LEVELS: usize = 8;
/// 最大 CPU 数量 (与 config 对齐)
pub const MAX_PM_CPUS: usize = 256;

// ============================================================================
// CpuIdle — CPU 空闲状态管理
// ============================================================================

/// CPU 空闲状态 (C-state)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CpuIdleState {
    /// C0: 运行状态
    C0Running = 0,
    /// C1: 停止时钟 (hlt/wfi), 延迟 < 1us
    C1Halt = 1,
    /// C2: 深度停止, 延迟 < 100us
    C2DeepHalt = 2,
    /// C3: 最深睡眠, 延迟 < 1ms
    C3Sleep = 3,
}

impl CpuIdleState {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::C1Halt,
            2 => Self::C2DeepHalt,
            3 => Self::C3Sleep,
            _ => Self::C0Running,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 进入该 C-state 的预期延迟 (微秒)
    pub fn latency_us(&self) -> u32 {
        match self {
            Self::C0Running => 0,
            Self::C1Halt => 1,
            Self::C2DeepHalt => 50,
            Self::C3Sleep => 500,
        }
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 该 C-state 的功耗节省比例 (0-100)
    pub fn power_saving(&self) -> u32 {
        match self {
            Self::C0Running => 0,
            Self::C1Halt => 40,
            Self::C2DeepHalt => 70,
            Self::C3Sleep => 90,
        }
    }
}

/// Per-CPU 空闲统计
#[derive(Debug)]
pub struct CpuIdleStats {
    /// 当前 C-state
    pub current_state: AtomicU32,
    /// 各状态累计停留时间 (毫秒)
    pub time_per_state: [AtomicU64; MAX_CSTATES],
    /// 各状态进入次数
    pub entry_count: [AtomicU64; MAX_CSTATES],
    /// 上次进入空闲的时间戳
    pub last_idle_entry: AtomicU64,
    /// 最深允许的 C-state
    pub max_cstate: AtomicU32,
}

impl CpuIdleStats {
    pub fn new() -> Self {
        Self {
            current_state: AtomicU32::new(CpuIdleState::C0Running as u32),
            time_per_state: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            entry_count: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            last_idle_entry: AtomicU64::new(0),
            max_cstate: AtomicU32::new(CpuIdleState::C2DeepHalt as u32),
        }
    }

    /// 进入空闲状态 (策略部分: 更新统计)
    pub fn enter_idle(&self, state: CpuIdleState) {
        let max = self.max_cstate.load(Ordering::Acquire);
        let effective = if state as u32 > max {
            CpuIdleState::from_u32(max)
        } else {
            state
        };
        self.current_state
            .store(effective as u32, Ordering::Release);
        self.entry_count[effective as usize].fetch_add(1, Ordering::Relaxed);
        // 时间戳由 privileged 的 read_timestamp() 设置
    }

    /// 设置进入空闲的时间戳
    pub fn set_idle_entry_ts(&self, ts: u64) {
        self.last_idle_entry.store(ts, Ordering::Release);
    }

    /// 退出空闲状态 (策略部分: 更新统计)
    pub fn exit_idle(&self, elapsed_ms: u64) {
        let prev = self
            .current_state
            .swap(CpuIdleState::C0Running as u32, Ordering::AcqRel);
        self.time_per_state[prev as usize].fetch_add(elapsed_ms, Ordering::Relaxed);
    }
}

/// CPU 空闲驱动 — 选择并进入最优 C-state
pub struct CpuIdleDriver {
    /// Per-CPU 空闲统计
    pub per_cpu_stats: IrqSpinLock<Vec<CpuIdleStats>>,
    /// 是否启用 idle governor
    pub enabled: AtomicBool,
}

impl CpuIdleDriver {
    pub const fn new() -> Self {
        Self {
            per_cpu_stats: IrqSpinLock::new(Vec::new()),
            enabled: AtomicBool::new(false),
        }
    }

    /// 初始化 (为每个 CPU 创建统计)
    pub fn init(&self, num_cpus: u32) {
        let mut stats = self.per_cpu_stats.lock();
        for _ in 0..num_cpus {
            stats.push(CpuIdleStats::new());
        }
        self.enabled.store(true, Ordering::Release);
    }

    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 选择最优 C-state (策略部分, 不执行 halt)
    pub fn select_cstate(&self, cpu_id: u32) -> Option<CpuIdleState> {
        if !self.enabled.load(Ordering::Acquire) {
            return Some(CpuIdleState::C1Halt);
        }
        let stats = self.per_cpu_stats.lock();
        if (cpu_id as usize) >= stats.len() {
            return Some(CpuIdleState::C1Halt);
        }
        let max_cstate = stats[cpu_id as usize].max_cstate.load(Ordering::Acquire);
        let state = CpuIdleState::from_u32(max_cstate.min(2));
        Some(state)
    }

    /// 获取 CPU 空闲统计
    pub fn get_stats(&self, cpu_id: u32) -> Option<CpuIdleStats> {
        let stats = self.per_cpu_stats.lock();
        stats.get(cpu_id as usize).map(|s| {
            let copy = CpuIdleStats::new();
            copy.current_state
                .store(s.current_state.load(Ordering::Acquire), Ordering::Release);
            for i in 0..MAX_CSTATES {
                copy.time_per_state[i].store(
                    s.time_per_state[i].load(Ordering::Acquire),
                    Ordering::Release,
                );
                copy.entry_count[i]
                    .store(s.entry_count[i].load(Ordering::Acquire), Ordering::Release);
            }
            copy.last_idle_entry
                .store(s.last_idle_entry.load(Ordering::Acquire), Ordering::Release);
            copy.max_cstate
                .store(s.max_cstate.load(Ordering::Acquire), Ordering::Release);
            copy
        })
    }

    /// 设置最深 C-state
    pub fn set_max_cstate(&self, cpu_id: u32, max: CpuIdleState) {
        let stats = self.per_cpu_stats.lock();
        if (cpu_id as usize) < stats.len() {
            stats[cpu_id as usize]
                .max_cstate
                .store(max as u32, Ordering::Release);
        }
    }
}

// ============================================================================
// CpuFreq — CPU 频率调节
// ============================================================================

/// 频率调节策略 (Governor)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FreqGovernor {
    /// 性能优先: 始终最高频率
    Performance = 0,
    /// 省电优先: 始终最低频率
    Powersave = 1,
    /// 按需调节: 根据负载自动调频
    Ondemand = 2,
}

impl FreqGovernor {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Performance,
            1 => Self::Powersave,
            2 => Self::Ondemand,
            _ => Self::Performance,
        }
    }
}

/// 频率等级
#[derive(Debug, Clone, Copy)]
pub struct FreqLevel {
    /// 频率 (MHz)
    pub freq_mhz: u32,
    /// 电压 (mV)
    pub voltage_mv: u32,
}

/// CPU 频率驱动
pub struct CpuFreqDriver {
    /// 可用频率等级表 (从高到低排序)
    pub freq_table: IrqSpinLock<Vec<FreqLevel>>,
    /// Per-CPU 当前频率索引
    pub per_cpu_freq_idx: IrqSpinLock<Vec<AtomicU32>>,
    /// 当前 Governor
    pub governor: IrqSpinLock<FreqGovernor>,
    /// Ondemand 参数: 升频阈值 (负载百分比)
    pub up_threshold: AtomicU32,
    /// Ondemand 参数: 降频阈值
    pub down_threshold: AtomicU32,
    /// 是否启用
    pub enabled: AtomicBool,
}

impl CpuFreqDriver {
    pub const fn new() -> Self {
        Self {
            freq_table: IrqSpinLock::new(Vec::new()),
            per_cpu_freq_idx: IrqSpinLock::new(Vec::new()),
            governor: IrqSpinLock::new(FreqGovernor::Performance),
            up_threshold: AtomicU32::new(80),
            down_threshold: AtomicU32::new(20),
            enabled: AtomicBool::new(false),
        }
    }

    /// 初始化 (设置频率表)
    pub fn init(&self, freq_table: Vec<FreqLevel>, num_cpus: u32) {
        *self.freq_table.lock() = freq_table;
        let mut indices = self.per_cpu_freq_idx.lock();
        for _ in 0..num_cpus {
            indices.push(AtomicU32::new(0));
        }
        self.enabled.store(true, Ordering::Release);
    }

    /// 用默认频率表初始化 (单频点)
    pub fn init_default(&self, base_freq_mhz: u32, num_cpus: u32) {
        let table = vec![FreqLevel {
            freq_mhz: base_freq_mhz,
            voltage_mv: 1000,
        }];
        self.init(table, num_cpus);
    }

    /// 获取当前频率
    pub fn get_freq(&self, cpu_id: u32) -> u32 {
        let table = self.freq_table.lock();
        let indices = self.per_cpu_freq_idx.lock();
        if table.is_empty() || (cpu_id as usize) >= indices.len() {
            return 0;
        }
        let idx = indices[cpu_id as usize].load(Ordering::Acquire) as usize;
        if idx < table.len() {
            table[idx].freq_mhz
        } else {
            0
        }
    }

    /// 设置目标频率 (策略: 找最接近的频率等级)
    pub fn set_freq(&self, cpu_id: u32, target_mhz: u32) -> bool {
        let table = self.freq_table.lock();
        if table.is_empty() {
            return false;
        }
        let mut best_idx = 0;
        let mut best_diff = u32::MAX;
        for (i, level) in table.iter().enumerate() {
            let diff = (level.freq_mhz as i32 - target_mhz as i32).unsigned_abs();
            if diff < best_diff {
                best_diff = diff;
                best_idx = i;
            }
        }
        let best_freq_mhz = table[best_idx].freq_mhz;
        drop(table);

        let indices = self.per_cpu_freq_idx.lock();
        if (cpu_id as usize) >= indices.len() {
            return false;
        }
        indices[cpu_id as usize].store(best_idx as u32, Ordering::Release);
        drop(indices);

        // 软件表更新后下写硬件调频寄存器 (x86_64 MSR; 其他架构/host-test no-op)
        apply_hw_freq(cpu_id, best_freq_mhz);
        true
    }

    /// 设置 Governor (策略)
    pub fn set_governor(&self, governor: FreqGovernor) {
        *self.governor.lock() = governor;
        match governor {
            FreqGovernor::Performance => {
                let indices = self.per_cpu_freq_idx.lock();
                for idx in indices.iter() {
                    idx.store(0, Ordering::Release);
                }
            }
            FreqGovernor::Powersave => {
                let table = self.freq_table.lock();
                let last = if table.is_empty() { 0 } else { table.len() - 1 };
                drop(table);
                let indices = self.per_cpu_freq_idx.lock();
                for idx in indices.iter() {
                    idx.store(last as u32, Ordering::Release);
                }
            }
            FreqGovernor::Ondemand => {}
        }
    }

    /// 获取当前 Governor
    pub fn get_governor(&self) -> FreqGovernor {
        *self.governor.lock()
    }

    /// Ondemand 调频检查 (由调度器 tick 调用)
    pub fn ondemand_check(&self, cpu_id: u32, load_percent: u32) {
        let gov = *self.governor.lock();
        if gov != FreqGovernor::Ondemand {
            return;
        }
        let up = self.up_threshold.load(Ordering::Acquire);
        let down = self.down_threshold.load(Ordering::Acquire);

        if load_percent > up {
            let indices = self.per_cpu_freq_idx.lock();
            if (cpu_id as usize) < indices.len() {
                let cur = indices[cpu_id as usize].load(Ordering::Acquire);
                if cur > 0 {
                    indices[cpu_id as usize].store(cur - 1, Ordering::Release);
                }
            }
        } else if load_percent < down {
            let table = self.freq_table.lock();
            let max_idx = if table.is_empty() { 0 } else { table.len() - 1 };
            drop(table);
            let indices = self.per_cpu_freq_idx.lock();
            if (cpu_id as usize) < indices.len() {
                let cur = indices[cpu_id as usize].load(Ordering::Acquire);
                if (cur as usize) < max_idx {
                    indices[cpu_id as usize].store(cur + 1, Ordering::Release);
                }
            }
        }
    }
}

// ============================================================================
// 调频硬件下写 — 频率控制机制
// ============================================================================

/// 将目标频率下写到硬件调频寄存器 (调频机制, 由 `set_freq` 在软件表更新后调用)。
///
/// x86_64: 写 `IA32_PERF_CTL` (0x199)。bits 15:0 为 P-state 控制值,
/// bits 15:8 = 目标比率, 以 100 MHz 基准时钟为单位折算。
/// aarch64 / host-test: 无统一调频 MSR (aarch64 走 PSCI/CPPC 平台协议;
/// host-test 执行 `wrmsr` 属特权指令会触发 #GP), 保持软件表权威 (no-op)。
///
/// # 失败语义
/// 硬件写失败不影响 `get_freq` 结果 (软件表仍为权威), 仅记录日志。
// SIMPLIFIED: P-state 比率按 100 MHz 基准时钟折算 (freq_mhz / 100 << 8), 仅适配
//             现代 Intel/AMD 主流约定; 影响面 = 非 100 MHz 基准时钟的硬件上
//             比率偏差 (软件表权威, get_freq 不受影响); 待接入 CPUID 基准时钟
//             探测后收敛编码.
#[cfg(all(target_arch = "x86_64", not(feature = "host-test")))]
fn apply_hw_freq(cpu_id: u32, freq_mhz: u32) {
    let ratio = (freq_mhz / 100).min(0xFF);
    let value = u64::from(ratio) << 8;
    // SAFETY: 真实内核态 (Ring 0) 下写 IA32_PERF_CTL; host-test 已被 cfg 排除,
    // 非法/不支持的 MSR 由 CPU 以 #GP 异常拒收 (与 `cpu_write_msr` 白名单同语义).
    unsafe {
        crate::privileged::cpu::msr::write_msr(0x199, value);
    }
    crate::klog_ffi!(
        klog_ffi_info,
        "[PM] CPU{} freq -> {} MHz (IA32_PERF_CTL ratio={})",
        cpu_id,
        freq_mhz,
        ratio
    );
}

/// 非 x86_64 或 host-test 变体: 软件表已更新, 硬件写 no-op。
#[cfg(not(all(target_arch = "x86_64", not(feature = "host-test"))))]
fn apply_hw_freq(cpu_id: u32, freq_mhz: u32) {
    let _ = (cpu_id, freq_mhz);
}

// ============================================================================
// Suspend/Resume — 系统挂起与恢复
// ============================================================================

/// 系统电源状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SystemPowerState {
    /// S0: 工作状态
    S0Working = 0,
    /// S3: 挂起到内存 (Suspend-to-RAM)
    S3SuspendToRam = 3,
    /// S5: 软关机
    S5SoftOff = 5,
}

impl SystemPowerState {
    pub fn from_u32(v: u32) -> Self {
        match v {
            3 => Self::S3SuspendToRam,
            5 => Self::S5SoftOff,
            _ => Self::S0Working,
        }
    }
}

/// 挂起/恢复回调
pub type SuspendCallback = fn() -> i32;
pub type ResumeCallback = fn() -> i32;

/// 挂起/恢复通知器
pub struct SuspendNotifier {
    pub name: [u8; 16],
    pub suspend: SuspendCallback,
    pub resume: ResumeCallback,
    pub priority: u32,
}

/// 系统电源管理
pub struct PmSubsystem {
    /// 当前系统电源状态
    pub state: AtomicU32,
    /// 挂起通知器列表
    pub notifiers: IrqSpinLock<Vec<SuspendNotifier>>,
    /// `CpuIdle` 驱动
    pub cpuidle: CpuIdleDriver,
    /// `CpuFreq` 驱动
    pub cpufreq: CpuFreqDriver,
    /// 是否已初始化
    pub initialized: AtomicBool,
}

impl PmSubsystem {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(SystemPowerState::S0Working as u32),
            notifiers: IrqSpinLock::new(Vec::new()),
            cpuidle: CpuIdleDriver::new(),
            cpufreq: CpuFreqDriver::new(),
            initialized: AtomicBool::new(false),
        }
    }

    /// 初始化电源管理子系统
    pub fn init(&self, num_cpus: u32) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        self.cpuidle.init(num_cpus);
        self.cpufreq.init_default(2000, num_cpus);
        self.initialized.store(true, Ordering::Release);
    }

    /// 注册挂起通知器
    pub fn register_notifier(&self, notifier: SuspendNotifier) {
        let mut notifiers = self.notifiers.lock();
        notifiers.push(notifier);
        notifiers.sort_by_key(|n| n.priority);
    }

    /// 挂起系统 (策略部分: 通知 + 状态管理, 不含硬件操作)
    ///
    /// 返回 Ok(()) 表示应执行硬件挂起, Err(i64) 表示失败.
    /// 恢复后需调用 `suspend_resume_notify()`.
    ///
    /// # Errors
    ///
    /// - 电源管理子系统尚未初始化时返回 `-11` (EAGAIN)
    /// - 当前系统状态不是 S0 (工作中) 时返回 `-16` (EBUSY)
    /// - 任一挂起通知器返回非 0 时返回 `-5` (EIO)
    pub fn suspend_prepare(&self, target: SystemPowerState) -> Result<(), i64> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(-(11i64)); // EAGAIN
        }

        let current = SystemPowerState::from_u32(self.state.load(Ordering::Acquire));
        if current != SystemPowerState::S0Working {
            return Err(-(16i64)); // EBUSY
        }

        // 1. 通知所有注册者 (按优先级)
        let notifiers = self.notifiers.lock();
        for n in notifiers.iter() {
            let ret = (n.suspend)();
            if ret != 0 {
                drop(notifiers);
                return Err(-(5i64)); // EIO
            }
        }
        drop(notifiers);

        // 2. 更新状态
        self.state.store(target as u32, Ordering::Release);
        Ok(())
    }

    /// 挂起恢复后通知
    pub fn suspend_resume_notify(&self) {
        self.state
            .store(SystemPowerState::S0Working as u32, Ordering::Release);
        let notifiers = self.notifiers.lock();
        for n in notifiers.iter() {
            (n.resume)();
        }
    }

    /// 获取当前电源状态
    pub fn get_state(&self) -> SystemPowerState {
        SystemPowerState::from_u32(self.state.load(Ordering::Acquire))
    }
}

// ============================================================================
// 全局实例
// ============================================================================

/// 全局电源管理子系统
static PM_SUBSYSTEM: PmSubsystem = PmSubsystem::new();

/// 初始化电源管理
pub fn pm_init(num_cpus: u32) {
    PM_SUBSYSTEM.init(num_cpus);
    crate::klog_ffi!(
        klog_ffi_info,
        "[PM] subsystem initialized: {} CPUs, default 2000 MHz",
        num_cpus
    );
}

/// 获取全局 PM 子系统
pub fn pm_subsystem() -> &'static PmSubsystem {
    &PM_SUBSYSTEM
}

/// PM 是否已初始化
pub fn pm_is_initialized() -> bool {
    PM_SUBSYSTEM.initialized.load(Ordering::Acquire)
}

// ============================================================================
// 硬件操作 (unsafe)
// ============================================================================

/// 读取时间戳 (TSC/cntvct)
pub fn read_timestamp() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: rdtsc 是用户态安全指令
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let cnt: u64;
        // SAFETY: cntvct_el0 是 EL0 可读虚拟计数器
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) cnt) };
        cnt
    }
}

/// 架构相关 halt (C1)
pub fn arch_halt() {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: hlt 使 CPU 进入 C1 直到中断, 中断已使能
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: wfi 等待中断, 中断已使能
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

#[expect(
    clippy::similar_names,
    reason = "similar_names: 变量名相似表达同族概念; 当前优先 expect"
)]
/// CPU 进入空闲 (调度器 idle 调用)
///
/// 组合策略选择 (functions) + 硬件 halt (privileged)
pub fn pm_idle(cpu_id: u32) {
    let state = PM_SUBSYSTEM
        .cpuidle
        .select_cstate(cpu_id)
        .unwrap_or(CpuIdleState::C1Halt);

    let stats = PM_SUBSYSTEM.cpuidle.per_cpu_stats.lock();
    if (cpu_id as usize) < stats.len() {
        stats[cpu_id as usize].enter_idle(state);
        stats[cpu_id as usize].set_idle_entry_ts(read_timestamp());
    }
    drop(stats);

    // 执行架构相关的 halt
    arch_halt();

    // 被中断唤醒, 退出空闲
    let stats = PM_SUBSYSTEM.cpuidle.per_cpu_stats.lock();
    if (cpu_id as usize) < stats.len() {
        let entry_ts = stats[cpu_id as usize]
            .last_idle_entry
            .load(Ordering::Acquire);
        let elapsed_ms = if entry_ts > 0 {
            (read_timestamp() - entry_ts) / 1_000_000
        } else {
            0
        };
        stats[cpu_id as usize].exit_idle(elapsed_ms);
    }
}

#[expect(
    clippy::match_wildcard_for_single_variants,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// 挂起系统 (组合策略 + 硬件操作)
pub fn pm_suspend(target: SystemPowerState) -> i64 {
    if let Err(e) = PM_SUBSYSTEM.suspend_prepare(target) {
        return e;
    }

    crate::klog_ffi!(klog_ffi_info, "[PM] suspending to S{}...", target as u32);

    // 执行架构相关挂起
    match target {
        SystemPowerState::S3SuspendToRam => {
            arch_suspend_to_ram();
        }
        SystemPowerState::S5SoftOff => {
            arch_shutdown();
        }
        _ => {}
    }

    // 恢复后通知
    PM_SUBSYSTEM.suspend_resume_notify();
    0
}

/// 架构相关: 挂起到内存
fn arch_suspend_to_ram() {
    crate::klog_ffi!(klog_ffi_info, "[PM] S3: entering halt loop (placeholder)");
    loop {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: hlt 等待中断唤醒
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: wfi 等待中断唤醒
            unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
        }
    }
}

/// 架构相关: 关机
fn arch_shutdown() {
    crate::privileged::driver::shutdown_all();
}

// ============================================================================
// 系统调用
// ============================================================================

/// `sys_pm` — 电源管理系统调用 (分发)
///
/// `cmd`: 0=挂起, 1=获取状态, 2=设置调速器, 3=获取调速器,
///        4=设置最大C态, 5=获取频率, 6=设置频率
pub fn sys_pm_dispatch(pm: &PmSubsystem, cmd: u64, a1: u64, a2: u64) -> i64 {
    if !pm.initialized.load(Ordering::Acquire) {
        return -(11i64);
    }

    match cmd {
        0 => {
            let target = SystemPowerState::from_u32(a1 as u32);
            // 挂起需要硬件操作
            pm_suspend(target)
        }
        1 => pm.get_state() as i64,
        2 => {
            let gov = FreqGovernor::from_u32(a1 as u32);
            pm.cpufreq.set_governor(gov);
            0
        }
        3 => pm.cpufreq.get_governor() as i64,
        4 => {
            let cpu = a1 as u32;
            let max = CpuIdleState::from_u32(a2 as u32);
            pm.cpuidle.set_max_cstate(cpu, max);
            0
        }
        5 => i64::from(pm.cpufreq.get_freq(a1 as u32)),
        6 => {
            if pm.cpufreq.set_freq(a1 as u32, a2 as u32) {
                0
            } else {
                -(22i64) // EINVAL
            }
        }
        _ => -(38i64), // ENOSYS
    }
}

/// `sys_pm` — 电源管理系统调用 (FFI 导出)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sys_pm(cmd: u64, a1: u64, a2: u64) -> i64 {
    sys_pm_dispatch(&PM_SUBSYSTEM, cmd, a1, a2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `set_freq` 软件表: 就近匹配 + 索引更新 + `get_freq` 可观测;
    /// host-test 下 `apply_hw_freq` 为 no-op, 不触碰特权指令.
    #[test]
    fn set_freq_software_table_and_observability() {
        let driver = CpuFreqDriver::new();
        driver.init(
            vec![
                FreqLevel {
                    freq_mhz: 3000,
                    voltage_mv: 1200,
                },
                FreqLevel {
                    freq_mhz: 2500,
                    voltage_mv: 1100,
                },
                FreqLevel {
                    freq_mhz: 2000,
                    voltage_mv: 1000,
                },
            ],
            2,
        );

        // 就近匹配: 2600 → 2500 档
        assert!(driver.set_freq(0, 2600));
        assert_eq!(driver.get_freq(0), 2500);
        // 目标低于最低档 → 就近最低
        assert!(driver.set_freq(0, 500));
        assert_eq!(driver.get_freq(0), 2000);
        // 目标高于最高档 → 就近最高
        assert!(driver.set_freq(0, 5000));
        assert_eq!(driver.get_freq(0), 3000);
        // cpu_id 越界 → false
        assert!(!driver.set_freq(99, 2500));
        // 未初始化 (空表) → false
        let empty = CpuFreqDriver::new();
        assert!(!empty.set_freq(0, 2500));
    }
}
