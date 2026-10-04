#![deny(unsafe_code)]
//! WASI errno 映射 (`wasi_snapshot_preview1`)

/// WASI errno 值
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasiErrno {
    Success = 0,
    Badf = 8,
    Exist = 20,
    Fault = 21,
    Inval = 28,
    Io = 29,
    Nametoolong = 37,
    Noent = 44,
    Nospc = 69,
    Notdir = 78,
    Notempty = 79,
    Notsock = 88,
    Notsup = 58,
    Overflow = 61,
    Perm = 63,
    Race = 26,
    Sknotconn = 107,
    Txtbsy = 112,
    Notcapable = 76,
    Pipe = 64,
    Addrinuse = 3,
    Addrnotavail = 4,
    Connaborted = 7,
    Connrefused = 14,
    Connreset = 15,
    Hostunreach = 24,
    Inprogress = 27,
    Interrupted = 25,
    Isdir = 65,
    Loop = 67,
    Mfile = 68,
    Msgsize = 91,
    Notconn = 105,
    Stale = 72,
}

impl WasiErrno {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    /// 从 `Edgine` `KernelError` 映射到 WASI errno
    pub fn from_kernel_error(err: crate::functions::wasm::types::WasmError) -> Self {
        match err {
            crate::functions::wasm::types::WasmError::MemoryOutOfBounds => Self::Fault,
            _ => Self::Io,
        }
    }
}

/// WASI 成功返回值
pub fn wasi_success() -> i32 {
    WasiErrno::Success.as_i32()
}

/// WASI 错误返回值
pub fn wasi_errno(e: WasiErrno) -> i32 {
    e.as_i32()
}

impl From<WasiErrno> for crate::functions::wasm::types::WasmError {
    fn from(e: WasiErrno) -> Self {
        match e {
            WasiErrno::Fault => Self::MemoryOutOfBounds,
            WasiErrno::Inval => Self::TypeMismatch,
            WasiErrno::Badf => Self::BadExport,
            _ => Self::InternalError,
        }
    }
}
