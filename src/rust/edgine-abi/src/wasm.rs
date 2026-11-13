//! 进程级 wasm 运行时私有 syscall ABI 常量与实例信息结构.
//!
//! 值域为 Edgine 私有功能段 `766-779`：Linux 没有"进程内加载并执行 wasm"
//! 的系统调用，占用 0-299 的 Linux 号位既无兼容收益，又破坏"0-299 == Linux"
//! 的承重不变式，故按 `docs/plan/archive/wasm-runtime-wiring.md` §3 丁 取私有段。
//!
//! 内核 `privileged::syscall::types` 的编号定义、`functions::wasm::registry`
//! 的策略实现与用户态入口桩（`src/user/wasmstub`）均引用本模块，结构上避免
//! "内核与用户态各写一份常量"的漂移.

/// 加载 wasm 模块到当前进程 (返回非 0 句柄).
///
/// 参数: (`path_ptr`: u64, `argv_ptr`: u64, `envp_ptr`: u64).
/// `argv_ptr` / `envp_ptr` 为 `NULL` 结尾的字符串指针数组, 可为 0 表示空.
pub const EG_WASM_LOAD: u64 = 766;

/// 运行当前进程已加载的 wasm 模块 (按 gas 上限执行到结束).
///
/// 参数: (`handle`: u64). 返回 0 表示模块已结束, 负值为 -errno.
pub const EG_WASM_RUN: u64 = 767;

/// 查询实例信息 (状态 / 退出码 / gas 消耗).
///
/// 参数: (`handle`: u64, `out_ptr`: u64 指向 [`EgWasmInfo`]).
pub const EG_WASM_GET_INFO: u64 = 768;

/// 卸载当前进程已加载的 wasm 实例.
///
/// 参数: (`handle`: u64).
pub const EG_WASM_UNLOAD: u64 = 769;

/// 实例状态: 已加载, 尚未运行.
pub const EG_WASM_STATUS_READY: u32 = 0;

/// 实例状态: 运行结束 (正常返回或 `proc_exit`).
pub const EG_WASM_STATUS_FINISHED: u32 = 1;

/// 实例状态: 运行中以 trap / 错误终止.
pub const EG_WASM_STATUS_TRAPPED: u32 = 2;

/// wasm 实例信息 (`EG_WASM_GET_INFO` 的写出结构).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EgWasmInfo {
    /// 实例状态: [`EG_WASM_STATUS_READY`] / [`EG_WASM_STATUS_FINISHED`] /
    /// [`EG_WASM_STATUS_TRAPPED`].
    pub status: u32,
    /// 模块退出码 (`proc_exit` 的实参; 正常返回为 0, trap 为 1).
    pub exit_code: i32,
    /// 已消耗的 gas (解释器逐指令计量).
    pub gas_used: u64,
}
