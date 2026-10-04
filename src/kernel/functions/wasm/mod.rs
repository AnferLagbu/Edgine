#![deny(unsafe_code)]
//! WASM 沙箱 (functions 层)
//!
//! ## 当前状态: 已完成迁移 (T6-9)
//!
//! 完整实现已从 `privileged/wasm/` 迁移到本目录:
//! - `interpreter.rs` — WASM 栈机解释器
//! - `runtime.rs` — ValueStack/CallFrame/LinearMemory 运行时
//! - `module.rs` — WASM 二进制格式解析器 (11 section)
//! - `types.rs` — WASM 1.0 类型定义
//! - `leb128.rs` — LEB128 编解码器
//!
//! `privileged/wasm/` 仅保留 re-export shim, 保持向后兼容.

/// T6-9: WASM 解释器 (原 privileged/wasm/interpreter.rs)
pub mod interpreter;
/// T6-9: LEB128 编解码器 (原 privileged/wasm/leb128.rs)
pub mod leb128;
/// T6-9: WASM 二进制格式解析器 (原 privileged/wasm/module.rs)
pub mod module;
/// T6-9: 运行时数据结构 (原 privileged/wasm/runtime.rs)
pub mod runtime;
/// T6-9: WASM 类型定义 (原 privileged/wasm/types.rs)
pub mod types;
/// WASI snapshot_preview1 适配层
pub mod wasi;
