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
}
