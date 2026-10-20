#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯算法实现。
//! LEB128 编解码器 — functions 层策略主体
//!
//! ## T6-9 迁移记录
//!
//! 原属 privileged/wasm/leb128.rs, 2026-06-16 提取到 functions.
//! 纯算法 (变长整数编解码), 0 unsafe, 0 外部依赖.
//! privileged 仅保留 re-export.

use alloc::vec::Vec;

use super::types::WasmError;

/// 从字节流读取一个无符号 32 位 LEB128 编码值.
///
/// # Errors
///
/// - 数据被截断 → `WasmError::Truncated`
/// - 编码溢出 32 位 → `WasmError::Leb128Overflow`
/// - 末字节仍有落在 32 位之外的有效位(超长效编码) → `WasmError::Leb128Overflow`
pub fn read_leb128_u32(bytes: &[u8], pos: &mut usize) -> Result<u32, WasmError> {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    loop {
        if *pos >= bytes.len() {
            return Err(WasmError::Truncated);
        }
        let byte = bytes[*pos];
        *pos += 1;
        let payload = u32::from(byte & 0x7F);
        result |= payload << shift;
        if byte & 0x80 == 0 {
            // P-42 根治: 末字节的 7 位有效载荷只要有任何一位落在 32 位之外, 该编码表示的就是
            // 一个装不进 u32 的大数, 按规范判溢出; 此前这些位被 `<<` 静默丢弃 ⇒ 大数解成小值.
            // 条件短路保证此处 `shift` 只能为 28 ⇒ `32 - shift` = 4, 不会移位越界 panic.
            if shift + 7 > 32 && payload >> (32 - shift) != 0 {
                return Err(WasmError::Leb128Overflow);
            }
            break;
        }
        shift += 7;
        if shift >= 32 {
            return Err(WasmError::Leb128Overflow);
        }
    }
    Ok(result)
}

/// 从字节流读取一个有符号 32 位 LEB128 编码值.
///
/// # Errors
///
/// - 数据被截断 → `WasmError::Truncated`
/// - 编码溢出 32 位 → `WasmError::Leb128Overflow`
/// - 末字节越出 32 位的有效位不是符号位的完整复制 → `WasmError::Leb128Overflow`
pub fn read_leb128_i32(bytes: &[u8], pos: &mut usize) -> Result<i32, WasmError> {
    let mut result: i32 = 0;
    let mut shift: u32 = 0;
    let size: u32 = 32;
    let mut byte: u8;
    loop {
        if *pos >= bytes.len() {
            return Err(WasmError::Truncated);
        }
        byte = bytes[*pos];
        *pos += 1;
        let payload = i32::from(byte & 0x7F);
        result |= payload << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            // P-42 根治: 末字节越出 i32 位宽的那些有效位必须是符号位的完整复制(全 0 或全 1),
            // 否则该编码表示的是一个装不进 i32 的大数. 条件成立时 `shift` 只能为 35 ⇒ 位移量安全.
            if shift > size {
                let spare = size - (shift - 7);
                let dropped = payload >> spare;
                if dropped != 0 && dropped + 1 != 1 << (7 - spare) {
                    return Err(WasmError::Leb128Overflow);
                }
            }
            break;
        }
        if shift >= 32 {
            return Err(WasmError::Leb128Overflow);
        }
    }
    if shift < size && (byte & 0x40) != 0 {
        result |= !0 << shift;
    }
    Ok(result)
}

/// 从字节流读取一个有符号 64 位 LEB128 编码值.
///
/// # Errors
///
/// - 数据被截断 → `WasmError::Truncated`
/// - 编码溢出 64 位 → `WasmError::Leb128Overflow`
/// - 末字节越出 64 位的有效位不是符号位的完整复制 → `WasmError::Leb128Overflow`
pub fn read_leb128_i64(bytes: &[u8], pos: &mut usize) -> Result<i64, WasmError> {
    let mut result: i64 = 0;
    let mut shift: u32 = 0;
    let size: u32 = 64;
    let mut byte: u8;
    loop {
        if *pos >= bytes.len() {
            return Err(WasmError::Truncated);
        }
        byte = bytes[*pos];
        *pos += 1;
        let payload = i64::from(byte & 0x7F);
        result |= payload << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            // P-42 根治: 同 i32 —— 末字节越出 i64 位宽的有效位只能是符号位的完整复制.
            // 条件成立时 `shift` 只能为 70 ⇒ `spare` = 1、位移量 6, 均不越界.
            if shift > size {
                let spare = size - (shift - 7);
                let dropped = payload >> spare;
                if dropped != 0 && dropped + 1 != 1 << (7 - spare) {
                    return Err(WasmError::Leb128Overflow);
                }
            }
            break;
        }
        if shift >= 64 {
            return Err(WasmError::Leb128Overflow);
        }
    }
    if shift < size && (byte & 0x40) != 0 {
        result |= !0 << shift;
    }
    Ok(result)
}

/// 从字节流读取一个长度前缀字符串(名称).
///
/// # Errors
///
/// - 长度前缀非法或数据被截断 → `WasmError::Truncated`
/// - 长度前缀编码溢出 → `WasmError::Leb128Overflow`
pub fn read_name(bytes: &[u8], pos: &mut usize) -> Result<Vec<u8>, WasmError> {
    let len = read_leb128_u32(bytes, pos)? as usize;
    if *pos + len > bytes.len() {
        return Err(WasmError::Truncated);
    }
    let name = bytes[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P-42 根治回归锁 —— 解码器必须**与规范一致**：末字节越出类型位宽的有效位一律判
    /// `Leb128Overflow`，不得被 `<<` 静默丢弃后解成一个"装得进类型"的小值。
    ///
    /// 改造前的实测失实读数（本组用例即为其反证）：
    /// `[0xFF, 0xFF, 0xFF, 0xFF, 0x10]` 曾被 `read_leb128_u32` 解为 `0x0FFFFFFF` 并接受。
    #[test]
    fn u32_rejects_overlong_tail_payload() {
        let cases: [&[u8]; 3] = [
            &[0xFF, 0xFF, 0xFF, 0xFF, 0x10],
            &[0x80, 0x80, 0x80, 0x80, 0x70],
            &[0xFF, 0xFF, 0xFF, 0xFF, 0x7F],
        ];
        for bytes in cases {
            let mut pos = 0usize;
            assert_eq!(
                read_leb128_u32(bytes, &mut pos),
                Err(WasmError::Leb128Overflow),
                "末字节高位载荷越出 u32 位宽应判溢出: {bytes:02X?}"
            );
        }
    }

    /// u32 的**合法**最宽编码不能被误判：末字节只允许 4 位有效载荷（`0x0F`）。
    #[test]
    fn u32_accepts_widest_legal_encodings() {
        let cases: &[(&[u8], u32)] = &[
            (&[0x00], 0),
            (&[0x7F], 127),
            (&[0x80, 0x01], 128),
            (&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F], u32::MAX),
            (&[0xFF, 0xFF, 0xFF, 0xFF, 0x07], 0x7FFF_FFFF),
        ];
        for (bytes, want) in cases {
            let mut pos = 0usize;
            assert_eq!(
                read_leb128_u32(bytes, &mut pos),
                Ok(*want),
                "{bytes:02X?} 应解为 {want}"
            );
            assert_eq!(pos, bytes.len(), "合法编码应恰好消费全部字节: {bytes:02X?}");
        }
    }

    /// i32 末字节越界的 3 位有效载荷只能是符号位的复制（全 0 或全 1）。
    #[test]
    fn i32_tail_must_be_sign_extension() {
        let accepted: &[(&[u8], i32)] = &[
            (&[0x7F], -1),
            (&[0xFF, 0xFF, 0xFF, 0xFF, 0x07], i32::MAX),
            (&[0x80, 0x80, 0x80, 0x80, 0x78], i32::MIN),
        ];
        for (bytes, want) in accepted {
            let mut pos = 0usize;
            assert_eq!(
                read_leb128_i32(bytes, &mut pos),
                Ok(*want),
                "{bytes:02X?} 应解为 {want}"
            );
        }
        let rejected: &[&[u8]] = &[
            &[0x80, 0x80, 0x80, 0x80, 0x18], // 越界位为 0b001: 既非全 0 也非全 1
            &[0xFF, 0xFF, 0xFF, 0xFF, 0x17], // 越界位为 0b001
            &[0xFF, 0xFF, 0xFF, 0xFF, 0x50], // 越界位为 0b101: 非全 1 的混合形态
        ];
        for bytes in rejected {
            let mut pos = 0usize;
            assert_eq!(
                read_leb128_i32(bytes, &mut pos),
                Err(WasmError::Leb128Overflow),
                "越界载荷非符号复制应判溢出: {bytes:02X?}"
            );
        }
    }

    /// i64 需 10 字节编码才可触发；同样只接受符号复制（末字节 1 位有效 + 6 位复制）。
    #[test]
    fn i64_tail_must_be_sign_extension() {
        let mut max_enc = [0xFFu8; 10];
        max_enc[9] = 0x00; // i64::MAX 的规范编码
        let mut min_enc = [0x80u8; 10];
        min_enc[9] = 0x7F; // i64::MIN 的规范编码
        for (bytes, want) in [(&max_enc, i64::MAX), (&min_enc, i64::MIN)] {
            let mut pos = 0usize;
            assert_eq!(
                read_leb128_i64(bytes, &mut pos),
                Ok(want),
                "合法极值应被接受"
            );
        }
        let mut bad_enc = [0x80u8; 10];
        bad_enc[9] = 0x78; // 越界位 0b111111 非全 1（应为 0x7F 的 6 个 1）
        let mut pos = 0usize;
        assert_eq!(
            read_leb128_i64(&bad_enc, &mut pos),
            Err(WasmError::Leb128Overflow),
            "i64 越界载荷非符号复制应判溢出"
        );
    }

    /// 收紧只改"越界有效位"的判定，不改截断语义与 `pos` 单调性（kani harness 的三条不变式面）。
    #[test]
    fn truncation_and_position_are_unchanged() {
        let mut pos = 0usize;
        assert_eq!(
            read_leb128_u32(&[0x80], &mut pos),
            Err(WasmError::Truncated)
        );
        let mut pos = 0usize;
        assert_eq!(
            read_leb128_i32(&[0x80, 0x80], &mut pos),
            Err(WasmError::Truncated)
        );
        let mut pos = 0usize;
        assert_eq!(
            read_leb128_i64(&[0xFF; 9], &mut pos),
            Err(WasmError::Truncated)
        );
        // 6 字节无符号编码在续行位上即判溢出，pos 已推进到第 5 字节之后。
        let mut pos = 0usize;
        assert_eq!(
            read_leb128_u32(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00], &mut pos),
            Err(WasmError::Leb128Overflow)
        );
        assert_eq!(pos, 5);
    }

    /// `read_name` 经由 u32 长度前缀继承同一收紧：超长效长度不得被静默解成小长度。
    #[test]
    fn read_name_inherits_tail_check() {
        let mut pos = 0usize;
        assert_eq!(
            read_name(&[0xFF, 0xFF, 0xFF, 0xFF, 0x10, b'x'], &mut pos),
            Err(WasmError::Leb128Overflow)
        );
        let mut pos = 0usize;
        assert_eq!(
            read_name(&[0x01, b'x'], &mut pos).as_deref(),
            Ok(&[b'x'][..])
        );
    }
}
