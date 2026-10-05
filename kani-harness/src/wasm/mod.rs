//! 最小 `wasm` 模块层级: 仅纳入 `leb128` 及其依赖 `types`, 引真实内核源码.
//!
//! `#[path]` 的基准目录即本文件所在目录 `kani-harness/src/wasm/`,
//! 因此 `../../../` 回到仓库根, 再进入 `src/kernel/functions/wasm/`.

/// WASM 类型定义 (真实源码: `src/kernel/functions/wasm/types.rs`).
#[path = "../../../src/kernel/functions/wasm/types.rs"]
pub mod types;

/// LEB128 编解码器 (真实源码: `src/kernel/functions/wasm/leb128.rs`).
#[path = "../../../src/kernel/functions/wasm/leb128.rs"]
pub mod leb128;
