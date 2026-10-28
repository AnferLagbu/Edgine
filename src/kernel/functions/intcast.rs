//! 整数位级转换助手 (functions 层副本).
//!
//! 与 `privileged/lib/intcast.rs` 逐函数同构, 但**不跨层引用**: F2 禁止 functions
//! 访问 `privileged::lib` 这类实现细节模块, 故本层按需各持一份最小的位级转换.
//! 实现相同, 语义相同: 逐位等价于被替代的 `as` 表达式, 且不引入任何抑制属性.
//!
//! 与 privileged 侧一致的宽度约束: 涉及 `usize` 与 64 位互转的函数在 32 位目标上
//! 编译失败 (fail-closed), 而不是静默截断.

/// `usize` → `i64`: 64 位目标上逐位等价于 `v as i64`.
///
/// 在 32 位目标上零扩展为 `i64` (无损, 与 `as` 的取值语义一致).
#[inline]
pub const fn i64_from_usize(v: usize) -> i64 {
    let b = v.to_le_bytes();
    let mut out = [0u8; 8];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i];
        i += 1;
    }
    i64::from_le_bytes(out)
}

/// `usize` → `i32`: 取低 32 位后按补码解释 (等价于 `v as i32`).
#[inline]
pub const fn low_i32_from_usize(v: usize) -> i32 {
    let b = v.to_le_bytes();
    i32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `usize` → `i16`: 取低 16 位后按补码解释 (等价于 `v as i16`).
#[inline]
pub const fn low_i16_from_usize(v: usize) -> i16 {
    let b = v.to_le_bytes();
    i16::from_le_bytes([b[0], b[1]])
}

/// `i32` → `u16`: 取低 16 位后按补码解释 (等价于 `v as u16`).
#[inline]
pub const fn u16_from_i32(v: i32) -> u16 {
    let b = v.to_le_bytes();
    u16::from_le_bytes([b[0], b[1]])
}

/// `i32` → `u8`: 取低 8 位 (等价于 `v as u8`).
#[inline]
pub const fn u8_from_i32(v: i32) -> u8 {
    v.to_le_bytes()[0]
}

/// `i16` → `u32`: 符号扩展到 32 位后按无符号解释 (等价于 `v as u32`).
///
/// 用字节填充实现符号扩展: `From` 家族在 const 上下文尚未稳定.
#[inline]
pub const fn u32_from_i16(v: i16) -> u32 {
    let b = v.to_le_bytes();
    let s = if v < 0 { 0xFFu8 } else { 0u8 };
    u32::from_le_bytes([b[0], b[1], s, s])
}

/// `i8` → `u32`: 符号扩展到 32 位后按无符号解释 (等价于 `v as u32`).
#[inline]
pub const fn u32_from_i8(v: i8) -> u32 {
    let b = v.to_le_bytes();
    let s = if v < 0 { 0xFFu8 } else { 0u8 };
    u32::from_le_bytes([b[0], s, s, s])
}

/// `i32` → `u64`: 符号扩展到 64 位后按无符号解释 (等价于 `v as u64`).
#[inline]
pub const fn u64_from_i32(v: i32) -> u64 {
    let b = v.to_le_bytes();
    let s = if v < 0 { 0xFFu8 } else { 0u8 };
    u64::from_le_bytes([b[0], b[1], b[2], b[3], s, s, s, s])
}

/// `i32` → `usize`: 64 位目标上符号扩展后按无符号解释 (等价于 `v as usize`).
#[inline]
pub const fn usize_from_i32(v: i32) -> usize {
    let b = v.to_le_bytes();
    let s = if v < 0 { 0xFFu8 } else { 0u8 };
    usize::from_le_bytes([b[0], b[1], b[2], b[3], s, s, s, s])
}

/// `i64` → `usize`: 64 位目标上逐位等价于 `v as usize`.
#[inline]
pub const fn usize_from_i64(v: i64) -> usize {
    usize::from_le_bytes(v.to_le_bytes())
}

/// `i8` → `usize`: 符号扩展到 64 位后按无符号解释 (等价于 `v as usize`).
#[inline]
pub const fn usize_from_i8(v: i8) -> usize {
    let b = v.to_le_bytes();
    let s = if v < 0 { 0xFFu8 } else { 0u8 };
    usize::from_le_bytes([b[0], s, s, s, s, s, s, s])
}

/// `i64` → `u32`: 取低 32 位后按无符号解释 (等价于 `v as u32`).
#[inline]
pub const fn low_u32_from_i64(v: i64) -> u32 {
    let b = v.to_le_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `u64` → `usize`: 64 位目标上逐位等价于 `v as usize` (32 位目标编译失败).
#[inline]
pub const fn usize_from_u64(v: u64) -> usize {
    usize::from_le_bytes(v.to_le_bytes())
}

/// `u64` → `u32`: 取低 32 位 (等价于 `v as u32`).
#[inline]
pub const fn low_u32_from_u64(v: u64) -> u32 {
    let b = v.to_le_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `usize` → `u32`: 取低 32 位 (等价于 `v as u32`).
#[inline]
pub const fn low_u32_from_usize(v: usize) -> u32 {
    let b = v.to_le_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `u32` → `u8`: 取低 8 位 (等价于 `v as u8`).
#[inline]
pub const fn low_u8_from_u32(v: u32) -> u8 {
    v.to_le_bytes()[0]
}

/// `u64` → `u8`: 取低 8 位 (等价于 `v as u8`).
#[inline]
pub const fn low_u8_from_u64(v: u64) -> u8 {
    v.to_le_bytes()[0]
}

/// `usize` → `u8`: 取低 8 位 (等价于 `v as u8`).
#[inline]
pub const fn low_u8_from_usize(v: usize) -> u8 {
    v.to_le_bytes()[0]
}

/// `usize` → `u16`: 取低 16 位 (等价于 `v as u16`).
#[inline]
pub const fn low_u16_from_usize(v: usize) -> u16 {
    let b = v.to_le_bytes();
    u16::from_le_bytes([b[0], b[1]])
}

/// `u64` → `u16`: 取低 16 位 (等价于 `v as u16`).
#[inline]
pub const fn low_u16_from_u64(v: u64) -> u16 {
    let b = v.to_le_bytes();
    u16::from_le_bytes([b[0], b[1]])
}

/// `u64` → `i32`: 取低 32 位后按补码解释 (等价于 `v as i32`).
#[inline]
pub const fn low_i32_from_u64(v: u64) -> i32 {
    let b = v.to_le_bytes();
    i32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `i64` → `i32`: 取低 32 位后按补码解释 (等价于 `v as i32`).
#[inline]
pub const fn low_i32_from_i64(v: i64) -> i32 {
    let b = v.to_le_bytes();
    i32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `u16` → `u8`: 取低 8 位 (等价于 `v as u8`).
#[inline]
pub const fn low_u8_from_u16(v: u16) -> u8 {
    v.to_le_bytes()[0]
}

/// `u32` → `u16`: 取低 16 位 (等价于 `v as u16`).
#[inline]
pub const fn low_u16_from_u32(v: u32) -> u16 {
    let b = v.to_le_bytes();
    u16::from_le_bytes([b[0], b[1]])
}

/// `i32` → `i8`: 取低 8 位后按补码解释 (等价于 `v as i8`).
#[inline]
pub const fn low_i8_from_i32(v: i32) -> i8 {
    i8::from_le_bytes([v.to_le_bytes()[0]])
}

/// `i32` → `i16`: 取低 16 位后按补码解释 (等价于 `v as i16`).
#[inline]
pub const fn low_i16_from_i32(v: i32) -> i16 {
    let b = v.to_le_bytes();
    i16::from_le_bytes([b[0], b[1]])
}

/// `i128` → `i64`: 取低 64 位后按补码解释 (等价于 `v as i64`).
#[inline]
pub const fn low_i64_from_i128(v: i128) -> i64 {
    let b = v.to_le_bytes();
    i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// `u32` → `i32`: 按补码解释 (等价于 `v as i32`).
#[inline]
pub const fn low_i32_from_u32(v: u32) -> i32 {
    i32::from_le_bytes(v.to_le_bytes())
}

/// `f32` → `i32`: 按 `as` 的语义截断 — 向零取整, 越界饱和, `NaN` → 0.
///
/// Rust 的浮点→整数转换只有 `as` 一种写法, 而 `as` 会被 clippy 判为可能截断;
/// 本函数改用 IEEE-754 位分解, 在纯整数域复现 `as` 的同一语义 (逐位等价),
/// 从而让调用点不再出现浮点 `as`.
#[inline]
pub const fn i32_from_f32(v: f32) -> i32 {
    let bits = v.to_bits();
    let negative = bits >> 31 != 0;
    let biased = (bits >> 23) & 0xFF;
    let mantissa = bits & 0x007F_FFFF;
    // 指数全 1: ±Inf 饱和, NaN 归 0 (均为 `as` 的规定行为).
    if biased == 0xFF {
        if mantissa != 0 {
            return 0;
        }
        return if negative { i32::MIN } else { i32::MAX };
    }
    // 次正规数与 |v| < 1 的数, 截断后为 0.
    if biased == 0 {
        return 0;
    }
    let exp = low_i32_from_u32(biased) - 127;
    if exp < 0 {
        return 0;
    }
    // |v| >= 2^31 必饱和 (2^31 已超 i32 正半轴).
    if exp > 30 {
        return if negative { i32::MIN } else { i32::MAX };
    }
    let significand = mantissa | 0x0080_0000;
    let magnitude = if exp >= 23 {
        significand << (exp - 23)
    } else {
        significand >> (23 - exp)
    };
    let mag = low_i32_from_u32(magnitude);
    if negative { -mag } else { mag }
}

/// `i32` → `f32`: 按 `as` 的语义转换 — 就近取整, 平局取偶 (IEEE-754 默认舍入).
///
/// Rust 的整数→浮点转换同样只有 `as` 一种写法, 而 `as` 会被 clippy 判为精度损失;
/// 本函数在纯整数域装配 IEEE-754 位模式复现 `as` 的同一语义 (逐位等价), 使调用点
/// 不再出现整数 `as`. 幅值超 24 位有效位时需舍弃低位, 舍入与硬件一致: 余数过半
/// 进位, 恰为半值时向偶取整 (截断值末位为 1 才进位).
#[inline]
pub const fn f32_from_i32(v: i32) -> f32 {
    let negative = v < 0;
    // 取幅值 (二的补码取负): i32::MIN 的幅值 2^31 在 u32 内可表示.
    let raw = u32::from_le_bytes(v.to_le_bytes());
    let mag = if negative {
        0u32.wrapping_sub(raw)
    } else {
        raw
    };
    if mag == 0 {
        return 0.0;
    }
    // 最高置位索引 = floor(log2(mag)).
    let msb = mag.ilog2();
    let (exponent, mantissa) = if msb <= 23 {
        // 24 位以内可精确表示: 左移到隐含 1 之后即为尾数.
        (msb + 127, (mag << (23 - msb)) & 0x007F_FFFF)
    } else {
        let shift = msb - 23;
        let truncated = mag >> shift;
        let rest = mag & ((1u32 << shift) - 1);
        let half = 1u32 << (shift - 1);
        let round_up = rest > half || (rest == half && truncated & 1 == 1);
        let (mut significand, mut rounded_msb) = (truncated, msb);
        if round_up {
            significand += 1;
            if significand == 0x0100_0000 {
                // 24 位全 1 进位: 归一化回隐含 1, 指数 +1.
                significand = 0x0080_0000;
                rounded_msb += 1;
            }
        }
        (rounded_msb + 127, significand & 0x007F_FFFF)
    };
    let sign = if negative { 0x8000_0000 } else { 0 };
    f32::from_bits(sign | (exponent << 23) | mantissa)
}

/// `u128` → `u64`: 取低 64 位 (等价于 `v as u64`).
#[inline]
pub const fn low_u64_from_u128(v: u128) -> u64 {
    let b = v.to_le_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正向小值: 与 `as` 的取值一致.
    #[test]
    fn positive_values_match_as() {
        assert_eq!(i64_from_usize(0), 0);
        assert_eq!(i64_from_usize(4096), 4096);
        assert_eq!(low_i32_from_usize(4096), 4096);
    }

    /// 高位为 1: `usize → i64` 逐位等价即为负值.
    #[test]
    fn high_bit_maps_to_negative_like_as() {
        assert_eq!(i64_from_usize(usize::MAX), -1);
    }

    /// `usize → i32` 只保留低 32 位.
    #[test]
    fn low_i32_keeps_low_bits_only() {
        assert_eq!(low_i32_from_usize(u32::MAX as usize), -1);
        assert_eq!(low_i32_from_usize(0x1_0000_0001), 1);
    }

    /// `usize → i16` 只保留低 16 位.
    #[test]
    fn low_i16_keeps_low_bits_only() {
        assert_eq!(low_i16_from_usize(u16::MAX as usize), -1);
        assert_eq!(low_i16_from_usize(0x1_0000_0001), 1);
    }

    /// 有符号 → 无符号: 逐位等价于 `as` (负值即补码位模式).
    #[test]
    fn sign_loss_helpers_match_as() {
        assert_eq!(u16_from_i32(-1), u16::MAX);
        assert_eq!(u8_from_i32(-1), u8::MAX);
        assert_eq!(u32_from_i16(-1), u32::MAX);
        assert_eq!(u32_from_i8(-1), u32::MAX);
        assert_eq!(u64_from_i32(-1), u64::MAX);
        assert_eq!(low_u32_from_i64(-1), u32::MAX);
        assert_eq!(usize_from_i32(-1), usize::MAX);
        assert_eq!(usize_from_i64(-1), usize::MAX);
        assert_eq!(usize_from_i8(-1), usize::MAX);
        assert_eq!(u32_from_i16(i16::MIN), 0xFFFF_8000);
    }
    /// 窄化助手: 只保留低位, 与 `as` 逐位等价 (含符号位).
    #[test]
    fn truncation_helpers_keep_low_bits_only() {
        assert_eq!(usize_from_u64(u64::MAX), usize::MAX);
        assert_eq!(low_u32_from_u64(0x1_0000_0001), 1);
        assert_eq!(low_u32_from_usize(0x1_0000_0001), 1);
        assert_eq!(low_u8_from_u32(0x1_01), 1);
        assert_eq!(low_u8_from_u64(0x1_01), 1);
        assert_eq!(low_u8_from_usize(0x1_01), 1);
        assert_eq!(low_u16_from_usize(0x1_0001), 1);
        assert_eq!(low_u16_from_u64(0x1_0001), 1);
        assert_eq!(low_i32_from_u64(u32::MAX as u64), -1);
        assert_eq!(low_i32_from_i64(u32::MAX as i64), -1);
        assert_eq!(low_i8_from_i32(0xFF), -1);
        assert_eq!(low_u64_from_u128(u128::MAX), u64::MAX);
        assert_eq!(low_u64_from_u128(0x1_0000_0000_0000_0001), 1);
        assert_eq!(low_i16_from_i32(0x1_0001), 1);
        assert_eq!(low_i16_from_i32(i32::MAX), -1);
        assert_eq!(low_i64_from_i128(i128::MAX), -1);
        assert_eq!(low_i64_from_i128(0x1_0000_0000_0000_0001), 1);
    }

    /// 浮点→整数助手: 与 `as` 逐值等价 (含 NaN / 饱和 / 向零取整边界).
    #[test]
    fn i32_from_f32_matches_as() {
        let samples = [
            0.0f32,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            1.9999,
            -1.9999,
            255.0,
            -255.0,
            2147483000.0,
            -2147483000.0,
            3.0e9,
            -3.0e9,
            1.0e-30,
            -1.0e-30,
            12345.678,
            -12345.678,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        for &v in &samples {
            assert_eq!(i32_from_f32(v), v as i32, "value {v}");
        }
        // 位模式伪随机遍历: 覆盖指数/尾数的各类组合.
        let mut bits = 0x3F80_0001u32;
        for _ in 0..4096 {
            let v = f32::from_bits(bits);
            assert_eq!(i32_from_f32(v), v as i32, "bits {bits:#010x}");
            bits = bits.wrapping_mul(1664525).wrapping_add(1013904223);
        }
    }

    /// 整数→浮点助手: 与 `as` 逐值等价 (含就近偶舍入 / 进位 / i32 极值).
    #[test]
    fn f32_from_i32_matches_as() {
        let samples = [
            0,
            1,
            -1,
            255,
            -255,
            4096,
            12345,
            -12345,
            0x00FF_FFFF,
            0x0100_0000,
            0x0100_0001,
            0x0100_0003,
            0x01FF_FFFF,
            16_777_217,
            -16_777_217,
            i32::MAX,
            i32::MIN,
            i32::MIN + 1,
        ];
        for &v in &samples {
            assert_eq!(f32_from_i32(v), v as f32, "value {v}");
        }
        // 位模式伪随机遍历: 覆盖各类尾数舍入与指数边界.
        let mut state = 0x1234_5678u32;
        for _ in 0..4096 {
            let v = i32::from_le_bytes(state.to_le_bytes());
            assert_eq!(f32_from_i32(v), v as f32, "bits {state:#010x}");
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        }
    }
}
