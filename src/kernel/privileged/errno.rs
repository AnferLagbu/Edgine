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
// P-59 裁定 ①: 本模块 F8 存量已清零, 转由编译器硬执法 (无基线、无豁免).
// 新增任何未文档的公共项会让门槛 1/2 直接编译失败, 而不是被债册记账.
#![deny(missing_docs)]

/// POSIX errno (使用 Linux 风格: 返回值 = -errno)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Errno {
    /// 操作不被允许 — 调用方不具备执行该操作所需的权限
    EPERM = 1,
    /// 目标不存在 — 路径分量或进程/资源查无此项
    ENOENT = 2,
    /// 进程不存在 — 给定的 pid 无对应进程或不可观测
    ESRCH = 3,
    /// 系统调用被中断 — 可重试, 通常由信号触发
    EINTR = 4,
    /// 底层 I/O 错误 — 设备或介质层面的失败
    EIO = 5,
    /// 无此设备或地址越界 — 打开设备号时对应槽位为空
    ENXIO = 6,
    /// 参数列表/单参过大 — 超出内核可传递的上限
    E2BIG = 7,
    /// 可执行文件格式错误 — 镜像魔数或架构不符
    ENOEXEC = 8,
    /// 文件描述符无效 — fd 越界、未开或已关
    EBADF = 9,
    /// 无子进程 — 等待的进程集合为空
    ECHILD = 10,
    /// 资源暂时不可用 — 非阻塞模式下操作会阻塞, 可稍后重试
    EAGAIN = 11,
    /// 内存不足 — 内核或地址空间无法完成分配
    ENOMEM = 12,
    /// 权限被拒 — 文件模式/属主检查不通过
    EACCES = 13,
    /// 地址无效 — 用户指针不在其可访问映射内 (I4 代理拒绝)
    EFAULT = 14,
    /// 需要块设备 — 请求的句柄不是块设备
    ENOTBLK = 15,
    /// 设备或资源忙 — 存在无法让出的占用者
    EBUSY = 16,
    /// 目标已存在 — 与"不覆盖"语义冲突
    EEXIST = 17,
    /// 跨设备操作 — 链接/重命名落在不同文件系统
    EXDEV = 18,
    /// 设备不存在 — 驱动未注册或设备已消失
    ENODEV = 19,
    /// 不是目录 — 路径中某分量类型不符
    ENOTDIR = 20,
    /// 是目录 — 对目录执行了只适用于文件的操作
    EISDIR = 21,
    /// 参数无效 — 值域/组合不合法 (亦作未知错误码的回退值)
    EINVAL = 22,
    /// 系统文件表已满 — 全局打开文件数达上限
    ENFILE = 23,
    /// 进程文件描述符表已满 — fd 资源达上限
    EMFILE = 24,
    /// 不是终端设备 — ioctl 作用在不支持它的对象上
    ENOTTY = 25,
    /// 可执行镜像正忙 — 正在运行的程序被写或被执行
    ETXTBSY = 26,
    /// 文件过大 — 超出文件系统或进程允许的单文件上限
    EFBIG = 27,
    /// 设备无剩余空间 — 分配块耗尽
    ENOSPC = 28,
    /// 非法 seek — 对象是管道等不可定位的流
    ESPIPE = 29,
    /// 只读文件系统 — 写操作被挂载模式拒绝
    EROFS = 30,
    /// 硬链接数超限 — 目录项链接计数达上限
    EMLINK = 31,
    /// 管道破裂 — 读端已关闭仍继续写
    EPIPE = 32,
    /// 数学函数定义域错误 — 实参不在函数定义域内
    EDOM = 33,
    /// 结果超出可表示范围 — 数值溢出/下溢
    ERANGE = 34,
    /// 会形成死锁 — 锁序检查 (lockdep) 判定为环路
    EDEADLK = 35,
    /// 名字过长 — 单分量或整路径超过上限
    ENAMETOOLONG = 36,
    /// 无可用锁资源 — 文件锁表耗尽
    ENOLCK = 37,
    /// 功能未实现 — 该 syscall/分支尚未实装
    ENOSYS = 38,
    /// 目录非空 — 无法删除仍含条目的目录
    ENOTEMPTY = 39,
    /// 符号链接层数过多 — 疑似环路或超过深度上限
    ELOOP = 40,
    /// 操作会阻塞 — 非阻塞套接字/I/O 的即时失败
    EWOULDBLOCK = 41,
    /// 无消息 — 消息队列空或无匹配消息
    ENOMSG = 42,
    /// 标识符已被移除 — IPC 对象在调用期间被删除
    EIDRM = 43,
    /// 不是 STREAM 设备 — 对非 STREAM 句柄执行 STREAM 操作
    ENOSTR = 60,
    /// 流上无数据 — 读端在超时窗口内没有可读消息
    ENODATA = 61,
    /// STREAM 操作超时 — 底层流控等待超过时限
    ETIME = 62,
    /// 无 STREAM 资源 — 流缓冲区分配失败
    ENOSR = 63,
    /// 主机不在网络中 — 网络层未启用或无可用网络
    ENONET = 64,
    /// 协议错误 — 收到的报文不符合协议
    EPROTO = 71,
    /// 消息损坏 — 内部数据校验失败
    EBADMSG = 74,
    /// 值超出接口可表示范围 — 类型宽度不足以承载结果
    EOVERFLOW = 75,
    /// 不是套接字 — 对普通文件句柄执行 socket 操作
    ENOTSOCK = 88,
    /// 需要目标地址 — 未连接且未给出目的地址
    EDESTADDRREQ = 89,
    /// 消息过长 — 超过套接字或协议的数据上限
    EMSGSIZE = 90,
    /// 协议类型不符 — 与套接字类型不匹配
    EPROTOTYPE = 91,
    /// 协议选项不可用 — 请求的选项不存在或不受支持
    ENOPROTOOPT = 92,
    /// 协议不受支持 — 该协议号未注册
    EPROTONOSUPPORT = 93,
    /// 套接字类型不受支持 — 该类型在本协议族下不可用
    ESOCKTNOSUPPORT = 94,
    /// 操作不受支持 — 对象拒绝该操作 (含 OPNOTSUPP 同值语义)
    ENOTSUP = 95,
    /// 协议族不受支持 — 该地址族未注册
    EPFNOSUPPORT = 96,
    /// 地址族不受支持 — 地址结构属于未知族
    EAFNOSUPPORT = 97,
    /// 地址已被占用 — 同一传输端点已有绑定
    EADDRINUSE = 98,
    /// 地址不可用 — 请求的地址不属于本机
    EADDRNOTAVAIL = 99,
    /// 网络已断开 — 所连网络接口 down
    ENETDOWN = 100,
    /// 网络不可达 — 无到目标网络的路由
    ENETUNREACH = 101,
    /// 连接因网络重置中断 — 协议栈主动废弃了连接
    ENETRESET = 102,
    /// 连接被中止 — 本端条件不满足 (含监听队列超时)
    ECONNABORTED = 103,
    /// 连接被对端重置 — 收到 RST 类终止
    ECONNRESET = 104,
    /// 无缓冲区空间 — 网络缓冲池耗尽
    ENOBUFS = 105,
    /// 已经连接 — 面向连接的套接字重复发起连接
    EISCONN = 106,
    /// 尚未连接 — 需要连接的调用在无连接状态下发起
    ENOTCONN = 107,
    /// 传输端点已关闭 — 关机后仍尝试发送
    ESHUTDOWN = 108,
    /// 操作超时 — 重传或等待超过时限且无响应
    ETIMEDOUT = 110,
    /// 连接被拒绝 — 对端未监听该端口
    ECONNREFUSED = 111,
    /// 主机已关机 — 路由已知但对端不可达且已下线
    EHOSTDOWN = 112,
    /// 主机不可达 — 无到目标主机的路由
    EHOSTUNREACH = 113,
    /// 操作已在进行中 — 重复发起异步连接
    EALREADY = 114,
    /// 操作正在进行中 — 非阻塞连接已受理未完成
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
