#![deny(unsafe_code)]
#![deny(missing_docs)]
//! WASM 实例 — 解释器与 WASI 上下文的生命周期归属
//!
//! 解释器 (`Interpreter`) 只认识 host 闭包, 不认识 WASI; `WasiContext`
//! 与宿主闭包通过 `Rc<RefCell<WasiContext>>` 共享, 由本结构持有并随实例释放。
//! 这样 runtime 不反向依赖 WASI, 而 WASI 的 fd 表 / 参数 / 环境变量仍有
//! 明确的落点 (W-6 起由装载路径填充)。

use alloc::rc::Rc;
use core::cell::RefCell;

use super::interpreter::Interpreter;
use super::runtime::InterpreterConfig;
use super::types::{Value, WasmError, WasmModule};
use super::wasi::WasiContext;
use super::wasi::inject::register_wasi_functions;

/// 单个 WASM 实例: 解释器 + 本实例独占的 WASI 上下文
pub struct WasmInstance {
    interp: Interpreter,
    ctx: Rc<RefCell<WasiContext>>,
}

impl WasmInstance {
    /// 由模块与配置构造实例, 注入全部 WASI 宿主函数并按 import 段装配下标.
    ///
    /// `ctx` 随实例生命周期结束; 之后的调用与上下文的交互都经过同一句柄。
    #[must_use]
    pub fn new(module: WasmModule, config: InterpreterConfig, ctx: WasiContext) -> Self {
        let ctx = Rc::new(RefCell::new(ctx));
        let mut interp = Interpreter::new(module, config);
        register_wasi_functions(&mut interp, &ctx);
        interp.auto_register_wasi();
        Self { interp, ctx }
    }

    /// 调用导出函数.
    ///
    /// # Errors
    ///
    /// 导出名不存在、参数不匹配或执行 trap 时返回对应 [`WasmError`]。
    pub fn call(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, WasmError> {
        self.interp.call(name, args)
    }

    /// 解释器只读视图 (线性内存 / 退出码检查)
    #[must_use]
    pub fn interpreter(&self) -> &Interpreter {
        &self.interp
    }

    /// 只读访问本实例的 WASI 上下文
    pub fn with_context<R>(&self, f: impl FnOnce(&WasiContext) -> R) -> R {
        f(&self.ctx.borrow())
    }

    /// 可变访问本实例的 WASI 上下文
    pub fn with_context_mut<R>(&mut self, f: impl FnOnce(&mut WasiContext) -> R) -> R {
        f(&mut self.ctx.borrow_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::wasm::types::{FuncType, FunctionBody};
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    fn empty_module() -> WasmModule {
        WasmModule {
            types: vec![FuncType {
                params: Vec::new(),
                results: Vec::new(),
            }],
            imports: Vec::new(),
            functions: vec![0],
            tables: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
            exports: Vec::new(),
            start_section: None,
            elements: Vec::new(),
            code: vec![FunctionBody {
                locals: Vec::new(),
                code: vec![0x0B],
            }],
            data: Vec::new(),
        }
    }

    /// 上下文句柄跨调用保持, 且与实例同生命周期
    #[test]
    fn context_survives_across_calls() {
        let mut ctx = WasiContext::new();
        ctx.args.push(String::from("prog"));
        let mut instance = WasmInstance::new(empty_module(), InterpreterConfig::default(), ctx);

        assert_eq!(instance.with_context(|c| c.args.len()), 1);
        instance.with_context_mut(|c| c.args.push(String::from("--flag")));
        assert_eq!(instance.with_context(|c| c.args.len()), 2);
        assert!(instance.with_context(|c| c.args[0] == "prog"));
    }
}
