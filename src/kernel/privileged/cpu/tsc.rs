//! TSC (Time Stamp Counter) 管理
//!
//! 提供高精度时间戳读取和频率校准。

/// 读取当前 TSC 值 (时钟周期数)
///
/// 通过 Arch trait 的 `timestamp()` 实现，架构无关。
/// 在 `x86_64` 上映射为 rdtsc，在 ARM64 上映射为 `CNTVCT_EL0` 读取。
///
/// # Returns
/// 自 CPU 复位以来的时钟周期数
///
/// # Note
/// 在 1GHz CPU 上, 1 个周期 ≈ 1 纳秒。
/// 在 3GHz CPU 上, 1 个周期 ≈ 0.33 纳秒。
#[inline(always)]
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
pub fn read_tsc() -> u64 {
    crate::arch!(timestamp())
}

/// 读取 TSC 并附带序列化 (防止乱序执行)
///
/// 比 `read_tsc()` 慢, 但结果更精确: 确保先前指令全部完成后才读数。
/// 适用于性能测量场景 (如 TSC 频率校准)。
///
/// # 实现 (2026-08-23 实装, B03-19 follow-up)
/// 走 `crate::arch!(timestamp_serialized())`:
/// - x86_64: `lfence` + `rdtsc` (参照 Linux `rdtsc_ordered`)
/// - aarch64: `isb` + `mrs cntpct_el0`
#[inline(always)]
pub fn read_tsc_serialized() -> u64 {
    crate::arch!(timestamp_serialized())
}
