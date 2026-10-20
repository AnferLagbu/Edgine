#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯解析算法。
//! WASM 二进制格式解析器 — functions 层策略主体
//!
//! ## T6-9 迁移记录
//!
//! 原属 privileged/wasm/module.rs, 2026-06-16 提取到 functions.
//! 纯解析算法 (WASM 二进制格式解析), 0 unsafe, 0 外部依赖.
//! privileged 仅保留 re-export.
//!
//! ## W-2 全函数化改造 (docs/plan/wasm-runtime-wiring.md)
//!
//! 改造前本解析器**不是全函数**: 段内取字节直接 `bytes[pos]` 索引, 段边界仅靠
//! 调用方传入的 `end` 参数约束 (多数子解析器忽略该参数), 条目数前缀未设上界.
//! 后果是畸形 / 截断字节码可在内核态触发 index-out-of-bounds panic 与
//! `Vec::with_capacity(0xFFFF_FFFF)` 分配失败, 即一条用户可触达的 DoS 路径.
//!
//! 现有不变式 (逐条有回归锁, 见本文件 `mod tests`):
//! 1. 每个段先切出**子切片**再交给子解析器 ⇒ 子解析器的 `bytes.len()` 就是段边界,
//!    跨段读取在类型上不可能发生;
//! 2. 段内所有取字节 / 取切片走 `take_byte` / `take_slice` 等边界安全原语,
//!    越界一律返 `WasmError` 而非 panic;
//! 3. 条目数前缀以"该条目至少占 1 字节"为语义, 用段内剩余字节数作上界
//!    (`take_entry_count`) ⇒ 声明数与输入尺寸不符即刻 `Err`;
//! 4. 段解析完毕后已消费字节数必须等于声明尺寸 (`require_consumed`),
//!    内容比声明短或长都判为畸形模块.
//!
//! 解析 WASM 1.0 二进制格式的 11 个标准段 (Section):
//! - 类型 (Type)、导入 (Import)、函数 (Function)、表格 (Table)、内存 (Memory)、全局 (Global)
//! - 导出 (Export)、启动 (Start)、元素 (Element)、代码 (Code)、数据 (Data)

use alloc::vec::Vec;

use super::leb128::{read_leb128_i32, read_leb128_i64, read_leb128_u32, read_name};
use super::types::{
    DataSegment, ElementSegment, ExportDesc, ExportKind, FuncType, FunctionBody, GlobalType,
    ImportDesc, ImportKind, Limits, MemoryType, SECTION_CODE, SECTION_DATA, SECTION_ELEMENT,
    SECTION_EXPORT, SECTION_FUNCTION, SECTION_GLOBAL, SECTION_IMPORT, SECTION_MEMORY,
    SECTION_START, SECTION_TABLE, SECTION_TYPE, TableType, ValueType, WASM_MAGIC, WASM_VERSION,
    WasmError, WasmModule,
};

// ============================================================================
// 段内边界安全原语
// ============================================================================

/// 取当前段内一字节并前进一位。
///
/// # Errors
///
/// 段内容已被读完 → `WasmError::Truncated`。
fn take_byte(bytes: &[u8], pos: &mut usize) -> Result<u8, WasmError> {
    let byte = *bytes.get(*pos).ok_or(WasmError::Truncated)?;
    *pos += 1;
    Ok(byte)
}

/// 取当前段内的值类型字节并解码。
///
/// # Errors
///
/// 段内容已被读完 → `WasmError::Truncated`; 字节不是合法值类型编码 →
/// `WasmError::BadValueType`。
fn take_value_type(bytes: &[u8], pos: &mut usize) -> Result<ValueType, WasmError> {
    let byte = take_byte(bytes, pos)?;
    ValueType::from_byte(byte).ok_or(WasmError::BadValueType(byte))
}

/// 读取"条目数"前缀, 并以段内剩余字节数为其上界。
///
/// 每个条目至少占 1 字节, 故声明数大于剩余字节 ⇒ 该声明必然无法兑现。不收上界时,
/// 攻击者可用一句 `count = 0xFFFF_FFFF` 让内核在 `Vec::with_capacity` 上分配失败,
/// 属解析期必须一次结清的结构面。
///
/// # Errors
///
/// 段内容已被读完 → `WasmError::Truncated`; 声明条目数超过剩余可用字节 →
/// `WasmError::TooManyEntries`。
fn take_entry_count(bytes: &[u8], pos: &mut usize) -> Result<usize, WasmError> {
    let declared = read_leb128_u32(bytes, pos)? as usize;
    let available = bytes.len().saturating_sub(*pos);
    if declared > available {
        return Err(WasmError::TooManyEntries {
            declared,
            available,
        });
    }
    Ok(declared)
}

/// 取当前段内连续 `len` 字节并前进 `len`。
///
/// # Errors
///
/// `len` 使得读取范围越过段尾 (含 `len` 自身导致的整数溢出) →
/// `WasmError::Truncated`。
fn take_slice<'a>(bytes: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], WasmError> {
    let start = *pos;
    let end = start.checked_add(len).ok_or(WasmError::Truncated)?;
    let slice = bytes.get(start..end).ok_or(WasmError::Truncated)?;
    *pos = end;
    Ok(slice)
}

/// 断言段内容已被完整消费。
///
/// 段声明尺寸与实际内容不符 (解析器提前停在段中, 或尾部有垃圾字节) 即判为畸形模块。
///
/// # Errors
///
/// 已消费字节数不等于段长度 → `WasmError::SectionSizeMismatch`。
fn require_consumed(bytes: &[u8], pos: usize) -> Result<(), WasmError> {
    if pos == bytes.len() {
        Ok(())
    } else {
        Err(WasmError::SectionSizeMismatch)
    }
}

// ============================================================================
// 模块级解析
// ============================================================================

/// 解析 WASM 二进制格式, 生成模块结构.
///
/// 全函数: 对任意输入 (空 / 截断 / 超长 / 非法条目数 / 半字节 LEB128) 只可能
/// 返回 `Ok` 或 `Err`, 不存在 panic 路径。
///
/// # Errors
///
/// 当输入过短、魔数或版本号不匹配、段数据截断或结构非法时
/// 返回对应的 `WasmError`.
pub fn parse_wasm(bytes: &[u8]) -> Result<WasmModule, WasmError> {
    let head = bytes.get(..8).ok_or(WasmError::Truncated)?;
    let magic = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let version = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);

    if magic != WASM_MAGIC {
        return Err(WasmError::InvalidMagic);
    }
    if version != WASM_VERSION {
        return Err(WasmError::InvalidVersion);
    }

    let mut module = WasmModule {
        types: Vec::new(),
        imports: Vec::new(),
        functions: Vec::new(),
        tables: Vec::new(),
        memories: Vec::new(),
        globals: Vec::new(),
        exports: Vec::new(),
        start_section: None,
        elements: Vec::new(),
        code: Vec::new(),
        data: Vec::new(),
    };

    let mut pos: usize = 8;
    while pos < bytes.len() {
        let section_id = *bytes.get(pos).ok_or(WasmError::Truncated)?;
        pos += 1;
        let section_size = read_leb128_u32(bytes, &mut pos)? as usize;
        let section_end = pos.checked_add(section_size).ok_or(WasmError::Truncated)?;
        if section_end > bytes.len() {
            return Err(WasmError::Truncated);
        }
        // 子切片即段边界: 子解析器内的 bytes.len() 就是本段末尾, 跨段读取不可能发生。
        let body = bytes.get(pos..section_end).ok_or(WasmError::Truncated)?;

        match section_id {
            SECTION_TYPE => {
                module.types = parse_type_section(body)?;
            }
            SECTION_IMPORT => {
                module.imports = parse_import_section(body)?;
            }
            SECTION_FUNCTION => {
                module.functions = parse_function_section(body)?;
            }
            SECTION_TABLE => {
                module.tables = parse_table_section(body)?;
            }
            SECTION_MEMORY => {
                module.memories = parse_memory_section(body)?;
            }
            SECTION_GLOBAL => {
                module.globals = parse_global_section(body)?;
            }
            SECTION_EXPORT => {
                module.exports = parse_export_section(body)?;
            }
            SECTION_START => {
                let mut inner = 0usize;
                let start = read_leb128_u32(body, &mut inner)?;
                require_consumed(body, inner)?;
                module.start_section = Some(start);
            }
            SECTION_ELEMENT => {
                module.elements = parse_element_section(body)?;
            }
            SECTION_CODE => {
                module.code = parse_code_section(body)?;
            }
            SECTION_DATA => {
                module.data = parse_data_section(body)?;
            }
            // 未知段 (含 custom section id 0): 按声明尺寸整体跳过, 不参与模块结构.
            _ => {}
        }
        pos = section_end;
    }

    Ok(module)
}

// ============================================================================
// 各段解析 (入参 bytes 恒为"本段子切片", pos 从 0 起)
// ============================================================================

/// 解析类型段: 每个函数类型形如 `0x60 params* results*`。
fn parse_type_section(bytes: &[u8]) -> Result<Vec<FuncType>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut types = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = take_byte(bytes, &mut pos)?;
        if tag != 0x60 {
            return Err(WasmError::BadFuncTypeTag(tag));
        }

        let param_count = take_entry_count(bytes, &mut pos)?;
        let mut params = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            params.push(take_value_type(bytes, &mut pos)?);
        }

        let result_count = take_entry_count(bytes, &mut pos)?;
        let mut results = Vec::with_capacity(result_count);
        for _ in 0..result_count {
            results.push(take_value_type(bytes, &mut pos)?);
        }

        types.push(FuncType { params, results });
    }
    require_consumed(bytes, pos)?;
    Ok(types)
}

/// 解析导入段: 四类导入 (function / table / memory / global)。
fn parse_import_section(bytes: &[u8]) -> Result<Vec<ImportDesc>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut imports = Vec::with_capacity(count);
    for _ in 0..count {
        let module_name = read_name(bytes, &mut pos)?;
        let name = read_name(bytes, &mut pos)?;
        let import_kind = take_byte(bytes, &mut pos)?;
        let desc = match import_kind {
            0x00 => {
                let type_idx = read_leb128_u32(bytes, &mut pos)?;
                ImportKind::Function(type_idx)
            }
            0x01 => {
                let element_type = take_value_type(bytes, &mut pos)?;
                let limits = parse_limits(bytes, &mut pos)?;
                ImportKind::Table(TableType {
                    element_type,
                    limits,
                })
            }
            0x02 => {
                let limits = parse_limits(bytes, &mut pos)?;
                ImportKind::Memory(MemoryType { limits })
            }
            0x03 => {
                let content_type = take_value_type(bytes, &mut pos)?;
                let mutable = take_byte(bytes, &mut pos)? != 0;
                ImportKind::Global(GlobalType {
                    content_type,
                    mutable,
                })
            }
            _ => return Err(WasmError::BadImport),
        };
        imports.push(ImportDesc {
            module: module_name,
            name,
            desc,
        });
    }
    require_consumed(bytes, pos)?;
    Ok(imports)
}

/// 解析 limits 结构 (`flag min [max]`)。
///
/// SIMPLIFIED: flag 只按"非 0 即带 max"判定, 未把 `flag > 1` 判为非法编码; 规范外
/// 编码不会造成越界或 panic, 只可能读到一个用不上的 max; 影响面为宽容度而非安全性;
/// 何时需扩展: 接入 `EG_WASM_LOAD` 的模块级校验门时一并收紧。
fn parse_limits(bytes: &[u8], pos: &mut usize) -> Result<Limits, WasmError> {
    let has_max = take_byte(bytes, pos)? != 0;
    let min = read_leb128_u32(bytes, pos)?;
    let max = if has_max {
        Some(read_leb128_u32(bytes, pos)?)
    } else {
        None
    };
    Ok(Limits { min, max })
}

/// 解析函数段: 函数索引 → 类型索引列表。
fn parse_function_section(bytes: &[u8]) -> Result<Vec<u32>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut funcs = Vec::with_capacity(count);
    for _ in 0..count {
        funcs.push(read_leb128_u32(bytes, &mut pos)?);
    }
    require_consumed(bytes, pos)?;
    Ok(funcs)
}

/// 解析表格段。
fn parse_table_section(bytes: &[u8]) -> Result<Vec<TableType>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut tables = Vec::with_capacity(count);
    for _ in 0..count {
        let element_type = take_value_type(bytes, &mut pos)?;
        let limits = parse_limits(bytes, &mut pos)?;
        tables.push(TableType {
            element_type,
            limits,
        });
    }
    require_consumed(bytes, pos)?;
    Ok(tables)
}

/// 解析内存段。
fn parse_memory_section(bytes: &[u8]) -> Result<Vec<MemoryType>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut memories = Vec::with_capacity(count);
    for _ in 0..count {
        let limits = parse_limits(bytes, &mut pos)?;
        memories.push(MemoryType { limits });
    }
    require_consumed(bytes, pos)?;
    Ok(memories)
}

/// 解析全局段: 每个全局 = 类型 + 可变性 + 初始化表达式原始字节。
fn parse_global_section(bytes: &[u8]) -> Result<Vec<(GlobalType, Vec<u8>)>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut globals = Vec::with_capacity(count);
    for _ in 0..count {
        let content_type = take_value_type(bytes, &mut pos)?;
        let mutable = take_byte(bytes, &mut pos)? != 0;
        let init_expr = read_init_expr(bytes, &mut pos)?;
        globals.push((
            GlobalType {
                content_type,
                mutable,
            },
            init_expr,
        ));
    }
    require_consumed(bytes, pos)?;
    Ok(globals)
}

/// 解析导出段。
fn parse_export_section(bytes: &[u8]) -> Result<Vec<ExportDesc>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut exports = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_name(bytes, &mut pos)?;
        let kind_byte = take_byte(bytes, &mut pos)?;
        let kind = match kind_byte {
            0 => ExportKind::Function,
            1 => ExportKind::Table,
            2 => ExportKind::Memory,
            3 => ExportKind::Global,
            _ => return Err(WasmError::BadExport),
        };
        let index = read_leb128_u32(bytes, &mut pos)?;
        exports.push(ExportDesc { name, kind, index });
    }
    require_consumed(bytes, pos)?;
    Ok(exports)
}

/// 解析代码段: 每个函数体 = 尺寸前缀 + 局部声明 + 指令字节。
fn parse_code_section(bytes: &[u8]) -> Result<Vec<FunctionBody>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut bodies = Vec::with_capacity(count);
    for _ in 0..count {
        // 函数体尺寸先作上界: body 只能取段内剩余字节, 越界即 Err.
        let body_size = read_leb128_u32(bytes, &mut pos)? as usize;
        let body = take_slice(bytes, &mut pos, body_size)?;
        let mut inner = 0usize;
        let local_count = take_entry_count(body, &mut inner)?;
        let mut locals = Vec::with_capacity(local_count);
        for _ in 0..local_count {
            let n = read_leb128_u32(body, &mut inner)?;
            let ty = take_value_type(body, &mut inner)?;
            locals.push((n, ty));
        }
        // 余下字节即指令序列 (末尾 `0x0B` 结束标记由解释器消费).
        let tail_len = body.len() - inner;
        let code = take_slice(body, &mut inner, tail_len)?.to_vec();
        bodies.push(FunctionBody { locals, code });
    }
    require_consumed(bytes, pos)?;
    Ok(bodies)
}

/// 解析数据段: 内存索引 + 偏移表达式 + 原始字节。
fn parse_data_section(bytes: &[u8]) -> Result<Vec<DataSegment>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut data = Vec::with_capacity(count);
    for _ in 0..count {
        let memory_index = read_leb128_u32(bytes, &mut pos)?;
        let offset = read_init_expr(bytes, &mut pos)?;
        let data_len = read_leb128_u32(bytes, &mut pos)? as usize;
        let segment_data = take_slice(bytes, &mut pos, data_len)?.to_vec();
        data.push(DataSegment {
            memory_index,
            offset,
            data: segment_data,
        });
    }
    require_consumed(bytes, pos)?;
    Ok(data)
}

/// 解析元素段: 表格索引 + 偏移表达式 + 函数索引列表。
fn parse_element_section(bytes: &[u8]) -> Result<Vec<ElementSegment>, WasmError> {
    let mut pos = 0usize;
    let count = take_entry_count(bytes, &mut pos)?;
    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        let table_index = read_leb128_u32(bytes, &mut pos)?;
        let offset = read_init_expr(bytes, &mut pos)?;
        let num_elems = take_entry_count(bytes, &mut pos)?;
        let mut func_indices = Vec::with_capacity(num_elems);
        for _ in 0..num_elems {
            func_indices.push(read_leb128_u32(bytes, &mut pos)?);
        }
        elements.push(ElementSegment {
            table_index,
            offset,
            func_indices,
        });
    }
    require_consumed(bytes, pos)?;
    Ok(elements)
}

/// 跳过一个常量初始化表达式, 把 `pos` 停在结束标记 `0x0B` 之后。
///
/// # Errors
///
/// 表达式缺结束标记而越过段尾 → `WasmError::Truncated`; 表达式内出现非
/// `i32.const` / `i64.const` / `global.get` 的指令 → `WasmError::UnknownOpcode`。
fn skip_init_expr(bytes: &[u8], pos: &mut usize) -> Result<(), WasmError> {
    loop {
        let byte = take_byte(bytes, pos)?;
        match byte {
            0x0B => return Ok(()),
            0x41 => {
                read_leb128_i32(bytes, pos)?;
            }
            0x42 => {
                read_leb128_i64(bytes, pos)?;
            }
            0x23 => {
                read_leb128_u32(bytes, pos)?;
            }
            _ => return Err(WasmError::UnknownOpcode(byte)),
        }
    }
}

/// 读取一个常量初始化表达式的原始字节 (含结束标记 `0x0B`), 供解释器后续求值。
fn read_init_expr(bytes: &[u8], pos: &mut usize) -> Result<Vec<u8>, WasmError> {
    let start = *pos;
    skip_init_expr(bytes, pos)?;
    let mut cursor = start;
    let raw = take_slice(bytes, &mut cursor, *pos - start)?;
    Ok(raw.to_vec())
}

// ============================================================================
// 回归锁: 畸形输入矩阵 (docs/plan/wasm-runtime-wiring.md W-2 / W-9(b))
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// 独立 LEB128 编码器: 与被测解码路径无共用实现, 可作对照 oracle。
    fn leb(value: u32) -> Vec<u8> {
        let mut rest = value;
        let mut out = Vec::new();
        loop {
            let mut byte = (rest & 0x7F) as u8;
            rest >>= 7;
            if rest != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if rest == 0 {
                return out;
            }
        }
    }

    /// 拼一个"尺寸前缀与内容自洽"的段。
    fn section(id: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![id];
        out.extend_from_slice(&leb(content.len() as u32));
        out.extend_from_slice(content);
        out
    }

    fn header() -> Vec<u8> {
        vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]
    }

    /// 各段的合法内容样本 (不含表格段: 其 funcref 元素类型 `0x70` 目前不被
    /// `ValueType::from_byte` 接受, 该缺口已登记给 W-4 的表格 / 间接调用实装补齐)。
    fn section_cases() -> Vec<(&'static str, u8, Vec<u8>)> {
        let type_content = {
            let mut c = leb(1);
            c.push(0x60);
            c.extend_from_slice(&leb(0));
            c.extend_from_slice(&leb(0));
            c
        };
        let import_content = {
            let mut c = leb(1);
            c.extend_from_slice(&leb(1));
            c.push(b'm');
            c.extend_from_slice(&leb(1));
            c.push(b'f');
            c.push(0x00);
            c.extend_from_slice(&leb(0));
            c
        };
        let function_content = leb(1).into_iter().chain(leb(0)).collect::<Vec<u8>>();
        let memory_content = {
            let mut c = leb(1);
            c.push(0x00);
            c.extend_from_slice(&leb(1));
            c
        };
        let global_content = {
            let mut c = leb(1);
            c.push(0x7F);
            c.push(0x01);
            c.push(0x41);
            c.push(0x2A);
            c.push(0x0B);
            c
        };
        let export_content = {
            let mut c = leb(1);
            c.extend_from_slice(&leb(4));
            c.extend_from_slice(b"main");
            c.push(0x00);
            c.extend_from_slice(&leb(0));
            c
        };
        let start_content = leb(0);
        let element_content = {
            let mut c = leb(1);
            c.extend_from_slice(&leb(0));
            c.push(0x41);
            c.push(0x00);
            c.push(0x0B);
            c.extend_from_slice(&leb(1));
            c.extend_from_slice(&leb(0));
            c
        };
        let code_content = {
            let body = {
                let mut b = leb(0); // 无局部声明
                b.push(0x01); // nop
                b.push(0x0B); // end
                b
            };
            let mut c = leb(1);
            c.extend_from_slice(&leb(body.len() as u32));
            c.extend_from_slice(&body);
            c
        };
        let data_content = {
            let mut c = leb(1);
            c.extend_from_slice(&leb(0));
            c.push(0x41);
            c.push(0x00);
            c.push(0x0B);
            c.extend_from_slice(&leb(2));
            c.extend_from_slice(&[0xAA, 0xBB]);
            c
        };

        alloc::vec![
            ("type", SECTION_TYPE, type_content),
            ("import", SECTION_IMPORT, import_content),
            ("function", SECTION_FUNCTION, function_content),
            ("memory", SECTION_MEMORY, memory_content),
            ("global", SECTION_GLOBAL, global_content),
            ("export", SECTION_EXPORT, export_content),
            ("start", SECTION_START, start_content),
            ("element", SECTION_ELEMENT, element_content),
            ("code", SECTION_CODE, code_content),
            ("data", SECTION_DATA, data_content),
        ]
    }

    /// 覆盖全部已知段的合法模块 (表格段除外, 理由见 `section_cases`)。
    fn valid_module() -> Vec<u8> {
        let mut out = header();
        for (_, id, content) in section_cases() {
            out.extend_from_slice(&section(id, &content));
        }
        out
    }

    #[test]
    fn test_valid_module_parses() {
        // 收紧判据的防回归面: 自洽模块必须能解析, 且关键字段落值正确。
        let module = parse_wasm(&valid_module())
            .expect("自洽模块应解析成功; 若本用例失败, 说明段边界/条目数校验过严");
        assert_eq!(module.types.len(), 1);
        assert!(module.types[0].params.is_empty());
        assert_eq!(module.functions, vec![0]);
        assert_eq!(module.memories.len(), 1);
        assert_eq!(module.memories[0].limits.min, 1);
        assert_eq!(module.globals.len(), 1);
        assert!(module.globals[0].1.contains(&0x0B));
        assert_eq!(module.exports.len(), 1);
        assert_eq!(module.exports[0].name, b"main".to_vec());
        assert_eq!(module.start_section, Some(0));
        assert_eq!(module.code.len(), 1);
        assert_eq!(module.code[0].code, vec![0x01, 0x0B]);
        assert_eq!(module.data[0].data, vec![0xAA, 0xBB]);
        assert_eq!(module.elements[0].func_indices, vec![0]);
    }

    #[test]
    fn test_too_short_and_empty_input() {
        assert!(matches!(
            parse_wasm(&[]),
            Err(WasmError::Truncated | WasmError::InvalidMagic)
        ));
        for len in 0..8 {
            let input = header()[..len].to_vec();
            assert!(parse_wasm(&input).is_err(), "长度 {len} 的前缀应报错");
        }
    }

    #[test]
    fn test_bad_magic_and_version() {
        let mut bad_magic = valid_module();
        bad_magic[0] = 0xFF;
        assert!(matches!(
            parse_wasm(&bad_magic),
            Err(WasmError::InvalidMagic)
        ));

        let mut bad_version = valid_module();
        bad_version[4] = 0x02;
        assert!(matches!(
            parse_wasm(&bad_version),
            Err(WasmError::InvalidVersion)
        ));
    }

    #[test]
    fn test_every_prefix_is_total() {
        // 全前缀遍历: 任何被截到任意长度的输入都只可能 Ok/Err, 不得 panic。
        let full = valid_module();
        for len in 0..=full.len() {
            let input = full[..len].to_vec();
            let _ = parse_wasm(&input);
        }
    }

    #[test]
    fn test_every_single_byte_mutation_is_total() {
        // 单字节篡改遍历: 覆盖各段的取值分支 (标签字节 / 条目数 / 尺寸前缀)。
        let base = valid_module();
        for idx in 0..base.len() {
            for probe in [0x00u8, 0x01, 0x41, 0x60, 0x70, 0x7F, 0x80, 0xFF] {
                let mut mutated = base.clone();
                mutated[idx] = probe;
                let _ = parse_wasm(&mutated);
            }
        }
    }

    /// 确定性伪随机源 (xorshift64, 无外部依赖): 固定种子 ⇒ 每轮复跑同一输入集,
    /// 失败可直接复现, 不需要保存随机种子。
    struct XorShift64(u64);

    impl XorShift64 {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % (bound as u64)) as usize
        }
    }

    #[test]
    fn test_randomized_mutation_is_total() {
        // 上两条穷举扫描只覆盖"合法骨架的近邻"(前缀链与单字节突变)。本例从合法模块
        // 出发做多字节突变 + 随机截断 + 随机追加, 另一半样本是纯随机字节, 用以打进
        // 段内深层分支与早期返回的任意组合。判据只有一条: 任意输入必须返 Ok 或 Err,
        // 不得 panic —— 这是 §4 验收判据第 2 条在解析面的承重回归锁。
        let base = valid_module();
        let mut rng = XorShift64(0x9E37_79B9_7F4A_7C15);
        for round in 0..20_000usize {
            let mut input = if round % 2 == 0 {
                base.clone()
            } else {
                // 纯随机: 先给合法头让解析走进段循环, 再填随机段字节。
                let mut raw = header();
                for _ in 0..rng.below(48) {
                    raw.push(rng.next_u64() as u8);
                }
                raw
            };
            for _ in 0..rng.below(9) {
                if input.is_empty() {
                    break;
                }
                let idx = rng.below(input.len());
                input[idx] = rng.next_u64() as u8;
            }
            if rng.below(3) == 0 {
                input.truncate(rng.below(input.len() + 1));
            }
            if rng.below(3) == 0 {
                for _ in 0..rng.below(24) {
                    input.push(rng.next_u64() as u8);
                }
            }
            let _ = parse_wasm(&input);
        }
    }

    #[test]
    fn test_truncated_section_payload_per_section() {
        // 每段各构造"内容被砍掉尾字节"的样本: 段声明尺寸不变, 内容不足。
        for (name, id, content) in section_cases() {
            assert!(!content.is_empty(), "样本段 {name} 内容不应为空");
            let truncated = &content[..content.len() - 1];
            let mut bytes = header();
            bytes.extend_from_slice(&section(id, truncated));
            let result = parse_wasm(&bytes);
            assert!(result.is_err(), "段 {name} 内容截断应报错, 实得 {result:?}");
        }
    }

    #[test]
    fn test_section_size_declared_beyond_module() {
        // 超长: 段声明尺寸大于其后真实可用字节 ⇒ 截断判据, 不得越界索引。
        let mut bytes = header();
        bytes.push(SECTION_TYPE);
        bytes.extend_from_slice(&leb(4_000_000_000));
        bytes.extend_from_slice(&leb(1));
        assert!(matches!(
            parse_wasm(&bytes),
            Err(WasmError::Truncated | WasmError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn test_section_size_mismatch_trailing_bytes() {
        // 段声明尺寸大于内容实际长度 (尾部塞垃圾) ⇒ 尺寸与内容不符。
        let mut content = leb(1);
        content.push(0x60);
        content.extend_from_slice(&leb(0));
        content.extend_from_slice(&leb(0));
        content.extend_from_slice(&[0x01, 0x02, 0x03]);
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_TYPE, &content));
        assert!(matches!(
            parse_wasm(&bytes),
            Err(WasmError::SectionSizeMismatch)
        ));
    }

    #[test]
    fn test_entry_count_explosion_rejected() {
        // 条目数爆炸: 声明 0xFFFF_FFFF 条目, 必须在分配前被上界拦下,
        // 否则内核会在 Vec::with_capacity 上分配失败。
        let explosion_cases: Vec<(&str, u8, Vec<u8>)> = alloc::vec![
            ("type", SECTION_TYPE, leb(0xFFFF_FFFF)),
            ("function", SECTION_FUNCTION, leb(0xFFFF_FFFF)),
            ("table", SECTION_TABLE, leb(0xFFFF_FFFF)),
            ("memory", SECTION_MEMORY, leb(0xFFFF_FFFF)),
            ("global", SECTION_GLOBAL, leb(0xFFFF_FFFF)),
            ("export", SECTION_EXPORT, leb(0xFFFF_FFFF)),
            ("element", SECTION_ELEMENT, leb(0xFFFF_FFFF)),
            ("code", SECTION_CODE, leb(0xFFFF_FFFF)),
            ("data", SECTION_DATA, leb(0xFFFF_FFFF)),
            ("import", SECTION_IMPORT, leb(0xFFFF_FFFF)),
        ];
        for (name, id, content) in explosion_cases {
            let mut bytes = header();
            bytes.extend_from_slice(&section(id, &content));
            let result = parse_wasm(&bytes);
            assert!(
                matches!(result, Err(WasmError::TooManyEntries { .. })),
                "段 {name} 的条目数爆炸应返 TooManyEntries, 实得 {result:?}"
            );
        }
    }

    #[test]
    fn test_params_count_explosion_rejected() {
        // 段内嵌条目数 (参数表) 同样受上界约束。
        let mut content = leb(1);
        content.push(0x60);
        content.extend_from_slice(&leb(0xFFFF_FFFF));
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_TYPE, &content));
        assert!(matches!(
            parse_wasm(&bytes),
            Err(WasmError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn test_bad_value_type_reported_as_such() {
        // 非法值类型字节: 旧实现误记为 InvalidVersion, 现单独返 BadValueType。
        let mut content = leb(1);
        content.push(0x60);
        content.extend_from_slice(&leb(1));
        content.push(0x09); // 非 0x7F/0x7E/0x7D/0x7C
        content.extend_from_slice(&leb(0));
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_TYPE, &content));
        assert!(matches!(
            parse_wasm(&bytes),
            Err(WasmError::BadValueType(0x09))
        ));
    }

    #[test]
    fn test_import_bad_kind() {
        let mut content = leb(1);
        content.extend_from_slice(&leb(1));
        content.push(b'm');
        content.extend_from_slice(&leb(1));
        content.push(b'f');
        content.push(0x07); // 未知导入种类
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_IMPORT, &content));
        assert!(matches!(parse_wasm(&bytes), Err(WasmError::BadImport)));
    }

    #[test]
    fn test_export_bad_kind() {
        let mut content = leb(1);
        content.extend_from_slice(&leb(4));
        content.extend_from_slice(b"main");
        content.push(0x09); // 未知导出种类
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_EXPORT, &content));
        assert!(matches!(parse_wasm(&bytes), Err(WasmError::BadExport)));
    }

    #[test]
    fn test_init_expr_without_end_is_total() {
        // 初始化表达式缺 `0x0B` 结束标记 ⇒ 只能因越段而 Truncated。
        for id in [SECTION_GLOBAL, SECTION_ELEMENT, SECTION_DATA] {
            let mut content = leb(1);
            match id {
                SECTION_GLOBAL => {
                    content.push(0x7F);
                    content.push(0x00);
                    content.push(0x41);
                    content.extend_from_slice(&leb(1));
                }
                SECTION_ELEMENT => {
                    content.extend_from_slice(&leb(0));
                    content.push(0x41);
                    content.push(0x00);
                }
                _ => {
                    content.extend_from_slice(&leb(0));
                    content.push(0x41);
                    content.push(0x00);
                }
            }
            let mut bytes = header();
            bytes.extend_from_slice(&section(id, &content));
            assert!(
                parse_wasm(&bytes).is_err(),
                "段 id {id} 的无结束标记表达式应报错"
            );
        }
    }

    #[test]
    fn test_init_expr_illegal_instruction() {
        let mut content = leb(1);
        content.push(0x7F);
        content.push(0x00);
        content.push(0x45); // 既非 const 亦非 global.get 也非 end
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_GLOBAL, &content));
        assert!(matches!(
            parse_wasm(&bytes),
            Err(WasmError::UnknownOpcode(0x45))
        ));
    }

    #[test]
    fn test_code_body_size_overflow_rejected() {
        // 函数体尺寸前缀大于段内剩余 ⇒ 不得越界切片。
        let mut content = leb(1);
        content.extend_from_slice(&leb(4_000_000_000));
        content.push(0x00);
        content.push(0x0B);
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_CODE, &content));
        assert!(matches!(
            parse_wasm(&bytes),
            Err(WasmError::Truncated | WasmError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn test_data_segment_len_beyond_section_rejected() {
        let mut content = leb(1);
        content.extend_from_slice(&leb(0));
        content.push(0x41);
        content.push(0x00);
        content.push(0x0B);
        content.extend_from_slice(&leb(4_000_000_000)); // 声明的 data 长度
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_DATA, &content));
        assert!(matches!(parse_wasm(&bytes), Err(WasmError::Truncated)));
    }

    #[test]
    fn test_half_terminated_leb_rejected() {
        // 末字节仍带续行位的 LEB128 ⇒ 读越段界即 Truncated, 不得把后续段字节当数据吃掉。
        let mut bytes = header();
        bytes.extend_from_slice(&section(SECTION_START, &[0x80]));
        assert!(matches!(parse_wasm(&bytes), Err(WasmError::Truncated)));
    }

    #[test]
    fn test_leb_overflow_rejected() {
        // 续行位链长到超出目标整数位宽 ⇒ Leb128Overflow。
        let mut bytes = header();
        bytes.extend_from_slice(&section(
            SECTION_START,
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x00],
        ));
        assert!(matches!(parse_wasm(&bytes), Err(WasmError::Leb128Overflow)));
        // 注: 5 字节编码且末字节载荷超出剩余 4 位空间时, 当前被静默截断接受
        // (read_leb128_u32 只在续行位把 shift 推过 32 时报错)。该宽容度不产生
        // panic 也不放大分配上界, 已按台账 P-42 登记待裁, 本轮不改 leb128 语义
        // (kani-harness 以该解码器为形式化验证对象)。
    }

    #[test]
    fn test_all_zero_section_ids_are_total() {
        // 极端输入: 全零字节流 (含无数 id=0 custom 段)。
        for len in [8usize, 16, 64, 256] {
            let mut bytes = header();
            bytes.resize(len, 0);
            let _ = parse_wasm(&bytes);
        }
    }

    #[test]
    fn test_unknown_section_id_skipped() {
        // 未知段 id 按声明尺寸整体跳过, 不影响其余段。
        let mut bytes = header();
        bytes.extend_from_slice(&section(99, &[0x01, 0x02, 0x03]));
        let module = parse_wasm(&bytes).expect("未知段应被跳过而非报错");
        assert!(module.types.is_empty());
    }

    #[test]
    fn test_zero_length_known_section_rejected() {
        // 已知段声明零长 ⇒ 连"条目数"字段都不存在, 判为截断。
        // 旧实现在此处会越过段尾把下一段的 id 当条目数吃掉。
        for id in [
            SECTION_TYPE,
            SECTION_IMPORT,
            SECTION_FUNCTION,
            SECTION_TABLE,
            SECTION_MEMORY,
            SECTION_GLOBAL,
            SECTION_EXPORT,
            SECTION_START,
            SECTION_ELEMENT,
            SECTION_CODE,
            SECTION_DATA,
        ] {
            let mut bytes = header();
            bytes.extend_from_slice(&section(id, &[]));
            let result = parse_wasm(&bytes);
            assert!(
                matches!(result, Err(WasmError::Truncated)),
                "已知段 id {id} 声明零长应返 Truncated, 实得 {result:?}"
            );
        }
    }

    #[test]
    fn test_zero_length_unknown_section_skipped() {
        // 未知 id (含 custom section 的 0) 声明零长 ⇒ 整体跳过, 模块仍合法.
        for id in [0u8, 99] {
            let mut bytes = header();
            bytes.extend_from_slice(&section(id, &[]));
            let module = parse_wasm(&bytes).expect("零长未知段应被跳过");
            assert!(module.types.is_empty());
        }
    }
}
