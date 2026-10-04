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

/// 乘除辅助 (128 位中间值): 先乘后除, 避免 u64 中间乘法溢出。
///
/// 相比 `checked_mul` 饱和方案, 仅在**最终结果**超 u64::MAX 时才饱和截断
/// (物理上不可达的防御), 中间乘法溢出不再误返回 u64::MAX。
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "cast_possible_truncation: 已先 min(u64::MAX) 饱和, 截断不可能发生; 当前优先 expect"
)]
fn mul_div_saturating(value: u64, mul: u64, div: u64) -> u64 {
    let product = u128::from(value) * u128::from(mul);
    (product / u128::from(div)).min(u128::from(u64::MAX)) as u64
}

/// 将 TSC 周期转换为纳秒 (近似值)
///
/// # Arguments
/// * `tsc_cycles` - TSC 周期数
/// * `tsc_freq_mhz` - TSC 频率 (MHz)
///
/// # Returns
/// 近似纳秒数
///
/// # B03-19 修复
/// u128 中间值乘法, 中间溢出不再饱和; 仅在最终结果超 u64::MAX 时截断
/// (理论上 4GHz 运行 ~146 年才可达, 纯防御)。
#[inline]
pub fn cycles_to_nanoseconds(tsc_cycles: u64, tsc_freq_mhz: u64) -> u64 {
    if tsc_freq_mhz == 0 {
        return tsc_cycles; // 无法转换, 返回原值
    }
    mul_div_saturating(tsc_cycles, 1000, tsc_freq_mhz)
}

/// 将纳秒转换为 TSC 周期 (近似值)
#[inline]
pub fn nanoseconds_to_cycles(ns: u64, tsc_freq_mhz: u64) -> u64 {
    if tsc_freq_mhz == 0 {
        return ns;
    }
    // B03-19: u128 中间值防 u64 乘法溢出。
    mul_div_saturating(ns, tsc_freq_mhz, 1000)
}
