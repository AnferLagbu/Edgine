#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯类型定义。
//! WASM 类型定义 — functions 层策略主体
//!
//! ## T6-9 迁移记录
//!
//! 原属 privileged/wasm/types.rs, 2026-06-16 提取到 functions.
//! 纯类型定义 (值类型/操作码/模块结构/运行时类型), 0 unsafe.
//! privileged 仅保留 re-export.
//!
//! WebAssembly 1.0 核心规范
//!
//! 定义 WASM 虚拟机的核心数据类型:
//! - 值类型 (i32, i64, f32, f64)
//! - 操作码 (完整 WASM 1.0 opcode 集合)
//! - 模块结构 (Type, Function, Memory, Export, Code, Data 段)
//! - 解释器运行时类型 (Value, Stack, Frame)

use alloc::vec::Vec;

pub const WASM_MAGIC: u32 = 0x6D736100;
pub const WASM_VERSION: u32 = 1;

pub const WASM_PAGE_SIZE: u32 = 65536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ValueType {
    I32 = 0x7F,
    I64 = 0x7E,
    F32 = 0x7D,
    F64 = 0x7C,
}

impl ValueType {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x7F => Some(Self::I32),
            0x7E => Some(Self::I64),
            0x7D => Some(Self::F32),
            0x7C => Some(Self::F64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

impl Value {
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Self::I32(v) => Some(*v),
            _ => None,
        }
    }
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::I32(v) => Some(*v as u32),
            _ => None,
        }
    }
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(v) => Some(*v),
            _ => None,
        }
    }

    pub fn default_for(ty: ValueType) -> Self {
        match ty {
            ValueType::I32 => Self::I32(0),
            ValueType::I64 => Self::I64(0),
            ValueType::F32 => Self::F32(0),
            ValueType::F64 => Self::F64(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<ValueType>,
    pub results: Vec<ValueType>,
}

#[derive(Debug, Clone)]
pub struct ImportDesc {
    pub module: Vec<u8>,
    pub name: Vec<u8>,
    pub desc: ImportKind,
}

#[derive(Debug, Clone)]
pub enum ImportKind {
    Function(u32),
    Table(TableType),
    Memory(MemoryType),
    Global(GlobalType),
}

#[derive(Debug, Clone)]
pub struct TableType {
    pub element_type: ValueType,
    pub limits: Limits,
}

#[derive(Debug, Clone)]
pub struct MemoryType {
    pub limits: Limits,
}

#[derive(Debug, Clone)]
pub struct GlobalType {
    pub content_type: ValueType,
    pub mutable: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub min: u32,
    pub max: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ExportDesc {
    pub name: Vec<u8>,
    pub kind: ExportKind,
    pub index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExportKind {
    Function = 0,
    Table = 1,
    Memory = 2,
    Global = 3,
}

#[derive(Debug, Clone)]
pub struct FunctionBody {
    pub locals: Vec<(u32, ValueType)>,
    pub code: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DataSegment {
    pub memory_index: u32,
    pub offset: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ElementSegment {
    pub table_index: u32,
    pub offset: Vec<u8>,
    pub func_indices: Vec<u32>,
}

/// 完整的 WASM 模块解析结果
#[derive(Debug)]
pub struct WasmModule {
    pub types: Vec<FuncType>,
    pub imports: Vec<ImportDesc>,
    pub functions: Vec<u32>,
    pub tables: Vec<TableType>,
    pub memories: Vec<MemoryType>,
    pub globals: Vec<(GlobalType, Vec<u8>)>,
    pub exports: Vec<ExportDesc>,
    pub start_section: Option<u32>,
    pub elements: Vec<ElementSegment>,
    pub code: Vec<FunctionBody>,
    pub data: Vec<DataSegment>,
}

// ============================================================================
// 操作码定义 (WASM 1.0)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Unreachable = 0x00,
    Nop = 0x01,
    Block = 0x02,
    Loop = 0x03,
    If = 0x04,
    Else = 0x05,
    End = 0x0B,
    Br = 0x0C,
    BrIf = 0x0D,
    BrTable = 0x0E,
    Return = 0x0F,
    Call = 0x10,
    CallIndirect = 0x11,

    Drop = 0x1A,
    Select = 0x1B,

    LocalGet = 0x20,
    LocalSet = 0x21,
    LocalTee = 0x22,
    GlobalGet = 0x23,
    GlobalSet = 0x24,

    I32Load = 0x28,
    I64Load = 0x29,
    I32Load8S = 0x2C,
    I32Load8U = 0x2D,
    I32Load16S = 0x2E,
    I32Load16U = 0x2F,
    I64Load8S = 0x30,
    I64Load8U = 0x31,
    I64Load16S = 0x32,
    I64Load16U = 0x33,
    I64Load32S = 0x34,
    I64Load32U = 0x35,
    I32Store = 0x36,
    I64Store = 0x37,
    I32Store8 = 0x3A,
    I32Store16 = 0x3B,
    I64Store8 = 0x3C,
    I64Store16 = 0x3D,
    I64Store32 = 0x3E,
    MemorySize = 0x3F,
    MemoryGrow = 0x40,

    I32Const = 0x41,
    I64Const = 0x42,

    I32Eqz = 0x45,
    I32Eq = 0x46,
    I32Ne = 0x47,
    I32LtS = 0x48,
    I32LtU = 0x49,
    I32GtS = 0x4A,
    I32GtU = 0x4B,
    I32LeS = 0x4C,
    I32LeU = 0x4D,
    I32GeS = 0x4E,
    I32GeU = 0x4F,

    I64Eqz = 0x50,
    I64Eq = 0x51,
    I64Ne = 0x52,
    I64LtS = 0x53,
    I64LtU = 0x54,
    I64GtS = 0x55,
    I64GtU = 0x56,
    I64LeS = 0x57,
    I64LeU = 0x58,
    I64GeS = 0x59,
    I64GeU = 0x5A,

    I32Add = 0x6A,
    I32Sub = 0x6B,
    I32Mul = 0x6C,
    I32DivS = 0x6D,
    I32DivU = 0x6E,
    I32RemS = 0x6F,
    I32RemU = 0x70,
    I32And = 0x71,
    I32Or = 0x72,
    I32Xor = 0x73,
    I32Shl = 0x74,
    I32ShrS = 0x75,
    I32ShrU = 0x76,
    I32Rotl = 0x77,
    I32Rotr = 0x78,

    I64Add = 0x7C,
    I64Sub = 0x7D,
    I64Mul = 0x7E,
    I64DivS = 0x7F,
    I64DivU = 0x80,
    I64RemS = 0x81,
    I64RemU = 0x82,
    I64And = 0x83,
    I64Or = 0x84,
    I64Xor = 0x85,
    I64Shl = 0x86,
    I64ShrS = 0x87,
    I64ShrU = 0x88,
    I64Rotl = 0x89,
    I64Rotr = 0x8A,

    I32WrapI64 = 0xA7,
    I64ExtendI32S = 0xAC,
    I64ExtendI32U = 0xAD,
}

impl Opcode {
    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(Self::Unreachable),
            0x01 => Some(Self::Nop),
            0x02 => Some(Self::Block),
            0x03 => Some(Self::Loop),
            0x04 => Some(Self::If),
            0x05 => Some(Self::Else),
            0x0B => Some(Self::End),
            0x0C => Some(Self::Br),
            0x0D => Some(Self::BrIf),
            0x0E => Some(Self::BrTable),
            0x0F => Some(Self::Return),
            0x10 => Some(Self::Call),
            0x11 => Some(Self::CallIndirect),
            0x1A => Some(Self::Drop),
            0x1B => Some(Self::Select),
            0x20 => Some(Self::LocalGet),
            0x21 => Some(Self::LocalSet),
            0x22 => Some(Self::LocalTee),
            0x23 => Some(Self::GlobalGet),
            0x24 => Some(Self::GlobalSet),
            0x28 => Some(Self::I32Load),
            0x29 => Some(Self::I64Load),
            0x2C => Some(Self::I32Load8S),
            0x2D => Some(Self::I32Load8U),
            0x2E => Some(Self::I32Load16S),
            0x2F => Some(Self::I32Load16U),
            0x30 => Some(Self::I64Load8S),
            0x31 => Some(Self::I64Load8U),
            0x32 => Some(Self::I64Load16S),
            0x33 => Some(Self::I64Load16U),
            0x34 => Some(Self::I64Load32S),
            0x35 => Some(Self::I64Load32U),
            0x36 => Some(Self::I32Store),
            0x37 => Some(Self::I64Store),
            0x3A => Some(Self::I32Store8),
            0x3B => Some(Self::I32Store16),
            0x3C => Some(Self::I64Store8),
            0x3D => Some(Self::I64Store16),
            0x3E => Some(Self::I64Store32),
            0x3F => Some(Self::MemorySize),
            0x40 => Some(Self::MemoryGrow),
            0x41 => Some(Self::I32Const),
            0x42 => Some(Self::I64Const),
            0x45 => Some(Self::I32Eqz),
            0x46 => Some(Self::I32Eq),
            0x47 => Some(Self::I32Ne),
            0x48 => Some(Self::I32LtS),
            0x49 => Some(Self::I32LtU),
            0x4A => Some(Self::I32GtS),
            0x4B => Some(Self::I32GtU),
            0x4C => Some(Self::I32LeS),
            0x4D => Some(Self::I32LeU),
            0x4E => Some(Self::I32GeS),
            0x4F => Some(Self::I32GeU),
            0x50 => Some(Self::I64Eqz),
            0x51 => Some(Self::I64Eq),
            0x52 => Some(Self::I64Ne),
            0x53 => Some(Self::I64LtS),
            0x54 => Some(Self::I64LtU),
            0x55 => Some(Self::I64GtS),
            0x56 => Some(Self::I64GtU),
            0x57 => Some(Self::I64LeS),
            0x58 => Some(Self::I64LeU),
            0x59 => Some(Self::I64GeS),
            0x5A => Some(Self::I64GeU),
            0x6A => Some(Self::I32Add),
            0x6B => Some(Self::I32Sub),
            0x6C => Some(Self::I32Mul),
            0x6D => Some(Self::I32DivS),
            0x6E => Some(Self::I32DivU),
            0x6F => Some(Self::I32RemS),
            0x70 => Some(Self::I32RemU),
            0x71 => Some(Self::I32And),
            0x72 => Some(Self::I32Or),
            0x73 => Some(Self::I32Xor),
            0x74 => Some(Self::I32Shl),
            0x75 => Some(Self::I32ShrS),
            0x76 => Some(Self::I32ShrU),
            0x77 => Some(Self::I32Rotl),
            0x78 => Some(Self::I32Rotr),
            0x7C => Some(Self::I64Add),
            0x7D => Some(Self::I64Sub),
            0x7E => Some(Self::I64Mul),
            0x7F => Some(Self::I64DivS),
            0x80 => Some(Self::I64DivU),
            0x81 => Some(Self::I64RemS),
            0x82 => Some(Self::I64RemU),
            0x83 => Some(Self::I64And),
            0x84 => Some(Self::I64Or),
            0x85 => Some(Self::I64Xor),
            0x86 => Some(Self::I64Shl),
            0x87 => Some(Self::I64ShrS),
            0x88 => Some(Self::I64ShrU),
            0x89 => Some(Self::I64Rotl),
            0x8A => Some(Self::I64Rotr),
            0xA7 => Some(Self::I32WrapI64),
            0xAC => Some(Self::I64ExtendI32S),
            0xAD => Some(Self::I64ExtendI32U),
            _ => None,
        }
    }
}

pub const SECTION_TYPE: u8 = 1;
pub const SECTION_IMPORT: u8 = 2;
pub const SECTION_FUNCTION: u8 = 3;
pub const SECTION_TABLE: u8 = 4;
pub const SECTION_MEMORY: u8 = 5;
pub const SECTION_GLOBAL: u8 = 6;
pub const SECTION_EXPORT: u8 = 7;
pub const SECTION_START: u8 = 8;
pub const SECTION_ELEMENT: u8 = 9;
pub const SECTION_CODE: u8 = 10;
pub const SECTION_DATA: u8 = 11;

// ============================================================================
// 错误类型
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmError {
    InvalidMagic,
    InvalidVersion,
    Truncated,
    Leb128Overflow,
    UnknownSection(u8),
    UnknownOpcode(u8),
    TypeMismatch,
    StackUnderflow,
    StackOverflow,
    DivisionByZero,
    IntegerOverflow,
    MemoryOutOfBounds,
    MemoryGrowFailed,
    CallDepthExceeded,
    GasExhausted,
    Unreachable,
    BadExport,
    BadImport,
    FunctionNotFound,
    BadTypeIndex(usize),
    BadFuncIndex(usize),
    InternalError,
    /// WASI `proc_exit` 触发的实例终止
    Terminated,
}

pub const BLOCK_TYPE_EMPTY: i32 = -64;
