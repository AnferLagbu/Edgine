//! PIT (Programmable Interval Timer - 8254) 驱动
//!
//! 提供对 Intel 8254/8254-2 PIT 芯片的底层控制：
//! - **通道 0**: 系统定时器 (IRQ0)
//! - **频率配置**: 支持自定义中断频率
//! - **精确计时**: 基于硬件的可靠时间源
//!
//! ## 硬件规格
//!
//! ```text
//! I/O 端口:
//! ├── 0x40: Channel 0 Data (计数器值)
//! ├── 0x41: Channel 1 Data (DRAM 刷新, 已废弃)
//! ├── 0x42: Channel 2 Data (PC 扬声器)
//! └── 0x43: Command Register (控制字)
//!
//! 工作模式:
//! └── Mode 2: Rate Generator (周期性中断)
//! ```
//!
//! # Safety
//! 此模块直接操作硬件端口，必须在内核初始化早期调用。

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

// ============================================================================
// 硬件常量定义
// ============================================================================

/// PIT I/O 端口地址
const PIT_CHANNEL_0_DATA: u16 = 0x40; // 通道 0 数据端口
const PIT_COMMAND_PORT: u16 = 0x43; // 命令/控制寄存器

/// PIT 基础时钟频率 (1.193182 MHz)
pub const PIT_BASE_FREQUENCY: u64 = 1_193_182;

/// 默认中断频率 (1000 Hz = 1ms 间隔)
pub const DEFAULT_INTERRUPT_FREQ_HZ: u32 = 1000;

/// 最大分频值 (16位计数器)
pub const PIT_MAX_COUNT: u16 = 0xFFFF;

/// 最小分频值
pub const PIT_MIN_COUNT: u16 = 0x0001;

/// PIT 8254 控制字 — Intel 8253/8254 规范
///
/// 当前使用: `SELECT_CHANNEL_0`, `LATCH_COUNT`, `LO_HI`, `MODE_2_RATE_GENERATOR`
/// 规范定义的其余模式供参考:
///   `SELECT_CHANNEL`_{`1,2}、READ_BACK_COMMAND、LOW/HIGH_BYTE_ONLY`、
///   MODE_{`0,1,3,4,5}、BCD_MODE`
mod control_word {
    pub const SELECT_CHANNEL_0: u8 = 0x00;
    pub const LATCH_COUNT: u8 = 0x00;
    pub const LOW_HIGH_BYTE: u8 = 0x30;
    pub const MODE_2_RATE_GEN: u8 = 0x04;
    pub const BINARY_MODE: u8 = 0x00;
}

// ============================================================================
// 全局状态管理
// ============================================================================

/// PIT 初始化标志
static PIT_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// 当前配置的中断频率 (Hz)
static CURRENT_FREQ_HZ: AtomicU32 = AtomicU32::new(0);

/// 当前配置的分频计数值
static CURRENT_DIVISOR: AtomicU16 = AtomicU16::new(0);

/// 上次读取的计数值 (用于计算已用时间)
static LAST_TICK_COUNT: AtomicU16 = AtomicU16::new(0);

// ============================================================================
// 底层 I/O 操作
// ============================================================================

/// 向指定端口写入字节
///
/// # Safety
/// 必须在特权级执行，且端口地址有效。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
unsafe fn outb(port: u16, value: u8) {
    crate::arch!(outb(port, value));
}

/// 从指定端口读入字节
///
/// # Safety
/// 必须在特权级执行，且端口地址有效。
#[inline(always)]
unsafe fn inb(port: u16) -> u8 {
    crate::arch!(inb(port))
}

// ============================================================================
// PIT 核心功能
// ============================================================================

/// 初始化 PIT 定时器
///
/// 配置通道 0 为速率发生器模式，产生周期性中断。
///
/// # Arguments
/// * `frequency_hz` - 目标中断频率 (Hz), 推荐 100-10000 Hz
///
/// # Returns
/// * `Ok(u32)` - 实际配置的频率 (可能与请求值有微小偏差)
/// * `Err(&str)` - 错误描述
///
/// # Example
/// ```rust,no_run
/// let actual_freq = pit_init(1000).unwrap();  // 1ms 间隔
/// assert!((actual_freq - 1000).abs() < 5);     // 允许 ±5 Hz 误差
/// ```
///
/// # Errors
/// 当 `frequency_hz` 为 0 时返回 `Err("Frequency must be > 0")`;
/// 当频率超过 PIT 基准频率时返回 `Err("Frequency exceeds PIT maximum")`;
/// 当计算出的分频值超出合法计数范围时返回 `Err("Divisor out of range")`.
pub fn pit_init(frequency_hz: u32) -> Result<u32, &'static str> {
    if frequency_hz == 0 {
        return Err("Frequency must be > 0");
    }

    if frequency_hz > PIT_BASE_FREQUENCY as u32 {
        return Err("Frequency exceeds PIT maximum");
    }

    // 计算分频值
    let divisor = (PIT_BASE_FREQUENCY / u64::from(frequency_hz)) as u16;

    if !(PIT_MIN_COUNT..=PIT_MAX_COUNT).contains(&divisor) {
        return Err("Divisor out of range");
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        // 1. 发送控制字到命令寄存器
        //    - 选择通道 0
        //    - 先低字节后高字节访问
        //    - Mode 2: 速率发生器
        //    - 16位二进制模式
        let command: u8 = control_word::SELECT_CHANNEL_0
            | control_word::LOW_HIGH_BYTE
            | control_word::MODE_2_RATE_GEN
            | control_word::BINARY_MODE;

        outb(PIT_COMMAND_PORT, command);

        // 2. 发送分频值低字节
        outb(PIT_CHANNEL_0_DATA, (divisor & 0xFF) as u8);

        // 3. 发送分频值高字节
        outb(PIT_CHANNEL_0_DATA, ((divisor >> 8) & 0xFF) as u8);
    }

    // 更新全局状态
    let actual_freq = (PIT_BASE_FREQUENCY / u64::from(divisor)) as u32;
    CURRENT_FREQ_HZ.store(actual_freq, Ordering::Relaxed);
    CURRENT_DIVISOR.store(divisor, Ordering::Relaxed);
    LAST_TICK_COUNT.store(divisor, Ordering::Relaxed);

    // 标记为已初始化
    PIT_INITIALIZED.store(true, Ordering::Release);

    Ok(actual_freq)
}

/// 读取当前通道 0 的计数值
///
/// 返回距离下一次中断剩余的时钟周期数。
/// 注意：这是倒计数，值越小越接近下次中断。
///
/// # Returns
/// * `Some(u16)` - 当前计数值 (如果 PIT 已初始化)
/// * `None` - PIT 未初始化
pub fn pit_read_count() -> Option<u16> {
    if !PIT_INITIALIZED.load(Ordering::Acquire) {
        return None;
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        // 发送锁存命令 (读取当前计数值而不影响计数)
        let latch_cmd: u8 = control_word::SELECT_CHANNEL_0 | control_word::LATCH_COUNT;
        outb(PIT_COMMAND_PORT, latch_cmd);

        // 读取低字节
        let low = u16::from(inb(PIT_CHANNEL_0_DATA));

        // 读取高字节
        let high = u16::from(inb(PIT_CHANNEL_0_DATA));

        Some((high << 8) | low)
    }
}

/// 计算 PIT 自上次 tick 以来的微秒数
///
/// 用于更精确的时间测量（比 tick 粒度更细）。
///
/// # Returns
/// * `Some(u64)` - 微秒数 (如果 PIT 已初始化)
/// * `None` - PIT 未初始化
pub fn pit_elapsed_since_tick_us() -> Option<u64> {
    let current_count = pit_read_count()?;
    let last_count = LAST_TICK_COUNT.load(Ordering::Relaxed);
    let divisor = CURRENT_DIVISOR.load(Ordering::Relaxed);

    if divisor == 0 {
        return None;
    }

    // 计算已过去的时钟周期数
    let elapsed_cycles = if current_count <= last_count {
        last_count - current_count
    } else {
        // 发生了回绕
        last_count + (PIT_MAX_COUNT - current_count) + 1
    };

    // 转换为微秒
    // us = cycles * 1_000_000 / PIT_BASE_FREQUENCY
    let us = (u64::from(elapsed_cycles) * 1_000_000) / PIT_BASE_FREQUENCY;

    Some(us)
}

/// 在每次 IRQ0 中断时调用此函数更新内部状态
///
/// # Safety
/// 只能从中断处理程序调用
#[inline]
pub fn pit_on_interrupt() {
    let divisor = CURRENT_DIVISOR.load(Ordering::Relaxed);
    if divisor != 0 {
        LAST_TICK_COUNT.store(divisor, Ordering::Relaxed);
    }
}

/// 获取当前配置的频率
///
/// # Returns
/// * `Some(u32)` - 当前频率 (Hz)
/// * `None` - PIT 未初始化
pub fn pit_get_frequency() -> Option<u32> {
    if !PIT_INITIALIZED.load(Ordering::Acquire) {
        return None;
    }

    Some(CURRENT_FREQ_HZ.load(Ordering::Relaxed))
}

/// 检查 PIT 是否已初始化
pub fn pit_is_initialized() -> bool {
    PIT_INITIALIZED.load(Ordering::Acquire)
}

/// 重置 PIT 到安全状态
///
/// 通常用于关机或紧急恢复场景。
pub fn pit_shutdown() {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        // 设置为最大分频 (最低频率 ~18.2 Hz)
        let command: u8 = control_word::SELECT_CHANNEL_0
            | control_word::LOW_HIGH_BYTE
            | control_word::MODE_2_RATE_GEN
            | control_word::BINARY_MODE;

        outb(PIT_COMMAND_PORT, command);
        outb(PIT_CHANNEL_0_DATA, 0xFF); // 低字节
        outb(PIT_CHANNEL_0_DATA, 0xFF); // 高字节
    }

    PIT_INITIALIZED.store(false, Ordering::Release);
}

// ============================================================================
// 单元测试
// ============================================================================

// UT-07 (2026-09-26): timer::pit 双份注册副本已删 (本文件内联注册 fn 与
// privileged/tests/sys.rs 的 timer::pit 组) — 本 #[cfg(test)] 模块为唯一归属.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pit_constants() {
        assert_eq!(PIT_BASE_FREQUENCY, 1_193_182);
        assert!(DEFAULT_INTERRUPT_FREQ_HZ > 0);
        assert_eq!(PIT_MAX_COUNT, 65535);
        assert_eq!(PIT_MIN_COUNT, 1);
    }

    #[test]
    fn test_divisor_calculation() {
        // 1000 Hz → 分频值应为 1193
        let divisor = PIT_BASE_FREQUENCY / 1000;
        assert_eq!(divisor, 1193);

        // 验证实际频率接近目标
        // UT-07 (2026-09-26): 原单侧容差 (actual_freq - 1000) < 5 弱于注册侧
        // 双侧形态, 改回 .abs() 双侧以免收敛后覆盖降级.
        let actual_freq = PIT_BASE_FREQUENCY / divisor;
        assert!((actual_freq as i64 - 1000).abs() < 5);
    }

    #[test]
    fn test_frequency_bounds() {
        // 测试边界条件
        assert!(PIT_MIN_COUNT >= 1);
        // 恒真断言删除 (u16 max 恒 <= 65535); 与 J-01 对 registry 轨
        // frequency_bounds 的既有处置同型 (见本文件下方注释).

        // 最大频率 (最小分频)
        let max_freq = PIT_BASE_FREQUENCY / PIT_MIN_COUNT as u64;
        assert!(max_freq > 1_000_000); // > 1 MHz

        // 最小频率 (最大分频)
        let min_freq = PIT_BASE_FREQUENCY / PIT_MAX_COUNT as u64;
        assert!(min_freq < 20); // ~18.2 Hz
    }

    #[test]
    fn test_initialization_state() {
        // 初始状态应该是未初始化
        assert!(!pit_is_initialized());

        // 未初始化时应该返回 None
        assert!(pit_get_frequency().is_none());
        assert!(pit_read_count().is_none());
        assert!(pit_elapsed_since_tick_us().is_none());
    }
}
