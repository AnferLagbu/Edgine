//! ARM Generic Timer 配置
//!
//! ARMv8-A 架构提供 Generic Timer (CNTPCT, CNTFRQ, CNTP_TVAL, CNTP_CTL)。
//! QEMU virt 机器默认频率约 62.5MHz。

use core::arch::asm;

/// 默认定时器频率 (QEMU virt 可配置, 62.5MHz 为常见值)
const TIMER_FREQ_HZ: u64 = 62_500_000;

/// 读取当前计数器频率
#[inline(always)]
pub fn read_frequency() -> u64 {
    let freq: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) freq);
    }
    freq
}

/// 设置定时器到期值 (倒计时)
///
/// 向 CNTP_TVAL_EL0 写入 ticks 后触发定时器中断。
#[inline(always)]
pub fn set_timer(ticks: u64) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        asm!("msr cntp_tval_el0, {}", in(reg) ticks);
    }
}

/// 设置定时器比较值 (绝对时间)
///
/// 向 CNTP_CVAL_EL0 写入绝对计数值, 达到时触发中断。
#[inline(always)]
pub fn set_compare(value: u64) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        asm!("msr cntp_cval_el0, {}", in(reg) value);
    }
}

/// 读取当前计数 (CNTPCT_EL0)
#[inline(always)]
pub fn read_count() -> u64 {
    let cnt: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        asm!("mrs {}, cntpct_el0", out(reg) cnt, options(nomem, nostack));
    }
    cnt
}

/// 使能定时器 (CNTP_CTL_EL0.ENABLE = 1, IMASK = 0)
#[inline(always)]
pub fn enable() {
    // bit 0: ENABLE, bit 1: IMASK (0=enabled, 1=masked)
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        asm!("msr cntp_ctl_el0, {}", in(reg) 1u64);
    }
}

/// 禁用定时器
#[inline(always)]
pub fn disable() {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        asm!("msr cntp_ctl_el0, {}", in(reg) 0u64);
    }
}

/// 读取定时器控制寄存器
#[inline(always)]
pub fn read_control() -> u64 {
    let ctl: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        asm!("mrs {}, cntp_ctl_el0", out(reg) ctl);
    }
    ctl
}

/// 初始化定时器子系统 (延迟启动)
///
/// 读取 CNTFRQ_EL0, 计算 10ms 间隔, 但不启动定时器。
/// 返回 (频率, 间隔ticks)。稍后调用 start_interval() 开始计时。
pub fn init_deferred() -> (u64, u64) {
    let freq = read_frequency();
    let freq = if freq == 0 {
        // QEMU 某些版本可能未设置 CNTFRQ, 使用默认值 62.5MHz
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!("msr cntfrq_el0, {}", in(reg) TIMER_FREQ_HZ);
        }
        TIMER_FREQ_HZ
    } else {
        freq
    };

    // 10ms 间隔
    let ticks_ms = freq / 1000;
    let interval = ticks_ms * 10;
    (freq, interval)
}

/// 启动周期性定时器 (必须在 GIC + 调度器初始化之后调用)
///
/// 设置 CNTP_TVAL_EL0 并启用 CNTP_CTL_EL0。
pub fn start_interval(interval_ticks: u64) {
    set_timer(interval_ticks);
    enable();
}

/// 重新装载定时器 (在中断处理中调用)
///
/// 重新设置 CNTP_TVAL_EL0, 确保下一个中断准时触发。
pub fn reload(interval_ticks: u64) {
    set_timer(interval_ticks);
    enable();
}

/// 设置下一次定时器中断 (毫秒)
pub fn set_timeout_ms(ms: u64) {
    let freq = read_frequency();
    if freq == 0 {
        return;
    }
    let ticks = freq / 1000 * ms;
    set_timer(ticks);
    enable();
}
