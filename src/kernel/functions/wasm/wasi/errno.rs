#![deny(unsafe_code)]
//! WASI errno 映射 (`wasi_snapshot_preview1`)
#![deny(missing_docs)]

/// WASI errno 值
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasiErrno {
    /// 成功
    Success = 0,
    /// 文件描述符无效
    Badf = 8,
    /// 文件已存在
    Exist = 20,
    /// 内存越界或地址非法
    Fault = 21,
    /// 参数无效
    Inval = 28,
    /// I/O 错误
    Io = 29,
    /// 文件名过长
    Nametoolong = 37,
    /// 文件或目录不存在
    Noent = 44,
    /// 存储空间不足
    Nospc = 69,
    /// 不是目录
    Notdir = 78,
    /// 目录非空
    Notempty = 79,
    /// 不是套接字
    Notsock = 88,
    /// 操作不支持
    Notsup = 58,
    /// 数值溢出
    Overflow = 61,
    /// 权限不足
    Perm = 63,
    /// 竞态 (路径已在检查后被替换)
    Race = 26,
    /// 套接字未连接
    Sknotconn = 107,
    /// 文件被占用 (文本忙)
    Txtbsy = 112,
    /// 能力不足
    Notcapable = 76,
    /// 管道错误
    Pipe = 64,
    /// 地址已被占用
    Addrinuse = 3,
    /// 地址不可用
    Addrnotavail = 4,
    /// 连接已中止
    Connaborted = 7,
    /// 连接被拒绝
    Connrefused = 14,
    /// 连接被重置
    Connreset = 15,
    /// 主机不可达
    Hostunreach = 24,
    /// 操作进行中
    Inprogress = 27,
    /// 被信号中断
    Interrupted = 25,
    /// 是目录
    Isdir = 65,
    /// 符号链接层级过多
    Loop = 67,
    /// 打开的文件过多
    Mfile = 68,
    /// 消息过长
    Msgsize = 91,
    /// 套接字未连接 (目标地址无效)
    Notconn = 105,
    /// 文件句柄已过期
    Stale = 72,
}

impl WasiErrno {
    /// 返回 errno 的整数值
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// 从 `Edgine` `KernelError` 映射到 WASI errno
    pub fn from_kernel_error(err: &crate::functions::wasm::types::WasmError) -> Self {
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
