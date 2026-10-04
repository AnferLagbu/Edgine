#![deny(unsafe_code)]
//! WASI 环境/参数: `environ_get`, `environ_sizes_get`, `args_get`, `args_sizes_get`

use super::WasiContext;
use super::fd_table::write_u32_to_memory;
use crate::functions::wasm::interpreter::Interpreter;
use crate::functions::wasm::types::{Value, WasmError};
use alloc::format;

/// WASI `environ_sizes_get`: 获取环境变量数量和总缓冲区大小
///
/// 参数: (`count_ptr`: i32, `buf_size_ptr`: i32)
/// 返回: 0 (成功)
pub fn wasi_environ_sizes_get(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let count_ptr = interp.stack.pop_i32()? as u32;
    let buf_size_ptr = interp.stack.pop_i32()? as u32;

    let count = ctx.env.len() as u32;
    let buf_size: u32 = ctx
        .env
        .iter()
        .map(|(k, v)| k.len() as u32 + 1 + v.len() as u32 + 1) // "key=value\0"
        .sum();

    write_u32_to_memory(interp, count_ptr, count);
    write_u32_to_memory(interp, buf_size_ptr, buf_size);
    interp.stack.push(Value::I32(0))?;
    Ok(())
}

/// WASI `environ_get`: 读取环境变量
///
/// 参数: (`environ_ptr`: i32, `buf_ptr`: i32)
/// 返回: 0 (成功)
pub fn wasi_environ_get(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let environ_ptr = interp.stack.pop_i32()? as u32;
    let buf_ptr = interp.stack.pop_i32()? as u32;

    let mut offset = 0u32;
    for (i, (key, val)) in ctx.env.iter().enumerate() {
        // 写入指针数组 (每个元素指向 buf 中的位置)
        write_u32_to_memory(interp, environ_ptr + (i as u32) * 4, buf_ptr + offset);
        // 写入 "key=value\0"
        let entry = format!("{key}={val}");
        let bytes = entry.as_bytes();
        if let Some(ref mut mem) = interp.memory {
            for (j, &byte) in bytes.iter().enumerate() {
                let _ = mem.write_u8(buf_ptr + offset + j as u32, byte);
            }
        }
        offset += bytes.len() as u32 + 1; // +1 for NUL terminator
    }

    interp.stack.push(Value::I32(0))?;
    Ok(())
}

/// WASI `args_sizes_get`: 获取参数数量和总缓冲区大小
///
/// 参数: (`count_ptr`: i32, `buf_size_ptr`: i32)
/// 返回: 0 (成功)
pub fn wasi_args_sizes_get(
    ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let count_ptr = interp.stack.pop_i32()? as u32;
    let buf_size_ptr = interp.stack.pop_i32()? as u32;

    let count = ctx.args.len() as u32;
    let buf_size: u32 = ctx
        .args
        .iter()
        .map(|a| a.len() as u32 + 1) // "arg\0"
        .sum();

    write_u32_to_memory(interp, count_ptr, count);
    write_u32_to_memory(interp, buf_size_ptr, buf_size);
    interp.stack.push(Value::I32(0))?;
    Ok(())
}

/// WASI `args_get`: 读取命令行参数
///
/// 参数: (`argv_ptr`: i32, `buf_ptr`: i32)
/// 返回: 0 (成功)
pub fn wasi_args_get(ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let argv_ptr = interp.stack.pop_i32()? as u32;
    let buf_ptr = interp.stack.pop_i32()? as u32;

    let mut offset = 0u32;
    for (i, arg) in ctx.args.iter().enumerate() {
        // 写入指针数组
        write_u32_to_memory(interp, argv_ptr + (i as u32) * 4, buf_ptr + offset);
        // 写入 "arg\0"
        let bytes = arg.as_bytes();
        if let Some(ref mut mem) = interp.memory {
            for (j, &byte) in bytes.iter().enumerate() {
                let _ = mem.write_u8(buf_ptr + offset + j as u32, byte);
            }
        }
        offset += bytes.len() as u32 + 1;
    }

    interp.stack.push(Value::I32(0))?;
    Ok(())
}
