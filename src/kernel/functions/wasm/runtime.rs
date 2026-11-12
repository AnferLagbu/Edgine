#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。纯数据结构定义。
//! 运行时数据结构 — functions 层策略主体
//!
//! ## T6-9 迁移记录
//!
//! 原属 privileged/wasm/runtime.rs, 2026-06-16 提取到 functions.
//! 纯数据结构 (值栈/调用帧/线性内存), 0 unsafe, 0 外部依赖.
//! privileged 仅保留 re-export.
//!
//! 包含:
//! - ValueStack: 值栈, 栈式虚拟机的核心数据结构
//! - CallFrame: 调用帧, 记录函数调用的上下文
//! - LinearMemory: 线性内存, WASM 规范定义的主内存抽象

use alloc::vec;
use alloc::vec::Vec;

use super::types::{Value, WASM_PAGE_SIZE, WasmError};

// ============================================================================
// 值栈
// ============================================================================

/// WASM 值栈 (栈式虚拟机操作数栈)
pub struct ValueStack {
    /// 栈内存储的操作数值
    pub data: Vec<Value>,
}

impl ValueStack {
    /// 构造空值栈 (预留 256 个槽位容量)
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Vec::with_capacity(256),
        }
    }

    /// 压入一个值到值栈顶.
    ///
    /// # Errors
    ///
    /// 当栈深度达到上限(4096)时返回 `WasmError::StackOverflow`.
    pub fn push(&mut self, v: Value) -> Result<(), WasmError> {
        if self.data.len() >= 4096 {
            return Err(WasmError::StackOverflow);
        }
        self.data.push(v);
        Ok(())
    }

    /// 弹出栈顶值.
    ///
    /// # Errors
    ///
    /// 当栈为空时返回 `WasmError::StackUnderflow`.
    pub fn pop(&mut self) -> Result<Value, WasmError> {
        self.data.pop().ok_or(WasmError::StackUnderflow)
    }

    /// 弹出栈顶值并解释为 i32.
    ///
    /// # Errors
    ///
    /// 栈为空时返回 `WasmError::StackUnderflow`; 类型不匹配时返回
    /// `WasmError::TypeMismatch`.
    pub fn pop_i32(&mut self) -> Result<i32, WasmError> {
        match self.pop()? {
            Value::I32(v) => Ok(v),
            _ => Err(WasmError::TypeMismatch),
        }
    }

    /// 弹出栈顶值并解释为 i64.
    ///
    /// # Errors
    ///
    /// 栈为空时返回 `WasmError::StackUnderflow`; 类型不匹配时返回
    /// `WasmError::TypeMismatch`.
    pub fn pop_i64(&mut self) -> Result<i64, WasmError> {
        match self.pop()? {
            Value::I64(v) => Ok(v),
            _ => Err(WasmError::TypeMismatch),
        }
    }

    /// 查看栈顶值 (不弹出)
    #[must_use]
    pub fn peek(&self) -> Option<&Value> {
        self.data.last()
    }

    /// 当前栈深度
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// 栈是否为空
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// 将栈截断到指定深度 (丢弃多余项)
    pub fn drain_to(&mut self, new_len: usize) {
        self.data.truncate(new_len);
    }

    /// 清空值栈
    pub fn clear(&mut self) {
        self.data.clear();
    }
}

impl Default for ValueStack {
    /// 等价于 [`ValueStack::new`] (空值栈)
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 调用帧
// ============================================================================

/// 控制标签种类 — `block` / `loop` / `if` 的 br 目标语义不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    /// `block`: br 跳到块尾 (前向退出)。
    Block,
    /// `loop`: br 跳到块首 (回边, 标签保持)。
    Loop,
    /// `if`: br 跳到 if 的匹配 `end` 之后 (前向退出)。
    If,
}

/// 块标签 — 每次进入 `block` / `loop` / `if` 压入一帧标签栈条目。
///
/// 记录进入时的操作数栈高度与块结果 arity, 供 `br*` / `end` / `else` 做
/// 值保留 + 栈截断归一化; `end_pc` 为匹配 `end` 之后的位置 (block/if 的 br
/// 目标), `start_pc` 为块体首指令 (loop 的回边目标)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlLabel {
    /// 标签种类
    pub kind: LabelKind,
    /// 块结果 arity (MVP 仅 0/1)
    pub arity: usize,
    /// 进入块时的操作数栈高度 (不含块结果)
    pub stack_height: usize,
    /// 块体首指令位置 (loop 回边目标)
    pub start_pc: usize,
    /// 匹配 `end` 之后的位置 (block/if 前向跳转目标)
    pub end_pc: usize,
    /// `if` 的 `else` 之后位置 (无 `else` 为 `None`)
    pub else_pc: Option<usize>,
}

/// 函数调用帧 (记录一次 WASM 函数调用的执行上下文)
pub struct CallFrame {
    /// 被调函数在模块函数表中的索引
    pub func_idx: u32,
    /// 局部变量槽
    pub locals: Vec<Value>,
    /// 指向 `code` 的指令指针
    pub pc: usize,
    /// 该函数体字节码
    pub code: Vec<u8>,
    /// 参数个数
    pub arity: usize,
    /// 返回后恢复的调用方指令指针
    pub return_pc: usize,
    /// 该帧在值栈中的基准位置
    pub stack_base: usize,
    /// 本帧的块标签栈 (最内层在末尾; 函数体本身不占标签)
    pub labels: Vec<ControlLabel>,
}

impl CallFrame {
    /// 读取 `pc` 处的一个字节 (越界时返回 `None`)
    #[must_use]
    pub fn read_u8(&self) -> Option<u8> {
        if self.pc < self.code.len() {
            Some(self.code[self.pc])
        } else {
            None
        }
    }
}

// ============================================================================
// 线性内存
// ============================================================================

/// WASM 线性内存 (按页组织的字节数组)
pub struct LinearMemory {
    /// 内存字节 (长度恒为页大小的整数倍)
    pub data: Vec<u8>,
    /// 页数上限 (`None` 表示不限制)
    pub max_pages: Option<u32>,
}

impl LinearMemory {
    /// 创建指定初始页数的线性内存 (内存分配失败会以 panic 形式暴露).
    #[must_use]
    pub fn new(initial_pages: u32, max_pages: Option<u32>) -> Self {
        let size = initial_pages as usize * WASM_PAGE_SIZE as usize;
        Self {
            data: vec![0u8; size],
            max_pages,
        }
    }

    /// 扩展线性内存指定的页数, 返回扩展前的页数.
    ///
    /// 超过上限或页数/字节数溢出时返回 `u32::MAX`(WASM 约定)表示失败.
    pub fn grow(&mut self, additional_pages: u32) -> u32 {
        let current_pages = crate::functions::intcast::low_u32_from_usize(
            self.data.len() / WASM_PAGE_SIZE as usize,
        );
        // B07-12: checked_add 防页数回绕 (恶意 huge additional_pages 不再静默回绕增长).
        let Some(new_pages) = current_pages.checked_add(additional_pages) else {
            return u32::MAX;
        };
        if let Some(max) = self.max_pages
            && new_pages > max
        {
            return u32::MAX;
        }
        // B07-12: checked_mul 防字节数溢出导致 resize panic.
        let Some(new_size) = (new_pages as usize).checked_mul(WASM_PAGE_SIZE as usize) else {
            return u32::MAX;
        };
        self.data.resize(new_size, 0);
        current_pages
    }

    /// 当前内存页数
    #[must_use]
    pub fn pages(&self) -> u32 {
        crate::functions::intcast::low_u32_from_usize(self.data.len() / WASM_PAGE_SIZE as usize)
    }

    fn check_access(&self, offset: u32, size: u32) -> Result<usize, WasmError> {
        let end = offset as usize + size as usize;
        if end > self.data.len() {
            return Err(WasmError::MemoryOutOfBounds);
        }
        Ok(offset as usize)
    }

    /// 从线性内存读取一个 u8.
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn read_u8(&self, offset: u32) -> Result<u8, WasmError> {
        let addr = self.check_access(offset, 1)?;
        Ok(self.data[addr])
    }

    /// 从线性内存读取一个 u16 (小端序).
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn read_u16(&self, offset: u32) -> Result<u16, WasmError> {
        let addr = self.check_access(offset, 2)?;
        Ok(u16::from_le_bytes([self.data[addr], self.data[addr + 1]]))
    }

    /// 从线性内存读取一个 u32 (小端序).
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn read_u32(&self, offset: u32) -> Result<u32, WasmError> {
        let addr = self.check_access(offset, 4)?;
        Ok(u32::from_le_bytes([
            self.data[addr],
            self.data[addr + 1],
            self.data[addr + 2],
            self.data[addr + 3],
        ]))
    }

    /// 从线性内存读取一个 u64 (小端序).
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn read_u64(&self, offset: u32) -> Result<u64, WasmError> {
        let addr = self.check_access(offset, 8)?;
        Ok(u64::from_le_bytes([
            self.data[addr],
            self.data[addr + 1],
            self.data[addr + 2],
            self.data[addr + 3],
            self.data[addr + 4],
            self.data[addr + 5],
            self.data[addr + 6],
            self.data[addr + 7],
        ]))
    }

    /// 向线性内存写入一个 u8.
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn write_u8(&mut self, offset: u32, value: u8) -> Result<(), WasmError> {
        let addr = self.check_access(offset, 1)?;
        self.data[addr] = value;
        Ok(())
    }

    /// 向线性内存写入一个 u16 (小端序).
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn write_u16(&mut self, offset: u32, value: u16) -> Result<(), WasmError> {
        let addr = self.check_access(offset, 2)?;
        let bytes = value.to_le_bytes();
        self.data[addr] = bytes[0];
        self.data[addr + 1] = bytes[1];
        Ok(())
    }

    /// 向线性内存写入一个 u32 (小端序).
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn write_u32(&mut self, offset: u32, value: u32) -> Result<(), WasmError> {
        let addr = self.check_access(offset, 4)?;
        let bytes = value.to_le_bytes();
        self.data[addr..addr + 4].copy_from_slice(&bytes);
        Ok(())
    }

    /// 向线性内存写入一个 u64 (小端序).
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn write_u64(&mut self, offset: u32, value: u64) -> Result<(), WasmError> {
        let addr = self.check_access(offset, 8)?;
        let bytes = value.to_le_bytes();
        self.data[addr..addr + 8].copy_from_slice(&bytes);
        Ok(())
    }

    /// 获取可写切片 (安全包装, 供 WASI 模块使用)
    ///
    /// 返回指向 WASM 线性内存 `[offset, offset+len)` 的可写切片。
    /// 调用方无需 unsafe。
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn get_slice_mut(&mut self, offset: u32, len: u32) -> Result<&mut [u8], WasmError> {
        let addr = self.check_access(offset, len)?;
        Ok(&mut self.data[addr..addr + len as usize])
    }

    /// 获取只读切片 (安全包装, 供 WASI 模块使用)
    ///
    /// 返回指向 WASM 线性内存 `[offset, offset+len)` 的只读切片。
    /// 调用方无需 unsafe。
    ///
    /// # Errors
    ///
    /// 当访问越界时返回 `WasmError::MemoryOutOfBounds`.
    pub fn get_slice(&self, offset: u32, len: u32) -> Result<&[u8], WasmError> {
        let addr = self.check_access(offset, len)?;
        Ok(&self.data[addr..addr + len as usize])
    }
}

// ============================================================================
// 解释器配置
// ============================================================================

/// 解释器资源上限配置
pub struct InterpreterConfig {
    /// 最大调用深度
    pub call_depth: u32,
    /// 最大 gas 消耗
    pub gas: u64,
    /// 最大内存页数
    pub memory_pages: u32,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self {
            call_depth: 256,
            gas: 10_000_000,
            memory_pages: 256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// `ValueStack` 容量上限: 4096 满, 第 4097 压入返 `StackOverflow`.
    #[test]
    fn value_stack_overflow_at_capacity() {
        let mut stack = ValueStack::new();
        for _ in 0..4096 {
            stack.push(Value::I32(0)).expect("4096 内压入应成功");
        }
        assert_eq!(stack.len(), 4096);
        assert_eq!(stack.push(Value::I32(0)), Err(WasmError::StackOverflow));
    }

    /// `ValueStack` 空栈下溢与 i32/i64 类型不匹配.
    #[test]
    fn value_stack_underflow_and_type_mismatch() {
        let mut stack = ValueStack::new();
        assert_eq!(stack.pop(), Err(WasmError::StackUnderflow));
        assert_eq!(stack.pop_i32(), Err(WasmError::StackUnderflow));
        stack.push(Value::I64(7)).expect("压入 i64");
        assert_eq!(stack.pop_i32(), Err(WasmError::TypeMismatch));
        stack.push(Value::I32(7)).expect("压入 i32");
        assert_eq!(stack.pop_i64(), Err(WasmError::TypeMismatch));
    }

    /// `ValueStack` peek 不弹栈, drain_to 截断, clear 清空.
    #[test]
    fn value_stack_peek_drain_clear() {
        let mut stack = ValueStack::new();
        assert!(stack.is_empty());
        stack.push(Value::I32(1)).expect("压入 1");
        stack.push(Value::I32(2)).expect("压入 2");
        stack.push(Value::I32(3)).expect("压入 3");
        assert_eq!(stack.peek(), Some(&Value::I32(3)));
        assert_eq!(stack.len(), 3, "peek 不改变栈深");
        stack.drain_to(1);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.peek(), Some(&Value::I32(1)));
        stack.clear();
        assert!(stack.is_empty());
    }

    /// `LinearMemory` grow: 正常增长返回旧页数; 超上限与页数回绕返 `u32::MAX`.
    #[test]
    fn linear_memory_grow_bounds() {
        let mut mem = LinearMemory::new(1, Some(4));
        assert_eq!(mem.pages(), 1);
        assert_eq!(mem.grow(1), 1, "增长 1 页返回旧页数 1");
        assert_eq!(mem.pages(), 2);
        assert_eq!(mem.grow(0), 2, "增长 0 页返回当前页数");
        assert_eq!(mem.grow(3), u32::MAX, "超出上限 4 应失败");
        assert_eq!(mem.pages(), 2, "失败后页数不变");
        // checked_add 回绕: 当前 2 页 + u32::MAX 溢出 ⇒ 失败.
        assert_eq!(mem.grow(u32::MAX), u32::MAX, "页数回绕应失败");
    }

    /// `LinearMemory` 越界读写返 `MemoryOutOfBounds`; 边界内正常.
    #[test]
    fn linear_memory_out_of_bounds() {
        let mut mem = LinearMemory::new(1, None); // 65536 字节
        assert_eq!(mem.read_u8(65535), Ok(0), "末字节可读");
        assert_eq!(mem.read_u8(65536), Err(WasmError::MemoryOutOfBounds));
        assert_eq!(mem.read_u32(65534), Err(WasmError::MemoryOutOfBounds));
        assert_eq!(mem.write_u8(65536, 1), Err(WasmError::MemoryOutOfBounds));
        assert!(mem.get_slice(65536, 1).is_err());
        assert!(mem.get_slice_mut(0, 65537).is_err());
        assert_eq!(mem.get_slice(0, 65536).expect("整页切片").len(), 65536);
    }

    /// `LinearMemory` 小端序读写往返 (u16/u32/u64).
    #[test]
    fn linear_memory_le_roundtrip() {
        let mut mem = LinearMemory::new(1, None);
        mem.write_u16(0, 0x1234).expect("写 u16");
        assert_eq!(mem.read_u16(0), Ok(0x1234));
        assert_eq!(
            mem.get_slice(0, 2).expect("切片"),
            [0x34u8, 0x12].as_slice()
        );
        mem.write_u32(4, 0x1234_5678).expect("写 u32");
        assert_eq!(mem.read_u32(4), Ok(0x1234_5678));
        assert_eq!(
            mem.get_slice(4, 4).expect("切片"),
            [0x78u8, 0x56, 0x34, 0x12].as_slice()
        );
        mem.write_u64(8, 0x0123_4567_89AB_CDEF).expect("写 u64");
        assert_eq!(mem.read_u64(8), Ok(0x0123_4567_89AB_CDEF));
    }

    /// `CallFrame::read_u8`: pc 在界内取字节, 越界返 None.
    #[test]
    fn call_frame_read_u8_bounds() {
        let mut frame = CallFrame {
            func_idx: 0,
            locals: Vec::new(),
            pc: 0,
            code: vec![0x41, 0x00],
            arity: 0,
            return_pc: 0,
            stack_base: 0,
            labels: Vec::new(),
        };
        assert_eq!(frame.read_u8(), Some(0x41));
        frame.pc = 1;
        assert_eq!(frame.read_u8(), Some(0x00));
        frame.pc = 2;
        assert_eq!(frame.read_u8(), None);
    }
}
