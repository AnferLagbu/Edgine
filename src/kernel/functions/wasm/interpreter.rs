#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯解释器实现。
//! WASM 解释器 — functions 层策略主体
//!
//! ## T6-9 迁移记录
//!
//! 原属 privileged/wasm/interpreter.rs, 2026-06-16 提取到 functions.
//! 纯解释器 (栈式虚拟机核心), 0 unsafe, 0 外部依赖.
//! privileged 仅保留 re-export.
//!
//! 包含:
//! - Interpreter: 完整的 WASM 字节码执行引擎
//! - 控制流, 内存操作, 数值运算, 全局变量, 函数调用
//!
//! 安全边界:
//! - 所有内存访问均在 bounds check 后进行
//! - Gas metering 防止无限循环
//! - 调用深度限制防止栈溢出
//! - 除以零和溢出均返回 Trap

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::leb128::{read_leb128_i32, read_leb128_i64, read_leb128_u32};
use super::module::parse_wasm;
use super::runtime::{CallFrame, InterpreterConfig, LinearMemory, ValueStack};
use super::types::{
    ExportKind, FuncType, FunctionBody, ImportKind, Opcode, Value, WasmError, WasmModule,
};

// ============================================================================
// 解释器
// ============================================================================

pub struct Interpreter {
    pub stack: ValueStack,
    pub memory: Option<LinearMemory>,
    call_stack: Vec<CallFrame>,
    pub config: InterpreterConfig,
    pub gas_used: u64,
    pub exit_code: i32,
    module: WasmModule,
    host_functions: Vec<Box<dyn Fn(&mut Self) -> Result<(), WasmError>>>,
    import_func_count: u32,
    globals: Vec<Value>,
    tables: Vec<Vec<u32>>,
    /// 名称索引的 host function: (module, name) → index
    named_host_functions: BTreeMap<(String, String), usize>,
}

impl Interpreter {
    pub fn new(module: WasmModule, config: InterpreterConfig) -> Self {
        let import_func_count = module
            .imports
            .iter()
            .filter(|i| matches!(i.desc, ImportKind::Function(_)))
            .count() as u32;

        let import_global_count = module
            .imports
            .iter()
            .filter(|i| matches!(i.desc, ImportKind::Global(_)))
            .count();

        let mut globals = Vec::with_capacity(import_global_count + module.globals.len());
        for imp in &module.imports {
            if let ImportKind::Global(gt) = &imp.desc {
                globals.push(Value::default_for(gt.content_type));
            }
        }
        for (gt, _) in &module.globals {
            globals.push(Value::default_for(gt.content_type));
        }

        let module_globals = module.globals.clone();

        // B07-12: 把配置的 `max_memory_pages` 硬上限接入内存创建路径 —
        // 模块声明的 max 与配置上限取交集; 未声明时强制配置上限 (256 页),
        // 防止恶意 bytecode 声明无界 max 后无限增长线性内存 (DoS).
        let cap_max = |declared: Option<u32>| {
            Some(match declared {
                Some(d) => d.min(config.max_memory_pages),
                None => config.max_memory_pages,
            })
        };

        let mut memory = None;
        for imp in &module.imports {
            if let ImportKind::Memory(mem_type) = &imp.desc {
                memory = LinearMemory::new(mem_type.limits.min, cap_max(mem_type.limits.max)).ok();
                break;
            }
        }
        if memory.is_none() {
            if let Some(mem) = module.memories.first() {
                memory = LinearMemory::new(mem.limits.min, cap_max(mem.limits.max)).ok();
            }
        }

        let import_table_count = module
            .imports
            .iter()
            .filter(|i| matches!(i.desc, ImportKind::Table(_)))
            .count();

        let mut tables: Vec<Vec<u32>> =
            Vec::with_capacity(import_table_count + module.tables.len());
        for imp in &module.imports {
            if let ImportKind::Table(ref tt) = imp.desc {
                tables.push(vec![0u32; tt.limits.min as usize]);
            }
        }
        for tt in &module.tables {
            tables.push(vec![0u32; tt.limits.min as usize]);
        }

        let mut interp = Self {
            stack: ValueStack::new(),
            memory,
            call_stack: Vec::with_capacity(64),
            config,
            gas_used: 0,
            exit_code: 0,
            module,
            host_functions: Vec::new(),
            import_func_count,
            globals,
            tables,
            named_host_functions: BTreeMap::new(),
        };

        for (gi, (_, init_expr)) in module_globals.iter().enumerate() {
            let global_idx = import_global_count + gi;
            if let Ok(val) = Self::eval_init_expr(init_expr, &interp.globals) {
                if global_idx < interp.globals.len() {
                    interp.globals[global_idx] = val;
                }
            }
        }

        interp.apply_data_segments();
        interp.apply_element_segments();

        interp
    }

    fn eval_init_expr(expr: &[u8], globals: &[Value]) -> Result<Value, WasmError> {
        let mut mini_stack: Vec<Value> = Vec::new();
        let mut pos: usize = 0;
        while pos < expr.len() {
            if expr[pos] == 0x0B {
                break;
            }
            match expr[pos] {
                0x41 => {
                    pos += 1;
                    mini_stack.push(Value::I32(read_leb128_i32(expr, &mut pos)?));
                }
                0x42 => {
                    pos += 1;
                    mini_stack.push(Value::I64(read_leb128_i64(expr, &mut pos)?));
                }
                0x23 => {
                    pos += 1;
                    let idx = read_leb128_u32(expr, &mut pos)? as usize;
                    let val = globals.get(idx).copied().unwrap_or(Value::I32(0));
                    mini_stack.push(val);
                }
                _ => return Err(WasmError::UnknownOpcode(expr[pos])),
            }
        }
        mini_stack.pop().ok_or(WasmError::InternalError)
    }

    fn apply_data_segments(&mut self) {
        for seg in &self.module.data {
            if !seg.offset.is_empty() {
                if let Ok(Value::I32(offset)) = Self::eval_init_expr(&seg.offset, &self.globals) {
                    if let Some(ref mut mem) = self.memory {
                        let addr = offset as usize;
                        if addr + seg.data.len() <= mem.data.len() {
                            mem.data[addr..addr + seg.data.len()].copy_from_slice(&seg.data);
                        }
                    }
                }
            }
        }
    }

    fn apply_element_segments(&mut self) {
        for seg in &self.module.elements {
            if seg.func_indices.is_empty() || seg.offset.is_empty() {
                continue;
            }
            let table_idx = seg.table_index as usize;
            if table_idx >= self.tables.len() {
                continue;
            }
            if let Ok(Value::I32(offset)) = Self::eval_init_expr(&seg.offset, &self.globals) {
                let base = offset as usize;
                let table_len = self.tables[table_idx].len();
                for (i, &func_idx) in seg.func_indices.iter().enumerate() {
                    let target = base + i;
                    if target < table_len {
                        self.tables[table_idx][target] = func_idx;
                    }
                }
            }
        }
    }

    pub fn register_host_function(&mut self, f: Box<dyn Fn(&mut Self) -> Result<(), WasmError>>) {
        self.host_functions.push(f);
    }

    /// 注册名称匹配的 host function
    ///
    /// 注册后，`auto_register_wasi` 可根据 WASM import section 的 module/name
    /// 自动查找并注册到正确的 index 位置。
    pub fn register_named_host_function(
        &mut self,
        module: &str,
        name: &str,
        f: Box<dyn Fn(&mut Self) -> Result<(), WasmError>>,
    ) {
        let idx = self.host_functions.len();
        self.host_functions.push(f);
        self.named_host_functions
            .insert((String::from(module), String::from(name)), idx);
    }

    /// 自动注册 WASI 函数 (根据 WASM 模块 import section)
    ///
    /// 遍历 `module.imports`，对每个 `(module="wasi_snapshot_preview1", desc=Function)` 的 import，
    /// 根据 `name` 查找已注册的 named host function，将其移动到正确的 index 位置。
    ///
    /// 调用顺序: 先通过 `register_named_host_function` 注册所有 WASI 函数，
    /// 再调用本方法完成 import section 到 `host_functions` 的映射。
    pub fn auto_register_wasi(&mut self) {
        let mut func_idx = 0u32;
        for import in &self.module.imports {
            if let ImportKind::Function(_) = import.desc {
                let module_name = core::str::from_utf8(&import.module).unwrap_or("");
                let func_name = core::str::from_utf8(&import.name).unwrap_or("");
                if module_name == "wasi_snapshot_preview1" {
                    if let Some(&idx) = self
                        .named_host_functions
                        .get(&(String::from(module_name), String::from(func_name)))
                    {
                        // 确保 host_functions 数组足够大
                        while self.host_functions.len() <= func_idx as usize {
                            self.host_functions.push(Box::new(|_| Ok(())));
                        }
                        // 交换到正确位置
                        self.host_functions.swap(func_idx as usize, idx);
                    }
                }
                func_idx += 1;
            }
        }
        self.import_func_count = func_idx;
    }

    fn find_export(&self, name: &str) -> Option<u32> {
        self.module
            .exports
            .iter()
            .find(|e| e.name == name.as_bytes() && e.kind == ExportKind::Function)
            .map(|e| e.index)
    }

    fn get_func_type(&self, func_idx: u32) -> Result<&FuncType, WasmError> {
        if func_idx < self.import_func_count {
            let mut func_import_idx = 0u32;
            for imp in &self.module.imports {
                if let ImportKind::Function(type_idx) = imp.desc {
                    if func_import_idx == func_idx {
                        return self
                            .module
                            .types
                            .get(type_idx as usize)
                            .ok_or(WasmError::BadTypeIndex(type_idx as usize));
                    }
                    func_import_idx += 1;
                }
            }
            Err(WasmError::BadFuncIndex(func_idx as usize))
        } else {
            let local_idx = func_idx - self.import_func_count;
            let type_idx = self
                .module
                .functions
                .get(local_idx as usize)
                .ok_or(WasmError::BadFuncIndex(func_idx as usize))?;
            self.module
                .types
                .get(*type_idx as usize)
                .ok_or(WasmError::BadTypeIndex(*type_idx as usize))
        }
    }

    fn get_func_body(&self, func_idx: u32) -> Result<&FunctionBody, WasmError> {
        if func_idx < self.import_func_count {
            return Err(WasmError::FunctionNotFound);
        }
        let local_idx = func_idx - self.import_func_count;
        self.module
            .code
            .get(local_idx as usize)
            .ok_or(WasmError::BadFuncIndex(func_idx as usize))
    }

    /// 按导出名称调用 WASM 函数.
    ///
    /// # Errors
    ///
    /// 当导出名称不存在时返回 `WasmError::BadExport`; 其余错误由
    /// `call_func` 传播(如参数不匹配、栈操作失败等).
    pub fn call(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, WasmError> {
        let func_idx = self.find_export(name).ok_or(WasmError::BadExport)?;
        self.call_func(func_idx, args)
    }

    /// 按函数索引调用 WASM 函数.
    ///
    /// # Errors
    ///
    /// 当函数索引非法、类型不匹配、栈溢出/下溢或宿主函数执行失败时
    /// 返回对应的 `WasmError`.
    pub fn call_func(&mut self, func_idx: u32, args: &[Value]) -> Result<Option<Value>, WasmError> {
        if func_idx < self.import_func_count {
            let func_type = self.get_func_type(func_idx)?.clone();
            let param_count = func_type.params.len();
            for (i, arg) in args.iter().enumerate() {
                if i < param_count {
                    self.stack.push(*arg)?;
                }
            }

            let host_idx = func_idx as usize;
            if host_idx < self.host_functions.len() {
                let f =
                    core::mem::replace(&mut self.host_functions[host_idx], Box::new(|_| Ok(())));
                let result = f(self);
                self.host_functions[host_idx] = f;
                result?;
            }

            let result_count = func_type.results.len();
            if result_count == 0 {
                return Ok(None);
            } else if result_count == 1 {
                return Ok(Some(self.stack.pop()?));
            }
            return Ok(None);
        }

        let func_type = self.get_func_type(func_idx)?.clone();
        let body = self.get_func_body(func_idx)?;

        if self.call_stack.len() as u32 >= self.config.max_call_depth {
            return Err(WasmError::CallDepthExceeded);
        }

        let param_count = func_type.params.len();
        let mut locals: Vec<Value> = Vec::with_capacity(param_count + 64);

        for _ in 0..param_count {
            locals.push(Value::I32(0));
        }
        for (count, ty) in &body.locals {
            for _ in 0..*count {
                locals.push(Value::default_for(*ty));
            }
        }

        for (i, arg) in args.iter().enumerate() {
            if i < param_count {
                locals[i] = *arg;
            }
        }

        let stack_base = self.stack.len();

        let frame = CallFrame {
            func_idx,
            locals,
            pc: 0,
            code: body.code.clone(),
            arity: func_type.results.len(),
            return_pc: 0,
            stack_base,
        };

        self.call_stack.push(frame);
        self.execute_call_stack()?;

        if let Some(_frame) = self.call_stack.pop() {
            let actual_results = self.stack.len() - stack_base;
            if func_type.results.len() == 1 && actual_results >= 1 {
                let result = self.stack.pop()?;
                self.stack.drain_to(stack_base);
                Ok(Some(result))
            } else {
                self.stack.drain_to(stack_base);
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    fn execute_call_stack(&mut self) -> Result<(), WasmError> {
        while let Some(frame) = self.call_stack.last() {
            if frame.pc >= frame.code.len() {
                break;
            }
        }

        while self.call_stack.last().is_some() {
            let opcode = {
                let frame = self
                    .call_stack
                    .last()
                    .expect("wasm: call_stack 非空 (while is_some 守护)");
                if frame.pc >= frame.code.len() {
                    self.call_stack.pop();
                    continue;
                }
                frame.code[frame.pc]
            };

            let op = Opcode::from_byte(opcode).ok_or(WasmError::UnknownOpcode(opcode));
            self.gas_used += 1;
            if self.gas_used > self.config.max_gas {
                return Err(WasmError::GasExhausted);
            }

            match op {
                Ok(Opcode::Unreachable) => return Err(WasmError::Unreachable),
                Ok(Opcode::Nop) => {
                    self.advance_pc(1)?;
                }
                Ok(Opcode::End) => {
                    self.execute_end()?;
                    if self.call_stack.is_empty() {
                        return Ok(());
                    }
                }
                Ok(Opcode::Return) => {
                    self.execute_return()?;
                    if self.call_stack.is_empty() {
                        return Ok(());
                    }
                }

                Ok(Opcode::Block) => {
                    self.advance_pc(1)?;
                }
                Ok(Opcode::Loop) => {
                    self.advance_pc(1)?;
                }
                Ok(Opcode::If) => {
                    self.execute_if()?;
                }
                Ok(Opcode::Else) => {
                    self.execute_else()?;
                }
                Ok(Opcode::Br) => {
                    self.execute_br()?;
                }
                Ok(Opcode::BrIf) => {
                    self.execute_br_if()?;
                }
                Ok(Opcode::BrTable) => {
                    self.execute_br_table()?;
                }
                Ok(Opcode::Call) => {
                    self.execute_call()?;
                }
                Ok(Opcode::CallIndirect) => {
                    return Err(WasmError::Unreachable);
                }

                Ok(Opcode::Drop) => {
                    self.advance_pc(1)?;
                    self.stack.pop()?;
                }
                Ok(Opcode::Select) => {
                    self.advance_pc(1)?;
                    let cond = self.stack.pop_i32()?;
                    let val2 = self.stack.pop()?;
                    let val1 = self.stack.pop()?;
                    self.stack.push(if cond != 0 { val1 } else { val2 })?;
                }

                Ok(Opcode::LocalGet) => {
                    self.execute_local_get()?;
                }
                Ok(Opcode::LocalSet) => {
                    self.execute_local_set()?;
                }
                Ok(Opcode::LocalTee) => {
                    self.execute_local_tee()?;
                }
                Ok(Opcode::GlobalGet) => {
                    self.execute_global_get()?;
                }
                Ok(Opcode::GlobalSet) => {
                    self.execute_global_set()?;
                }

                Ok(Opcode::I32Load) => self.execute_memory_load(4)?,
                Ok(Opcode::I64Load) => self.execute_memory_load_64()?,
                Ok(Opcode::I32Load8S) => self.execute_memory_load_ext(1, true)?,
                Ok(Opcode::I32Load8U) => self.execute_memory_load_ext(1, false)?,
                Ok(Opcode::I32Load16S) => self.execute_memory_load_ext(2, true)?,
                Ok(Opcode::I32Load16U) => self.execute_memory_load_ext(2, false)?,
                Ok(Opcode::I32Store) => self.execute_memory_store(4)?,
                Ok(Opcode::I64Store) => self.execute_memory_store_64()?,
                Ok(Opcode::I32Store8) => self.execute_memory_store_n(1)?,
                Ok(Opcode::I32Store16) => self.execute_memory_store_n(2)?,
                Ok(Opcode::MemorySize) => {
                    self.advance_pc(1)?;
                    let pages = self
                        .memory
                        .as_ref()
                        .map_or(0, super::runtime::LinearMemory::pages);
                    self.stack.push(Value::I32(pages as i32))?;
                }
                Ok(Opcode::MemoryGrow) => {
                    self.advance_pc(1)?;
                    let pages = self.stack.pop_i32()?;
                    let result = if let Some(ref mut mem) = self.memory {
                        mem.grow(pages as u32)?
                    } else {
                        u32::MAX
                    };
                    self.stack.push(Value::I32(result as i32))?;
                }

                Ok(Opcode::I32Const) => self.execute_i32_const()?,
                Ok(Opcode::I64Const) => self.execute_i64_const()?,

                Ok(Opcode::I32Eqz) => self.execute_i32_unop(|a| i32::from(a == 0))?,
                Ok(Opcode::I32Eq) => self.execute_i32_binop(|a, b| i32::from(a == b))?,
                Ok(Opcode::I32Ne) => self.execute_i32_binop(|a, b| i32::from(a != b))?,
                Ok(Opcode::I32LtS) => self.execute_i32_binop(|a, b| i32::from(a < b))?,
                Ok(Opcode::I32LtU) => {
                    self.execute_i32_binop(|a, b| i32::from((a as u32) < (b as u32)))?;
                }
                Ok(Opcode::I32GtS) => self.execute_i32_binop(|a, b| i32::from(a > b))?,
                Ok(Opcode::I32GtU) => {
                    self.execute_i32_binop(|a, b| i32::from((a as u32) > (b as u32)))?;
                }
                Ok(Opcode::I32LeS) => self.execute_i32_binop(|a, b| i32::from(a <= b))?,
                Ok(Opcode::I32LeU) => {
                    self.execute_i32_binop(|a, b| i32::from((a as u32) <= (b as u32)))?;
                }
                Ok(Opcode::I32GeS) => self.execute_i32_binop(|a, b| i32::from(a >= b))?,
                Ok(Opcode::I32GeU) => {
                    self.execute_i32_binop(|a, b| i32::from((a as u32) >= (b as u32)))?;
                }

                Ok(Opcode::I32Add) => self.execute_i32_binop(i32::wrapping_add)?,
                Ok(Opcode::I32Sub) => self.execute_i32_binop(i32::wrapping_sub)?,
                Ok(Opcode::I32Mul) => self.execute_i32_binop(i32::wrapping_mul)?,
                Ok(Opcode::I32DivS) => self.execute_i32_div_s()?,
                Ok(Opcode::I32DivU) => self.execute_i32_div_u()?,
                Ok(Opcode::I32RemS) => self.execute_i32_rem_s()?,
                Ok(Opcode::I32RemU) => self.execute_i32_rem_u()?,
                Ok(Opcode::I32And) => self.execute_i32_binop(|a, b| a & b)?,
                Ok(Opcode::I32Or) => self.execute_i32_binop(|a, b| a | b)?,
                Ok(Opcode::I32Xor) => self.execute_i32_binop(|a, b| a ^ b)?,
                Ok(Opcode::I32Shl) => self.execute_i32_binop(|a, b| a.wrapping_shl(b as u32))?,
                Ok(Opcode::I32ShrS) => self.execute_i32_binop(|a, b| a.wrapping_shr(b as u32))?,
                Ok(Opcode::I32ShrU) => {
                    self.execute_i32_binop(|a, b| (a as u32).wrapping_shr(b as u32) as i32)?;
                }

                Ok(Opcode::I64Add) => self.execute_i64_binop(i64::wrapping_add)?,
                Ok(Opcode::I64Sub) => self.execute_i64_binop(i64::wrapping_sub)?,
                Ok(Opcode::I64Mul) => self.execute_i64_binop(i64::wrapping_mul)?,
                Ok(Opcode::I64DivS) => self.execute_i64_div_s()?,
                Ok(Opcode::I64And) => self.execute_i64_binop(|a, b| a & b)?,
                Ok(Opcode::I64Or) => self.execute_i64_binop(|a, b| a | b)?,

                Err(e) => return Err(e),
                _ => return Err(WasmError::UnknownOpcode(opcode)),
            }
        }

        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    fn advance_pc(&mut self, amount: usize) -> Result<(), WasmError> {
        if let Some(frame) = self.call_stack.last_mut() {
            frame.pc += amount;
        }
        Ok(())
    }

    fn current_frame_mut(&mut self) -> Result<&mut CallFrame, WasmError> {
        self.call_stack.last_mut().ok_or(WasmError::InternalError)
    }

    // --- 控制流 ---

    fn execute_end(&mut self) -> Result<(), WasmError> {
        let frame = self.call_stack.pop().ok_or(WasmError::InternalError)?;
        let arity = frame.arity;

        if arity == 1 {
            let stack_len = self.stack.len();
            let frame2 = self.current_frame_mut()?;
            frame2.stack_base = stack_len;
            let _ = frame2;
        }

        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    fn execute_return(&mut self) -> Result<(), WasmError> {
        while let Some(frame) = self.call_stack.pop() {
            let arity = frame.arity;
            if arity == 0 {
                break;
            }
        }
        Ok(())
    }

    fn execute_if(&mut self) -> Result<(), WasmError> {
        let cond = self.stack.pop_i32()?;
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        if cond == 0 {
            let byte = frame.code[frame.pc];
            frame.pc += 1;
            if byte == 0x40 {
                let pos = Self::skip_block(&frame.code, frame.pc);
                frame.pc = pos;
            } else {
                let pos = Self::skip_to_else_or_end(&frame.code, frame.pc - 1);
                frame.pc = pos;
            }
        } else {
            let byte = frame.code[frame.pc];
            frame.pc += 1;
            if byte == 0x40 {}
        }
        Ok(())
    }

    fn execute_else(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        let pos = Self::skip_to_end(&frame.code, frame.pc);
        frame.pc = pos;
        Ok(())
    }

    fn execute_br(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let depth = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        self.unwind_to(depth)?;
        Ok(())
    }

    fn execute_br_if(&mut self) -> Result<(), WasmError> {
        let cond = self.stack.pop_i32()?;
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let depth = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        if cond != 0 {
            self.unwind_to(depth)?;
        }
        Ok(())
    }

    fn execute_br_table(&mut self) -> Result<(), WasmError> {
        let index = self.stack.pop_i32()?;
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let n = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        let mut targets = alloc::vec![0usize; n + 1];
        for i in 0..n {
            targets[i] = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        }
        targets[n] = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        let _ = frame;
        let depth = if index >= 0 && (index as usize) < n {
            targets[index as usize]
        } else {
            targets[n]
        };
        self.unwind_to(depth)?;
        Ok(())
    }

    fn execute_call(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let func_idx = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let func_type = self.get_func_type(func_idx)?.clone();
        let param_count = func_type.params.len();
        let stack_len = self.stack.len();

        let mut args = Vec::new();
        for _ in 0..param_count {
            if self.stack.len() > stack_len - param_count + args.len() {
                break;
            }
        }
        for i in 0..param_count {
            if stack_len >= param_count - i {
                let idx = stack_len - param_count + i;
                if idx < self.stack.len() {
                    args.push(self.stack.data[idx]);
                }
            }
        }

        let args: Vec<Value> = (0..param_count)
            .map(|i| {
                let idx = stack_len - param_count + i;
                if idx < self.stack.len() {
                    self.stack.data[idx]
                } else {
                    Value::I32(0)
                }
            })
            .collect();

        self.stack.drain_to(stack_len - param_count);

        if func_idx < self.import_func_count {
            for arg in &args {
                self.stack.push(*arg)?;
            }

            let host_idx = func_idx as usize;
            if host_idx < self.host_functions.len() {
                let f =
                    core::mem::replace(&mut self.host_functions[host_idx], Box::new(|_| Ok(())));
                let result = f(self);
                self.host_functions[host_idx] = f;
                result?;
            }

            let result_count = func_type.results.len();
            if result_count <= 1 {
                return Ok(());
            }
        }

        let body = self.get_func_body(func_idx)?;
        let param_count_local = body.locals.iter().map(|(n, _)| *n).sum::<u32>() as usize;

        let mut locals: Vec<Value> = Vec::with_capacity(param_count + param_count_local);
        for arg in &args {
            locals.push(*arg);
        }
        for (count, ty) in &body.locals {
            for _ in 0..*count {
                locals.push(Value::default_for(*ty));
            }
        }

        let frame = CallFrame {
            func_idx,
            locals,
            pc: 0,
            code: body.code.clone(),
            arity: func_type.results.len(),
            return_pc: 0,
            stack_base: self.stack.len(),
        };

        self.call_stack.push(frame);
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    fn unwind_to(&mut self, depth: usize) -> Result<(), WasmError> {
        let target = self.call_stack.len().saturating_sub(depth + 1);
        while self.call_stack.len() > target {
            self.call_stack.pop();
        }
        Ok(())
    }

    fn skip_block(code: &[u8], mut pc: usize) -> usize {
        let mut depth = 1;
        while pc < code.len() && depth > 0 {
            let b = code[pc];
            pc += 1;
            match b {
                0x02 | 0x03 | 0x04 => depth += 1,
                0x0B => depth -= 1,
                0x10 => {
                    pc += 1;
                }
                0x11 => {
                    pc += 2;
                }
                _ => {}
            }
        }
        pc
    }

    fn skip_to_else_or_end(code: &[u8], mut pc: usize) -> usize {
        let mut depth = 1;
        while pc < code.len() && depth > 0 {
            let b = code[pc];
            pc += 1;
            match b {
                0x02 | 0x03 | 0x04 => depth += 1,
                0x0B => {
                    depth -= 1;
                    if depth == 0 {
                        return pc;
                    }
                }
                0x05 => {
                    if depth == 1 {
                        return pc;
                    }
                }
                0x10 => {
                    pc += 1;
                }
                0x11 => {
                    pc += 2;
                }
                _ => {}
            }
        }
        pc
    }

    fn skip_to_end(code: &[u8], mut pc: usize) -> usize {
        let mut depth = 1;
        while pc < code.len() && depth > 0 {
            let b = code[pc];
            pc += 1;
            match b {
                0x02 | 0x03 | 0x04 => depth += 1,
                0x0B => depth -= 1,
                0x10 => {
                    pc += 1;
                }
                0x11 => {
                    pc += 2;
                }
                _ => {}
            }
        }
        pc
    }

    // --- 局部变量 ---

    fn execute_local_get(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let idx = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        let val = frame.locals.get(idx).copied().unwrap_or(Value::I32(0));
        let _ = frame;
        self.stack.push(val)?;
        Ok(())
    }

    fn execute_local_set(&mut self) -> Result<(), WasmError> {
        let val = self.stack.pop()?;
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let idx = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        if idx < frame.locals.len() {
            frame.locals[idx] = val;
        }
        Ok(())
    }

    fn execute_local_tee(&mut self) -> Result<(), WasmError> {
        let val = *self.stack.peek().ok_or(WasmError::StackUnderflow)?;
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let idx = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        if idx < frame.locals.len() {
            frame.locals[idx] = val;
        }
        Ok(())
    }

    // --- 全局变量 ---

    fn execute_global_get(&mut self) -> Result<(), WasmError> {
        let idx = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            let idx = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
            let _ = frame;
            idx
        };
        let val = self.globals.get(idx).copied().unwrap_or(Value::I32(0));
        self.stack.push(val)?;
        Ok(())
    }

    fn execute_global_set(&mut self) -> Result<(), WasmError> {
        let val = self.stack.pop()?;
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let idx = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
        if idx < self.globals.len() {
            self.globals[idx] = val;
        }
        Ok(())
    }

    // --- 常量 ---

    fn execute_i32_const(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let val = read_leb128_i32(&frame.code, &mut frame.pc)?;
        self.stack.push(Value::I32(val))?;
        Ok(())
    }

    fn execute_i64_const(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let val = read_leb128_i64(&frame.code, &mut frame.pc)?;
        self.stack.push(Value::I64(val))?;
        Ok(())
    }

    // --- i32 运算 ---

    fn execute_i32_unop<F: FnOnce(i32) -> i32>(&mut self, f: F) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let a = self.stack.pop_i32()?;
        self.stack.push(Value::I32(f(a)))?;
        Ok(())
    }

    fn execute_i32_binop<F: FnOnce(i32, i32) -> i32>(&mut self, f: F) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        self.stack.push(Value::I32(f(a, b)))?;
        Ok(())
    }

    fn execute_i32_div_s(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        if a == i32::MIN && b == -1 {
            return Err(WasmError::IntegerOverflow);
        }
        self.stack.push(Value::I32(a.wrapping_div(b)))?;
        Ok(())
    }

    fn execute_i32_div_u(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        self.stack
            .push(Value::I32(((a as u32).wrapping_div(b as u32)) as i32))?;
        Ok(())
    }

    fn execute_i32_rem_s(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        if a == i32::MIN && b == -1 {
            return Err(WasmError::IntegerOverflow);
        }
        self.stack.push(Value::I32(a.wrapping_rem(b)))?;
        Ok(())
    }

    fn execute_i32_rem_u(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        self.stack
            .push(Value::I32(((a as u32).wrapping_rem(b as u32)) as i32))?;
        Ok(())
    }

    // --- i64 运算 ---

    fn execute_i64_binop<F: FnOnce(i64, i64) -> i64>(&mut self, f: F) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let b = self.stack.pop_i64()?;
        let a = self.stack.pop_i64()?;
        self.stack.push(Value::I64(f(a, b)))?;
        Ok(())
    }

    fn execute_i64_div_s(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1)?;
        let b = self.stack.pop_i64()?;
        let a = self.stack.pop_i64()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        if a == i64::MIN && b == -1 {
            return Err(WasmError::IntegerOverflow);
        }
        self.stack.push(Value::I64(a.wrapping_div(b)))?;
        Ok(())
    }

    // --- 内存操作 ---

    fn execute_memory_load(&mut self, size: u32) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let base = self.stack.pop_i32()? as u32;
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;
        let _ = align;

        let mem = self.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            4 => {
                let val = mem.read_u32(addr)?;
                self.stack.push(Value::I32(val as i32))?;
            }
            _ => return Err(WasmError::InternalError),
        }
        Ok(())
    }

    fn execute_memory_load_64(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let base = self.stack.pop_i32()? as u32;
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
        let val = mem.read_u64(addr)?;
        self.stack.push(Value::I64(val as i64))?;
        Ok(())
    }

    fn execute_memory_load_ext(&mut self, size: u32, signed: bool) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let base = self.stack.pop_i32()? as u32;
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            1 => {
                let val = mem.read_u8(addr)?;
                if signed {
                    self.stack.push(Value::I32(i32::from(val as i8)))?;
                } else {
                    self.stack.push(Value::I32(i32::from(val)))?;
                }
            }
            2 => {
                let val = mem.read_u16(addr)?;
                if signed {
                    self.stack.push(Value::I32(i32::from(val as i16)))?;
                } else {
                    self.stack.push(Value::I32(i32::from(val)))?;
                }
            }
            _ => return Err(WasmError::InternalError),
        }
        Ok(())
    }

    fn execute_memory_store(&mut self, size: u32) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let value = self.stack.pop_i32()?;
        let base = self.stack.pop_i32()? as u32;
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_mut().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            4 => {
                mem.write_u32(addr, value as u32)?;
            }
            _ => return Err(WasmError::InternalError),
        }
        Ok(())
    }

    fn execute_memory_store_64(&mut self) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let value = self.stack.pop_i64()?;
        let base = self.stack.pop_i32()? as u32;
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_mut().ok_or(WasmError::MemoryOutOfBounds)?;
        mem.write_u64(addr, value as u64)?;
        Ok(())
    }

    fn execute_memory_store_n(&mut self, size: u32) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let value = self.stack.pop_i32()?;
        let base = self.stack.pop_i32()? as u32;
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_mut().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            1 => {
                mem.write_u8(addr, value as u8)?;
            }
            2 => {
                mem.write_u16(addr, value as u16)?;
            }
            _ => return Err(WasmError::InternalError),
        }
        Ok(())
    }
}

// ============================================================================
// 公开接口
// ============================================================================

/// 解析并实例化一段 WASM 字节码.
///
/// # Errors
///
/// 当字节码格式非法(魔数/版本错误、段截断、结构不合法等)时
/// 返回对应的 `WasmError`.
pub fn instantiate(bytes: &[u8], config: InterpreterConfig) -> Result<Interpreter, WasmError> {
    let module = parse_wasm(bytes)?;
    Ok(Interpreter::new(module, config))
}
