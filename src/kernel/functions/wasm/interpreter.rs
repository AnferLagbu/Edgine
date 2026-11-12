#![deny(unsafe_code)]
#![deny(missing_docs)]
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

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::leb128::{read_leb128_i32, read_leb128_i64, read_leb128_u32};
use super::module::parse_wasm;
use super::runtime::{
    CallFrame, ControlLabel, InterpreterConfig, LabelKind, LinearMemory, ValueStack,
};
use super::types::{
    ExportKind, FuncType, FunctionBody, ImportKind, Opcode, Value, WasmError, WasmModule,
};

// ============================================================================
// 解释器
// ============================================================================

/// 宿主函数签名: 接收解释器可变引用, 返回执行结果
///
/// 用 `Rc` 而非 `Box`: 宿主调用期间允许闭包被多处共享持有 (例如
/// `WasmInstance` 与已装配的调用表), 不再需要把闭包从调用表里临时取出。
pub type HostFunction = Rc<dyn Fn(&mut Interpreter) -> Result<(), WasmError>>;

/// WASM 解释器 — 字节码执行引擎实例
pub struct Interpreter {
    /// 操作数栈
    pub stack: ValueStack,
    /// 线性内存 (模块无内存段时为 None)
    pub memory: Option<LinearMemory>,
    call_stack: Vec<CallFrame>,
    /// 解释器配置 (gas 上限等)
    pub config: InterpreterConfig,
    /// 已消耗的 gas 量
    pub gas_used: u64,
    /// 退出码
    pub exit_code: i32,
    module: WasmModule,
    host_functions: Vec<HostFunction>,
    import_func_count: u32,
    globals: Vec<Value>,
    /// 函数表: `None` = 未初始化 (null funcref)
    tables: Vec<Vec<Option<u32>>>,
    /// 名称索引的 host function: (module, name) → 已注册闭包
    named_host_functions: BTreeMap<(String, String), HostFunction>,
}

impl Interpreter {
    /// 由 WASM 模块与配置构造解释器 (统计 import 数并初始化全局量与表)
    #[must_use]
    pub fn new(module: WasmModule, config: InterpreterConfig) -> Self {
        let import_func_count = crate::functions::intcast::low_u32_from_usize(
            module
                .imports
                .iter()
                .filter(|i| matches!(i.desc, ImportKind::Function(_)))
                .count(),
        );

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

        // B07-12: 把配置的 `memory_pages` 硬上限接入内存创建路径 —
        // 模块声明的 max 与配置上限取交集; 未声明时强制配置上限 (256 页),
        // 防止恶意 bytecode 声明无界 max 后无限增长线性内存 (DoS).
        let cap_max = |declared: Option<u32>| {
            Some(match declared {
                Some(d) => d.min(config.memory_pages),
                None => config.memory_pages,
            })
        };

        let mut memory = None;
        for imp in &module.imports {
            if let ImportKind::Memory(mem_type) = &imp.desc {
                memory = Some(LinearMemory::new(
                    mem_type.limits.min,
                    cap_max(mem_type.limits.max),
                ));
                break;
            }
        }
        if memory.is_none()
            && let Some(mem) = module.memories.first()
        {
            memory = Some(LinearMemory::new(mem.limits.min, cap_max(mem.limits.max)));
        }

        let import_table_count = module
            .imports
            .iter()
            .filter(|i| matches!(i.desc, ImportKind::Table(_)))
            .count();

        let mut tables: Vec<Vec<Option<u32>>> =
            Vec::with_capacity(import_table_count + module.tables.len());
        for imp in &module.imports {
            if let ImportKind::Table(ref tt) = imp.desc {
                tables.push(vec![None; tt.limits.min as usize]);
            }
        }
        for tt in &module.tables {
            tables.push(vec![None; tt.limits.min as usize]);
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
            if let Ok(val) = Self::eval_init_expr(init_expr, &interp.globals)
                && global_idx < interp.globals.len()
            {
                interp.globals[global_idx] = val;
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
            if !seg.offset.is_empty()
                && let Ok(Value::I32(offset)) = Self::eval_init_expr(&seg.offset, &self.globals)
                && let Some(ref mut mem) = self.memory
            {
                let addr = crate::functions::intcast::usize_from_i32(offset);
                if addr + seg.data.len() <= mem.data.len() {
                    mem.data[addr..addr + seg.data.len()].copy_from_slice(&seg.data);
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
                let base = crate::functions::intcast::usize_from_i32(offset);
                let table_len = self.tables[table_idx].len();
                for (i, &func_idx) in seg.func_indices.iter().enumerate() {
                    let target = base + i;
                    if target < table_len {
                        self.tables[table_idx][target] = Some(func_idx);
                    }
                }
            }
        }
    }

    /// 注册匿名 host function (追加到调用表末尾)
    pub fn register_host_function(&mut self, f: HostFunction) {
        self.host_functions.push(f);
    }

    /// 注册名称匹配的 host function
    ///
    /// 注册后，`auto_register_wasi` 根据 WASM import section 的 module/name
    /// 把闭包装配到 `host_functions` 的对应 index 位置。
    pub fn register_named_host_function(&mut self, module: &str, name: &str, f: HostFunction) {
        self.named_host_functions
            .insert((String::from(module), String::from(name)), f);
    }

    /// 自动注册 WASI 函数 (根据 WASM 模块 import section)
    ///
    /// 按 import section 的声明顺序重建 `host_functions`: 每个函数 import 占一个槽位，
    /// 命中 `named_host_functions` 的用已注册闭包，其余（未知模块 / 未注册名称）
    /// 填无操作闭包。乱序、重复与未知 import 均按声明位置正确映射。
    ///
    /// 调用顺序: 先通过 `register_named_host_function` 注册所有 WASI 函数，
    /// 再调用本方法完成 import section 到 `host_functions` 的映射。
    pub fn auto_register_wasi(&mut self) {
        let mut ordered: Vec<HostFunction> = Vec::with_capacity(self.module.imports.len());
        let mut func_idx = 0u32;
        for import in &self.module.imports {
            if let ImportKind::Function(_) = import.desc {
                let module_name = core::str::from_utf8(&import.module).unwrap_or("");
                let func_name = core::str::from_utf8(&import.name).unwrap_or("");
                let registered = if module_name == "wasi_snapshot_preview1" {
                    self.named_host_functions
                        .get(&(String::from(module_name), String::from(func_name)))
                        .cloned()
                } else {
                    None
                };
                let host: HostFunction = match registered {
                    Some(f) => f,
                    None => Rc::new(|_: &mut Interpreter| Ok(())),
                };
                ordered.push(host);
                func_idx += 1;
            }
        }
        self.host_functions = ordered;
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
            // 宿主函数先弹 arg0 (WASI 约定) ⇒ 逆序压栈使 arg0 位于栈顶
            for arg in args.iter().take(param_count).rev() {
                self.stack.push(*arg)?;
            }

            let host_idx = func_idx as usize;
            if let Some(f) = self.host_functions.get(host_idx).cloned() {
                f(self)?;
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

        if crate::functions::intcast::low_u32_from_usize(self.call_stack.len())
            >= self.config.call_depth
        {
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
            labels: Vec::new(),
        };

        self.call_stack.push(frame);
        self.execute_call_stack()?;

        // 函数帧在 End (标签栈空) 时已由执行循环弹出; 结果值留在栈上,
        // 此处按声明 arity 取回并按帧基准截断 (原实现在此处二次 pop 帧,
        // 导致返回值恒丢).
        let actual_results = self.stack.len() - stack_base;
        let result = if func_type.results.len() == 1 && actual_results >= 1 {
            Some(self.stack.pop()?)
        } else {
            None
        };
        self.stack.drain_to(stack_base);
        Ok(result)
    }

    /// 执行调用栈直至全部帧结束: 取指令 → 计量 gas → 分派单条指令
    fn execute_call_stack(&mut self) -> Result<(), WasmError> {
        while let Some(opcode) = self.fetch_opcode() {
            let Some(op) = Opcode::from_byte(opcode) else {
                // 浮点族字节未进 Opcode 枚举 (甲 裁定不纳入): 明确报 Unsupported
                return Err(if Self::is_float_opcode(opcode) {
                    WasmError::Unsupported
                } else {
                    WasmError::UnknownOpcode(opcode)
                });
            };
            self.gas_used += 1;
            if self.gas_used > self.config.gas {
                return Err(WasmError::GasExhausted);
            }
            self.execute_op(op, opcode)?;
        }

        Ok(())
    }

    /// 取栈顶帧的下一条指令字节; 当前帧已执行完则弹出, 栈空返回 `None`
    fn fetch_opcode(&mut self) -> Option<u8> {
        while let Some(frame) = self.call_stack.last() {
            if frame.pc < frame.code.len() {
                return Some(frame.code[frame.pc]);
            }
            self.call_stack.pop();
        }
        None
    }

    /// 按 opcode 组分派单条指令; 未实装 opcode 返回 `UnknownOpcode`
    fn execute_op(&mut self, op: Opcode, opcode: u8) -> Result<(), WasmError> {
        if let Some(result) = self.execute_control_op(op) {
            return result;
        }
        if let Some(result) = self.execute_var_op(op) {
            return result;
        }
        if let Some(result) = self.execute_memory_op(op) {
            return result;
        }
        if let Some(result) = self.execute_i32_op(op) {
            return result;
        }
        if let Some(result) = self.execute_i64_op(op) {
            return result;
        }
        if Self::is_float_opcode(opcode) {
            // §3 甲 裁定: 浮点明确不纳入, 遇浮点 opcode 返 Unsupported
            return Err(WasmError::Unsupported);
        }
        Err(WasmError::UnknownOpcode(opcode))
    }

    /// 是否为 WASM 1.0 浮点族 opcode (本实现明确不支持).
    fn is_float_opcode(byte: u8) -> bool {
        matches!(byte, 0x2A | 0x2B | 0x38 | 0x39 | 0x43 | 0x44 | 0x8B..=0xBF)
    }

    /// 控制流指令组 (`None` = 不属本组)
    fn execute_control_op(&mut self, op: Opcode) -> Option<Result<(), WasmError>> {
        match op {
            Opcode::Unreachable => Some(Err(WasmError::Unreachable)),
            Opcode::CallIndirect => Some(self.execute_call_indirect()),
            Opcode::Nop => {
                self.advance_pc(1);
                Some(Ok(()))
            }
            Opcode::Block => Some(self.execute_block(LabelKind::Block)),
            Opcode::Loop => Some(self.execute_block(LabelKind::Loop)),
            Opcode::End => Some(self.execute_end()),
            Opcode::Return => Some(self.execute_return()),
            Opcode::If => Some(self.execute_if()),
            Opcode::Else => Some(self.execute_else()),
            Opcode::Br => Some(self.execute_br()),
            Opcode::BrIf => Some(self.execute_br_if()),
            Opcode::BrTable => Some(self.execute_br_table()),
            Opcode::Call => Some(self.execute_call()),
            _ => None,
        }
    }

    /// 局部/全局变量与栈操作指令组 (`None` = 不属本组)
    fn execute_var_op(&mut self, op: Opcode) -> Option<Result<(), WasmError>> {
        match op {
            Opcode::Drop => {
                self.advance_pc(1);
                Some(self.stack.pop().map(|_| ()))
            }
            Opcode::Select => Some(self.execute_select()),
            Opcode::LocalGet => Some(self.execute_local_get()),
            Opcode::LocalSet => Some(self.execute_local_set()),
            Opcode::LocalTee => Some(self.execute_local_tee()),
            Opcode::GlobalGet => Some(self.execute_global_get()),
            Opcode::GlobalSet => Some(self.execute_global_set()),
            _ => None,
        }
    }

    /// `select`: 按条件从栈顶两值中选一
    fn execute_select(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        let cond = self.stack.pop_i32()?;
        let val2 = self.stack.pop()?;
        let val1 = self.stack.pop()?;
        self.stack.push(if cond != 0 { val1 } else { val2 })
    }

    /// 内存读写与内存尺寸指令组 (`None` = 不属本组)
    fn execute_memory_op(&mut self, op: Opcode) -> Option<Result<(), WasmError>> {
        match op {
            Opcode::I32Load => Some(self.execute_memory_load(4)),
            Opcode::I64Load => Some(self.execute_memory_load_64()),
            Opcode::I32Load8S => Some(self.execute_memory_load_ext(1, true)),
            Opcode::I32Load8U => Some(self.execute_memory_load_ext(1, false)),
            Opcode::I32Load16S => Some(self.execute_memory_load_ext(2, true)),
            Opcode::I32Load16U => Some(self.execute_memory_load_ext(2, false)),
            Opcode::I64Load8S => Some(self.execute_memory_load_64_ext(1, true)),
            Opcode::I64Load8U => Some(self.execute_memory_load_64_ext(1, false)),
            Opcode::I64Load16S => Some(self.execute_memory_load_64_ext(2, true)),
            Opcode::I64Load16U => Some(self.execute_memory_load_64_ext(2, false)),
            Opcode::I64Load32S => Some(self.execute_memory_load_64_ext(4, true)),
            Opcode::I64Load32U => Some(self.execute_memory_load_64_ext(4, false)),
            Opcode::I32Store => Some(self.execute_memory_store(4)),
            Opcode::I64Store => Some(self.execute_memory_store_64()),
            Opcode::I32Store8 => Some(self.execute_memory_store_n(1)),
            Opcode::I32Store16 => Some(self.execute_memory_store_n(2)),
            Opcode::I64Store8 => Some(self.execute_memory_store_64_n(1)),
            Opcode::I64Store16 => Some(self.execute_memory_store_64_n(2)),
            Opcode::I64Store32 => Some(self.execute_memory_store_64_n(4)),
            Opcode::MemorySize => {
                self.advance_pc(1);
                let pages = self
                    .memory
                    .as_ref()
                    .map_or(0, super::runtime::LinearMemory::pages);
                Some(self.stack.push(Value::I32(pages.cast_signed())))
            }
            Opcode::MemoryGrow => Some(self.execute_memory_grow()),
            _ => None,
        }
    }

    /// `memory.grow`: 增长线性内存一页数, 失败返回 `u32::MAX`
    fn execute_memory_grow(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        let pages = self.stack.pop_i32()?;
        let result = if let Some(ref mut mem) = self.memory {
            mem.grow(pages.cast_unsigned())
        } else {
            u32::MAX
        };
        self.stack.push(Value::I32(result.cast_signed()))
    }

    /// 32 位整数常量/比较/算术指令组 (`None` = 不属本组)
    fn execute_i32_op(&mut self, op: Opcode) -> Option<Result<(), WasmError>> {
        match op {
            Opcode::I32Const => Some(self.execute_i32_const()),

            Opcode::I32Eqz => Some(self.execute_i32_unop(|a| i32::from(a == 0))),
            Opcode::I32Eq => Some(self.execute_i32_binop(|a, b| i32::from(a == b))),
            Opcode::I32Ne => Some(self.execute_i32_binop(|a, b| i32::from(a != b))),
            Opcode::I32LtS => Some(self.execute_i32_binop(|a, b| i32::from(a < b))),
            Opcode::I32LtU => Some(
                self.execute_i32_binop(|a, b| i32::from(a.cast_unsigned() < b.cast_unsigned())),
            ),
            Opcode::I32GtS => Some(self.execute_i32_binop(|a, b| i32::from(a > b))),
            Opcode::I32GtU => Some(
                self.execute_i32_binop(|a, b| i32::from(a.cast_unsigned() > b.cast_unsigned())),
            ),
            Opcode::I32LeS => Some(self.execute_i32_binop(|a, b| i32::from(a <= b))),
            Opcode::I32LeU => Some(
                self.execute_i32_binop(|a, b| i32::from(a.cast_unsigned() <= b.cast_unsigned())),
            ),
            Opcode::I32GeS => Some(self.execute_i32_binop(|a, b| i32::from(a >= b))),
            Opcode::I32GeU => Some(
                self.execute_i32_binop(|a, b| i32::from(a.cast_unsigned() >= b.cast_unsigned())),
            ),

            Opcode::I32Add => Some(self.execute_i32_binop(i32::wrapping_add)),
            Opcode::I32Sub => Some(self.execute_i32_binop(i32::wrapping_sub)),
            Opcode::I32Mul => Some(self.execute_i32_binop(i32::wrapping_mul)),
            Opcode::I32DivS => Some(self.execute_i32_div_s()),
            Opcode::I32DivU => Some(self.execute_i32_div_u()),
            Opcode::I32RemS => Some(self.execute_i32_rem_s()),
            Opcode::I32RemU => Some(self.execute_i32_rem_u()),
            Opcode::I32And => Some(self.execute_i32_binop(|a, b| a & b)),
            Opcode::I32Or => Some(self.execute_i32_binop(|a, b| a | b)),
            Opcode::I32Xor => Some(self.execute_i32_binop(|a, b| a ^ b)),
            Opcode::I32Shl => {
                Some(self.execute_i32_binop(|a, b| a.wrapping_shl(b.cast_unsigned())))
            }
            Opcode::I32ShrS => {
                Some(self.execute_i32_binop(|a, b| a.wrapping_shr(b.cast_unsigned())))
            }
            Opcode::I32ShrU => Some(self.execute_i32_binop(|a, b| {
                a.cast_unsigned()
                    .wrapping_shr(b.cast_unsigned())
                    .cast_signed()
            })),
            Opcode::I32Rotl => {
                Some(self.execute_i32_binop(|a, b| a.rotate_left(b.cast_unsigned())))
            }
            Opcode::I32Rotr => {
                Some(self.execute_i32_binop(|a, b| a.rotate_right(b.cast_unsigned())))
            }
            Opcode::I32WrapI64 => Some(self.execute_i32_wrap_i64()),
            _ => None,
        }
    }

    /// `i32.wrap_i64`: 取 i64 低 32 位
    fn execute_i32_wrap_i64(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        let value = self.stack.pop_i64()?;
        self.stack
            .push(Value::I32(crate::functions::intcast::low_i32_from_i64(
                value,
            )))
    }

    /// 64 位整数常量/算术指令组 (`None` = 不属本组)
    fn execute_i64_op(&mut self, op: Opcode) -> Option<Result<(), WasmError>> {
        match op {
            Opcode::I64Const => Some(self.execute_i64_const()),
            Opcode::I64Add => Some(self.execute_i64_binop(i64::wrapping_add)),
            Opcode::I64Sub => Some(self.execute_i64_binop(i64::wrapping_sub)),
            Opcode::I64Mul => Some(self.execute_i64_binop(i64::wrapping_mul)),
            Opcode::I64DivS => Some(self.execute_i64_div_s()),
            Opcode::I64And => Some(self.execute_i64_binop(|a, b| a & b)),
            Opcode::I64Or => Some(self.execute_i64_binop(|a, b| a | b)),

            Opcode::I64Eqz => Some(self.execute_i64_eqz()),
            Opcode::I64Eq => Some(self.execute_i64_cmp(|a, b| i32::from(a == b))),
            Opcode::I64Ne => Some(self.execute_i64_cmp(|a, b| i32::from(a != b))),
            Opcode::I64LtS => Some(self.execute_i64_cmp(|a, b| i32::from(a < b))),
            Opcode::I64LtU => {
                Some(self.execute_i64_cmp(|a, b| i32::from(a.cast_unsigned() < b.cast_unsigned())))
            }
            Opcode::I64GtS => Some(self.execute_i64_cmp(|a, b| i32::from(a > b))),
            Opcode::I64GtU => {
                Some(self.execute_i64_cmp(|a, b| i32::from(a.cast_unsigned() > b.cast_unsigned())))
            }
            Opcode::I64LeS => Some(self.execute_i64_cmp(|a, b| i32::from(a <= b))),
            Opcode::I64LeU => {
                Some(self.execute_i64_cmp(|a, b| i32::from(a.cast_unsigned() <= b.cast_unsigned())))
            }
            Opcode::I64GeS => Some(self.execute_i64_cmp(|a, b| i32::from(a >= b))),
            Opcode::I64GeU => {
                Some(self.execute_i64_cmp(|a, b| i32::from(a.cast_unsigned() >= b.cast_unsigned())))
            }

            Opcode::I64DivU => Some(self.execute_i64_div_u()),
            Opcode::I64RemS => Some(self.execute_i64_rem_s()),
            Opcode::I64RemU => Some(self.execute_i64_rem_u()),
            Opcode::I64Xor => Some(self.execute_i64_binop(|a, b| a ^ b)),
            Opcode::I64Shl => Some(self.execute_i64_binop(|a, b| {
                a.wrapping_shl(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            })),
            Opcode::I64ShrS => Some(self.execute_i64_binop(|a, b| {
                a.wrapping_shr(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            })),
            Opcode::I64ShrU => Some(self.execute_i64_binop(|a, b| {
                a.cast_unsigned()
                    .wrapping_shr(crate::functions::intcast::low_u32_from_u64(
                        b.cast_unsigned(),
                    ))
                    .cast_signed()
            })),
            Opcode::I64Rotl => Some(self.execute_i64_binop(|a, b| {
                a.rotate_left(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            })),
            Opcode::I64Rotr => Some(self.execute_i64_binop(|a, b| {
                a.rotate_right(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            })),

            Opcode::I64ExtendI32S => Some(self.execute_i64_extend_i32(true)),
            Opcode::I64ExtendI32U => Some(self.execute_i64_extend_i32(false)),
            _ => None,
        }
    }

    /// `i64.eqz`: 消费一个 i64, 推 i32 布尔结果.
    fn execute_i64_eqz(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        let a = self.stack.pop_i64()?;
        self.stack.push(Value::I32(i32::from(a == 0)))
    }

    /// `i64` 比较族: 消费两个 i64, 推 i32 布尔结果.
    fn execute_i64_cmp<F: FnOnce(i64, i64) -> i32>(&mut self, f: F) -> Result<(), WasmError> {
        self.advance_pc(1);
        let b = self.stack.pop_i64()?;
        let a = self.stack.pop_i64()?;
        self.stack.push(Value::I32(f(a, b)))
    }

    fn advance_pc(&mut self, amount: usize) {
        if let Some(frame) = self.call_stack.last_mut() {
            frame.pc += amount;
        }
    }

    fn current_frame_mut(&mut self) -> Result<&mut CallFrame, WasmError> {
        self.call_stack.last_mut().ok_or(WasmError::InternalError)
    }

    // --- 控制流 ---

    /// 读取块类型字节, 返回 `(结果 arity, 块体首指令位置)`.
    ///
    /// 支持空块 `0x40` 与单值 i32/i64; 浮点与多值块类型是本实现的明确
    /// 范围边界 (返 `Unsupported`), 其余非法字节返 `BadValueType`.
    fn read_block_type(code: &[u8], pc: usize) -> Result<(usize, usize), WasmError> {
        let byte = *code.get(pc).ok_or(WasmError::Truncated)?;
        match byte {
            0x40 => Ok((0, pc + 1)),
            0x7F | 0x7E => Ok((1, pc + 1)),
            0x7D | 0x7C | 0x7B | 0x70 | 0x6F => Err(WasmError::Unsupported),
            _ => {
                // s33 类型索引 (多值块): 先按 LEB 解码确认位置, 再判不支持 —
                // 直接把类型索引字节当指令继续消费会错位.
                let mut next = pc;
                let value = read_leb128_i32(code, &mut next)?;
                if value >= 0 {
                    Err(WasmError::Unsupported)
                } else {
                    Err(WasmError::BadValueType(byte))
                }
            }
        }
    }

    /// 跳过块类型字节 (结构扫描用; 允许浮点等本实现不执行的类型).
    fn skip_block_type(code: &[u8], pc: usize) -> Result<usize, WasmError> {
        match code.get(pc) {
            Some(0x40 | 0x7D | 0x7C | 0x7F | 0x7E | 0x7B | 0x70 | 0x6F) => Ok(pc + 1),
            Some(_) => {
                let mut next = pc;
                let _ = read_leb128_i32(code, &mut next)?;
                Ok(next)
            }
            None => Err(WasmError::Truncated),
        }
    }

    /// 扫描块体结构: 返回 `(else 落点, 匹配 end 之后位置)`.
    ///
    /// 逐指令跳过立即数, 不把立即数/嵌套块类型字节误判为块标签.
    fn scan_block_layout(code: &[u8], body_pc: usize) -> Result<(Option<usize>, usize), WasmError> {
        let mut depth = 0usize;
        let mut else_pc = None;
        let mut pc = body_pc;
        loop {
            let byte = *code.get(pc).ok_or(WasmError::Truncated)?;
            match byte {
                0x02..=0x04 => {
                    depth += 1;
                    pc = Self::skip_block_type(code, pc + 1)?;
                }
                0x0B => {
                    if depth == 0 {
                        return Ok((else_pc, pc + 1));
                    }
                    depth -= 1;
                    pc += 1;
                }
                0x05 => {
                    if depth == 0 {
                        if else_pc.is_some() {
                            return Err(WasmError::BadBlockStructure(byte));
                        }
                        else_pc = Some(pc + 1);
                    }
                    pc += 1;
                }
                _ => pc = Self::skip_instruction(code, pc)?,
            }
        }
    }

    /// 跳过一个指令 (含立即数); 未知/未支持 opcode 返错.
    fn skip_instruction(code: &[u8], pc: usize) -> Result<usize, WasmError> {
        let byte = *code.get(pc).ok_or(WasmError::Truncated)?;
        let after = pc + 1;
        match byte {
            0x02..=0x04 => Self::skip_block_type(code, after),
            0x0C | 0x0D | 0x10 | 0x20 | 0x21 | 0x22 | 0x23 | 0x24 => {
                Self::skip_leb_u32(code, after)
            }
            0x0E => {
                let mut next = after;
                let count = read_leb128_u32(code, &mut next)? as usize;
                // 每个目标至少 1 字节: 以上界先于循环/分配
                if count + 1 > code.len().saturating_sub(next) {
                    return Err(WasmError::Truncated);
                }
                for _ in 0..=count {
                    next = Self::skip_leb_u32(code, next)?;
                }
                Ok(next)
            }
            // call_indirect (类型索引 + 表索引) 与内存访问 (align + offset):
            // 均为两个 LEB 立即数
            0x11 | 0x28..=0x3E => {
                let next = Self::skip_leb_u32(code, after)?;
                Self::skip_leb_u32(code, next)
            }
            // memory.size / memory.grow: 保留内存索引字节 (恒 0x00)
            0x3F | 0x40 => {
                if after < code.len() {
                    Ok(after + 1)
                } else {
                    Err(WasmError::Truncated)
                }
            }
            0x41 => {
                let mut next = after;
                let _ = read_leb128_i32(code, &mut next)?;
                Ok(next)
            }
            0x42 => {
                let mut next = after;
                let _ = read_leb128_i64(code, &mut next)?;
                Ok(next)
            }
            _ => {
                // 其余已声明 opcode 无立即数; 未声明字节按未知 opcode 处理
                if Self::is_float_opcode(byte) {
                    return Err(WasmError::Unsupported);
                }
                Opcode::from_byte(byte).ok_or(WasmError::UnknownOpcode(byte))?;
                Ok(after)
            }
        }
    }

    fn skip_leb_u32(code: &[u8], pc: usize) -> Result<usize, WasmError> {
        let mut next = pc;
        let _ = read_leb128_u32(code, &mut next)?;
        Ok(next)
    }

    /// 弹出栈顶 `arity` 个分支值 (保持原始顺序).
    fn take_values(&mut self, arity: usize) -> Result<Vec<Value>, WasmError> {
        let mut values = Vec::with_capacity(arity);
        for _ in 0..arity {
            values.push(self.stack.pop()?);
        }
        values.reverse();
        Ok(values)
    }

    /// 截断到 `height` 后压回分支值 (块归一化).
    fn normalize_stack(&mut self, height: usize, values: Vec<Value>) -> Result<(), WasmError> {
        self.stack.drain_to(height);
        for value in values {
            self.stack.push(value)?;
        }
        Ok(())
    }

    /// 进入 block/loop: 消费块类型字节并压入标签.
    fn execute_block(&mut self, kind: LabelKind) -> Result<(), WasmError> {
        let (arity, body_pc, else_pc, end_pc) = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            let (arity, body_pc) = Self::read_block_type(&frame.code, frame.pc)?;
            let (else_pc, end_pc) = Self::scan_block_layout(&frame.code, body_pc)?;
            (arity, body_pc, else_pc, end_pc)
        };
        if else_pc.is_some() {
            // else 只允许出现在 if 内
            return Err(WasmError::BadBlockStructure(0x05));
        }
        let label = ControlLabel {
            kind,
            arity,
            stack_height: self.stack.len(),
            start_pc: body_pc,
            end_pc,
            else_pc: None,
        };
        let frame = self.current_frame_mut()?;
        frame.labels.push(label);
        frame.pc = body_pc;
        Ok(())
    }

    /// 函数返回: 保留结果 arity、截断到帧基准并弹出当前帧.
    fn return_from_function(&mut self) -> Result<(), WasmError> {
        let (stack_base, arity) = {
            let frame = self.call_stack.last().ok_or(WasmError::InternalError)?;
            (frame.stack_base, frame.arity)
        };
        let values = self.take_values(arity)?;
        self.normalize_stack(stack_base, values)?;
        let _ = self.call_stack.pop();
        Ok(())
    }

    fn execute_end(&mut self) -> Result<(), WasmError> {
        let maybe_label = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            frame.labels.pop()
        };
        if let Some(label) = maybe_label {
            let values = self.take_values(label.arity)?;
            self.normalize_stack(label.stack_height, values)
        } else {
            // 标签栈为空 ⇒ 函数体 end
            self.return_from_function()
        }
    }

    fn execute_return(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        self.return_from_function()
    }

    fn execute_if(&mut self) -> Result<(), WasmError> {
        let cond = self.stack.pop_i32()?;
        let (arity, body_pc, else_pc, end_pc) = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            let (arity, body_pc) = Self::read_block_type(&frame.code, frame.pc)?;
            let (else_pc, end_pc) = Self::scan_block_layout(&frame.code, body_pc)?;
            (arity, body_pc, else_pc, end_pc)
        };
        let stack_height = self.stack.len();
        let label = ControlLabel {
            kind: LabelKind::If,
            arity,
            stack_height,
            start_pc: body_pc,
            end_pc,
            else_pc,
        };
        let frame = self.current_frame_mut()?;
        if cond != 0 {
            frame.labels.push(label);
            frame.pc = body_pc;
        } else if let Some(else_start) = else_pc {
            frame.labels.push(label);
            frame.pc = else_start;
        } else {
            // 无 else: 假分支直接落到 end 之后; 结果 arity 必须为 0
            frame.pc = end_pc;
            self.stack.drain_to(stack_height);
        }
        Ok(())
    }

    fn execute_else(&mut self) -> Result<(), WasmError> {
        let label = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            let Some(label) = frame.labels.last().copied() else {
                return Err(WasmError::BadBlockStructure(0x05));
            };
            label
        };
        if label.kind != LabelKind::If {
            return Err(WasmError::BadBlockStructure(0x05));
        }
        // 真分支正常完成: 跳到匹配 end 之后并弹出 if 标签
        let frame = self.current_frame_mut()?;
        frame.labels.pop();
        frame.pc = label.end_pc;
        let values = self.take_values(label.arity)?;
        self.normalize_stack(label.stack_height, values)
    }

    /// br 系列公共落点: `depth` 为标签深度 (0 = 最内层; `labels.len()` = 函数返回).
    fn branch_to(&mut self, depth: usize) -> Result<(), WasmError> {
        let labels_len = self
            .call_stack
            .last()
            .ok_or(WasmError::InternalError)?
            .labels
            .len();
        if depth == labels_len {
            return self.return_from_function();
        }
        if depth > labels_len {
            return Err(WasmError::BadLabelDepth(depth));
        }
        let target_idx = labels_len - 1 - depth;
        let label = {
            let frame = self.current_frame_mut()?;
            frame.labels[target_idx]
        };
        let values = self.take_values(label.arity)?;
        if label.kind == LabelKind::Loop {
            // 回边: 保留 loop 标签, 截断到入口高度后回到块体首指令
            self.normalize_stack(label.stack_height, values)?;
            let frame = self.current_frame_mut()?;
            frame.labels.truncate(target_idx + 1);
            frame.pc = label.start_pc;
        } else {
            // 前向退出: 弹出目标及其内层标签, 跳到匹配 end 之后
            self.normalize_stack(label.stack_height, values)?;
            let frame = self.current_frame_mut()?;
            frame.labels.truncate(target_idx);
            frame.pc = label.end_pc;
        }
        Ok(())
    }

    fn execute_br(&mut self) -> Result<(), WasmError> {
        let depth = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            read_leb128_u32(&frame.code, &mut frame.pc)? as usize
        };
        self.branch_to(depth)
    }

    fn execute_br_if(&mut self) -> Result<(), WasmError> {
        let cond = self.stack.pop_i32()?;
        let depth = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            read_leb128_u32(&frame.code, &mut frame.pc)? as usize
        };
        if cond != 0 {
            self.branch_to(depth)?;
        }
        Ok(())
    }

    fn execute_br_table(&mut self) -> Result<(), WasmError> {
        let index = self.stack.pop_i32()?;
        let depth = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            let count = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
            // 每个目标至少 1 字节: 先做上界校验, 防止恶意计数造成长循环
            if count + 1 > frame.code.len().saturating_sub(frame.pc) {
                return Err(WasmError::Truncated);
            }
            let index_usize = if index >= 0 {
                Some(crate::functions::intcast::usize_from_i32(index))
            } else {
                None
            };
            let mut depth = 0usize;
            let mut picked = false;
            for i in 0..=count {
                let target = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
                if !picked && (i == count || index_usize == Some(i)) {
                    depth = target;
                    picked = true;
                }
            }
            depth
        };
        self.branch_to(depth)
    }

    fn execute_call(&mut self) -> Result<(), WasmError> {
        let func_idx = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            read_leb128_u32(&frame.code, &mut frame.pc)?
        };
        self.call_function(func_idx)
    }

    /// `call_indirect`: 经表取出函数索引, 做越界/空元素/签名校验后调用.
    ///
    /// 越界与未初始化元素、类型不符分别返回不同错误 (W-4 判据).
    fn execute_call_indirect(&mut self) -> Result<(), WasmError> {
        let (type_idx, table_idx) = {
            let frame = self.current_frame_mut()?;
            frame.pc += 1;
            let type_idx = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
            let table_idx = read_leb128_u32(&frame.code, &mut frame.pc)? as usize;
            (type_idx, table_idx)
        };
        let element_index = self.stack.pop_i32()?;
        if element_index < 0 {
            return Err(WasmError::TableIndexOutOfBounds(
                crate::functions::intcast::usize_from_i32(element_index),
            ));
        }
        let elem_idx = crate::functions::intcast::usize_from_i32(element_index);
        let table = self
            .tables
            .get(table_idx)
            .ok_or(WasmError::TableIndexOutOfBounds(table_idx))?;
        let Some(entry) = table.get(elem_idx) else {
            return Err(WasmError::TableIndexOutOfBounds(elem_idx));
        };
        let Some(func_idx) = *entry else {
            return Err(WasmError::UninitializedElement(elem_idx));
        };
        let expected = self
            .module
            .types
            .get(type_idx)
            .cloned()
            .ok_or(WasmError::BadTypeIndex(type_idx))?;
        let actual = self.get_func_type(func_idx)?.clone();
        if expected != actual {
            return Err(WasmError::IndirectCallTypeMismatch);
        }
        self.call_function(func_idx)
    }

    /// 按函数索引调用 (直接 `call` 与 `call_indirect` 共用).
    fn call_function(&mut self, func_idx: u32) -> Result<(), WasmError> {
        let func_type = self.get_func_type(func_idx)?.clone();
        let param_count = func_type.params.len();
        let stack_len = self.stack.len();
        if stack_len < param_count {
            return Err(WasmError::StackUnderflow);
        }

        let args: Vec<Value> = (0..param_count)
            .map(|i| self.stack.data[stack_len - param_count + i])
            .collect();
        self.stack.drain_to(stack_len - param_count);

        if func_idx < self.import_func_count {
            // 宿主函数先弹 arg0 (WASI 约定) ⇒ 逆序压栈使 arg0 位于栈顶
            for arg in args.iter().rev() {
                self.stack.push(*arg)?;
            }

            let host_idx = func_idx as usize;
            if let Some(f) = self.host_functions.get(host_idx).cloned() {
                f(self)?;
            }
            return Ok(());
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

        let stack_base = self.stack.len();
        let frame = CallFrame {
            func_idx,
            locals,
            pc: 0,
            code: body.code.clone(),
            arity: func_type.results.len(),
            return_pc: 0,
            stack_base,
            labels: Vec::new(),
        };

        self.call_stack.push(frame);
        Ok(())
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
        self.advance_pc(1);
        let a = self.stack.pop_i32()?;
        self.stack.push(Value::I32(f(a)))?;
        Ok(())
    }

    fn execute_i32_binop<F: FnOnce(i32, i32) -> i32>(&mut self, f: F) -> Result<(), WasmError> {
        self.advance_pc(1);
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        self.stack.push(Value::I32(f(a, b)))?;
        Ok(())
    }

    fn execute_i32_div_s(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
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
        self.advance_pc(1);
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        self.stack.push(Value::I32(
            (a.cast_unsigned().wrapping_div(b.cast_unsigned())).cast_signed(),
        ))?;
        Ok(())
    }

    fn execute_i32_rem_s(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
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
        self.advance_pc(1);
        let b = self.stack.pop_i32()?;
        let a = self.stack.pop_i32()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        self.stack.push(Value::I32(
            (a.cast_unsigned().wrapping_rem(b.cast_unsigned())).cast_signed(),
        ))?;
        Ok(())
    }

    // --- i64 运算 ---

    fn execute_i64_binop<F: FnOnce(i64, i64) -> i64>(&mut self, f: F) -> Result<(), WasmError> {
        self.advance_pc(1);
        let b = self.stack.pop_i64()?;
        let a = self.stack.pop_i64()?;
        self.stack.push(Value::I64(f(a, b)))?;
        Ok(())
    }

    fn execute_i64_div_s(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
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

    fn execute_i64_div_u(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        let b = self.stack.pop_i64()?;
        let a = self.stack.pop_i64()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        self.stack.push(Value::I64(
            (a.cast_unsigned().wrapping_div(b.cast_unsigned())).cast_signed(),
        ))?;
        Ok(())
    }

    fn execute_i64_rem_s(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        let b = self.stack.pop_i64()?;
        let a = self.stack.pop_i64()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        if a == i64::MIN && b == -1 {
            return Err(WasmError::IntegerOverflow);
        }
        self.stack.push(Value::I64(a.wrapping_rem(b)))?;
        Ok(())
    }

    fn execute_i64_rem_u(&mut self) -> Result<(), WasmError> {
        self.advance_pc(1);
        let b = self.stack.pop_i64()?;
        let a = self.stack.pop_i64()?;
        if b == 0 {
            return Err(WasmError::DivisionByZero);
        }
        self.stack.push(Value::I64(
            (a.cast_unsigned().wrapping_rem(b.cast_unsigned())).cast_signed(),
        ))?;
        Ok(())
    }

    /// `i64.extend_i32_s/u`: 符号/零扩展 i32 到 i64
    fn execute_i64_extend_i32(&mut self, signed: bool) -> Result<(), WasmError> {
        self.advance_pc(1);
        let value = self.stack.pop_i32()?;
        let extended = if signed {
            i64::from(value)
        } else {
            i64::from(value.cast_unsigned())
        };
        self.stack.push(Value::I64(extended))
    }

    // --- 内存操作 ---

    fn execute_memory_load(&mut self, size: u32) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;
        let _ = align;

        let mem = self.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            4 => {
                let val = mem.read_u32(addr)?;
                self.stack.push(Value::I32(val.cast_signed()))?;
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

        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
        let val = mem.read_u64(addr)?;
        self.stack.push(Value::I64(val.cast_signed()))?;
        Ok(())
    }

    fn execute_memory_load_ext(&mut self, size: u32, signed: bool) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            1 => {
                let val = mem.read_u8(addr)?;
                if signed {
                    self.stack.push(Value::I32(i32::from(val.cast_signed())))?;
                } else {
                    self.stack.push(Value::I32(i32::from(val)))?;
                }
            }
            2 => {
                let val = mem.read_u16(addr)?;
                if signed {
                    self.stack.push(Value::I32(i32::from(val.cast_signed())))?;
                } else {
                    self.stack.push(Value::I32(i32::from(val)))?;
                }
            }
            _ => return Err(WasmError::InternalError),
        }
        Ok(())
    }

    /// i64 窄加载 (`i64.load8_s/u` / `load16_s/u` / `load32_s/u`): 读取后
    /// 按符号/零扩展为 i64.
    fn execute_memory_load_64_ext(&mut self, size: u32, signed: bool) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_ref().ok_or(WasmError::MemoryOutOfBounds)?;
        let value = match size {
            1 => {
                let v = mem.read_u8(addr)?;
                if signed {
                    i64::from(v.cast_signed())
                } else {
                    i64::from(v)
                }
            }
            2 => {
                let v = mem.read_u16(addr)?;
                if signed {
                    i64::from(v.cast_signed())
                } else {
                    i64::from(v)
                }
            }
            4 => {
                let v = mem.read_u32(addr)?;
                if signed {
                    i64::from(v.cast_signed())
                } else {
                    i64::from(v)
                }
            }
            _ => return Err(WasmError::InternalError),
        };
        self.stack.push(Value::I64(value))?;
        Ok(())
    }

    fn execute_memory_store(&mut self, size: u32) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let value = self.stack.pop_i32()?;
        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_mut().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            4 => {
                mem.write_u32(addr, value.cast_unsigned())?;
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
        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_mut().ok_or(WasmError::MemoryOutOfBounds)?;
        mem.write_u64(addr, value.cast_unsigned())?;
        Ok(())
    }

    fn execute_memory_store_n(&mut self, size: u32) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let value = self.stack.pop_i32()?;
        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_mut().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            1 => {
                mem.write_u8(addr, crate::functions::intcast::u8_from_i32(value))?;
            }
            2 => {
                mem.write_u16(addr, crate::functions::intcast::u16_from_i32(value))?;
            }
            _ => return Err(WasmError::InternalError),
        }
        Ok(())
    }

    /// i64 窄存储 (`i64.store8/16/32`): 写 i64 的低 8/16/32 位.
    fn execute_memory_store_64_n(&mut self, size: u32) -> Result<(), WasmError> {
        let frame = self.current_frame_mut()?;
        frame.pc += 1;
        let _align = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let mem_offset = read_leb128_u32(&frame.code, &mut frame.pc)?;
        let _ = frame;

        let value = self.stack.pop_i64()?;
        let base = self.stack.pop_i32()?.cast_unsigned();
        let addr = base
            .checked_add(mem_offset)
            .ok_or(WasmError::MemoryOutOfBounds)?;

        let mem = self.memory.as_mut().ok_or(WasmError::MemoryOutOfBounds)?;
        match size {
            1 => {
                mem.write_u8(
                    addr,
                    crate::functions::intcast::low_u8_from_u64(value.cast_unsigned()),
                )?;
            }
            2 => {
                mem.write_u16(
                    addr,
                    crate::functions::intcast::low_u16_from_u64(value.cast_unsigned()),
                )?;
            }
            4 => {
                mem.write_u32(
                    addr,
                    crate::functions::intcast::low_u32_from_u64(value.cast_unsigned()),
                )?;
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

// ============================================================================
// W-3 结构化控制流回归锁 (host-side)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::wasm::types::{
        ElementSegment, ExportDesc, FuncType, FunctionBody, ImportDesc, ImportKind, Limits,
        MemoryType, RefType, TableType, ValueType, WasmModule,
    };

    /// 构造单函数模块: 导出 "f", 参数/结果均为 i32.
    fn module(
        body: Vec<u8>,
        locals: Vec<(u32, ValueType)>,
        params: usize,
        results: usize,
    ) -> WasmModule {
        let param_types: Vec<ValueType> = (0..params).map(|_| ValueType::I32).collect();
        let result_types: Vec<ValueType> = (0..results).map(|_| ValueType::I32).collect();
        WasmModule {
            types: vec![FuncType {
                params: param_types,
                results: result_types,
            }],
            imports: Vec::new(),
            functions: vec![0],
            tables: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
            exports: vec![ExportDesc {
                name: b"f".to_vec(),
                kind: ExportKind::Function,
                index: 0,
            }],
            start_section: None,
            elements: Vec::new(),
            code: vec![FunctionBody { locals, code: body }],
            data: Vec::new(),
        }
    }

    fn run(body: Vec<u8>, locals: Vec<(u32, ValueType)>) -> Result<Option<Value>, WasmError> {
        let mut interp = Interpreter::new(module(body, locals, 0, 1), InterpreterConfig::default());
        interp.call("f", &[])
    }

    #[test]
    fn block_result_falls_through_and_normalizes() {
        // i32.const 7; block (result i32) { i32.const 42 } end; end
        // 块结果落穿: 块入口高度以上的 7 被截断, 只留 42.
        let body = vec![0x41, 0x07, 0x02, 0x7F, 0x41, 0x2A, 0x0B, 0x0B];
        assert_eq!(run(body, Vec::new()), Ok(Some(Value::I32(42))));
    }

    #[test]
    fn empty_block_type_is_consumed_not_memory_grow() {
        // 0x40 在旧实现中会被当 memory.grow 执行 (空栈即下溢);
        // W-3 必须把 0x40 消费为块类型. 块体留空, 常量在块后取值.
        let body = vec![0x02, 0x40, 0x0B, 0x41, 0x03, 0x0B];
        assert_eq!(run(body, Vec::new()), Ok(Some(Value::I32(3))));
    }

    #[test]
    fn nested_block_cross_level_br_unwinds_labels_not_frames() {
        // const 1; block(i32){ block(i32){ const 5; br 1 } const 9 } end
        // br 1 命中外层 block (而非调用帧): 5 落穿, 9 不执行.
        let body = vec![
            0x41, 0x01, 0x02, 0x7F, 0x02, 0x7F, 0x41, 0x05, 0x0C, 0x01, 0x41, 0x09, 0x0B, 0x0B,
            0x0B,
        ];
        assert_eq!(run(body, Vec::new()), Ok(Some(Value::I32(5))));
    }

    #[test]
    fn loop_early_exit_via_br_if_to_outer_block() {
        // block { loop { local0 += 1; if local0 >= 3 { br_if 1 } ; br 0 } } ; local0
        // 校验 loop 回边 + 跨层前向退出 + 栈清零.
        let body = vec![
            0x02, 0x40, 0x03, 0x40, 0x20, 0x00, 0x41, 0x01, 0x6A, 0x22, 0x00, 0x41, 0x03, 0x4E,
            0x0D, 0x01, 0x0C, 0x00, 0x0B, 0x0B, 0x20, 0x00, 0x0B,
        ];
        assert_eq!(
            run(body, alloc::vec![(1, ValueType::I32)]),
            Ok(Some(Value::I32(3)))
        );
    }

    #[test]
    fn if_then_and_else_paths() {
        let then_body = vec![
            0x41, 0x01, 0x04, 0x7F, 0x41, 0x0A, 0x05, 0x41, 0x14, 0x0B, 0x0B,
        ];
        assert_eq!(run(then_body, Vec::new()), Ok(Some(Value::I32(10))));
        let else_body = vec![
            0x41, 0x00, 0x04, 0x7F, 0x41, 0x0A, 0x05, 0x41, 0x14, 0x0B, 0x0B,
        ];
        assert_eq!(run(else_body, Vec::new()), Ok(Some(Value::I32(20))));
    }

    #[test]
    fn if_without_else_skips_body_when_false() {
        // cond 0; if { i32.const 9 }; i32.const 3; end → 3 (9 被跳过)
        let body = vec![0x41, 0x00, 0x04, 0x40, 0x41, 0x09, 0x0B, 0x41, 0x03, 0x0B];
        assert_eq!(run(body, Vec::new()), Ok(Some(Value::I32(3))));
    }

    #[test]
    fn br_table_default_target_with_value() {
        // block(i32){ block(i32){ const 7; const 5(index); br_table [0,1] default 1 } }
        // index 5 >= 2 ⇒ default 深度 1 (外层), 分支值 7 落穿.
        let body = vec![
            0x02, 0x7F, 0x02, 0x7F, 0x41, 0x07, 0x41, 0x05, 0x0E, 0x02, 0x00, 0x01, 0x01, 0x41,
            0x09, 0x0B, 0x0B, 0x0B,
        ];
        assert_eq!(run(body, Vec::new()), Ok(Some(Value::I32(7))));
    }

    #[test]
    fn br_to_function_depth_returns_value() {
        // block { const 6; br 1 (= 函数深度, 与 return 同义); const 9 } end
        let body = vec![0x02, 0x40, 0x41, 0x06, 0x0C, 0x01, 0x41, 0x09, 0x0B, 0x0B];
        assert_eq!(run(body, Vec::new()), Ok(Some(Value::I32(6))));
    }

    #[test]
    fn return_opcode_returns_value() {
        let body = vec![0x41, 0x04, 0x0F, 0x41, 0x09, 0x0B];
        assert_eq!(run(body, Vec::new()), Ok(Some(Value::I32(4))));
    }

    #[test]
    fn excessive_branch_depth_is_error() {
        let body = vec![0x0C, 0x05, 0x0B];
        assert_eq!(run(body, Vec::new()), Err(WasmError::BadLabelDepth(5)));
    }

    #[test]
    fn unsupported_block_type_is_error() {
        // f32 块类型: 明确不支持 (§3 甲 浮点不纳入)
        let body = vec![0x02, 0x7D, 0x0B, 0x0B];
        assert_eq!(run(body, Vec::new()), Err(WasmError::Unsupported));
    }

    #[test]
    fn else_without_if_is_error() {
        let body = vec![0x05, 0x0B];
        assert_eq!(
            run(body, Vec::new()),
            Err(WasmError::BadBlockStructure(0x05))
        );
    }

    #[test]
    fn unterminated_block_is_truncated_error() {
        let body = vec![0x02, 0x40, 0x41, 0x01];
        assert_eq!(run(body, Vec::new()), Err(WasmError::Truncated));
    }

    // ========================================================================
    // W-4 回归锁: call_indirect / 缺失 opcode / F13 funcref 类型建模
    // ========================================================================

    fn run_module(m: WasmModule, args: &[Value]) -> Result<Option<Value>, WasmError> {
        let mut interp = Interpreter::new(m, InterpreterConfig::default());
        interp.call("f", args)
    }

    fn build_module(
        types: Vec<FuncType>,
        function_types: Vec<u32>,
        code: Vec<FunctionBody>,
        export: (&str, u32),
        tables: Vec<TableType>,
        elements: Vec<ElementSegment>,
        memories: Vec<MemoryType>,
    ) -> WasmModule {
        WasmModule {
            types,
            imports: Vec::new(),
            functions: function_types,
            tables,
            memories,
            globals: Vec::new(),
            exports: vec![ExportDesc {
                name: export.0.as_bytes().to_vec(),
                kind: ExportKind::Function,
                index: export.1,
            }],
            start_section: None,
            elements,
            code,
            data: Vec::new(),
        }
    }

    fn unary_module(op: u8, input: ValueType, output: ValueType) -> WasmModule {
        build_module(
            vec![FuncType {
                params: vec![input],
                results: vec![output],
            }],
            vec![0],
            vec![FunctionBody {
                locals: Vec::new(),
                code: vec![0x20, 0x00, op, 0x0B],
            }],
            ("f", 0),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    fn binary_module(op: u8, input: ValueType, output: ValueType) -> WasmModule {
        build_module(
            vec![FuncType {
                params: vec![input, input],
                results: vec![output],
            }],
            vec![0],
            vec![FunctionBody {
                locals: Vec::new(),
                code: vec![0x20, 0x00, 0x20, 0x01, op, 0x0B],
            }],
            ("f", 0),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    fn memory_module(body: Vec<u8>) -> WasmModule {
        build_module(
            vec![FuncType {
                params: Vec::new(),
                results: vec![ValueType::I64],
            }],
            vec![0],
            vec![FunctionBody {
                locals: Vec::new(),
                code: body,
            }],
            ("f", 0),
            Vec::new(),
            Vec::new(),
            vec![MemoryType {
                limits: Limits { min: 1, max: None },
            }],
        )
    }

    /// 构造带单表两函数模块: func0 ()->i32 返回值 42; func1 为导出 "f",
    /// 其函数体由调用方给出 (含 call_indirect 立即数与元素索引压栈).
    fn indirect_module(
        caller_body: Vec<u8>,
        table_min: u32,
        element_indices: Vec<u32>,
    ) -> WasmModule {
        let types = vec![
            FuncType {
                params: Vec::new(),
                results: vec![ValueType::I32],
            },
            FuncType {
                params: vec![ValueType::I32],
                results: vec![ValueType::I32],
            },
        ];
        let code = vec![
            FunctionBody {
                locals: Vec::new(),
                code: vec![0x41, 0x2A, 0x0B],
            },
            FunctionBody {
                locals: Vec::new(),
                code: caller_body,
            },
        ];
        build_module(
            types,
            vec![0, 0],
            code,
            ("f", 1),
            vec![TableType {
                element_type: RefType::FuncRef,
                limits: Limits {
                    min: table_min,
                    max: None,
                },
            }],
            vec![ElementSegment {
                table_index: 0,
                offset: vec![0x41, 0x00, 0x0B],
                func_indices: element_indices,
            }],
            Vec::new(),
        )
    }

    #[test]
    fn call_indirect_dispatches_via_table() {
        // const 0 (元素下标); call_indirect type0 table0
        let body = vec![0x41, 0x00, 0x11, 0x00, 0x00, 0x0B];
        let m = indirect_module(body, 2, vec![0]);
        assert_eq!(run_module(m, &[]), Ok(Some(Value::I32(42))));
    }

    #[test]
    fn call_indirect_type_mismatch_is_error() {
        // 声明 type1 (i32->i32) 但表中函数为 type0 ()->i32
        let body = vec![0x41, 0x00, 0x11, 0x01, 0x00, 0x0B];
        let m = indirect_module(body, 2, vec![0]);
        assert_eq!(run_module(m, &[]), Err(WasmError::IndirectCallTypeMismatch));
    }

    #[test]
    fn call_indirect_element_out_of_bounds() {
        let body = vec![0x41, 0x05, 0x11, 0x00, 0x00, 0x0B];
        let m = indirect_module(body, 2, vec![0]);
        assert_eq!(run_module(m, &[]), Err(WasmError::TableIndexOutOfBounds(5)));
    }

    #[test]
    fn call_indirect_uninitialized_element() {
        // 表容量 2, 元素段只填下标 0; 下标 1 为 null funcref
        let body = vec![0x41, 0x01, 0x11, 0x00, 0x00, 0x0B];
        let m = indirect_module(body, 2, vec![0]);
        assert_eq!(run_module(m, &[]), Err(WasmError::UninitializedElement(1)));
    }

    #[test]
    fn call_indirect_table_index_out_of_bounds() {
        let body = vec![0x41, 0x00, 0x11, 0x00, 0x01, 0x0B];
        let m = indirect_module(body, 2, vec![0]);
        assert_eq!(run_module(m, &[]), Err(WasmError::TableIndexOutOfBounds(1)));
    }

    #[test]
    fn call_indirect_bad_function_index_in_table() {
        // 表元素指向不存在的函数索引 9
        let body = vec![0x41, 0x00, 0x11, 0x00, 0x00, 0x0B];
        let m = indirect_module(body, 2, vec![9]);
        assert_eq!(run_module(m, &[]), Err(WasmError::BadFuncIndex(9)));
    }

    #[test]
    fn i64_eqz_and_comparisons_cover_all_variants() {
        for (input, expect) in [(0i64, 1i32), (7, 0)] {
            let m = unary_module(0x50, ValueType::I64, ValueType::I32);
            assert_eq!(
                run_module(m, &[Value::I64(input)]),
                Ok(Some(Value::I32(expect)))
            );
        }

        let cases: [(u8, fn(i64, i64) -> bool); 10] = [
            (0x51, |a, b| a == b),
            (0x52, |a, b| a != b),
            (0x53, |a, b| a < b),
            (0x54, |a, b| a.cast_unsigned() < b.cast_unsigned()),
            (0x55, |a, b| a > b),
            (0x56, |a, b| a.cast_unsigned() > b.cast_unsigned()),
            (0x57, |a, b| a <= b),
            (0x58, |a, b| a.cast_unsigned() <= b.cast_unsigned()),
            (0x59, |a, b| a >= b),
            (0x5A, |a, b| a.cast_unsigned() >= b.cast_unsigned()),
        ];
        for (op, pred) in cases {
            for (a, b) in [(5i64, 7i64), (-1, 0)] {
                let m = binary_module(op, ValueType::I64, ValueType::I32);
                assert_eq!(
                    run_module(m, &[Value::I64(a), Value::I64(b)]),
                    Ok(Some(Value::I32(i32::from(pred(a, b))))),
                    "i64 比较 opcode 0x{op:02X} ({a},{b})"
                );
            }
        }
    }

    #[test]
    fn i64_arithmetic_bitwise_shift_rotate_cover_all_variants() {
        let cases: [(u8, fn(i64, i64) -> i64); 11] = [
            (0x80, |a, b| {
                a.cast_unsigned()
                    .wrapping_div(b.cast_unsigned())
                    .cast_signed()
            }),
            (0x81, |a, b| a.wrapping_rem(b)),
            (0x82, |a, b| {
                a.cast_unsigned()
                    .wrapping_rem(b.cast_unsigned())
                    .cast_signed()
            }),
            (0x83, |a, b| a & b),
            (0x84, |a, b| a | b),
            (0x85, |a, b| a ^ b),
            (0x86, |a, b| {
                a.wrapping_shl(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            }),
            (0x87, |a, b| {
                a.wrapping_shr(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            }),
            (0x88, |a, b| {
                a.cast_unsigned()
                    .wrapping_shr(crate::functions::intcast::low_u32_from_u64(
                        b.cast_unsigned(),
                    ))
                    .cast_signed()
            }),
            (0x89, |a, b| {
                a.rotate_left(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            }),
            (0x8A, |a, b| {
                a.rotate_right(crate::functions::intcast::low_u32_from_u64(
                    b.cast_unsigned(),
                ))
            }),
        ];
        let (a, b) = (0x0F0F_i64, 4i64);
        for (op, f) in cases {
            let m = binary_module(op, ValueType::I64, ValueType::I64);
            assert_eq!(
                run_module(m, &[Value::I64(a), Value::I64(b)]),
                Ok(Some(Value::I64(f(a, b)))),
                "i64 运算 opcode 0x{op:02X}"
            );
        }

        // 除零 trap
        let m = binary_module(0x80, ValueType::I64, ValueType::I64);
        assert_eq!(
            run_module(m, &[Value::I64(1), Value::I64(0)]),
            Err(WasmError::DivisionByZero)
        );
    }

    #[test]
    fn i32_rotations_wrap_and_extend() {
        let rotl = binary_module(0x77, ValueType::I32, ValueType::I32);
        assert_eq!(
            run_module(rotl, &[Value::I32(1), Value::I32(1)]),
            Ok(Some(Value::I32(2)))
        );
        let rotr = binary_module(0x78, ValueType::I32, ValueType::I32);
        assert_eq!(
            run_module(rotr, &[Value::I32(1), Value::I32(1)]),
            Ok(Some(Value::I32(1i32.rotate_right(1))))
        );

        let wrap = unary_module(0xA7, ValueType::I64, ValueType::I32);
        assert_eq!(run_module(wrap, &[Value::I64(7)]), Ok(Some(Value::I32(7))));

        let extend_s = unary_module(0xAC, ValueType::I32, ValueType::I64);
        assert_eq!(
            run_module(extend_s, &[Value::I32(-1)]),
            Ok(Some(Value::I64(-1)))
        );
        let extend_u = unary_module(0xAD, ValueType::I32, ValueType::I64);
        assert_eq!(
            run_module(extend_u, &[Value::I32(-1)]),
            Ok(Some(Value::I64(4_294_967_295)))
        );
    }

    #[test]
    fn i64_memory_narrow_load_store() {
        // (store 立即数, load 立即数, 期望值): 覆盖 i64 store8/16/32 与 load8_s/u、16_s/u、32_s/u
        let cases: [(Vec<u8>, u8, i64); 8] = [
            (vec![0x3E, 0x00, 0x00], 0x30, -1),
            (vec![0x3E, 0x00, 0x00], 0x31, 255),
            (vec![0x3E, 0x00, 0x00], 0x32, -1),
            (vec![0x3E, 0x00, 0x00], 0x33, 65_535),
            (vec![0x3E, 0x00, 0x00], 0x34, -1),
            (vec![0x3E, 0x00, 0x00], 0x35, 4_294_967_295),
            (vec![0x3C, 0x00, 0x00], 0x30, -1),
            (vec![0x3D, 0x00, 0x00], 0x32, -1),
        ];
        for (store, load, expect) in cases {
            let mut body = vec![0x41, 0x00, 0x42, 0x7F];
            body.extend_from_slice(&store);
            body.extend_from_slice(&[0x41, 0x00, load, 0x00, 0x00, 0x0B]);
            let m = memory_module(body);
            assert_eq!(
                run_module(m, &[]),
                Ok(Some(Value::I64(expect))),
                "i64 store/load 组合 {store:02X?} / 0x{load:02X}"
            );
        }
    }

    #[test]
    fn float_opcode_is_unsupported() {
        // f32.const (0x43) 属浮点族: 明确 Unsupported (非 UnknownOpcode)
        let body = vec![0x43, 0x0B];
        assert_eq!(
            run_module(module(body, Vec::new(), 0, 1), &[]),
            Err(WasmError::Unsupported)
        );
    }

    fn import_only_module(names: &[(&str, &str)], params: usize) -> WasmModule {
        WasmModule {
            types: vec![FuncType {
                params: vec![ValueType::I32; params],
                results: vec![ValueType::I32],
            }],
            imports: names
                .iter()
                .map(|(module_name, func_name)| ImportDesc {
                    module: module_name.as_bytes().to_vec(),
                    name: func_name.as_bytes().to_vec(),
                    desc: ImportKind::Function(0),
                })
                .collect(),
            functions: Vec::new(),
            tables: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
            exports: Vec::new(),
            start_section: None,
            elements: Vec::new(),
            code: Vec::new(),
            data: Vec::new(),
        }
    }

    /// 装配按 import 声明顺序: 乱序 + 重复 + 未知模块均按下标对齐, 且可重复调用
    #[test]
    fn auto_register_wasi_follows_import_order_and_duplicates() {
        let m = import_only_module(
            &[
                ("wasi_snapshot_preview1", "b"),
                ("wasi_snapshot_preview1", "a"),
                ("wasi_snapshot_preview1", "a"),
                ("env", "unknown"),
            ],
            0,
        );
        let mut interp = Interpreter::new(m, InterpreterConfig::default());
        let host_a: HostFunction = Rc::new(|i: &mut Interpreter| {
            i.stack.push(Value::I32(11))?;
            Ok(())
        });
        let host_b: HostFunction = Rc::new(|i: &mut Interpreter| {
            i.stack.push(Value::I32(22))?;
            Ok(())
        });
        interp.register_named_host_function("wasi_snapshot_preview1", "a", host_a);
        interp.register_named_host_function("wasi_snapshot_preview1", "b", host_b);
        interp.auto_register_wasi();

        assert_eq!(interp.call_func(0, &[]), Ok(Some(Value::I32(22))));
        assert_eq!(interp.call_func(1, &[]), Ok(Some(Value::I32(11))));
        assert_eq!(interp.call_func(2, &[]), Ok(Some(Value::I32(11))));
        assert_eq!(interp.host_functions.len(), 4);
        assert_eq!(interp.import_func_count, 4);

        // 幂等: 再次装配不改变映射
        interp.auto_register_wasi();
        assert_eq!(interp.call_func(0, &[]), Ok(Some(Value::I32(22))));
        assert_eq!(interp.call_func(1, &[]), Ok(Some(Value::I32(11))));
    }

    /// 入口直调 (`call_func`) 的宿主实参序: arg0 位于栈顶, 先被弹出
    #[test]
    fn host_call_func_pops_arg0_first() {
        let m = import_only_module(&[("wasi_snapshot_preview1", "first")], 2);
        let mut interp = Interpreter::new(m, InterpreterConfig::default());
        let host: HostFunction = Rc::new(|i: &mut Interpreter| {
            let first = i.stack.pop_i32()?;
            let _second = i.stack.pop_i32()?;
            i.stack.push(Value::I32(first))?;
            Ok(())
        });
        interp.register_named_host_function("wasi_snapshot_preview1", "first", host);
        interp.auto_register_wasi();
        assert_eq!(
            interp.call_func(0, &[Value::I32(7), Value::I32(9)]),
            Ok(Some(Value::I32(7)))
        );
    }
}
