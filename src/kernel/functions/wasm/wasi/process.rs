#![deny(unsafe_code)]
//! WASI 进程控制: `proc_exit`, `sched_yield`

use super::{WasiContext, wasi_success};
use crate::functions::wasm::interpreter::Interpreter;
use crate::functions::wasm::types::{Value, WasmError};

/// WASI `proc_exit`: 终止当前 WASM 实例
///
/// 设置 `exit_code` 并返回 Terminated 错误，由解释器主循环捕获。
pub fn wasi_proc_exit(_ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    let code = interp.stack.pop_i32()?;
    interp.exit_code = code;
    // 通过 gas 耗尽强制终止解释器主循环
    interp.gas_used = interp.config.max_gas;
    Err(WasmError::Terminated)
}

/// WASI `sched_yield`: 让出 CPU
///
/// 简化实现: 立即返回成功。在单线程解释器中无需真正让出。
pub fn wasi_sched_yield(_ctx: &mut WasiContext, interp: &mut Interpreter) -> Result<(), WasmError> {
    interp.stack.push(Value::I32(wasi_success()))?;
    Ok(())
}
