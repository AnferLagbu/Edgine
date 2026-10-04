#![deny(unsafe_code)]
//! WASI 时钟/随机: `clock_time_get`, `random_get`

use super::fd_table::write_i64_to_memory;
use super::{WasiContext, WasiErrno, wasi_errno, wasi_success};
use crate::functions::wasm::interpreter::Interpreter;
use crate::functions::wasm::types::{Value, WasmError};

/// WASI clock IDs
const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;

/// WASI `clock_time_get`: 获取时钟时间 (纳秒)
///
/// 参数: (`clock_id`: i32, precision: i64, `result_ptr`: i32)
/// 返回: 0 (成功) 或 errno
pub fn wasi_clock_time_get(
    _ctx: &mut WasiContext,
    interp: &mut Interpreter,
) -> Result<(), WasmError> {
    let clock_id = interp.stack.pop_i32()? as u32;
    let _precision = interp.stack.pop_i64()?;
    let result_ptr = interp.stack.pop_i32()? as u32;

    let nanos: u64 = match clock_id {
        CLOCK_MONOTONIC | CLOCK_REALTIME => {
            // 单一实时时钟 (简化: 单一时钟源)
            crate::privileged::timer::calibration::get_time_ns().unwrap_or(0)
        }
        _ => {
            interp
                .stack
                .push(Value::I32(wasi_errno(WasiErrno::Inval)))?;
            return Ok(());
        }
    };

    write_i64_to_memory(interp, result_ptr, nanos as i64);
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}

/// WASI `random_get`: 填充随机字节
///
/// 参数: (`buf_ptr`: i32, `buf_len`: i32)
/// 返回: 0 (成功)
pub fn wasi_random_get(_ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let buf_ptr = interp.stack.pop_i32()? as u32;
    let buf_len = interp.stack.pop_i32()? as u32;

    if let Some(ref mut mem) = interp.memory {
        for i in 0..buf_len {
            let byte = crate::privileged::proc::canary::next_random_u64() as u8;
            let _ = mem.write_u8(buf_ptr + i, byte);
        }
    }

    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}
