#![deny(unsafe_code)]
//! WASI snapshot_preview1 适配层
//!
//! 实现 WASI preview1 标准接口，使 Edgine 可运行 WASI 编译的 WASM 模块。
//!
//! ## 架构
//!
//! ```text
//! WASM 模块
//!   │ import "wasi_snapshot_preview1" "fd_read" ...
//!   ▼
//! WASI 适配层 (本模块)
//!   │ WasiContext { fd_table, args, env }
//!   │ fn wasi_fd_read() → 查 fd_table → read_bytes()
//!   ▼
//! 现有 POSIX 服务层
//!   functions::fs (VFS)
//!   functions::mm (mmap)
//!   functions::proc (exit)
//!   privileged::timer (clock)
//! ```

pub mod errno;
pub mod fd_table;

// WASI 函数模块
mod clock_random;
mod env_args;
pub mod fd_ops;
pub mod path_ops;
mod process;
pub mod sock;

pub use errno::{WasiErrno, wasi_errno, wasi_success};
pub use fd_table::{
    WASI_STDERR, WASI_STDIN, WASI_STDOUT, WasiFdEntry, WasiFdTable, WasiFileType, WasiIoVec,
    WasiRights, read_iovec_from_memory, write_i32_to_memory, write_i64_to_memory,
    write_u32_to_memory,
};

use crate::functions::wasm::interpreter::Interpreter;
use crate::functions::wasm::types::WasmError;
use alloc::string::String;
use alloc::vec::Vec;

/// WASI 运行时上下文 (每个 WASM 实例一个)
pub struct WasiContext {
    pub fd_table: WasiFdTable,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl WasiContext {
    pub fn new() -> Self {
        Self {
            fd_table: WasiFdTable::new(256),
            args: Vec::new(),
            env: Vec::new(),
        }
    }
}

/// WASI 函数签名: (`WasiContext`, Interpreter) -> Result
pub type WasiFunc = fn(&mut WasiContext, &mut Interpreter) -> Result<(), WasmError>;

/// WASI 函数注册表: name → 函数指针
///
/// 用于 `Interpreter::auto_register_wasi()` 根据 WASM import section
/// 自动查找并注册对应的 host function。
pub fn wasi_function_table() -> &'static [(&'static str, WasiFunc)] {
    &[
        // G1: 进程控制
        ("proc_exit", process::wasi_proc_exit as WasiFunc),
        ("sched_yield", process::wasi_sched_yield as WasiFunc),
        // G2: 时钟/随机
        (
            "clock_time_get",
            clock_random::wasi_clock_time_get as WasiFunc,
        ),
        ("random_get", clock_random::wasi_random_get as WasiFunc),
        // G3: 环境/参数
        (
            "environ_sizes_get",
            env_args::wasi_environ_sizes_get as WasiFunc,
        ),
        ("environ_get", env_args::wasi_environ_get as WasiFunc),
        ("args_sizes_get", env_args::wasi_args_sizes_get as WasiFunc),
        ("args_get", env_args::wasi_args_get as WasiFunc),
        // G4: FD 管理
        ("fd_close", fd_ops::wasi_fd_close as WasiFunc),
        ("fd_seek", fd_ops::wasi_fd_seek as WasiFunc),
        ("fd_tell", fd_ops::wasi_fd_tell as WasiFunc),
        ("fd_sync", fd_ops::wasi_fd_sync as WasiFunc),
        ("fd_prestat_get", fd_ops::wasi_fd_prestat_get as WasiFunc),
        (
            "fd_prestat_dir_name",
            fd_ops::wasi_fd_prestat_dir_name as WasiFunc,
        ),
        ("fd_stat_get", fd_ops::wasi_fd_stat_get as WasiFunc),
        // G5: FD I/O
        ("fd_read", fd_ops::wasi_fd_read as WasiFunc),
        ("fd_write", fd_ops::wasi_fd_write as WasiFunc),
        ("fd_pread", fd_ops::wasi_fd_pread as WasiFunc),
        ("fd_pwrite", fd_ops::wasi_fd_pwrite as WasiFunc),
        ("fd_allocate", fd_ops::wasi_fd_allocate as WasiFunc),
        ("fd_advise", fd_ops::wasi_fd_advise as WasiFunc),
        // G7: 高级 FD
        ("fd_renumber", fd_ops::wasi_fd_renumber as WasiFunc),
        ("fd_dup", fd_ops::wasi_fd_dup as WasiFunc),
        ("fd_readdir", fd_ops::wasi_fd_readdir as WasiFunc),
        // G6: 路径操作
        ("path_open", path_ops::wasi_path_open as WasiFunc),
        (
            "path_create_directory",
            path_ops::wasi_path_create_directory as WasiFunc,
        ),
        (
            "path_remove_directory",
            path_ops::wasi_path_remove_directory as WasiFunc,
        ),
        (
            "path_unlink_file",
            path_ops::wasi_path_unlink_file as WasiFunc,
        ),
        ("path_symlink", path_ops::wasi_path_symlink as WasiFunc),
        ("path_readlink", path_ops::wasi_path_readlink as WasiFunc),
        ("path_rename", path_ops::wasi_path_rename as WasiFunc),
        (
            "path_filestat_get",
            path_ops::wasi_path_filestat_get as WasiFunc,
        ),
        (
            "path_filestat_set_times",
            path_ops::wasi_path_filestat_set_times as WasiFunc,
        ),
        ("path_link", path_ops::wasi_path_link as WasiFunc),
        // G8: Socket
        ("sock_accept", sock::wasi_sock_accept as WasiFunc),
        ("sock_connect", sock::wasi_sock_connect as WasiFunc),
        ("sock_recv", sock::wasi_sock_recv as WasiFunc),
        ("sock_send", sock::wasi_sock_send as WasiFunc),
    ]
}
