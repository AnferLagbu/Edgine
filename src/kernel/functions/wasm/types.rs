#![deny(unsafe_code)]
#![deny(missing_docs)]
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

/// 模块头魔数 (小端读数, 即 ASCII 的 `\0asm`)。
pub const WASM_MAGIC: u32 = 0x6D73_6100;
/// 当前支持的 WASM 规范版本号 (1)。
pub const WASM_VERSION: u32 = 1;

/// 线性内存的页大小 (字节); 一页固定 64 KiB。
pub const WASM_PAGE_SIZE: u32 = 65536;

/// WASM 1.0 值类型 — 函数参数/返回值与局部变量的类型标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ValueType {
    /// 32 位整数类型 (编码 `0x7F`)。
    I32 = 0x7F,
    /// 64 位整数类型 (编码 `0x7E`)。
    I64 = 0x7E,
    /// 32 位浮点类型 (编码 `0x7D`)。
    F32 = 0x7D,
    /// 64 位浮点类型 (编码 `0x7C`)。
    F64 = 0x7C,
}

impl ValueType {
    /// 按类型标签字节解析值类型; 非法字节返回 `None`。
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

/// 解释器运行期的栈值 — 四种值类型各对应一个变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// 32 位整数栈值。
    I32(i32),
    /// 64 位整数栈值。
    I64(i64),
    /// 32 位浮点栈值, 以 IEEE-754 位模式 `u32` 保存。
    F32(u32),
    /// 64 位浮点栈值, 以 IEEE-754 位模式 `u64` 保存。
    F64(u64),
}

impl Value {
    /// 若为 `I32` 则取出 32 位整数, 否则返回 `None`。
    pub fn as_i32(self) -> Option<i32> {
        match self {
            Self::I32(v) => Some(v),
            _ => None,
        }
    }
    /// 若为 `I32` 则按无符号解释取出, 否则返回 `None`。
    pub fn as_u32(self) -> Option<u32> {
        match self {
            Self::I32(v) => Some(v.cast_unsigned()),
            _ => None,
        }
    }
    /// 若为 `I64` 则取出 64 位整数, 否则返回 `None`。
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::I64(v) => Some(v),
            _ => None,
        }
    }

    /// 返回给定值类型的零值。
    pub fn default_for(ty: ValueType) -> Self {
        match ty {
            ValueType::I32 => Self::I32(0),
            ValueType::I64 => Self::I64(0),
            ValueType::F32 => Self::F32(0),
            ValueType::F64 => Self::F64(0),
        }
    }
}

/// 函数类型 — 参数与结果的值类型列表 (WASM `functype`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    /// 参数值类型序列。
    pub params: Vec<ValueType>,
    /// 返回值类型序列。
    pub results: Vec<ValueType>,
}

/// 导入项描述 — 来源模块名/字段名与导入种类。
#[derive(Debug, Clone)]
pub struct ImportDesc {
    /// 导入来源模块名 (UTF-8 字节)。
    pub module: Vec<u8>,
    /// 导入字段名 (UTF-8 字节)。
    pub name: Vec<u8>,
    /// 导入的具体种类与类型索引。
    pub desc: ImportKind,
}

/// 导入种类 — 函数/表/内存/全局量四选一。
#[derive(Debug, Clone)]
pub enum ImportKind {
    /// 函数导入, 携带类型索引。
    Function(u32),
    /// 表导入, 携带表类型。
    Table(TableType),
    /// 内存导入, 携带内存类型。
    Memory(MemoryType),
    /// 全局量导入, 携带全局类型。
    Global(GlobalType),
}

/// 表类型 — 元素类型与尺寸限制。
#[derive(Debug, Clone)]
pub struct TableType {
    /// 表元素类型 (WASM 1.0 只允许 `funcref`)。
    pub element_type: ValueType,
    /// 表的初始/最大元素数限制。
    pub limits: Limits,
}

/// 内存类型 — 线性内存的尺寸限制。
#[derive(Debug, Clone)]
pub struct MemoryType {
    /// 内存的初始/最大页数限制。
    pub limits: Limits,
}

/// 全局量类型 — 值类型与可变性。
#[derive(Debug, Clone)]
pub struct GlobalType {
    /// 全局量的值类型。
    pub content_type: ValueType,
    /// 是否可写 (`true` 为可变)。
    pub mutable: bool,
}

/// 尺寸限制 — 最小/最大元素数 (最大值可缺省)。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// 最小尺寸。
    pub min: u32,
    /// 最大尺寸; `None` 表示无上界。
    pub max: Option<u32>,
}

/// 导出项描述 — 导出名/种类与索引。
#[derive(Debug, Clone)]
pub struct ExportDesc {
    /// 导出名 (UTF-8 字节)。
    pub name: Vec<u8>,
    /// 导出种类。
    pub kind: ExportKind,
    /// 被导出项在对应索引空间中的下标。
    pub index: u32,
}

/// 导出种类 — 函数/表/内存/全局量四选一。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExportKind {
    /// 函数导出 (编码 0)。
    Function = 0,
    /// 表导出 (编码 1)。
    Table = 1,
    /// 内存导出 (编码 2)。
    Memory = 2,
    /// 全局量导出 (编码 3)。
    Global = 3,
}

/// 函数体 — 局部变量声明与指令字节码。
#[derive(Debug, Clone)]
pub struct FunctionBody {
    /// 局部变量声明 (数量, 类型) 列表。
    pub locals: Vec<(u32, ValueType)>,
    /// 指令字节码 (含结尾 `end`)。
    pub code: Vec<u8>,
}

/// 数据段 — 初始化线性内存的字节块。
#[derive(Debug, Clone)]
pub struct DataSegment {
    /// 目标内存索引。
    pub memory_index: u32,
    /// 初始偏移量的常量表达式字节码。
    pub offset: Vec<u8>,
    /// 写入内存的原始字节。
    pub data: Vec<u8>,
}

/// 元素段 — 初始化表的函数索引集合。
#[derive(Debug, Clone)]
pub struct ElementSegment {
    /// 目标表索引。
    pub table_index: u32,
    /// 初始偏移量的常量表达式字节码。
    pub offset: Vec<u8>,
    /// 写入表的函数索引列表。
    pub func_indices: Vec<u32>,
}

/// 完整的 WASM 模块解析结果
#[derive(Debug)]
pub struct WasmModule {
    /// 类型段: 函数类型列表。
    pub types: Vec<FuncType>,
    /// 导入段: 导入项列表。
    pub imports: Vec<ImportDesc>,
    /// 函数段: 各函数对应的类型索引。
    pub functions: Vec<u32>,
    /// 表段: 表类型列表。
    pub tables: Vec<TableType>,
    /// 内存段: 内存类型列表。
    pub memories: Vec<MemoryType>,
    /// 全局量段: 类型与初始值字节码的配对列表。
    pub globals: Vec<(GlobalType, Vec<u8>)>,
    /// 导出段: 导出项列表。
    pub exports: Vec<ExportDesc>,
    /// 起始段: 入口函数索引 (可缺省)。
    pub start_section: Option<u32>,
    /// 元素段: 表初始化片段列表。
    pub elements: Vec<ElementSegment>,
    /// 代码段: 函数体列表, 与 `functions` 一一对应。
    pub code: Vec<FunctionBody>,
    /// 数据段: 线性内存初始化片段列表。
    pub data: Vec<DataSegment>,
}

// ============================================================================
// 操作码定义 (WASM 1.0)
// ============================================================================

/// WASM 1.0 操作码 — 全部核心指令的字节编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    /// `unreachable`: 立即陷入, 执行到此即报错。
    Unreachable = 0x00,
    /// `nop`: 空操作。
    Nop = 0x01,
    /// `block`: 进入块 (可带块类型)。
    Block = 0x02,
    /// `loop`: 进入循环块。
    Loop = 0x03,
    /// `if`: 条件分支入口 (消费栈顶条件)。
    If = 0x04,
    /// `else`: `if` 的真分支转入假分支。
    Else = 0x05,
    /// `end`: 结束当前块或函数体。
    End = 0x0B,
    /// `br`: 无条件跳转到指定深度的标签。
    Br = 0x0C,
    /// `br_if`: 条件跳转 (消费栈顶条件)。
    BrIf = 0x0D,
    /// `br_table`: 按索引在多目标间跳转。
    BrTable = 0x0E,
    /// `return`: 从当前函数返回。
    Return = 0x0F,
    /// `call`: 直接调用函数索引。
    Call = 0x10,
    /// `call_indirect`: 经表间接调用函数。
    CallIndirect = 0x11,

    /// `drop`: 丢弃栈顶值。
    Drop = 0x1A,
    /// `select`: 按条件在栈顶两值间选择。
    Select = 0x1B,

    /// `local.get`: 读取局部变量。
    LocalGet = 0x20,
    /// `local.set`: 写入局部变量。
    LocalSet = 0x21,
    /// `local.tee`: 写入局部变量并保留副本。
    LocalTee = 0x22,
    /// `global.get`: 读取全局量。
    GlobalGet = 0x23,
    /// `global.set`: 写入全局量。
    GlobalSet = 0x24,

    /// `i32.load`: 从内存加载 32 位整数。
    I32Load = 0x28,
    /// `i64.load`: 从内存加载 64 位整数。
    I64Load = 0x29,
    /// `i32.load8_s`: 加载 8 位并按符号扩展为 i32。
    I32Load8S = 0x2C,
    /// `i32.load8_u`: 加载 8 位并按零扩展为 i32。
    I32Load8U = 0x2D,
    /// `i32.load16_s`: 加载 16 位并按符号扩展为 i32。
    I32Load16S = 0x2E,
    /// `i32.load16_u`: 加载 16 位并按零扩展为 i32。
    I32Load16U = 0x2F,
    /// `i64.load8_s`: 加载 8 位并按符号扩展为 i64。
    I64Load8S = 0x30,
    /// `i64.load8_u`: 加载 8 位并按零扩展为 i64。
    I64Load8U = 0x31,
    /// `i64.load16_s`: 加载 16 位并按符号扩展为 i64。
    I64Load16S = 0x32,
    /// `i64.load16_u`: 加载 16 位并按零扩展为 i64。
    I64Load16U = 0x33,
    /// `i64.load32_s`: 加载 32 位并按符号扩展为 i64。
    I64Load32S = 0x34,
    /// `i64.load32_u`: 加载 32 位并按零扩展为 i64。
    I64Load32U = 0x35,
    /// `i32.store`: 向内存存储 32 位整数。
    I32Store = 0x36,
    /// `i64.store`: 向内存存储 64 位整数。
    I64Store = 0x37,
    /// `i32.store8`: 存储 i32 的低 8 位。
    I32Store8 = 0x3A,
    /// `i32.store16`: 存储 i32 的低 16 位。
    I32Store16 = 0x3B,
    /// `i64.store8`: 存储 i64 的低 8 位。
    I64Store8 = 0x3C,
    /// `i64.store16`: 存储 i64 的低 16 位。
    I64Store16 = 0x3D,
    /// `i64.store32`: 存储 i64 的低 32 位。
    I64Store32 = 0x3E,
    /// `memory.size`: 查询线性内存当前页数。
    MemorySize = 0x3F,
    /// `memory.grow`: 增长线性内存页数。
    MemoryGrow = 0x40,

    /// `i32.const`: 压入 32 位整型常量。
    I32Const = 0x41,
    /// `i64.const`: 压入 64 位整型常量。
    I64Const = 0x42,

    /// `i32.eqz`: 判断 i32 是否为 0。
    I32Eqz = 0x45,
    /// `i32.eq`: i32 相等比较。
    I32Eq = 0x46,
    /// `i32.ne`: i32 不等比较。
    I32Ne = 0x47,
    /// `i32.lt_s`: i32 有符号小于比较。
    I32LtS = 0x48,
    /// `i32.lt_u`: i32 无符号小于比较。
    I32LtU = 0x49,
    /// `i32.gt_s`: i32 有符号大于比较。
    I32GtS = 0x4A,
    /// `i32.gt_u`: i32 无符号大于比较。
    I32GtU = 0x4B,
    /// `i32.le_s`: i32 有符号小于等于比较。
    I32LeS = 0x4C,
    /// `i32.le_u`: i32 无符号小于等于比较。
    I32LeU = 0x4D,
    /// `i32.ge_s`: i32 有符号大于等于比较。
    I32GeS = 0x4E,
    /// `i32.ge_u`: i32 无符号大于等于比较。
    I32GeU = 0x4F,

    /// `i64.eqz`: 判断 i64 是否为 0。
    I64Eqz = 0x50,
    /// `i64.eq`: i64 相等比较。
    I64Eq = 0x51,
    /// `i64.ne`: i64 不等比较。
    I64Ne = 0x52,
    /// `i64.lt_s`: i64 有符号小于比较。
    I64LtS = 0x53,
    /// `i64.lt_u`: i64 无符号小于比较。
    I64LtU = 0x54,
    /// `i64.gt_s`: i64 有符号大于比较。
    I64GtS = 0x55,
    /// `i64.gt_u`: i64 无符号大于比较。
    I64GtU = 0x56,
    /// `i64.le_s`: i64 有符号小于等于比较。
    I64LeS = 0x57,
    /// `i64.le_u`: i64 无符号小于等于比较。
    I64LeU = 0x58,
    /// `i64.ge_s`: i64 有符号大于等于比较。
    I64GeS = 0x59,
    /// `i64.ge_u`: i64 无符号大于等于比较。
    I64GeU = 0x5A,

    /// `i32.add`: 32 位整数加法。
    I32Add = 0x6A,
    /// `i32.sub`: 32 位整数减法。
    I32Sub = 0x6B,
    /// `i32.mul`: 32 位整数乘法。
    I32Mul = 0x6C,
    /// `i32.div_s`: 32 位有符号整数除法。
    I32DivS = 0x6D,
    /// `i32.div_u`: 32 位无符号整数除法。
    I32DivU = 0x6E,
    /// `i32.rem_s`: 32 位有符号整数取余。
    I32RemS = 0x6F,
    /// `i32.rem_u`: 32 位无符号整数取余。
    I32RemU = 0x70,
    /// `i32.and`: 32 位按位与。
    I32And = 0x71,
    /// `i32.or`: 32 位按位或。
    I32Or = 0x72,
    /// `i32.xor`: 32 位按位异或。
    I32Xor = 0x73,
    /// `i32.shl`: 32 位左移。
    I32Shl = 0x74,
    /// `i32.shr_s`: 32 位算术右移。
    I32ShrS = 0x75,
    /// `i32.shr_u`: 32 位逻辑右移。
    I32ShrU = 0x76,
    /// `i32.rotl`: 32 位循环左移。
    I32Rotl = 0x77,
    /// `i32.rotr`: 32 位循环右移。
    I32Rotr = 0x78,

    /// `i64.add`: 64 位整数加法。
    I64Add = 0x7C,
    /// `i64.sub`: 64 位整数减法。
    I64Sub = 0x7D,
    /// `i64.mul`: 64 位整数乘法。
    I64Mul = 0x7E,
    /// `i64.div_s`: 64 位有符号整数除法。
    I64DivS = 0x7F,
    /// `i64.div_u`: 64 位无符号整数除法。
    I64DivU = 0x80,
    /// `i64.rem_s`: 64 位有符号整数取余。
    I64RemS = 0x81,
    /// `i64.rem_u`: 64 位无符号整数取余。
    I64RemU = 0x82,
    /// `i64.and`: 64 位按位与。
    I64And = 0x83,
    /// `i64.or`: 64 位按位或。
    I64Or = 0x84,
    /// `i64.xor`: 64 位按位异或。
    I64Xor = 0x85,
    /// `i64.shl`: 64 位左移。
    I64Shl = 0x86,
    /// `i64.shr_s`: 64 位算术右移。
    I64ShrS = 0x87,
    /// `i64.shr_u`: 64 位逻辑右移。
    I64ShrU = 0x88,
    /// `i64.rotl`: 64 位循环左移。
    I64Rotl = 0x89,
    /// `i64.rotr`: 64 位循环右移。
    I64Rotr = 0x8A,

    /// `i32.wrap_i64`: 将 i64 截断为 i32。
    I32WrapI64 = 0xA7,
    /// `i64.extend_i32_s`: 将 i32 按符号扩展为 i64。
    I64ExtendI32S = 0xAC,
    /// `i64.extend_i32_u`: 将 i32 按零扩展为 i64。
    I64ExtendI32U = 0xAD,
}

impl Opcode {
    /// 按操作码字节解析指令; 非法字节返回 `None`。
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

/// 类型段 (Type) 的段 id。
pub const SECTION_TYPE: u8 = 1;
/// 导入段 (Import) 的段 id。
pub const SECTION_IMPORT: u8 = 2;
/// 函数段 (Function) 的段 id。
pub const SECTION_FUNCTION: u8 = 3;
/// 表段 (Table) 的段 id。
pub const SECTION_TABLE: u8 = 4;
/// 内存段 (Memory) 的段 id。
pub const SECTION_MEMORY: u8 = 5;
/// 全局量段 (Global) 的段 id。
pub const SECTION_GLOBAL: u8 = 6;
/// 导出段 (Export) 的段 id。
pub const SECTION_EXPORT: u8 = 7;
/// 起始段 (Start) 的段 id。
pub const SECTION_START: u8 = 8;
/// 元素段 (Element) 的段 id。
pub const SECTION_ELEMENT: u8 = 9;
/// 代码段 (Code) 的段 id。
pub const SECTION_CODE: u8 = 10;
/// 数据段 (Data) 的段 id。
pub const SECTION_DATA: u8 = 11;

// ============================================================================
// 错误类型
// ============================================================================

/// WASM 解析/执行错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmError {
    /// 模块头魔数不符。
    InvalidMagic,
    /// 模块版本号不受支持。
    InvalidVersion,
    /// 输入在读取途中耗尽 (字节流不完整)。
    Truncated,
    /// LEB128 变长整数超出目标宽度。
    Leb128Overflow,
    /// 段声明尺寸与其内容实际长度不符 (解析器提前停在段中, 或段尾有残余字节)
    SectionSizeMismatch,
    /// 段内声明的条目数超过该段剩余可用字节数 (每个条目至少占 1 字节)
    ///
    /// 解析期分配的上界判据: 不收此界时一句 `0xFFFF_FFFF` 即可让内核在
    /// `Vec::with_capacity` 上分配失败。
    TooManyEntries {
        /// 段内声明的条目数
        declared: usize,
        /// 声明之后该段剩余的字节数
        available: usize,
    },
    /// 值类型字节非法 (不是 `0x7F`/`0x7E`/`0x7D`/`0x7C` 之一)
    BadValueType(u8),
    /// 函数类型标签非法 (不是 `0x60`)
    BadFuncTypeTag(u8),
    /// 未知段 id。
    UnknownSection(u8),
    /// 未知操作码字节。
    UnknownOpcode(u8),
    /// 指令操作数的类型与期望不符。
    TypeMismatch,
    /// 操作数栈下溢。
    StackUnderflow,
    /// 操作数栈超出容量上限。
    StackOverflow,
    /// 整数除法的除数为零。
    DivisionByZero,
    /// 整数运算溢出 (如有符号除法最小值取反)。
    IntegerOverflow,
    /// 内存访问越出线性内存边界。
    MemoryOutOfBounds,
    /// 内存增长请求失败 (超出上限或分配失败)。
    MemoryGrowFailed,
    /// 调用深度超出上限。
    CallDepthExceeded,
    /// 执行配额 (gas) 耗尽。
    GasExhausted,
    /// 执行到 `unreachable` 指令, 或落入无返回的分支。
    Unreachable,
    /// 导出项非法 (索引越界或种类不支持)。
    BadExport,
    /// 导入项非法或无法解析。
    BadImport,
    /// 找不到目标函数。
    FunctionNotFound,
    /// 类型索引越界。
    BadTypeIndex(usize),
    /// 函数索引越界。
    BadFuncIndex(usize),
    /// 解释器内部不变量被破坏。
    InternalError,
    /// WASI `proc_exit` 触发的实例终止
    Terminated,
}

/// 空块类型的编码 (`-64`, 即 LEB128 的 `0x40`)。
pub const BLOCK_TYPE_EMPTY: i32 = -64;
