#![deny(unsafe_code)]
//! WASI 文件描述符表 (独立于 POSIX fd 表)

use super::errno::WasiErrno;
use alloc::vec::Vec;

/// WASI 标准 fd 编号
pub const WASI_STDIN: u32 = 0;
pub const WASI_STDOUT: u32 = 1;
pub const WASI_STDERR: u32 = 2;

/// WASI 文件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasiFileType {
    Directory,
    RegularFile,
    Symlink,
    CharacterDevice,
    Socket,
}

/// WASI 权限 (base + inheriting)
#[derive(Debug, Clone, Copy)]
pub struct WasiRights {
    pub base: u64,
    pub inheriting: u64,
}

impl WasiRights {
    pub const ALL: Self = Self {
        base: u64::MAX,
        inheriting: u64::MAX,
    };

    pub const DIRECTORY: Self = Self {
        base: 0x10000000
            | 0x800
            | 0x400
            | 0x200
            | 0x100
            | 0x80
            | 0x40
            | 0x20
            | 0x10
            | 0x08
            | 0x04
            | 0x02
            | 0x01,
        inheriting: u64::MAX,
    };

    pub const FILE: Self = Self {
        base: 0x800 | 0x400 | 0x200 | 0x100 | 0x80 | 0x40 | 0x20 | 0x10 | 0x08 | 0x04 | 0x02 | 0x01,
        inheriting: 0x800
            | 0x400
            | 0x200
            | 0x100
            | 0x80
            | 0x40
            | 0x20
            | 0x10
            | 0x08
            | 0x04
            | 0x02
            | 0x01,
    };
}

/// WASI fd 表条目
pub struct WasiFdEntry {
    pub file_type: WasiFileType,
    pub rights: WasiRights,
    /// 映射到 VFS 的内部 fd (edgine 内部使用)
    pub inner_fd: i32,
    /// preopen 路径 (仅 preopen fd 有值)
    pub path: Option<alloc::string::String>,
}

/// WASI 文件描述符表 (独立于 POSIX fd 表)
///
/// WASI fd 0/1/2 映射到进程 stdin/stdout/stderr，
/// 其余 fd 通过 `alloc()` 独立分配，不与 POSIX fd 共享。
pub struct WasiFdTable {
    entries: Vec<Option<WasiFdEntry>>,
    max_fds: u32,
}

impl WasiFdTable {
    pub fn new(max_fds: u32) -> Self {
        let mut entries = Vec::with_capacity(max_fds as usize);
        entries.resize_with(max_fds as usize, || None);
        Self { entries, max_fds }
    }

    /// 分配一个新的 fd，返回 fd 编号
    ///
    /// # Errors
    ///
    /// 当 fd 表已满(从 3 起无空槽位)时返回 `WasiErrno::Badf`.
    pub fn alloc(&mut self, entry: WasiFdEntry) -> Result<u32, WasiErrno> {
        for i in 3..self.max_fds {
            if self.entries[i as usize].is_none() {
                self.entries[i as usize] = Some(entry);
                return Ok(i);
            }
        }
        Err(WasiErrno::Badf)
    }

    /// 获取 fd 条目引用
    ///
    /// # Errors
    ///
    /// 当 fd 无效或尚未分配时返回 `WasiErrno::Badf`.
    pub fn get(&self, fd: u32) -> Result<&WasiFdEntry, WasiErrno> {
        self.entries
            .get(fd as usize)
            .and_then(|e| e.as_ref())
            .ok_or(WasiErrno::Badf)
    }

    /// 获取 fd 条目可变引用
    ///
    /// # Errors
    ///
    /// 当 fd 无效或尚未分配时返回 `WasiErrno::Badf`.
    pub fn get_mut(&mut self, fd: u32) -> Result<&mut WasiFdEntry, WasiErrno> {
        self.entries
            .get_mut(fd as usize)
            .and_then(|e| e.as_mut())
            .ok_or(WasiErrno::Badf)
    }

    /// 关闭 fd
    ///
    /// # Errors
    ///
    /// 当 `fd < 3` 或 fd 无效/未分配时返回 `WasiErrno::Badf`.
    pub fn close(&mut self, fd: u32) -> Result<WasiFdEntry, WasiErrno> {
        if fd < 3 {
            return Err(WasiErrno::Badf);
        }
        self.entries
            .get_mut(fd as usize)
            .ok_or(WasiErrno::Badf)?
            .take()
            .ok_or(WasiErrno::Badf)
    }

    /// 重编号 fd (`fd_renumber`)
    ///
    /// # Errors
    ///
    /// 当 `from`/`to` 超出表范围或 `from` 未分配时返回 `WasiErrno::Badf`.
    pub fn renumber(&mut self, from: u32, to: u32) -> Result<(), WasiErrno> {
        if from >= self.max_fds || to >= self.max_fds {
            return Err(WasiErrno::Badf);
        }
        let entry = self.close(from)?;
        self.entries[to as usize] = Some(entry);
        Ok(())
    }
}

/// 从 WASM 线性内存读取 iovec 数组
pub struct WasiIoVec {
    pub buf: u32,
    pub len: u32,
}

/// 从线性内存 `iovs_ptr` 起读取 `iovs_len` 个 iovec 条目.
///
/// # Errors
///
/// - 解释器未配置线性内存 → `WasiErrno::Inval`
/// - 读取越界 → `WasiErrno::Fault`
pub fn read_iovec_from_memory(
    interp: &crate::services::wasm::interpreter::Interpreter,
    iovs_ptr: u32,
    iovs_len: u32,
) -> Result<Vec<WasiIoVec>, WasiErrno> {
    let mem = interp.memory.as_ref().ok_or(WasiErrno::Inval)?;
    let mut iovecs = Vec::with_capacity(iovs_len as usize);
    for i in 0..iovs_len {
        let ptr = iovs_ptr + i * 8; // 每个 iovec: {ptr: u32, len: u32}
        let buf_ptr = mem.read_u32(ptr).map_err(|_| WasiErrno::Fault)?;
        let buf_len = mem.read_u32(ptr + 4).map_err(|_| WasiErrno::Fault)?;
        iovecs.push(WasiIoVec {
            buf: buf_ptr,
            len: buf_len,
        });
    }
    Ok(iovecs)
}

/// 向 WASM 线性内存写入 u32
pub fn write_u32_to_memory(
    interp: &mut crate::services::wasm::interpreter::Interpreter,
    ptr: u32,
    val: u32,
) {
    if let Some(ref mut mem) = interp.memory {
        let _ = mem.write_u32(ptr, val);
    }
}

/// 向 WASM 线性内存写入 i64
pub fn write_i64_to_memory(
    interp: &mut crate::services::wasm::interpreter::Interpreter,
    ptr: u32,
    val: i64,
) {
    if let Some(ref mut mem) = interp.memory {
        let _ = mem.write_u64(ptr, val as u64);
    }
}

/// 向 WASM 线性内存写入 i32
pub fn write_i32_to_memory(
    interp: &mut crate::services::wasm::interpreter::Interpreter,
    ptr: u32,
    val: i32,
) {
    write_u32_to_memory(interp, ptr, val as u32);
}

/// 向 WASM 线性内存写入字节序列
pub fn write_bytes_to_memory(
    interp: &mut crate::services::wasm::interpreter::Interpreter,
    ptr: u32,
    data: &[u8],
) {
    if let Some(ref mut mem) = interp.memory {
        for (i, &byte) in data.iter().enumerate() {
            let _ = mem.write_u8(ptr + i as u32, byte);
        }
    }
}

/// 从 WASM 线性内存读取字节序列
///
/// # Errors
///
/// - 解释器未配置线性内存 → `WasiErrno::Inval`
/// - 读取越界 → `WasiErrno::Fault`
pub fn read_bytes_from_memory(
    interp: &crate::services::wasm::interpreter::Interpreter,
    ptr: u32,
    len: u32,
) -> Result<alloc::vec::Vec<u8>, WasiErrno> {
    let mem = interp.memory.as_ref().ok_or(WasiErrno::Inval)?;
    let mut buf = alloc::vec::Vec::with_capacity(len as usize);
    for i in 0..len {
        let byte = mem.read_u8(ptr + i).map_err(|_| WasiErrno::Fault)?;
        buf.push(byte);
    }
    Ok(buf)
}
