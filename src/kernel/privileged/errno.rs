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

// P-59 裁定 ①: 本模块 F8 存量已清零, 转由编译器硬执法 (无基线、无豁免).
// 新增任何未文档的公共项会让门槛 1/2 直接编译失败, 而不是被债册记账.
#![deny(missing_docs)]

/// POSIX errno (使用 Linux 风格: 返回值 = -errno)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Errno {
    /// 操作不被允许 — 调用方不具备执行该操作所需的权限
    Eperm = 1,
    /// 目标不存在 — 路径分量或进程/资源查无此项
    Enoent = 2,
    /// 进程不存在 — 给定的 pid 无对应进程或不可观测
    Esrch = 3,
    /// 系统调用被中断 — 可重试, 通常由信号触发
    Eintr = 4,
    /// 底层 I/O 错误 — 设备或介质层面的失败
    Eio = 5,
    /// 无此设备或地址越界 — 打开设备号时对应槽位为空
    Enxio = 6,
    /// 参数列表/单参过大 — 超出内核可传递的上限
    E2BIG = 7,
    /// 可执行文件格式错误 — 镜像魔数或架构不符
    Enoexec = 8,
    /// 文件描述符无效 — fd 越界、未开或已关
    Ebadf = 9,
    /// 无子进程 — 等待的进程集合为空
    Echild = 10,
    /// 资源暂时不可用 — 非阻塞模式下操作会阻塞, 可稍后重试
    Eagain = 11,
    /// 内存不足 — 内核或地址空间无法完成分配
    Enomem = 12,
    /// 权限被拒 — 文件模式/属主检查不通过
    Eacces = 13,
    /// 地址无效 — 用户指针不在其可访问映射内 (I4 代理拒绝)
    Efault = 14,
    /// 需要块设备 — 请求的句柄不是块设备
    Enotblk = 15,
    /// 设备或资源忙 — 存在无法让出的占用者
    Ebusy = 16,
    /// 目标已存在 — 与"不覆盖"语义冲突
    Eexist = 17,
    /// 跨设备操作 — 链接/重命名落在不同文件系统
    Exdev = 18,
    /// 设备不存在 — 驱动未注册或设备已消失
    Enodev = 19,
    /// 不是目录 — 路径中某分量类型不符
    Enotdir = 20,
    /// 是目录 — 对目录执行了只适用于文件的操作
    Eisdir = 21,
    /// 参数无效 — 值域/组合不合法 (亦作未知错误码的回退值)
    Einval = 22,
    /// 系统文件表已满 — 全局打开文件数达上限
    Enfile = 23,
    /// 进程文件描述符表已满 — fd 资源达上限
    Emfile = 24,
    /// 不是终端设备 — ioctl 作用在不支持它的对象上
    Enotty = 25,
    /// 可执行镜像正忙 — 正在运行的程序被写或被执行
    Etxtbsy = 26,
    /// 文件过大 — 超出文件系统或进程允许的单文件上限
    Efbig = 27,
    /// 设备无剩余空间 — 分配块耗尽
    Enospc = 28,
    /// 非法 seek — 对象是管道等不可定位的流
    Espipe = 29,
    /// 只读文件系统 — 写操作被挂载模式拒绝
    Erofs = 30,
    /// 硬链接数超限 — 目录项链接计数达上限
    Emlink = 31,
    /// 管道破裂 — 读端已关闭仍继续写
    Epipe = 32,
    /// 数学函数定义域错误 — 实参不在函数定义域内
    Edom = 33,
    /// 结果超出可表示范围 — 数值溢出/下溢
    Erange = 34,
    /// 会形成死锁 — 锁序检查 (lockdep) 判定为环路
    Edeadlk = 35,
    /// 名字过长 — 单分量或整路径超过上限
    Enametoolong = 36,
    /// 无可用锁资源 — 文件锁表耗尽
    Enolck = 37,
    /// 功能未实现 — 该 syscall/分支尚未实装
    Enosys = 38,
    /// 目录非空 — 无法删除仍含条目的目录
    Enotempty = 39,
    /// 符号链接层数过多 — 疑似环路或超过深度上限
    Eloop = 40,
    /// 操作会阻塞 — 非阻塞套接字/I/O 的即时失败
    Ewouldblock = 41,
    /// 无消息 — 消息队列空或无匹配消息
    Enomsg = 42,
    /// 标识符已被移除 — IPC 对象在调用期间被删除
    Eidrm = 43,
    /// 不是 STREAM 设备 — 对非 STREAM 句柄执行 STREAM 操作
    Enostr = 60,
    /// 流上无数据 — 读端在超时窗口内没有可读消息
    Enodata = 61,
    /// STREAM 操作超时 — 底层流控等待超过时限
    Etime = 62,
    /// 无 STREAM 资源 — 流缓冲区分配失败
    Enosr = 63,
    /// 主机不在网络中 — 网络层未启用或无可用网络
    Enonet = 64,
    /// 协议错误 — 收到的报文不符合协议
    Eproto = 71,
    /// 消息损坏 — 内部数据校验失败
    Ebadmsg = 74,
    /// 值超出接口可表示范围 — 类型宽度不足以承载结果
    Eoverflow = 75,
    /// 不是套接字 — 对普通文件句柄执行 socket 操作
    Enotsock = 88,
    /// 需要目标地址 — 未连接且未给出目的地址
    Edestaddrreq = 89,
    /// 消息过长 — 超过套接字或协议的数据上限
    Emsgsize = 90,
    /// 协议类型不符 — 与套接字类型不匹配
    Eprototype = 91,
    /// 协议选项不可用 — 请求的选项不存在或不受支持
    Enoprotoopt = 92,
    /// 协议不受支持 — 该协议号未注册
    Eprotonosupport = 93,
    /// 套接字类型不受支持 — 该类型在本协议族下不可用
    Esocktnosupport = 94,
    /// 操作不受支持 — 对象拒绝该操作 (含 OPNOTSUPP 同值语义)
    Enotsup = 95,
    /// 协议族不受支持 — 该地址族未注册
    Epfnosupport = 96,
    /// 地址族不受支持 — 地址结构属于未知族
    Eafnosupport = 97,
    /// 地址已被占用 — 同一传输端点已有绑定
    Eaddrinuse = 98,
    /// 地址不可用 — 请求的地址不属于本机
    Eaddrnotavail = 99,
    /// 网络已断开 — 所连网络接口 down
    Enetdown = 100,
    /// 网络不可达 — 无到目标网络的路由
    Enetunreach = 101,
    /// 连接因网络重置中断 — 协议栈主动废弃了连接
    Enetreset = 102,
    /// 连接被中止 — 本端条件不满足 (含监听队列超时)
    Econnaborted = 103,
    /// 连接被对端重置 — 收到 RST 类终止
    Econnreset = 104,
    /// 无缓冲区空间 — 网络缓冲池耗尽
    Enobufs = 105,
    /// 已经连接 — 面向连接的套接字重复发起连接
    Eisconn = 106,
    /// 尚未连接 — 需要连接的调用在无连接状态下发起
    Enotconn = 107,
    /// 传输端点已关闭 — 关机后仍尝试发送
    Eshutdown = 108,
    /// 操作超时 — 重传或等待超过时限且无响应
    Etimedout = 110,
    /// 连接被拒绝 — 对端未监听该端口
    Econnrefused = 111,
    /// 主机已关机 — 路由已知但对端不可达且已下线
    Ehostdown = 112,
    /// 主机不可达 — 无到目标主机的路由
    Ehostunreach = 113,
    /// 操作已在进行中 — 重复发起异步连接
    Ealready = 114,
    /// 操作正在进行中 — 非阻塞连接已受理未完成
    Einprogress = 115,
}

impl Errno {
    /// 返回 POSIX errno 数值 (正整数)
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// 返回 POSIX 负返回码 (`-errno`, syscall 返回值约定)
    #[must_use]
    pub const fn as_ret(self) -> i64 {
        -(self as i64)
    }

    /// 从负返回值恢复 Errno
    ///
    /// 输入: privileged 层返回的负错误码 (如 -ENOMEM)
    /// 输出: 对应的 Errno 枚举值
    ///
    /// # Errors
    ///
    /// 未知/未定义错误码回退为 `EINVAL`.
    #[must_use]
    pub fn from_ret(ret: i64) -> Self {
        let errno = (-ret).cast_unsigned();
        match errno {
            1 => Self::Eperm,
            2 => Self::Enoent,
            3 => Self::Esrch,
            4 => Self::Eintr,
            5 => Self::Eio,
            6 => Self::Enxio,
            7 => Self::E2BIG,
            8 => Self::Enoexec,
            9 => Self::Ebadf,
            10 => Self::Echild,
            11 => Self::Eagain,
            12 => Self::Enomem,
            13 => Self::Eacces,
            14 => Self::Efault,
            15 => Self::Enotblk,
            16 => Self::Ebusy,
            17 => Self::Eexist,
            18 => Self::Exdev,
            19 => Self::Enodev,
            20 => Self::Enotdir,
            21 => Self::Eisdir,
            23 => Self::Enfile,
            24 => Self::Emfile,
            25 => Self::Enotty,
            26 => Self::Etxtbsy,
            27 => Self::Efbig,
            28 => Self::Enospc,
            29 => Self::Espipe,
            30 => Self::Erofs,
            31 => Self::Emlink,
            32 => Self::Epipe,
            33 => Self::Edom,
            34 => Self::Erange,
            35 => Self::Edeadlk,
            36 => Self::Enametoolong,
            37 => Self::Enolck,
            38 => Self::Enosys,
            39 => Self::Enotempty,
            40 => Self::Eloop,
            41 => Self::Ewouldblock,
            42 => Self::Enomsg,
            43 => Self::Eidrm,
            60 => Self::Enostr,
            61 => Self::Enodata,
            62 => Self::Etime,
            63 => Self::Enosr,
            64 => Self::Enonet,
            71 => Self::Eproto,
            74 => Self::Ebadmsg,
            75 => Self::Eoverflow,
            88 => Self::Enotsock,
            89 => Self::Edestaddrreq,
            90 => Self::Emsgsize,
            91 => Self::Eprototype,
            92 => Self::Enoprotoopt,
            93 => Self::Eprotonosupport,
            94 => Self::Esocktnosupport,
            95 => Self::Enotsup,
            96 => Self::Epfnosupport,
            97 => Self::Eafnosupport,
            98 => Self::Eaddrinuse,
            99 => Self::Eaddrnotavail,
            100 => Self::Enetdown,
            101 => Self::Enetunreach,
            102 => Self::Enetreset,
            103 => Self::Econnaborted,
            104 => Self::Econnreset,
            105 => Self::Enobufs,
            106 => Self::Eisconn,
            107 => Self::Enotconn,
            108 => Self::Eshutdown,
            110 => Self::Etimedout,
            111 => Self::Econnrefused,
            112 => Self::Ehostdown,
            113 => Self::Ehostunreach,
            114 => Self::Ealready,
            115 => Self::Einprogress,
            _ => Self::Einval, // 未知错误码回退到 EINVAL
        }
    }

    /// 从 i32 错误码构造 `Errno` (POSIX 反向: 整数值 = errno 编号)
    #[must_use]
    pub fn try_from_i32(code: i32) -> Option<Self> {
        // Errno 编号范围 1..=133 (POSIX 范围)
        // 我们通过 #[repr(i32)] 枚举, 直接 transmute 不安全;
        // 用 match 列举所有已知变体
        Some(match code {
            1 => Self::Eperm,
            2 => Self::Enoent,
            3 => Self::Esrch,
            4 => Self::Eintr,
            5 => Self::Eio,
            6 => Self::Enxio,
            7 => Self::E2BIG,
            8 => Self::Enoexec,
            9 => Self::Ebadf,
            10 => Self::Echild,
            11 => Self::Eagain,
            12 => Self::Enomem,
            13 => Self::Eacces,
            14 => Self::Efault,
            15 => Self::Enotblk,
            16 => Self::Ebusy,
            17 => Self::Eexist,
            18 => Self::Exdev,
            19 => Self::Enodev,
            20 => Self::Enotdir,
            21 => Self::Eisdir,
            22 => Self::Einval,
            23 => Self::Enfile,
            // 大于 23 的暂不识别, 返回 None
            _ => return None,
        })
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Eperm => write!(f, "Operation not permitted"),
            Self::Enoent => write!(f, "No such file or directory"),
            Self::Esrch => write!(f, "No such process"),
            Self::Eintr => write!(f, "Interrupted system call"),
            Self::Eio => write!(f, "I/O error"),
            Self::Enoexec => write!(f, "Exec format error"),
            Self::Ebadf => write!(f, "Bad file descriptor"),
            Self::Echild => write!(f, "No child processes"),
            Self::Eagain => write!(f, "Resource temporarily unavailable"),
            Self::Enomem => write!(f, "Cannot allocate memory"),
            Self::Eacces => write!(f, "Permission denied"),
            Self::Efault => write!(f, "Bad address"),
            Self::Ebusy => write!(f, "Device or resource busy"),
            Self::Eexist => write!(f, "File exists"),
            Self::Enotdir => write!(f, "Not a directory"),
            Self::Eisdir => write!(f, "Is a directory"),
            Self::Einval => write!(f, "Invalid argument"),
            Self::Enospc => write!(f, "No space left on device"),
            Self::Erofs => write!(f, "Read-only file system"),
            Self::Erange => write!(f, "Result too large"),
            Self::Enametoolong => write!(f, "File name too long"),
            Self::Enotempty => write!(f, "Directory not empty"),
            Self::Enosys => write!(f, "Function not implemented"),
            _ => write!(f, "Error {}", -(*self as i64)),
        }
    }
}

/// i64 → `Errno` (POSIX: 负数 = -errno)
#[must_use]
pub fn errno_from_i64(rc: i64) -> Option<Errno> {
    if rc < 0 {
        Errno::try_from_i32(crate::privileged::glue::low_i32_from_i64(-rc))
    } else {
        None
    }
}
