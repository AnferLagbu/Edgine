//! POSIX Errno — privileged 层完整定义
//!
//! ## 迁移记录 (B09-12/DECISION-H13 P0-1)
//!
//! Errno 是内核通用错误类型, 不应属于 syscall 子系统.
//! 原定义在 `functions::syscall::types`, privileged 经本文件 re-export 引用,
//! 形成 privileged→functions 反向依赖. 现将定义整体迁回 privileged,
//! `functions::syscall::types` 改 re-export 本文件保持调用方兼容.
//!
//! 迁移内容 (0 语义变更, 纯搬移):
//! - enum Errno (原 functions/syscall/types.rs)
//! - impl Errno::{as_i32, as_ret, from_ret, try_from_i32} (原 functions/syscall/types.rs + mod.rs)
//! - impl Display for Errno (原 functions/syscall/types.rs)
//! - fn errno_from_i64 (原 functions/syscall/mod.rs)

// POSIX errno 命名约定 (EAGAIN/EACCES/...) — 全大写缩写是有意的
#![allow(clippy::upper_case_acronyms)]

/// POSIX errno (使用 Linux 风格: 返回值 = -errno)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Errno {
    EPERM = 1,
    ENOENT = 2,
    ESRCH = 3,
    EINTR = 4,
    EIO = 5,
    ENXIO = 6,
    E2BIG = 7,
    ENOEXEC = 8,
    EBADF = 9,
    ECHILD = 10,
    EAGAIN = 11,
    ENOMEM = 12,
    EACCES = 13,
    EFAULT = 14,
    ENOTBLK = 15,
    EBUSY = 16,
    EEXIST = 17,
    EXDEV = 18,
    ENODEV = 19,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    ENFILE = 23,
    EMFILE = 24,
    ENOTTY = 25,
    ETXTBSY = 26,
    EFBIG = 27,
    ENOSPC = 28,
    ESPIPE = 29,
    EROFS = 30,
    EMLINK = 31,
    EPIPE = 32,
    EDOM = 33,
    ERANGE = 34,
    EDEADLK = 35,
    ENAMETOOLONG = 36,
    ENOLCK = 37,
    ENOSYS = 38,
    ENOTEMPTY = 39,
    ELOOP = 40,
    EWOULDBLOCK = 41,
    ENOMSG = 42,
    EIDRM = 43,
    ENOSTR = 60,
    ENODATA = 61,
    ETIME = 62,
    ENOSR = 63,
    ENONET = 64,
    EPROTO = 71,
    EBADMSG = 74,
    EOVERFLOW = 75,
    ENOTSOCK = 88,
    EDESTADDRREQ = 89,
    EMSGSIZE = 90,
    EPROTOTYPE = 91,
    ENOPROTOOPT = 92,
    EPROTONOSUPPORT = 93,
    ESOCKTNOSUPPORT = 94,
    ENOTSUP = 95,
    EPFNOSUPPORT = 96,
    EAFNOSUPPORT = 97,
    EADDRINUSE = 98,
    EADDRNOTAVAIL = 99,
    ENETDOWN = 100,
    ENETUNREACH = 101,
    ENETRESET = 102,
    ECONNABORTED = 103,
    ECONNRESET = 104,
    ENOBUFS = 105,
    EISCONN = 106,
    ENOTCONN = 107,
    ESHUTDOWN = 108,
    ETIMEDOUT = 110,
    ECONNREFUSED = 111,
    EHOSTDOWN = 112,
    EHOSTUNREACH = 113,
    EALREADY = 114,
    EINPROGRESS = 115,
}

impl Errno {
    /// 返回 POSIX errno 数值 (正整数)
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// 返回 POSIX 负返回码 (`-errno`, syscall 返回值约定)
    pub const fn as_ret(self) -> i64 {
        -(self as i64)
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 从负返回值恢复 Errno
    ///
    /// 输入: privileged 层返回的负错误码 (如 -ENOMEM)
    /// 输出: 对应的 Errno 枚举值
    ///
    /// # Errors
    ///
    /// 未知/未定义错误码回退为 `EINVAL`.
    pub fn from_ret(ret: i64) -> Self {
        let errno = (-ret) as u64;
        match errno {
            1 => Self::EPERM,
            2 => Self::ENOENT,
            3 => Self::ESRCH,
            4 => Self::EINTR,
            5 => Self::EIO,
            6 => Self::ENXIO,
            7 => Self::E2BIG,
            8 => Self::ENOEXEC,
            9 => Self::EBADF,
            10 => Self::ECHILD,
            11 => Self::EAGAIN,
            12 => Self::ENOMEM,
            13 => Self::EACCES,
            14 => Self::EFAULT,
            15 => Self::ENOTBLK,
            16 => Self::EBUSY,
            17 => Self::EEXIST,
            18 => Self::EXDEV,
            19 => Self::ENODEV,
            20 => Self::ENOTDIR,
            21 => Self::EISDIR,
            22 => Self::EINVAL,
            23 => Self::ENFILE,
            24 => Self::EMFILE,
            25 => Self::ENOTTY,
            26 => Self::ETXTBSY,
            27 => Self::EFBIG,
            28 => Self::ENOSPC,
            29 => Self::ESPIPE,
            30 => Self::EROFS,
            31 => Self::EMLINK,
            32 => Self::EPIPE,
            33 => Self::EDOM,
            34 => Self::ERANGE,
            35 => Self::EDEADLK,
            36 => Self::ENAMETOOLONG,
            37 => Self::ENOLCK,
            38 => Self::ENOSYS,
            39 => Self::ENOTEMPTY,
            40 => Self::ELOOP,
            41 => Self::EWOULDBLOCK,
            42 => Self::ENOMSG,
            43 => Self::EIDRM,
            60 => Self::ENOSTR,
            61 => Self::ENODATA,
            62 => Self::ETIME,
            63 => Self::ENOSR,
            64 => Self::ENONET,
            71 => Self::EPROTO,
            74 => Self::EBADMSG,
            75 => Self::EOVERFLOW,
            88 => Self::ENOTSOCK,
            89 => Self::EDESTADDRREQ,
            90 => Self::EMSGSIZE,
            91 => Self::EPROTOTYPE,
            92 => Self::ENOPROTOOPT,
            93 => Self::EPROTONOSUPPORT,
            94 => Self::ESOCKTNOSUPPORT,
            95 => Self::ENOTSUP,
            96 => Self::EPFNOSUPPORT,
            97 => Self::EAFNOSUPPORT,
            98 => Self::EADDRINUSE,
            99 => Self::EADDRNOTAVAIL,
            100 => Self::ENETDOWN,
            101 => Self::ENETUNREACH,
            102 => Self::ENETRESET,
            103 => Self::ECONNABORTED,
            104 => Self::ECONNRESET,
            105 => Self::ENOBUFS,
            106 => Self::EISCONN,
            107 => Self::ENOTCONN,
            108 => Self::ESHUTDOWN,
            110 => Self::ETIMEDOUT,
            111 => Self::ECONNREFUSED,
            112 => Self::EHOSTDOWN,
            113 => Self::EHOSTUNREACH,
            114 => Self::EALREADY,
            115 => Self::EINPROGRESS,
            _ => Self::EINVAL, // 未知错误码回退到 EINVAL
        }
    }

    /// 从 i32 错误码构造 `Errno` (POSIX 反向: 整数值 = errno 编号)
    pub fn try_from_i32(code: i32) -> Option<Self> {
        // Errno 编号范围 1..=133 (POSIX 范围)
        // 我们通过 #[repr(i32)] 枚举, 直接 transmute 不安全;
        // 用 match 列举所有已知变体
        Some(match code {
            1 => Self::EPERM,
            2 => Self::ENOENT,
            3 => Self::ESRCH,
            4 => Self::EINTR,
            5 => Self::EIO,
            6 => Self::ENXIO,
            7 => Self::E2BIG,
            8 => Self::ENOEXEC,
            9 => Self::EBADF,
            10 => Self::ECHILD,
            11 => Self::EAGAIN,
            12 => Self::ENOMEM,
            13 => Self::EACCES,
            14 => Self::EFAULT,
            15 => Self::ENOTBLK,
            16 => Self::EBUSY,
            17 => Self::EEXIST,
            18 => Self::EXDEV,
            19 => Self::ENODEV,
            20 => Self::ENOTDIR,
            21 => Self::EISDIR,
            22 => Self::EINVAL,
            23 => Self::ENFILE,
            // 大于 23 的暂不识别, 返回 None
            _ => return None,
        })
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EPERM => write!(f, "Operation not permitted"),
            Self::ENOENT => write!(f, "No such file or directory"),
            Self::ESRCH => write!(f, "No such process"),
            Self::EINTR => write!(f, "Interrupted system call"),
            Self::EIO => write!(f, "I/O error"),
            Self::ENOEXEC => write!(f, "Exec format error"),
            Self::EBADF => write!(f, "Bad file descriptor"),
            Self::ECHILD => write!(f, "No child processes"),
            Self::EAGAIN => write!(f, "Resource temporarily unavailable"),
            Self::ENOMEM => write!(f, "Cannot allocate memory"),
            Self::EACCES => write!(f, "Permission denied"),
            Self::EFAULT => write!(f, "Bad address"),
            Self::EBUSY => write!(f, "Device or resource busy"),
            Self::EEXIST => write!(f, "File exists"),
            Self::ENOTDIR => write!(f, "Not a directory"),
            Self::EISDIR => write!(f, "Is a directory"),
            Self::EINVAL => write!(f, "Invalid argument"),
            Self::ENOSPC => write!(f, "No space left on device"),
            Self::EROFS => write!(f, "Read-only file system"),
            Self::ERANGE => write!(f, "Result too large"),
            Self::ENAMETOOLONG => write!(f, "File name too long"),
            Self::ENOTEMPTY => write!(f, "Directory not empty"),
            Self::ENOSYS => write!(f, "Function not implemented"),
            _ => write!(f, "Error {}", -(*self as i64)),
        }
    }
}

/// i64 → `Errno` (POSIX: 负数 = -errno)
pub fn errno_from_i64(rc: i64) -> Option<Errno> {
    if rc < 0 {
        Errno::try_from_i32(-rc as i32)
    } else {
        None
    }
}
