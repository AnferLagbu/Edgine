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
    pub fn peek(&self) -> Option<&Value> {
        self.data.last()
    }

    /// 当前栈深度
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// 栈是否为空
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

// ============================================================================
// 调用帧
// ============================================================================

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
}

impl CallFrame {
    /// 读取 `pc` 处的一个字节 (越界时返回 `None`)
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
        let current_pages = (self.data.len() / WASM_PAGE_SIZE as usize) as u32;
        // B07-12: checked_add 防页数回绕 (恶意 huge additional_pages 不再静默回绕增长).
        let Some(new_pages) = current_pages.checked_add(additional_pages) else {
            return u32::MAX;
        };
        if let Some(max) = self.max_pages {
            if new_pages > max {
                return u32::MAX;
            }
        }
        // B07-12: checked_mul 防字节数溢出导致 resize panic.
        let Some(new_size) = (new_pages as usize).checked_mul(WASM_PAGE_SIZE as usize) else {
            return u32::MAX;
        };
        self.data.resize(new_size, 0);
        current_pages
    }

    /// 当前内存页数
    pub fn pages(&self) -> u32 {
        (self.data.len() / WASM_PAGE_SIZE as usize) as u32
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
    pub max_call_depth: u32,
    /// 最大 gas 消耗
    pub max_gas: u64,
    /// 最大内存页数
    pub max_memory_pages: u32,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self {
            max_call_depth: 256,
            max_gas: 10_000_000,
            max_memory_pages: 256,
        }
    }
}
