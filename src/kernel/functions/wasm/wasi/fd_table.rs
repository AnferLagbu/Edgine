#![deny(unsafe_code)]
#![deny(missing_docs)]
//! WASI 文件描述符表 (独立于 POSIX fd 表)

use super::errno::WasiErrno;
use crate::functions::net::SocketHandle;
use alloc::vec::Vec;

/// WASI 标准 fd 编号
pub const WASI_STDIN: u32 = 0;
/// 标准输出 fd 编号 (1)
pub const WASI_STDOUT: u32 = 1;
/// 标准错误 fd 编号 (2)
pub const WASI_STDERR: u32 = 2;

/// WASI 文件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasiFileType {
    /// 目录
    Directory,
    /// 普通文件
    RegularFile,
    /// 符号链接
    Symlink,
    /// 字符设备
    CharacterDevice,
    /// 套接字
    Socket,
}

/// WASI 权限 (base + inheriting)
#[derive(Debug, Clone, Copy)]
pub struct WasiRights {
    /// 当前对象上的权限位
    pub base: u64,
    /// 新建对象继承的权限位
    pub inheriting: u64,
}

impl WasiRights {
    /// 全部权限 (base / inheriting 均取满位)
    pub const ALL: Self = Self {
        base: u64::MAX,
        inheriting: u64::MAX,
    };

    /// 目录默认权限 (含遍历 / 创建 / 删除等目录类权限)
    pub const DIRECTORY: Self = Self {
        base: 0x1000_0000
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

    /// 普通文件默认权限 (不含目录类权限)
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
    /// 文件类型
    pub file_type: WasiFileType,
    /// 该 fd 携带的权限
    pub rights: WasiRights,
    /// 映射到 VFS 的内部 fd (`-1` 表示无 VFS 后端, 如 socket)
    pub inner_fd: i32,
    /// 该 fd 绑定的网络 socket 句柄 (非 socket fd 为 `None`)
    pub socket: Option<SocketHandle>,
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
    /// 创建容量为 `max_fds` 的 fd 表
    ///
    /// 预置标准流 fd 0/1/2 (与进程 VFS 同号 fd 一一对应, 宿主 I/O 直接落
    /// VFS) 与根 preopen fd 3 (`"/"`, 供 `path_open` 等相对路径解析);
    /// 容量不足 4 时只预置能容纳的槽位。
    #[must_use]
    pub fn new(max_fds: u32) -> Self {
        let mut entries = Vec::with_capacity(max_fds as usize);
        entries.resize_with(max_fds as usize, || None);
        let mut table = Self { entries, max_fds };

        // 标准流: WASI fd N 映射进程 VFS fd N, `fd_read` / `fd_write` 据此落真实 VFS.
        for (fd, inner_fd) in [(WASI_STDIN, 0i32), (WASI_STDOUT, 1i32), (WASI_STDERR, 2i32)] {
            if let Some(slot) = table.entries.get_mut(fd as usize) {
                *slot = Some(WasiFdEntry {
                    file_type: WasiFileType::CharacterDevice,
                    rights: WasiRights::ALL,
                    inner_fd,
                    socket: None,
                    path: None,
                });
            }
        }

        // SIMPLIFIED: 根 preopen 只承载路径解析 (`resolve_path` 读 `path` 字段),
        // 不预开目录 VFS fd (inner_fd = -1); 影响面: 直接以 preopen fd 做
        // `fd_readdir` / `fd_stat_get` 会返错, 经 `path_*` 打开子路径不受影响;
        // 何时需扩展: 需要遍历 preopen 目录的 WASI 程序接入时, 在实例构造期
        // `vfs_open("/", O_DIRECTORY)` 并回填 inner_fd.
        if let Some(slot) = table.entries.get_mut(3) {
            *slot = Some(WasiFdEntry {
                file_type: WasiFileType::Directory,
                rights: WasiRights::DIRECTORY,
                inner_fd: -1,
                socket: None,
                path: Some(alloc::string::String::from("/")),
            });
        }

        table
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
    /// 线性内存中的缓冲区起始偏移
    pub buf: u32,
    /// 缓冲区字节长度
    pub len: u32,
}

/// 从线性内存 `iovs_ptr` 起读取 `iovs_len` 个 iovec 条目.
///
/// # Errors
///
/// - 解释器未配置线性内存 → `WasiErrno::Inval`
/// - 读取越界 → `WasiErrno::Fault`
pub fn read_iovec_from_memory(
    interp: &crate::functions::wasm::interpreter::Interpreter,
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
    interp: &mut crate::functions::wasm::interpreter::Interpreter,
    ptr: u32,
    val: u32,
) {
    if let Some(ref mut mem) = interp.memory {
        let _ = mem.write_u32(ptr, val);
    }
}

/// 向 WASM 线性内存写入 i64
pub fn write_i64_to_memory(
    interp: &mut crate::functions::wasm::interpreter::Interpreter,
    ptr: u32,
    val: i64,
) {
    if let Some(ref mut mem) = interp.memory {
        let _ = mem.write_u64(ptr, val.cast_unsigned());
    }
}

/// 向 WASM 线性内存写入 i32
pub fn write_i32_to_memory(
    interp: &mut crate::functions::wasm::interpreter::Interpreter,
    ptr: u32,
    val: i32,
) {
    write_u32_to_memory(interp, ptr, val.cast_unsigned());
}

/// 向 WASM 线性内存写入字节序列
pub fn write_bytes_to_memory(
    interp: &mut crate::functions::wasm::interpreter::Interpreter,
    ptr: u32,
    data: &[u8],
) {
    if let Some(ref mut mem) = interp.memory {
        for (i, &byte) in data.iter().enumerate() {
            let _ = mem.write_u8(ptr + crate::functions::intcast::low_u32_from_usize(i), byte);
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
    interp: &crate::functions::wasm::interpreter::Interpreter,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn file_entry() -> WasiFdEntry {
        WasiFdEntry {
            file_type: WasiFileType::RegularFile,
            rights: WasiRights::FILE,
            inner_fd: 10,
            socket: None,
            path: None,
        }
    }

    /// 预置标准流与根 preopen: 类型 / 后端 fd / 路径三面固定
    #[test]
    fn new_presets_stdio_and_preopen_root() {
        let mut table = WasiFdTable::new(16);
        for (fd, inner_fd) in [(WASI_STDIN, 0), (WASI_STDOUT, 1), (WASI_STDERR, 2)] {
            let entry = table.get(fd).unwrap();
            assert_eq!(entry.file_type, WasiFileType::CharacterDevice);
            assert_eq!(entry.inner_fd, inner_fd);
            assert!(entry.path.is_none());
        }

        let preopen = table.get(3).unwrap();
        assert_eq!(preopen.file_type, WasiFileType::Directory);
        assert_eq!(preopen.inner_fd, -1);
        assert_eq!(preopen.path.as_deref(), Some("/"));

        // 首个动态分配落在 preopen 之后, 不覆盖任何预置槽位.
        assert_eq!(table.alloc(file_entry()).unwrap(), 4);
        assert_eq!(table.get(4).unwrap().inner_fd, 10);
    }

    /// 容量不足 4 时预置按槽位截断, 不 panic 且不越界写
    #[test]
    fn presets_fit_small_capacity() {
        let mut table = WasiFdTable::new(2);
        assert!(table.get(WASI_STDIN).is_ok());
        assert!(table.get(WASI_STDOUT).is_ok());
        assert!(table.get(WASI_STDERR).is_err());
        assert!(table.get(3).is_err());
        assert!(table.alloc(file_entry()).is_err());
    }
}
