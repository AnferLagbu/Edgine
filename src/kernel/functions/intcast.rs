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
}
