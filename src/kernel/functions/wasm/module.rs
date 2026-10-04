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

/// 解析 WASM 二进制格式, 生成模块结构.
///
/// # Errors
///
/// 当输入过短、魔数或版本号不匹配、段数据截断或结构非法时
/// 返回对应的 `WasmError`.
pub fn parse_wasm(bytes: &[u8]) -> Result<WasmModule, WasmError> {
    let mut pos: usize;

    if bytes.len() < 8 {
        return Err(WasmError::Truncated);
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    pos = 8;

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

    while pos < bytes.len() {
        let section_id = bytes[pos];
        pos += 1;
        let section_size = read_leb128_u32(bytes, &mut pos)? as usize;
        let section_end = pos + section_size;
        if section_end > bytes.len() {
            return Err(WasmError::Truncated);
        }

        match section_id {
            SECTION_TYPE => {
                module.types = parse_type_section(bytes, &mut pos, section_end)?;
            }
            SECTION_IMPORT => {
                module.imports = parse_import_section(bytes, &mut pos, section_end)?;
            }
            SECTION_FUNCTION => {
                module.functions = parse_function_section(bytes, &mut pos, section_end)?;
            }
            SECTION_TABLE => {
                module.tables = parse_table_section(bytes, &mut pos, section_end)?;
            }
            SECTION_MEMORY => {
                module.memories = parse_memory_section(bytes, &mut pos, section_end)?;
            }
            SECTION_GLOBAL => {
                module.globals = parse_global_section(bytes, &mut pos, section_end)?;
            }
            SECTION_EXPORT => {
                module.exports = parse_export_section(bytes, &mut pos, section_end)?;
            }
            SECTION_START => {
                module.start_section = Some(read_leb128_u32(bytes, &mut pos)?);
            }
            SECTION_ELEMENT => {
                module.elements = parse_element_section(bytes, &mut pos, section_end)?;
            }
            SECTION_CODE => {
                module.code = parse_code_section(bytes, &mut pos, section_end)?;
            }
            SECTION_DATA => {
                module.data = parse_data_section(bytes, &mut pos, section_end)?;
            }
            _ => {
                pos = section_end;
            }
        }
    }

    Ok(module)
}

fn parse_type_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<FuncType>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut types = Vec::with_capacity(count);
    for _ in 0..count {
        if *pos >= end || bytes[*pos] != 0x60 {
            return Err(WasmError::InternalError);
        }
        *pos += 1;

        let param_count = read_leb128_u32(bytes, pos)? as usize;
        let mut params = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            let ty = ValueType::from_byte(bytes[*pos]).ok_or(WasmError::InvalidVersion)?;
            *pos += 1;
            params.push(ty);
        }

        let result_count = read_leb128_u32(bytes, pos)? as usize;
        let mut results = Vec::with_capacity(result_count);
        for _ in 0..result_count {
            let ty = ValueType::from_byte(bytes[*pos]).ok_or(WasmError::InvalidVersion)?;
            *pos += 1;
            results.push(ty);
        }

        types.push(FuncType { params, results });
    }
    Ok(types)
}

fn parse_import_section(
    bytes: &[u8],
    pos: &mut usize,
    _end: usize,
) -> Result<Vec<ImportDesc>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut imports = Vec::with_capacity(count);
    for _ in 0..count {
        let module = read_name(bytes, pos)?;
        let name = read_name(bytes, pos)?;
        let import_kind = bytes[*pos];
        *pos += 1;
        let desc = match import_kind {
            0x00 => {
                let type_idx = read_leb128_u32(bytes, pos)?;
                ImportKind::Function(type_idx)
            }
            0x01 => {
                let element_type = ValueType::from_byte(bytes[*pos]).ok_or(WasmError::BadImport)?;
                *pos += 1;
                let limits = parse_limits(bytes, pos)?;
                ImportKind::Table(TableType {
                    element_type,
                    limits,
                })
            }
            0x02 => {
                let limits = parse_limits(bytes, pos)?;
                ImportKind::Memory(MemoryType { limits })
            }
            0x03 => {
                let content_type = ValueType::from_byte(bytes[*pos]).ok_or(WasmError::BadImport)?;
                *pos += 1;
                let mutable = bytes[*pos] != 0;
                *pos += 1;
                ImportKind::Global(GlobalType {
                    content_type,
                    mutable,
                })
            }
            _ => return Err(WasmError::BadImport),
        };
        imports.push(ImportDesc { module, name, desc });
    }
    Ok(imports)
}

fn parse_limits(bytes: &[u8], pos: &mut usize) -> Result<Limits, WasmError> {
    let has_max = bytes[*pos] != 0;
    *pos += 1;
    let min = read_leb128_u32(bytes, pos)?;
    let max = if has_max {
        Some(read_leb128_u32(bytes, pos)?)
    } else {
        None
    };
    Ok(Limits { min, max })
}

fn parse_function_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<u32>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut funcs = Vec::with_capacity(count);
    for _ in 0..count {
        funcs.push(read_leb128_u32(bytes, pos)?);
    }
    let _ = end;
    Ok(funcs)
}

fn parse_table_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<TableType>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut tables = Vec::with_capacity(count);
    for _ in 0..count {
        let element_type = ValueType::from_byte(bytes[*pos]).ok_or(WasmError::InvalidVersion)?;
        *pos += 1;
        let limits = parse_limits(bytes, pos)?;
        tables.push(TableType {
            element_type,
            limits,
        });
    }
    let _ = end;
    Ok(tables)
}

fn parse_memory_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<MemoryType>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut memories = Vec::with_capacity(count);
    for _ in 0..count {
        let limits = parse_limits(bytes, pos)?;
        memories.push(MemoryType { limits });
    }
    let _ = end;
    Ok(memories)
}

fn parse_global_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<(GlobalType, Vec<u8>)>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut globals = Vec::with_capacity(count);
    for _ in 0..count {
        let content_type = ValueType::from_byte(bytes[*pos]).ok_or(WasmError::InvalidVersion)?;
        *pos += 1;
        let mutable = bytes[*pos] != 0;
        *pos += 1;
        let expr_start = *pos;
        let expr_end = read_init_expr(bytes, pos)?;
        let init_expr = bytes[expr_start..expr_end].to_vec();
        *pos = expr_end;
        globals.push((
            GlobalType {
                content_type,
                mutable,
            },
            init_expr,
        ));
    }
    let _ = end;
    Ok(globals)
}

fn read_init_expr(bytes: &[u8], pos: &mut usize) -> Result<usize, WasmError> {
    loop {
        if *pos >= bytes.len() {
            return Err(WasmError::Truncated);
        }
        let byte = bytes[*pos];
        *pos += 1;
        match byte {
            0x0B => break,
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
    Ok(*pos)
}

fn parse_export_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<ExportDesc>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut exports = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_name(bytes, pos)?;
        let kind_byte = bytes[*pos];
        *pos += 1;
        let kind = match kind_byte {
            0 => ExportKind::Function,
            1 => ExportKind::Table,
            2 => ExportKind::Memory,
            3 => ExportKind::Global,
            _ => return Err(WasmError::BadExport),
        };
        let index = read_leb128_u32(bytes, pos)?;
        exports.push(ExportDesc { name, kind, index });
    }
    let _ = end;
    Ok(exports)
}

fn parse_code_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<FunctionBody>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut bodies = Vec::with_capacity(count);
    for _ in 0..count {
        let body_size = read_leb128_u32(bytes, pos)? as usize;
        let body_end = *pos + body_size;

        let local_count = read_leb128_u32(bytes, pos)? as usize;
        let mut locals = Vec::new();
        for _ in 0..local_count {
            let n = read_leb128_u32(bytes, pos)?;
            let ty = ValueType::from_byte(bytes[*pos]).ok_or(WasmError::InvalidVersion)?;
            *pos += 1;
            locals.push((n, ty));
        }

        let code = bytes[*pos..body_end].to_vec();
        *pos = body_end;
        bodies.push(FunctionBody { locals, code });
    }
    let _ = end;
    Ok(bodies)
}

fn parse_data_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<DataSegment>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut data = Vec::with_capacity(count);
    for _ in 0..count {
        let memory_index = read_leb128_u32(bytes, pos)?;
        let offset = read_init_expr_bytes(bytes, pos)?;
        let data_len = read_leb128_u32(bytes, pos)? as usize;
        if *pos + data_len > bytes.len() {
            return Err(WasmError::Truncated);
        }
        let segment_data = bytes[*pos..*pos + data_len].to_vec();
        *pos += data_len;
        data.push(DataSegment {
            memory_index,
            offset,
            data: segment_data,
        });
    }
    let _ = end;
    Ok(data)
}

fn read_init_expr_bytes(bytes: &[u8], pos: &mut usize) -> Result<Vec<u8>, WasmError> {
    let start = *pos;
    loop {
        if *pos >= bytes.len() {
            return Err(WasmError::Truncated);
        }
        let byte = bytes[*pos];
        *pos += 1;
        match byte {
            0x0B => break,
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
    Ok(bytes[start..*pos].to_vec())
}

fn parse_element_section(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Result<Vec<ElementSegment>, WasmError> {
    let count = read_leb128_u32(bytes, pos)? as usize;
    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        let table_index = read_leb128_u32(bytes, pos)?;
        let offset = read_init_expr_bytes(bytes, pos)?;
        let num_elems = read_leb128_u32(bytes, pos)? as usize;
        let mut func_indices = Vec::with_capacity(num_elems);
        for _ in 0..num_elems {
            func_indices.push(read_leb128_u32(bytes, pos)?);
        }
        elements.push(ElementSegment {
            table_index,
            offset,
            func_indices,
        });
    }
    let _ = end;
    Ok(elements)
}
