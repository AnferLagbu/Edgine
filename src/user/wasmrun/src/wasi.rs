//! WASI (`wasi_snapshot_preview1`) 的纯 ABI 层: errno、fd 表、路径解析与结构编码.
//!
//! 本模块不依赖 wasmi 与 syscall, 可在 host 侧单测; 真正的 host 闭包
//! (读 wasm 线性内存 + 翻译到 Edgine syscall) 见 `host.rs`.

use alloc::vec::Vec;

/// 关闭宿主 fd; 单测环境无 syscall, 走空桩.
#[cfg(not(test))]
pub(crate) fn close_host(fd: i32) -> i32 {
    userlib::sys::close(fd)
}

#[cfg(test)]
pub(crate) fn close_host(_fd: i32) -> i32 {
    0
}

// ============================================================================
// WASI errno (wasi_snapshot_preview1 官方值域)
// ============================================================================
pub const ERRNO_SUCCESS: i32 = 0;
pub const ERRNO_TOOBIG: i32 = 1;
pub const ERRNO_ACCES: i32 = 2;
pub const ERRNO_ADDRINUSE: i32 = 3;
pub const ERRNO_ADDRNOTAVAIL: i32 = 4;
pub const ERRNO_AFNOSUPPORT: i32 = 5;
pub const ERRNO_AGAIN: i32 = 6;
pub const ERRNO_ALREADY: i32 = 7;
pub const ERRNO_BADF: i32 = 8;
pub const ERRNO_BADMSG: i32 = 9;
pub const ERRNO_BUSY: i32 = 10;
pub const ERRNO_CANCELED: i32 = 11;
pub const ERRNO_CHILD: i32 = 12;
pub const ERRNO_CONNABORTED: i32 = 13;
pub const ERRNO_CONNREFUSED: i32 = 14;
pub const ERRNO_CONNRESET: i32 = 15;
pub const ERRNO_DEADLK: i32 = 16;
pub const ERRNO_DESTADDRREQ: i32 = 17;
pub const ERRNO_DOM: i32 = 18;
pub const ERRNO_DQUOT: i32 = 19;
pub const ERRNO_EXIST: i32 = 20;
pub const ERRNO_FAULT: i32 = 21;
pub const ERRNO_FBIG: i32 = 22;
pub const ERRNO_HOSTUNREACH: i32 = 23;
pub const ERRNO_IDRM: i32 = 24;
pub const ERRNO_ILSEQ: i32 = 25;
pub const ERRNO_INPROGRESS: i32 = 26;
pub const ERRNO_INTR: i32 = 27;
pub const ERRNO_INVAL: i32 = 28;
pub const ERRNO_IO: i32 = 29;
pub const ERRNO_ISCONN: i32 = 30;
pub const ERRNO_ISDIR: i32 = 31;
pub const ERRNO_LOOP: i32 = 32;
pub const ERRNO_MFILE: i32 = 33;
pub const ERRNO_MLINK: i32 = 34;
pub const ERRNO_MSGSIZE: i32 = 35;
pub const ERRNO_MULTIHOP: i32 = 36;
pub const ERRNO_NAMETOOLONG: i32 = 37;
pub const ERRNO_NETDOWN: i32 = 38;
pub const ERRNO_NETRESET: i32 = 39;
pub const ERRNO_NETUNREACH: i32 = 40;
pub const ERRNO_NFILE: i32 = 41;
pub const ERRNO_NOBUFS: i32 = 42;
pub const ERRNO_NODEV: i32 = 43;
pub const ERRNO_NOENT: i32 = 44;
pub const ERRNO_NOEXEC: i32 = 45;
pub const ERRNO_NOLCK: i32 = 46;
pub const ERRNO_NOLINK: i32 = 47;
pub const ERRNO_NOMEM: i32 = 48;
pub const ERRNO_NOMSG: i32 = 49;
pub const ERRNO_NOPROTOOPT: i32 = 50;
pub const ERRNO_NOSPC: i32 = 51;
pub const ERRNO_NOSYS: i32 = 52;
pub const ERRNO_NOTCONN: i32 = 53;
pub const ERRNO_NOTDIR: i32 = 54;
pub const ERRNO_NOTEMPTY: i32 = 55;
pub const ERRNO_NOTRECOVERABLE: i32 = 56;
pub const ERRNO_NOTSOCK: i32 = 57;
pub const ERRNO_NOTSUP: i32 = 58;
pub const ERRNO_NOTTY: i32 = 59;
pub const ERRNO_NXIO: i32 = 60;
pub const ERRNO_OVERFLOW: i32 = 61;
pub const ERRNO_OWNERDEAD: i32 = 62;
pub const ERRNO_PERM: i32 = 63;
pub const ERRNO_PIPE: i32 = 64;
pub const ERRNO_PROTO: i32 = 65;
pub const ERRNO_PROTONOSUPPORT: i32 = 66;
pub const ERRNO_PROTOTYPE: i32 = 67;
pub const ERRNO_RANGE: i32 = 68;
pub const ERRNO_ROFS: i32 = 69;
pub const ERRNO_SPIPE: i32 = 70;
pub const ERRNO_SRCH: i32 = 71;
pub const ERRNO_STALE: i32 = 72;
pub const ERRNO_TIMEDOUT: i32 = 73;
pub const ERRNO_TXTBSY: i32 = 74;
pub const ERRNO_XDEV: i32 = 75;
pub const ERRNO_NOTCAPABLE: i32 = 76;

/// 把负返回值或 Linux errno 转成 WASI errno.
///
/// 内核 syscall 统一返回 `-errno`; host 层只把正 errno 传进来.
#[must_use]
pub fn errno_from_linux(errno: i32) -> i32 {
    match errno {
        1 => ERRNO_PERM,
        2 => ERRNO_NOENT,
        3 => ERRNO_SRCH,
        4 => ERRNO_INTR,
        5 => ERRNO_IO,
        6 => ERRNO_NXIO,
        7 => ERRNO_TOOBIG,
        8 => ERRNO_NOEXEC,
        9 => ERRNO_BADF,
        10 => ERRNO_CHILD,
        11 => ERRNO_AGAIN,
        12 => ERRNO_NOMEM,
        13 => ERRNO_ACCES,
        14 => ERRNO_FAULT,
        16 => ERRNO_BUSY,
        17 => ERRNO_EXIST,
        18 => ERRNO_XDEV,
        19 => ERRNO_NODEV,
        20 => ERRNO_NOTDIR,
        21 => ERRNO_ISDIR,
        22 => ERRNO_INVAL,
        23 => ERRNO_NFILE,
        24 => ERRNO_MFILE,
        25 => ERRNO_NOTTY,
        26 => ERRNO_TXTBSY,
        27 => ERRNO_FBIG,
        28 => ERRNO_NOSPC,
        29 => ERRNO_SPIPE,
        30 => ERRNO_ROFS,
        31 => ERRNO_MLINK,
        32 => ERRNO_PIPE,
        33 => ERRNO_DOM,
        34 => ERRNO_RANGE,
        35 => ERRNO_DEADLK,
        36 => ERRNO_NAMETOOLONG,
        37 => ERRNO_NOLCK,
        38 => ERRNO_NOSYS,
        39 => ERRNO_NOTEMPTY,
        42 => ERRNO_NOMSG,
        43 => ERRNO_IDRM,
        60 | 61 | 62 | 63 => ERRNO_NOTSUP,
        71 => ERRNO_PROTO,
        74 => ERRNO_BADMSG,
        75 => ERRNO_OVERFLOW,
        84 => ERRNO_ILSEQ,
        88 => ERRNO_NOTSOCK,
        89 => ERRNO_DESTADDRREQ,
        90 => ERRNO_MSGSIZE,
        91 => ERRNO_PROTOTYPE,
        92 => ERRNO_NOPROTOOPT,
        93 => ERRNO_PROTONOSUPPORT,
        95 => ERRNO_NOTSUP,
        97 => ERRNO_AFNOSUPPORT,
        98 => ERRNO_ADDRINUSE,
        99 => ERRNO_ADDRNOTAVAIL,
        100 => ERRNO_NETDOWN,
        101 => ERRNO_NETUNREACH,
        102 => ERRNO_NETRESET,
        103 => ERRNO_CONNABORTED,
        104 => ERRNO_CONNRESET,
        105 => ERRNO_NOBUFS,
        106 => ERRNO_ISCONN,
        107 => ERRNO_NOTCONN,
        110 => ERRNO_TIMEDOUT,
        111 => ERRNO_CONNREFUSED,
        113 => ERRNO_HOSTUNREACH,
        114 => ERRNO_ALREADY,
        115 => ERRNO_INPROGRESS,
        122 => ERRNO_DQUOT,
        125 => ERRNO_CANCELED,
        130 => ERRNO_OWNERDEAD,
        131 => ERRNO_NOTRECOVERABLE,
        _ => ERRNO_IO,
    }
}

// ============================================================================
// WASI 常量与结构
// ============================================================================
pub const FILETYPE_UNKNOWN: u8 = 0;
pub const FILETYPE_BLOCK_DEVICE: u8 = 1;
pub const FILETYPE_CHARACTER_DEVICE: u8 = 2;
pub const FILETYPE_DIRECTORY: u8 = 3;
pub const FILETYPE_REGULAR_FILE: u8 = 4;
pub const FILETYPE_SOCKET_DGRAM: u8 = 5;
pub const FILETYPE_SOCKET_STREAM: u8 = 6;
pub const FILETYPE_SYMBOLIC_LINK: u8 = 7;

pub const FDFLAG_APPEND: u16 = 1 << 0;
pub const FDFLAG_DSYNC: u16 = 1 << 1;
pub const FDFLAG_NONBLOCK: u16 = 1 << 2;
pub const FDFLAG_RSYNC: u16 = 1 << 3;
pub const FDFLAG_SYNC: u16 = 1 << 4;

pub const OFLAG_CREAT: u16 = 1 << 0;
pub const OFLAG_DIRECTORY: u16 = 1 << 1;
pub const OFLAG_EXCL: u16 = 1 << 2;
pub const OFLAG_TRUNC: u16 = 1 << 3;

pub const LOOKUP_SYMLINK_FOLLOW: u32 = 1;
pub const FSTFLAG_ATIM: u16 = 1 << 0;
pub const FSTFLAG_ATIM_NOW: u16 = 1 << 1;
pub const FSTFLAG_MTIM: u16 = 1 << 2;
pub const FSTFLAG_MTIM_NOW: u16 = 1 << 3;

pub const RIGHTS_FD_DATASYNC: u64 = 1 << 0;
pub const RIGHTS_FD_READ: u64 = 1 << 1;
pub const RIGHTS_FD_SEEK: u64 = 1 << 2;
pub const RIGHTS_FD_FDSTAT_SET_FLAGS: u64 = 1 << 3;
pub const RIGHTS_FD_SYNC: u64 = 1 << 4;
pub const RIGHTS_FD_TELL: u64 = 1 << 5;
pub const RIGHTS_FD_WRITE: u64 = 1 << 6;
pub const RIGHTS_FD_ADVISE: u64 = 1 << 7;
pub const RIGHTS_FD_ALLOCATE: u64 = 1 << 8;
pub const RIGHTS_PATH_CREATE_DIRECTORY: u64 = 1 << 9;
pub const RIGHTS_PATH_CREATE_FILE: u64 = 1 << 10;
pub const RIGHTS_PATH_LINK_SOURCE: u64 = 1 << 11;
pub const RIGHTS_PATH_LINK_TARGET: u64 = 1 << 12;
pub const RIGHTS_PATH_OPEN: u64 = 1 << 13;
pub const RIGHTS_FD_READDIR: u64 = 1 << 14;
pub const RIGHTS_PATH_READLINK: u64 = 1 << 15;
pub const RIGHTS_PATH_RENAME_SOURCE: u64 = 1 << 16;
pub const RIGHTS_PATH_RENAME_TARGET: u64 = 1 << 17;
pub const RIGHTS_PATH_FILESTAT_GET: u64 = 1 << 18;
pub const RIGHTS_PATH_FILESTAT_SET_SIZE: u64 = 1 << 19;
pub const RIGHTS_PATH_FILESTAT_SET_TIMES: u64 = 1 << 20;
pub const RIGHTS_FD_FILESTAT_GET: u64 = 1 << 21;
pub const RIGHTS_FD_FILESTAT_SET_SIZE: u64 = 1 << 22;
pub const RIGHTS_FD_FILESTAT_SET_TIMES: u64 = 1 << 23;
pub const RIGHTS_PATH_SYMLINK: u64 = 1 << 24;
pub const RIGHTS_PATH_REMOVE_DIRECTORY: u64 = 1 << 25;
pub const RIGHTS_PATH_UNLINK_FILE: u64 = 1 << 26;
pub const RIGHTS_POLL_FD_READWRITE: u64 = 1 << 27;
pub const RIGHTS_SOCK_SHUTDOWN: u64 = 1 << 28;
pub const RIGHTS_SOCK_ACCEPT: u64 = 1 << 29;

/// 所有 rights 位 (30 位).
pub const RIGHTS_ALL: u64 = (1 << 30) - 1;

/// 标准流 rights: 与内核 `WasiFdTable` 给 stdio 的位段同口径.
pub const RIGHTS_STDIO: u64 = (1 << 0)
    | (1 << 1)
    | (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 7)
    | (1 << 8);

/// 普通文件 rights: 读写/定位/同步/分配/元数据.
pub const RIGHTS_FILE: u64 = (1 << 0)
    | (1 << 1)
    | (1 << 2)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 7)
    | (1 << 8)
    | (1 << 21)
    | (1 << 22)
    | (1 << 23)
    | (1 << 27);

/// 目录 rights: 路径操作 + 目录枚举.
pub const RIGHTS_DIR: u64 = (1 << 1)
    | (1 << 2)
    | (1 << 4)
    | (1 << 5)
    | (1 << 7)
    | (1 << 9)
    | (1 << 10)
    | (1 << 11)
    | (1 << 12)
    | (1 << 13)
    | (1 << 14)
    | (1 << 15)
    | (1 << 16)
    | (1 << 17)
    | (1 << 18)
    | (1 << 19)
    | (1 << 20)
    | (1 << 21)
    | (1 << 24)
    | (1 << 25)
    | (1 << 26)
    | (1 << 27);

/// fd 表容量. WASI preview1 无上限, 取 128 防 guest 灌爆用户堆.
pub const FD_TABLE_CAP: usize = 128;

/// 一个 WASI fd 表项.
#[derive(Debug, Clone)]
pub struct WasiFdEntry {
    /// 宿主 fd; -1 表示无宿主后端 (preopen root).
    pub host_fd: i32,
    /// 绝对路径 (不含 NUL), 有宿主后端时用于 path 操作/`utimensat`.
    pub path: Option<Vec<u8>>,
    /// WASI filetype.
    pub file_type: u8,
    /// 基础 rights.
    pub rights_base: u64,
    /// 可继承 rights.
    pub rights_inheriting: u64,
    /// 是否 socket 后端 (无宿主 fd).
    pub socket: bool,
}

impl WasiFdEntry {
    /// 标准流表项.
    #[must_use]
    pub fn stdio(host_fd: i32) -> Self {
        Self {
            host_fd,
            path: None,
            file_type: FILETYPE_CHARACTER_DEVICE,
            rights_base: RIGHTS_STDIO,
            rights_inheriting: 0,
            socket: false,
        }
    }

    /// 根 preopen 表项.
    #[must_use]
    pub fn root() -> Self {
        Self {
            host_fd: -1,
            path: Some(alloc::vec![b'/']),
            file_type: FILETYPE_DIRECTORY,
            rights_base: RIGHTS_DIR,
            rights_inheriting: RIGHTS_ALL,
            socket: false,
        }
    }

    /// 普通文件/目录表项.
    #[must_use]
    pub fn file(host_fd: i32, path: Vec<u8>, file_type: u8) -> Self {
        let rights = if file_type == FILETYPE_DIRECTORY {
            RIGHTS_DIR
        } else {
            RIGHTS_FILE
        };
        Self {
            host_fd,
            path: Some(path),
            file_type,
            rights_base: rights,
            rights_inheriting: rights,
            socket: false,
        }
    }
}

/// fd 表.
#[derive(Debug)]
pub struct WasiFdTable {
    entries: Vec<Option<WasiFdEntry>>,
}

impl WasiFdTable {
    /// 建表并预置 0/1/2 标准流 + 3 根 preopen.
    #[must_use]
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(FD_TABLE_CAP);
        for _ in 0..FD_TABLE_CAP {
            entries.push(None);
        }
        entries[0] = Some(WasiFdEntry::stdio(0));
        entries[1] = Some(WasiFdEntry::stdio(1));
        entries[2] = Some(WasiFdEntry::stdio(2));
        entries[3] = Some(WasiFdEntry::root());
        Self { entries }
    }

    /// 取表项.
    #[must_use]
    pub fn get(&self, fd: u32) -> Option<&WasiFdEntry> {
        self.entries.get(fd as usize).and_then(Option::as_ref)
    }

    /// 取可变表项.
    pub fn get_mut(&mut self, fd: u32) -> Option<&mut WasiFdEntry> {
        self.entries.get_mut(fd as usize).and_then(Option::as_mut)
    }

    /// 分配最小空闲槽.
    ///
    /// # Errors
    /// 表满返回 `ERRNO_NOSPC`.
    pub fn alloc(&mut self, entry: WasiFdEntry) -> Result<u32, i32> {
        for (i, slot) in self.entries.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(entry);
                return Ok(i as u32);
            }
        }
        Err(ERRNO_NOSPC)
    }

    /// 关闭 fd 并释放宿主 fd.
    ///
    /// # Errors
    /// fd 越界或未分配返回 `ERRNO_BADF`.
    pub fn close(&mut self, fd: u32) -> Result<(), i32> {
        let Some(slot) = self.entries.get_mut(fd as usize) else {
            return Err(ERRNO_BADF);
        };
        let Some(entry) = slot.take() else {
            return Err(ERRNO_BADF);
        };
        if entry.host_fd >= 0 {
            let _ = close_host(entry.host_fd);
        }
        Ok(())
    }

    /// `fd_renumber`: 把 `from` 移到 `to`, 目标若存在先关闭.
    ///
    /// # Errors
    /// `from`/`to` 越界或 `from` 空返回 `ERRNO_BADF`.
    pub fn renumber(&mut self, from: u32, to: u32) -> Result<(), i32> {
        if from == to {
            return if self.get(from).is_some() {
                Ok(())
            } else {
                Err(ERRNO_BADF)
            };
        }
        if from as usize >= FD_TABLE_CAP || to as usize >= FD_TABLE_CAP {
            return Err(ERRNO_BADF);
        }
        let Some(entry) = self.entries[from as usize].take() else {
            return Err(ERRNO_BADF);
        };
        if let Some(old) = self.entries[to as usize].take() {
            if old.host_fd >= 0 {
                let _ = close_host(old.host_fd);
            }
        }
        self.entries[to as usize] = Some(entry);
        Ok(())
    }
}

impl Default for WasiFdTable {
    fn default() -> Self {
        Self::new()
    }
}

/// 运行时 WASI 上下文.
#[derive(Debug)]
pub struct WasiCtx {
    /// 参数 (含 argv[0] 模块路径).
    pub args: Vec<Vec<u8>>,
    /// 环境变量, 每项已是 `KEY=VALUE` 字节串.
    pub env: Vec<Vec<u8>>,
    /// fd 表.
    pub fds: WasiFdTable,
}

impl WasiCtx {
    /// 建上下文.
    #[must_use]
    pub fn new(args: Vec<Vec<u8>>, env: Vec<Vec<u8>>) -> Self {
        Self {
            args,
            env,
            fds: WasiFdTable::new(),
        }
    }

    /// 把 `dirfd` 下的相对路径解析成绝对路径 + NUL.
    ///
    /// 拒绝穿越到 preopen 之外 (`..` 上溯到根外) 与 NUL 字节.
    ///
    /// # Errors
    /// fd 不存在/不是目录/路径越界返回对应 WASI errno.
    pub fn resolve_path(&self, dirfd: u32, rel: &[u8]) -> Result<Vec<u8>, i32> {
        if rel.contains(&0) {
            return Err(ERRNO_INVAL);
        }
        let entry = self.fds.get(dirfd).ok_or(ERRNO_BADF)?;
        if entry.file_type != FILETYPE_DIRECTORY {
            return Err(ERRNO_NOTDIR);
        }
        let base = entry.path.as_deref().unwrap_or(b"/");
        normalize_path(base, rel)
    }
}

/// 归一化 `base` + `rel`; 返回带 NUL 的绝对路径.
///
/// # Errors
/// `..` 越过根或分量超长返回 `ERRNO_NOTCAPABLE` / `ERRNO_NAMETOOLONG`.
pub fn normalize_path(base: &[u8], rel: &[u8]) -> Result<Vec<u8>, i32> {
    if base.contains(&0) || rel.contains(&0) {
        return Err(ERRNO_INVAL);
    }
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for comp in base.split(|&b| b == b'/') {
        if !comp.is_empty() && comp != b"." {
            parts.push(comp.to_vec());
        }
    }
    for comp in rel.split(|&b| b == b'/') {
        if comp.is_empty() || comp == b"." {
            continue;
        }
        if comp == b".." {
            if parts.pop().is_none() {
                return Err(ERRNO_NOTCAPABLE);
            }
            continue;
        }
        if comp.len() > 255 {
            return Err(ERRNO_NAMETOOLONG);
        }
        parts.push(comp.to_vec());
    }
    let mut out = Vec::with_capacity(1 + parts.iter().map(Vec::len).sum::<usize>() + 1);
    out.push(b'/');
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(part);
    }
    if out.len() > 4095 {
        return Err(ERRNO_NAMETOOLONG);
    }
    out.push(0);
    Ok(out)
}

// ============================================================================
// 编码
// ============================================================================
/// 编码 WASI `fdstat` 结构 (24 字节).
#[must_use]
pub fn encode_fdstat(
    filetype: u8,
    flags: u16,
    rights_base: u64,
    rights_inheriting: u64,
) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0] = filetype;
    b[2..4].copy_from_slice(&flags.to_le_bytes());
    b[8..16].copy_from_slice(&rights_base.to_le_bytes());
    b[16..24].copy_from_slice(&rights_inheriting.to_le_bytes());
    b
}

/// 编码 WASI `prestat` 结构 (8 字节): tag=0(dir) + name_len.
#[must_use]
pub fn encode_prestat(name_len: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = 0; // __WASI_PREOPENTYPE_DIR
    b[4..8].copy_from_slice(&name_len.to_le_bytes());
    b
}

/// 编码 WASI `filestat` 结构 (64 字节).
#[must_use]
pub fn encode_filestat(
    dev: u64,
    ino: u64,
    filetype: u8,
    nlink: u64,
    size: u64,
    atim_ns: u64,
    mtim_ns: u64,
    ctim_ns: u64,
) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[0..8].copy_from_slice(&dev.to_le_bytes());
    b[8..16].copy_from_slice(&ino.to_le_bytes());
    b[16] = filetype;
    b[24..32].copy_from_slice(&nlink.to_le_bytes());
    b[32..40].copy_from_slice(&size.to_le_bytes());
    b[40..48].copy_from_slice(&atim_ns.to_le_bytes());
    b[48..56].copy_from_slice(&mtim_ns.to_le_bytes());
    b[56..64].copy_from_slice(&ctim_ns.to_le_bytes());
    b
}

/// 编码 WASI `dirent` (24 字节头 + 名字随尾).
#[must_use]
pub fn encode_dirent(next_cookie: u64, ino: u64, namlen: u32, filetype: u8, name: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(24 + name.len());
    b.extend_from_slice(&next_cookie.to_le_bytes());
    b.extend_from_slice(&ino.to_le_bytes());
    b.extend_from_slice(&namlen.to_le_bytes());
    b.push(filetype);
    b.extend_from_slice(&[0u8; 3]); // 对齐填充
    b.extend_from_slice(name);
    b
}

/// 从 Linux `struct stat` 取 WASI `filestat` (按目标架构布局).
///
/// # Errors
/// 缓冲区不足以容纳目标架构的 `struct stat` 时返回 `ERRNO_FAULT`.
pub fn filestat_from_linux(stat: &[u8]) -> Result<[u8; 64], i32> {
    #[cfg(target_arch = "x86_64")]
    {
        filestat_from_linux_x86_64(stat)
    }
    #[cfg(target_arch = "aarch64")]
    {
        filestat_from_linux_aarch64(stat)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = stat;
        Err(ERRNO_NOTSUP)
    }
}

/// x86_64 glibc `struct stat` (144 字节) -> WASI filestat.
///
/// # Errors
/// `stat.len() < 144` 返回 `ERRNO_FAULT`.
pub fn filestat_from_linux_x86_64(stat: &[u8]) -> Result<[u8; 64], i32> {
    if stat.len() < 144 {
        return Err(ERRNO_FAULT);
    }
    let dev = u64::from_le_bytes(stat[0..8].try_into().unwrap());
    let ino = u64::from_le_bytes(stat[8..16].try_into().unwrap());
    let nlink = u64::from_le_bytes(stat[16..24].try_into().unwrap());
    let mode = u32::from_le_bytes(stat[24..28].try_into().unwrap());
    let size = u64::from_le_bytes(stat[48..56].try_into().unwrap());
    let atim = read_timespec(stat, 72);
    let mtim = read_timespec(stat, 88);
    let ctim = read_timespec(stat, 104);
    Ok(encode_filestat(
        dev,
        ino,
        filetype_from_mode(mode),
        nlink,
        size,
        atim,
        mtim,
        ctim,
    ))
}

/// aarch64 glibc `struct stat` (128 字节) -> WASI filestat.
///
/// # Errors
/// `stat.len() < 128` 返回 `ERRNO_FAULT`.
pub fn filestat_from_linux_aarch64(stat: &[u8]) -> Result<[u8; 64], i32> {
    if stat.len() < 128 {
        return Err(ERRNO_FAULT);
    }
    let dev = u64::from_le_bytes(stat[0..8].try_into().unwrap());
    let ino = u64::from_le_bytes(stat[8..16].try_into().unwrap());
    let mode = u32::from_le_bytes(stat[16..20].try_into().unwrap());
    let nlink = u32::from_le_bytes(stat[20..24].try_into().unwrap());
    let size = u64::from_le_bytes(stat[48..56].try_into().unwrap());
    let atim = read_timespec(stat, 72);
    let mtim = read_timespec(stat, 88);
    let ctim = read_timespec(stat, 104);
    Ok(encode_filestat(
        dev,
        ino,
        filetype_from_mode(mode),
        u64::from(nlink),
        size,
        atim,
        mtim,
        ctim,
    ))
}

fn read_timespec(stat: &[u8], off: usize) -> u64 {
    let sec = i64::from_le_bytes(stat[off..off + 8].try_into().unwrap());
    let nsec = i64::from_le_bytes(stat[off + 8..off + 16].try_into().unwrap());
    (sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(nsec as u64)
}

/// Linux `st_mode` -> WASI filetype.
#[must_use]
pub fn filetype_from_mode(mode: u32) -> u8 {
    match mode & 0o170_000 {
        0o040_000 => FILETYPE_DIRECTORY,
        0o100_000 => FILETYPE_REGULAR_FILE,
        0o120_000 => FILETYPE_SYMBOLIC_LINK,
        0o020_000 => FILETYPE_CHARACTER_DEVICE,
        0o060_000 => FILETYPE_BLOCK_DEVICE,
        0o140_000 => FILETYPE_SOCKET_STREAM,
        _ => FILETYPE_UNKNOWN,
    }
}

/// 从 8 字节小端缓冲解码 WASI `iovec` (`buf_ptr`: u32, `buf_len`: u32).
#[must_use]
pub fn iovec_from_le_bytes(bytes: [u8; 8]) -> (u32, u32) {
    let ptr = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (ptr, len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn errno_matches_wasi_snapshot_preview1() {
        assert_eq!(ERRNO_SUCCESS, 0);
        assert_eq!(ERRNO_BADF, 8);
        assert_eq!(ERRNO_FAULT, 21);
        assert_eq!(ERRNO_IO, 29);
        assert_eq!(ERRNO_NOENT, 44);
        assert_eq!(ERRNO_NOSPC, 51);
        assert_eq!(ERRNO_NOTDIR, 54);
        assert_eq!(ERRNO_NOTSOCK, 57);
        assert_eq!(ERRNO_NOTCAPABLE, 76);
        assert_eq!(errno_from_linux(2), ERRNO_NOENT);
        assert_eq!(errno_from_linux(20), ERRNO_NOTDIR);
        assert_eq!(errno_from_linux(88), ERRNO_NOTSOCK);
        assert_eq!(errno_from_linux(9999), ERRNO_IO);
    }

    #[test]
    fn fdstat_layout_offsets() {
        let b = encode_fdstat(
            FILETYPE_CHARACTER_DEVICE,
            0x0102,
            0x1122_3344_5566_7788,
            0x99aa,
        );
        assert_eq!(b[0], 2);
        assert_eq!(b[1], 0);
        assert_eq!(&b[4..8], &[0, 0, 0, 0]);
        assert_eq!(&b[2..4], &[0x02, 0x01]);
        assert_eq!(&b[8..16], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&b[16..24], &0x99aau64.to_le_bytes());
    }

    #[test]
    fn prestat_layout() {
        let b = encode_prestat(1);
        assert_eq!(b[0], 0);
        assert_eq!(&b[4..8], &1u32.to_le_bytes());
    }

    #[test]
    fn dirent_layout_with_name() {
        let d = encode_dirent(7, 0xdead, 3, FILETYPE_REGULAR_FILE, b"foo");
        assert_eq!(d.len(), 27);
        assert_eq!(&d[0..8], &7u64.to_le_bytes());
        assert_eq!(&d[8..16], &0xdeadu64.to_le_bytes());
        assert_eq!(&d[16..20], &3u32.to_le_bytes());
        assert_eq!(d[20], FILETYPE_REGULAR_FILE);
        assert_eq!(&d[21..24], &[0, 0, 0]);
        assert_eq!(&d[24..27], b"foo");
    }

    #[test]
    fn x86_filestat_offsets() {
        let mut st = [0u8; 144];
        st[0..8].copy_from_slice(&9u64.to_le_bytes());
        st[8..16].copy_from_slice(&10u64.to_le_bytes());
        st[16..24].copy_from_slice(&2u64.to_le_bytes());
        st[24..28].copy_from_slice(&0o100_644u32.to_le_bytes());
        st[48..56].copy_from_slice(&1234u64.to_le_bytes());
        st[72..80].copy_from_slice(&1i64.to_le_bytes());
        st[80..88].copy_from_slice(&2i64.to_le_bytes());
        st[88..96].copy_from_slice(&3i64.to_le_bytes());
        st[96..104].copy_from_slice(&4i64.to_le_bytes());
        st[104..112].copy_from_slice(&5i64.to_le_bytes());
        st[112..120].copy_from_slice(&6i64.to_le_bytes());
        let f = filestat_from_linux_x86_64(&st).unwrap();
        assert_eq!(u64::from_le_bytes(f[0..8].try_into().unwrap()), 9);
        assert_eq!(u64::from_le_bytes(f[8..16].try_into().unwrap()), 10);
        assert_eq!(f[16], FILETYPE_REGULAR_FILE);
        assert_eq!(u64::from_le_bytes(f[24..32].try_into().unwrap()), 2);
        assert_eq!(u64::from_le_bytes(f[32..40].try_into().unwrap()), 1234);
        assert_eq!(
            u64::from_le_bytes(f[40..48].try_into().unwrap()),
            1_000_000_002
        );
        assert_eq!(
            u64::from_le_bytes(f[48..56].try_into().unwrap()),
            3_000_000_004
        );
        assert_eq!(
            u64::from_le_bytes(f[56..64].try_into().unwrap()),
            5_000_000_006
        );
    }

    #[test]
    fn aarch64_filestat_offsets() {
        let mut st = [0u8; 128];
        st[0..8].copy_from_slice(&9u64.to_le_bytes());
        st[8..16].copy_from_slice(&10u64.to_le_bytes());
        st[16..20].copy_from_slice(&0o040_755u32.to_le_bytes());
        st[20..24].copy_from_slice(&3u32.to_le_bytes());
        st[48..56].copy_from_slice(&42u64.to_le_bytes());
        let f = filestat_from_linux_aarch64(&st).unwrap();
        assert_eq!(f[16], FILETYPE_DIRECTORY);
        assert_eq!(u64::from_le_bytes(f[24..32].try_into().unwrap()), 3);
        assert_eq!(u64::from_le_bytes(f[32..40].try_into().unwrap()), 42);
    }

    #[test]
    fn fd_table_preopen_and_alloc() {
        let mut t = WasiFdTable::new();
        assert_eq!(t.get(0).unwrap().file_type, FILETYPE_CHARACTER_DEVICE);
        assert_eq!(t.get(3).unwrap().file_type, FILETYPE_DIRECTORY);
        assert_eq!(t.get(3).unwrap().path.as_deref(), Some(b"/".as_slice()));
        let new = t.alloc(WasiFdEntry::file(7, vec![b'/'], FILETYPE_DIRECTORY)).unwrap();
        assert_eq!(new, 4);
        assert!(t.get(4).is_some());
        t.close(4).unwrap();
        assert!(t.get(4).is_none());
    }

    #[test]
    fn fd_renumber_moves_entry() {
        let mut t = WasiFdTable::new();
        let a = t.alloc(WasiFdEntry::file(9, vec![b'/'], FILETYPE_REGULAR_FILE)).unwrap();
        t.renumber(a, 10).unwrap();
        assert!(t.get(a).is_none());
        assert_eq!(t.get(10).unwrap().host_fd, 9);
    }

    #[test]
    fn resolve_path_root_and_traversal() {
        let ctx = WasiCtx::new(Vec::new(), Vec::new());
        assert_eq!(ctx.resolve_path(3, b"foo").unwrap(), b"/foo\0");
        assert_eq!(ctx.resolve_path(3, b"./a/../b").unwrap(), b"/b\0");
        assert_eq!(ctx.resolve_path(3, b"..").unwrap_err(), ERRNO_NOTCAPABLE);
        assert_eq!(ctx.resolve_path(9, b"foo").unwrap_err(), ERRNO_BADF);
        assert_eq!(ctx.resolve_path(0, b"foo").unwrap_err(), ERRNO_NOTDIR);
        assert_eq!(ctx.resolve_path(3, b"a\0b").unwrap_err(), ERRNO_INVAL);
        assert_eq!(ctx.resolve_path(3, b"/abs").unwrap(), b"/abs\0");
    }

    #[test]
    fn normalize_subpreopen_escape() {
        assert_eq!(normalize_path(b"/tmp", b"a").unwrap(), b"/tmp/a\0");
        assert_eq!(normalize_path(b"/tmp", b"../etc").unwrap(), b"/etc\0");
        assert_eq!(normalize_path(b"/tmp", b"../../etc").unwrap_err(), ERRNO_NOTCAPABLE);
    }
}
