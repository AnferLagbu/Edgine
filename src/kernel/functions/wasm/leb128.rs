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
pub fn read_leb128_u32(bytes: &[u8], pos: &mut usize) -> Result<u32, WasmError> {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    loop {
        if *pos >= bytes.len() {
            return Err(WasmError::Truncated);
        }
        let byte = bytes[*pos];
        *pos += 1;
        result |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
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
        result |= i32::from(byte & 0x7F) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
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
        result |= i64::from(byte & 0x7F) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
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
