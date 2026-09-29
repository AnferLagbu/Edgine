//! KernelError — framework 层完整定义 (全内核统一错误类型)
//!
//! ## 迁移记录 (B09-12/DECISION-H13 P0-2)
//!
//! KernelError 是全内核跨子系统共享的统一错误类型 (POSIX errno → 强类型),
//! 原定义在 `services::error`, 依赖 `framework::errno::Errno` (P0-1 已迁回),
//! 形成 framework→services 反向依赖. 现将定义整体迁回 framework,
//! `services::error` 改 re-export 本文件保持调用方兼容.
//!
//! 迁移内容 (0 语义变更, 纯搬移):
//! - enum KernelError (原 services/error.rs, 26 变体)
//! - impl Display / 向后兼容别名 / from_i32 / as_errno / as_vfs_ret / From

use crate::framework::errno::Errno;

/// 全内核统一错误 (POSIX errno → 强类型).
///
/// 跨子系统共享: 任何 `Result<T, KernelError>` 都可直接 `?` 传播.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelError {
    /// 权限不足 (EPERM=1)
    PermissionDenied,
    /// I/O 错误 (EIO=5)
    Io,
    /// 文件/路径不存在 (ENOENT=2)
    FileNotFound,
    /// 资源/进程不存在 (ESRCH=3)
    NoSuchProcess,
    /// 文件描述符无效 (EBADF=9)
    BadFd,
    /// 操作会阻塞 (EAGAIN=11)
    WouldBlock,
    /// 内存不足 (ENOMEM=12)
    NoMemory,
    /// 错误地址 (EFAULT=14)
    Fault,
    /// 设备不存在 (ENODEV=19)
    NoDevice,
    /// 无效参数 (EINVAL=22)
    InvalidArgument,
    /// 进程打开文件过多 (EMFILE=23)
    ProcessFileLimit,
    /// 操作不支持 (EOPNOTSUPP/ENOTSUP=95)
    NotSupported,
    /// 地址族不支持 (EAFNOSUPPORT=97)
    AddrFamilyNotSupported,
    /// 地址已被使用 (EADDRINUSE=98)
    AddrInUse,
    /// 地址不可用 (EADDRNOTAVAIL=99)
    AddrNotAvailable,
    /// 连接被重置 (ECONNRESET=104)
    ConnectionReset,
    /// 未连接 (ENOTCONN=107)
    NotConnected,
    /// 连接被拒绝 (ECONNREFUSED=111)
    ConnectionRefused,
    /// 网络未初始化 (自定义, 非 POSIX)
    NotReady,
    /// 文件已存在 (EEXIST=17)
    AlreadyExists,
    /// 设备或资源忙 (EBUSY=16)
    Busy,
    /// 不是目录 (ENOTDIR=20)
    NotADirectory,
    /// 是目录 (EISDIR=21)
    IsDirectory,
    /// 文件系统只读 (EROFS=30)
    ReadOnlyFilesystem,
    /// 文件名过长 (ENAMETOOLONG=36)
    NameTooLong,
    /// 设备空间不足 (ENOSPC=28)
    NoSpace,
    /// 跨设备链接 (EXDEV=18)
    CrossDevice,
    /// 子系统未初始化 (自定义, 非 POSIX)
    NotInitialized,
    /// 数值溢出 (自定义, 非 POSIX)
    Overflow,
    /// 其他未分类
    Other(i32),
}

impl core::fmt::Display for KernelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PermissionDenied => write!(f, "权限不足"),
            Self::Io => write!(f, "I/O 错误"),
            Self::FileNotFound => write!(f, "文件不存在"),
            Self::NoSuchProcess => write!(f, "进程不存在"),
            Self::BadFd => write!(f, "文件描述符无效"),
            Self::WouldBlock => write!(f, "操作会阻塞"),
            Self::NoMemory => write!(f, "内存不足"),
            Self::Fault => write!(f, "错误地址"),
            Self::NoDevice => write!(f, "设备不存在"),
            Self::InvalidArgument => write!(f, "无效参数"),
            Self::ProcessFileLimit => write!(f, "进程打开文件过多"),
            Self::NotSupported => write!(f, "操作不支持"),
            Self::AddrFamilyNotSupported => write!(f, "地址族不支持"),
            Self::AddrInUse => write!(f, "地址已被使用"),
            Self::AddrNotAvailable => write!(f, "地址不可用"),
            Self::ConnectionReset => write!(f, "连接被重置"),
            Self::NotConnected => write!(f, "未连接"),
            Self::ConnectionRefused => write!(f, "连接被拒绝"),
            Self::NotReady => write!(f, "未就绪"),
            Self::AlreadyExists => write!(f, "已存在"),
            Self::Busy => write!(f, "资源忙"),
            Self::NotADirectory => write!(f, "不是目录"),
            Self::IsDirectory => write!(f, "是目录"),
            Self::ReadOnlyFilesystem => write!(f, "文件系统只读"),
            Self::NameTooLong => write!(f, "文件名过长"),
            Self::NoSpace => write!(f, "空间不足"),
            Self::CrossDevice => write!(f, "跨设备链接"),
            Self::NotInitialized => write!(f, "未初始化"),
            Self::Overflow => write!(f, "数值溢出"),
            Self::Other(code) => write!(f, "错误码 {code}"),
        }
    }
}

impl KernelError {
    /// POSIX errno 强类型映射.
    pub const fn from_i32(rc: i32) -> Self {
        match rc {
            1 => Self::PermissionDenied,
            5 => Self::Io,
            2 => Self::FileNotFound,
            3 => Self::NoSuchProcess,
            9 => Self::BadFd,
            11 => Self::WouldBlock,
            12 => Self::NoMemory,
            14 => Self::Fault,
            16 => Self::Busy,
            17 => Self::AlreadyExists,
            18 => Self::CrossDevice,
            19 => Self::NoDevice,
            20 => Self::NotADirectory,
            21 => Self::IsDirectory,
            22 => Self::InvalidArgument,
            23 => Self::ProcessFileLimit,
            28 => Self::NoSpace,
            30 => Self::ReadOnlyFilesystem,
            36 => Self::NameTooLong,
            95 => Self::NotSupported,
            97 => Self::AddrFamilyNotSupported,
            98 => Self::AddrInUse,
            99 => Self::AddrNotAvailable,
            104 => Self::ConnectionReset,
            107 => Self::NotConnected,
            111 => Self::ConnectionRefused,
            _ => Self::Other(rc),
        }
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// 反向映射: 强类型 → POSIX errno.
    pub const fn as_errno(self) -> Errno {
        match self {
            Self::PermissionDenied => Errno::EPERM,
            Self::Io => Errno::EIO,
            Self::FileNotFound => Errno::ENOENT,
            Self::NoSuchProcess => Errno::ESRCH,
            Self::BadFd => Errno::EBADF,
            Self::WouldBlock => Errno::EAGAIN,
            Self::NoMemory => Errno::ENOMEM,
            Self::Fault => Errno::EFAULT,
            Self::Busy => Errno::EBUSY,
            Self::AlreadyExists => Errno::EEXIST,
            Self::CrossDevice => Errno::EXDEV,
            Self::NoDevice => Errno::ENODEV,
            Self::NotADirectory => Errno::ENOTDIR,
            Self::IsDirectory => Errno::EISDIR,
            Self::InvalidArgument => Errno::EINVAL,
            Self::ProcessFileLimit => Errno::EMFILE,
            Self::NoSpace => Errno::ENOSPC,
            Self::ReadOnlyFilesystem => Errno::EROFS,
            Self::NameTooLong => Errno::ENAMETOOLONG,
            Self::NotSupported => Errno::ENOTSUP,
            Self::AddrFamilyNotSupported => Errno::EAFNOSUPPORT,
            Self::AddrInUse => Errno::EADDRINUSE,
            Self::AddrNotAvailable => Errno::EADDRNOTAVAIL,
            Self::ConnectionReset => Errno::ECONNRESET,
            Self::NotConnected => Errno::ENOTCONN,
            Self::ConnectionRefused => Errno::ECONNREFUSED,
            Self::NotReady => Errno::EAGAIN,
            Self::NotInitialized => Errno::EINVAL,
            Self::Overflow => Errno::EINVAL,
            Self::Other(_) => Errno::EINVAL,
        }
    }
}

impl From<i32> for KernelError {
    fn from(rc: i32) -> Self {
        Self::from_i32(rc)
    }
}

impl From<Errno> for KernelError {
    fn from(e: Errno) -> Self {
        Self::from_i32(e.as_i32())
    }
}

impl KernelError {
    /// VFS 风格返回: 负 errno (POSIX `-|errno|` 约定)
    ///
    /// 用于替换裸 `return -1`, 让 VFS/syscall 边界传递的"错误"携带明确语义.
    /// 调用方可在 syscall handler 中通过 `-ret` 还原 errno.
    pub const fn as_vfs_ret(self) -> i32 {
        -(self.as_errno().as_i32())
    }
}

/// 全内核统一 Result 别名 (默认错误类型为 `KernelError`).
///
/// 阶段 4b 由 `services::fs::vfs_types` 迁回 framework: 本别名被 framework
/// 生产代码消费 (`driver/block.rs` 的 `KernelResult<()>`), 定义必须留在
/// framework, services 侧经 re-export 保持既有路径兼容.
pub type KernelResult<T> = Result<T, KernelError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_common_errnos() {
        for raw in [1, 9, 11, 12, 14, 22, 95, 97, 98, 99, 104, 107, 111] {
            let e = KernelError::from_i32(raw);
            // 反向映射经 as_errno() (无 From<KernelError> for i32 实现).
            let back = e.as_errno().as_i32();
            assert_eq!(back, raw, "round-trip failed for {raw}");
        }
    }

    #[test]
    fn other_preserves_raw() {
        let e = KernelError::from_i32(12345);
        assert_eq!(e, KernelError::Other(12345));
        let raw = e.as_errno().as_i32();
        assert_eq!(raw, 22 /*EINVAL fallback*/);
    }
}
