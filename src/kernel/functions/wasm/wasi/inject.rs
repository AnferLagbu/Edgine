#![deny(unsafe_code)]
#![deny(missing_docs)]
//! WASI 宿主函数注入
//!
//! 把 [`wasi_function_table`] 的 `(name, WasiFunc)` 适配为解释器可调用的 host
//! 闭包并登记到解释器名称表; 下标装配由 `Interpreter::auto_register_wasi`
//! 按模块 import section 的声明顺序完成。
//!
//! 上下文归属: 每个闭包与 [`WasmInstance`] 共享同一个
//! `Rc<RefCell<WasiContext>>` — 解释器本身不依赖 WASI, 上下文随实例生命周期结束。
//!
//! [`WasmInstance`]: crate::functions::wasm::instance::WasmInstance

use alloc::rc::Rc;
use core::cell::RefCell;

use super::{WasiContext, wasi_function_table};
use crate::functions::wasm::interpreter::{HostFunction, Interpreter};

/// 把全部 WASI 函数按名称注册进解释器
///
/// 必须在 `Interpreter::auto_register_wasi` 之前调用; 本函数只登记名称,
/// 不决定 import 下标, 因此重复调用以最后一次注册为准。
pub fn register_wasi_functions(interp: &mut Interpreter, ctx: &Rc<RefCell<WasiContext>>) {
    for &(name, func) in wasi_function_table() {
        let ctx = Rc::clone(ctx);
        let host: HostFunction = Rc::new(move |interp: &mut Interpreter| {
            let mut context = ctx.borrow_mut();
            func(&mut context, interp)
        });
        interp.register_named_host_function("wasi_snapshot_preview1", name, host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::wasm::instance::WasmInstance;
    use crate::functions::wasm::runtime::InterpreterConfig;
    use crate::functions::wasm::types::{
        ExportDesc, ExportKind, FuncType, FunctionBody, ImportDesc, ImportKind, Limits, MemoryType,
        Value, ValueType, WasmModule,
    };
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    /// 构造只 import `environ_sizes_get` 并导出 `f` 的最小模块:
    /// `f() -> i32` 调用 `environ_sizes_get(0, 4)` 后返回 `i32.load(0)`.
    fn environ_sizes_module() -> WasmModule {
        WasmModule {
            types: vec![
                FuncType {
                    params: vec![ValueType::I32, ValueType::I32],
                    results: vec![ValueType::I32],
                },
                FuncType {
                    params: Vec::new(),
                    results: vec![ValueType::I32],
                },
            ],
            imports: vec![ImportDesc {
                module: b"wasi_snapshot_preview1".to_vec(),
                name: b"environ_sizes_get".to_vec(),
                desc: ImportKind::Function(0),
            }],
            functions: vec![1],
            tables: Vec::new(),
            memories: vec![MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
            }],
            globals: Vec::new(),
            exports: vec![ExportDesc {
                name: b"f".to_vec(),
                kind: ExportKind::Function,
                index: 1,
            }],
            start_section: None,
            elements: Vec::new(),
            code: vec![FunctionBody {
                locals: Vec::new(),
                // i32.const 0; i32.const 4; call 0; drop; i32.const 0; i32.load 0 0; end
                code: vec![
                    0x41, 0x00, 0x41, 0x04, 0x10, 0x00, 0x1A, 0x41, 0x00, 0x28, 0x02, 0x00, 0x0B,
                ],
            }],
            data: Vec::new(),
        }
    }

    fn read_u32(mem: &[u8], addr: usize) -> u32 {
        u32::from_le_bytes([mem[addr], mem[addr + 1], mem[addr + 2], mem[addr + 3]])
    }

    /// 端到端参数序回归: `environ_sizes_get(count_ptr, buf_size_ptr)`.
    ///
    /// 若 host 调用按正序压栈 (旧实现), 栈顶是 `buf_size_ptr` ⇒ 计数与
    /// 缓冲区大小写入位置互换, 本用例读 `[0] == 2` 即失败。
    #[test]
    fn environ_sizes_get_receives_arg0_as_count_ptr() {
        let mut ctx = WasiContext::new();
        ctx.env.push((String::from("K"), String::from("V")));
        ctx.env.push((String::from("A"), String::from("B")));
        let mut instance =
            WasmInstance::new(environ_sizes_module(), InterpreterConfig::default(), ctx);

        let ret = instance.call("f", &[]).expect("WASI host call should run");
        assert_eq!(ret, Some(Value::I32(2)), "environ count should be 2");

        let memory = instance
            .interpreter()
            .memory
            .as_ref()
            .expect("module declares memory");
        assert_eq!(read_u32(&memory.data, 0), 2, "count written at arg0");
        assert_eq!(read_u32(&memory.data, 4), 8, "buf size written at arg1");
    }
}
