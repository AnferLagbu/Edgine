//! 整数位级转换助手.
//!
//! 本模块承接 clippy `cast_*` 族清零工程: `as` 在整数之间做窄化 / 符号改写 /
//! 指针宽度转换时会被 clippy 判为「可能截断 / 可能回绕 / 可能丢符号」, 而内核里
//! 这些位置大多是**刻意的位级语义** (取低位、64 位目标上的地址与长度互转).
//!
//! ## 实现原则
//!
//! 1. **逐位等价**: 每个函数与它所替代的 `as` 表达式在 x86_64 / aarch64 上产生
//!    完全相同的比特, 不改任何可观测行为.
//! 2. **零抑制属性**: 只用 `to_le_bytes` / `from_le_bytes` / `cast_signed` /
//!    `cast_unsigned` 实现, 不引入任何 `#[allow]` / `#[expect]`.
//! 3. **宽度 fail-closed**: `usize` 与 64 位互转的函数靠字节数组长度在编译期绑定
//!    指针宽度 —— 在 32 位目标上**编译失败**, 而不是静默截断 (Edgine 仅支持
//!    x86_64 / aarch64, 二者 `usize` 均为 64 位).
//! 4. **const 友好**: 全部为 `const fn`, 可直接用于常量定义.

/// `usize` → `i64`: 64 位目标上逐位等价于 `v as i64`.
///
/// 在 32 位目标上零扩展为 `i64` (无损, 与 `as` 的取值语义一致); 本函数刻意用
/// 字节搬运实现, 从而让 32 位目标下 `to_le_bytes` 的长度差异不成为编译障碍.
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
///
/// 32 位目标与 64 位目标一致: 都是截取最低 4 字节.
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

/// `u128` → `u64`: 取低 64 位 (等价于 `v as u64`).
#[inline]
pub const fn low_u64_from_u128(v: u128) -> u64 {
    let b = v.to_le_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 位正向值: 两个方向都是恒等映射.
    #[test]
    fn positive_values_are_identity_on_64bit() {
        assert_eq!(i64_from_usize(0), 0);
        assert_eq!(i64_from_usize(4096), 4096);
        assert_eq!(low_i32_from_usize(4096), 4096);
    }

    /// 高位为 1 的地址值: `usize → i64` 逐位等价即为负值 (与 `as` 相同).
    #[test]
    fn high_bit_maps_to_negative_like_as() {
        let addr = usize::MAX;
        assert_eq!(i64_from_usize(addr), -1);
    }

    /// `usize → i32` 只保留低 32 位, 与 `as` 一致 (含符号位).
    #[test]
    fn low_i32_keeps_low_bits_only() {
        assert_eq!(low_i32_from_usize(u32::MAX as usize), -1);
        assert_eq!(low_i32_from_usize(0x1_0000_0001), 1);
    }

    /// `usize → i16` 只保留低 16 位, 与 `as` 一致 (含符号位).
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
}
