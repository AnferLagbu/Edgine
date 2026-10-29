#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。纯类型定义和常量。
//! Syscall 类型定义和常量 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义于 T5-4 (2026-06-16) 迁至 `functions::syscall::types`, 本文件仅
//! re-export。按"机制持有的数据结构/常量归 privileged"统一判据反转：
//! **syscall 编号表是用户态 ABI 机制** (编号空间分配 DECISION-037 承载物,
//! 用户态程序直接依赖), 被 privileged dispatch/dispatch_trait 消费 +
//! functions dispatch 消费, 依赖闭包为空 (纯 const + type alias, 仅
//! re-export privileged::errno::Errno) — 属机制项, 迁回 (同 fd_alloc 模式)。
//!
//! functions 侧改 `pub use crate::privileged::syscall::types::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。

// POSIX errno 命名约定 (EAGAIN/EACCES/...) — 全大写缩写是有意的
#![allow(clippy::upper_case_acronyms)]

/// Syscall 类型定义和常量
///
/// 编号空间分配 (DECISION-037 + edgine-naming-standpoint.md):
///   0-299   : Linux 兼容编号 (SYS_*), 直接使用 Linux 标准编号
///   300-399 : 保留
///   400-499 : SGEG 私有 syscall (避开 424-452 的 Linux 现代扩展区)
///   500-599 : 进程 / 内存 / 文件基础
///   600-699 : 网络 / IPC
///   700-799 : 设备 / 系统
///   800-899 : 扩展

pub const SYSCALL_INT: u8 = 0x80;

/// syscall 编号空间上界 (非 dispatch 数组边界 — dispatch 全为 match, 无 SYSCALL_TABLE).
///
/// 覆盖全部 EG_* 扩展区 (0-899), 与 `EG_FTRACE_ENABLE = 800` 等 800 段常量错开,
/// 避免编号常量语义误导.
pub const MAX_SYSCALLS: u64 = 900;

/// functions 层未处理 syscall 的返回码 (=-ENOSYS), 供 privileged 回退处理.
///
/// 作为 functions→privileged 分发回退哨兵, 与 `FallbackSyscallDispatch` 返回值一致.
pub const ENOSYS_RET: i64 = -38;

// ==================== POSIX 标准 syscall 编号 ====================

// 文件 I/O
/// `SYS_READ` — 从文件描述符读取数据
pub const SYS_READ: u64 = 0;
/// `SYS_WRITE` — 向文件描述符写入数据
pub const SYS_WRITE: u64 = 1;
/// `SYS_OPEN` — 打开文件
pub const SYS_OPEN: u64 = 2;
/// `SYS_CLOSE` — 关闭文件描述符
pub const SYS_CLOSE: u64 = 3;
/// `SYS_STAT` — 按路径获取文件状态
pub const SYS_STAT: u64 = 4;
/// `SYS_FSTAT` — 按文件描述符获取文件状态
pub const SYS_FSTAT: u64 = 5;
/// `SYS_LSTAT` — 获取符号链接自身的状态
pub const SYS_LSTAT: u64 = 6;
/// `SYS_POLL` — 等待一组文件描述符的就绪事件
pub const SYS_POLL: u64 = 7;
/// `SYS_LSEEK` — 调整文件读写偏移
pub const SYS_LSEEK: u64 = 8;

// 内存管理
/// `SYS_MMAP` — 建立内存映射
pub const SYS_MMAP: u64 = 9;
/// `SYS_MPROTECT` — 修改内存映射的保护属性
pub const SYS_MPROTECT: u64 = 10;
/// `SYS_MUNMAP` — 解除内存映射
pub const SYS_MUNMAP: u64 = 11;
/// `SYS_BRK` — 调整进程数据段上界
pub const SYS_BRK: u64 = 12;

// 信号 (基础存根)
/// `SYS_RT_SIGACTION` — 设置信号处理动作 (实时信号)
pub const SYS_RT_SIGACTION: u64 = 13;
/// `SYS_RT_SIGPROCMASK` — 设置信号屏蔽字 (实时信号)
pub const SYS_RT_SIGPROCMASK: u64 = 14;
/// `SYS_RT_SIGRETURN` — 从信号处理函数返回 (实时信号)
pub const SYS_RT_SIGRETURN: u64 = 15;

// 设备 I/O
/// `SYS_IOCTL` — 设备控制操作
pub const SYS_IOCTL: u64 = 16;

// 文件访问
/// `SYS_ACCESS` — 检查文件访问权限
pub const SYS_ACCESS: u64 = 21;
/// `SYS_PIPE` — 创建匿名管道
pub const SYS_PIPE: u64 = 22;
/// `SYS_SELECT` — 等待多个文件描述符的就绪事件
pub const SYS_SELECT: u64 = 23;
/// `SYS_SCHED_YIELD` — 主动让出 CPU
pub const SYS_SCHED_YIELD: u64 = 24;

// 内存重映射
/// `SYS_MREMAP` — 调整已有内存映射的大小
pub const SYS_MREMAP: u64 = 25;

// 文件描述符
/// `SYS_DUP` — 复制文件描述符
pub const SYS_DUP: u64 = 32;
/// `SYS_DUP2` — 复制文件描述符到指定编号
pub const SYS_DUP2: u64 = 33;

// 进程优先级
/// `SYS_NICE` — 调整进程静态优先级
pub const SYS_NICE: u64 = 34;

// 暂停
/// `SYS_NANOSLEEP` — 高精度休眠
pub const SYS_NANOSLEEP: u64 = 35;

// ITIMER
/// `SYS_GETITIMER` — 读取间隔定时器状态
pub const SYS_GETITIMER: u64 = 36;
/// `SYS_ALARM` — 设置进程闹钟
pub const SYS_ALARM: u64 = 37;
/// `SYS_SETITIMER` — 设置间隔定时器
pub const SYS_SETITIMER: u64 = 38;

// 进程基础
/// `SYS_GETPID` — 获取当前进程 PID
pub const SYS_GETPID: u64 = 39;

// 网络 socket
/// `SYS_SOCKET` — 创建套接字
pub const SYS_SOCKET: u64 = 41;
/// `SYS_CONNECT` — 向目标地址发起连接
pub const SYS_CONNECT: u64 = 42;
/// `SYS_ACCEPT` — 接受连接
pub const SYS_ACCEPT: u64 = 43;
/// `SYS_SENDTO` — 发送数据报
pub const SYS_SENDTO: u64 = 44;
/// `SYS_RECVFROM` — 接收数据报并获取来源地址
pub const SYS_RECVFROM: u64 = 45;
/// `SYS_SENDMSG` — 以消息头发送数据 (分散/聚集)
pub const SYS_SENDMSG: u64 = 46;
/// `SYS_RECVMSG` — 以消息头接收数据 (分散/聚集)
pub const SYS_RECVMSG: u64 = 47;
/// `SYS_SHUTDOWN` — 关闭套接字的单向或双向通信
pub const SYS_SHUTDOWN: u64 = 48;
/// `SYS_BIND` — 绑定套接字地址
pub const SYS_BIND: u64 = 49;
/// `SYS_LISTEN` — 将套接字置为监听状态
pub const SYS_LISTEN: u64 = 50;
/// `SYS_GETSOCKNAME` — 获取套接字的本地地址
pub const SYS_GETSOCKNAME: u64 = 51;
/// `SYS_GETPEERNAME` — 获取套接字的对端地址
pub const SYS_GETPEERNAME: u64 = 52;
/// `SYS_SETSOCKOPT` — 设置套接字选项
pub const SYS_SETSOCKOPT: u64 = 54;
/// `SYS_GETSOCKOPT` — 读取套接字选项
pub const SYS_GETSOCKOPT: u64 = 55;

// 进程
/// `SYS_CLONE` — 创建进程或线程 (可指定共享资源)
pub const SYS_CLONE: u64 = 56;
/// `SYS_FORK` — 复制当前进程
pub const SYS_FORK: u64 = 57;
/// `SYS_EXECVE` — 加载并执行新程序
pub const SYS_EXECVE: u64 = 59;
/// `SYS_EXIT` — 终止当前线程
pub const SYS_EXIT: u64 = 60;
/// `SYS_WAIT4` — 等待子进程状态变化并回收
pub const SYS_WAIT4: u64 = 61;
/// `SYS_KILL` — 向进程发送信号
pub const SYS_KILL: u64 = 62;

// 系统信息
/// `SYS_UNAME` — 获取系统标识信息
pub const SYS_UNAME: u64 = 63;

// 文件描述符操作
/// `SYS_FCNTL` — 操作文件描述符属性
pub const SYS_FCNTL: u64 = 72;

// 文件截断
/// `SYS_TRUNCATE` — 按路径截断文件长度
pub const SYS_TRUNCATE: u64 = 76;
/// `SYS_FTRUNCATE` — 按文件描述符截断文件长度
pub const SYS_FTRUNCATE: u64 = 77;

// 目录
/// `SYS_GETDENTS` — 读取目录项
pub const SYS_GETDENTS: u64 = 78;

// 路径
/// `SYS_GETCWD` — 获取当前工作目录
pub const SYS_GETCWD: u64 = 79;
/// `SYS_CHDIR` — 切换当前工作目录
pub const SYS_CHDIR: u64 = 80;

// 文件重命名
/// `SYS_RENAME` — 重命名文件
pub const SYS_RENAME: u64 = 82;

// 目录操作
/// `SYS_MKDIR` — 创建目录
pub const SYS_MKDIR: u64 = 83;
/// `SYS_RMDIR` — 删除空目录
pub const SYS_RMDIR: u64 = 84;

// 文件创建
/// `SYS_CREAT` — 创建文件 (等价于 open 的 O_CREAT|O_TRUNC)
pub const SYS_CREAT: u64 = 85;

// 文件链接
/// `SYS_LINK` — 创建硬链接
pub const SYS_LINK: u64 = 86;
/// `SYS_UNLINK` — 删除目录项
pub const SYS_UNLINK: u64 = 87;
/// `SYS_SYMLINK` — 创建符号链接
pub const SYS_SYMLINK: u64 = 88;
/// `SYS_READLINK` — 读取符号链接目标
pub const SYS_READLINK: u64 = 89;

// 文件权限
/// `SYS_CHMOD` — 修改文件权限
pub const SYS_CHMOD: u64 = 90;
/// `SYS_FCHMOD` — 按文件描述符修改文件权限
pub const SYS_FCHMOD: u64 = 91;
/// `SYS_CHOWN` — 修改文件属主与属组
pub const SYS_CHOWN: u64 = 92;
/// `SYS_FCHOWN` — 按文件描述符修改文件属主与属组
pub const SYS_FCHOWN: u64 = 93;

// 文件属性
/// `SYS_UMASK` — 设置文件创建权限掩码
pub const SYS_UMASK: u64 = 95;

// 时间
/// `SYS_GETTIMEOFDAY` — 获取当前时间
pub const SYS_GETTIMEOFDAY: u64 = 96;
/// `SYS_GETRLIMIT` — 获取资源限制
pub const SYS_GETRLIMIT: u64 = 97;
/// `SYS_GETRUSAGE` — 获取进程资源使用统计
pub const SYS_GETRUSAGE: u64 = 98;
/// `SYS_SYSINFO` — 获取系统整体信息
pub const SYS_SYSINFO: u64 = 99;

// 系统
/// `SYS_TIMES` — 获取进程时间统计
pub const SYS_TIMES: u64 = 100;

// 用户/组
/// `SYS_GETUID` — 获取真实用户 ID
pub const SYS_GETUID: u64 = 102;
/// `SYS_GETGID` — 获取真实组 ID
pub const SYS_GETGID: u64 = 104;
/// `SYS_SETUID` — 设置真实用户 ID
pub const SYS_SETUID: u64 = 105;
/// `SYS_SETGID` — 设置真实组 ID
pub const SYS_SETGID: u64 = 106;
/// `SYS_GETEUID` — 获取有效用户 ID
pub const SYS_GETEUID: u64 = 107;
/// `SYS_GETEGID` — 获取有效组 ID
pub const SYS_GETEGID: u64 = 108;

/// `SYS_SETEUID` — 设置有效用户 ID (EG 私有编号)
pub const SYS_SETEUID: u64 = 597; // EG 私有 (x86_64 无独立 seteuid syscall, 与 EG_SETEUID 同值)
/// `SYS_SETEGID` — 设置有效组 ID (EG 私有编号)
pub const SYS_SETEGID: u64 = 598; // EG 私有 (x86_64 无独立 setegid syscall, 与 EG_SETEGID 同值)
/// `SYS_SETREUID` — 设置真实与有效用户 ID
pub const SYS_SETREUID: u64 = 113;
/// `SYS_SETREGID` — 设置真实与有效组 ID
pub const SYS_SETREGID: u64 = 114;

// 进程组
/// `SYS_GETPPID` — 获取父进程 PID
pub const SYS_GETPPID: u64 = 110;
/// `SYS_GETPGID` — 获取进程组 ID
pub const SYS_GETPGID: u64 = 121;
/// `SYS_SETSID` — 创建新会话并成为会话首进程
pub const SYS_SETSID: u64 = 112;
/// `SYS_GETSID` — 获取会话 ID
pub const SYS_GETSID: u64 = 124;
/// `SYS_SETPGID` — 设置进程组 ID
pub const SYS_SETPGID: u64 = 109;

// 进程调度
/// `SYS_GETPRIORITY` — 获取进程 / 线程优先级
pub const SYS_GETPRIORITY: u64 = 140;
/// `SYS_SETPRIORITY` — 设置进程 / 线程优先级
pub const SYS_SETPRIORITY: u64 = 141;

// 文件同步
/// `SYS_SYNC` — 回写全部缓存到存储设备
pub const SYS_SYNC: u64 = 162;
/// `SYS_FSYNC` — 回写指定文件的数据与元数据
pub const SYS_FSYNC: u64 = 74;
/// `SYS_FDATASYNC` — 回写指定文件的数据
pub const SYS_FDATASYNC: u64 = 75;

// 挂载
/// `SYS_MOUNT` — 挂载文件系统
pub const SYS_MOUNT: u64 = 165;
/// `SYS_UMOUNT2` — 卸载文件系统
pub const SYS_UMOUNT2: u64 = 166;

// 其他 POSIX
/// `SYS_GETTID` — 获取当前线程 TID
pub const SYS_GETTID: u64 = 186;
/// `SYS_TIME` — 获取秒级时间戳
pub const SYS_TIME: u64 = 201;
/// `SYS_CLOCK_GETTIME` — 读取指定时钟的时间
pub const SYS_CLOCK_GETTIME: u64 = 228;
/// `SYS_EXIT_GROUP` — 终止进程内所有线程
pub const SYS_EXIT_GROUP: u64 = 231;
/// `SYS_TGKILL` — 向指定进程的指定线程发送信号
pub const SYS_TGKILL: u64 = 234;

// 同步
/// `SYS_FUTEX` — 快速用户态互斥原语
pub const SYS_FUTEX: u64 = 202;
// CPU 亲和性 (Linux 兼容号)
/// `SYS_SCHED_SETAFFINITY` — 设置线程 CPU 亲和性
pub const SYS_SCHED_SETAFFINITY: u64 = 203;
/// `SYS_SCHED_GETAFFINITY` — 获取线程 CPU 亲和性
pub const SYS_SCHED_GETAFFINITY: u64 = 204;

// 事件轮询
/// `SYS_EPOLL_CREATE` — 创建 epoll 实例
pub const SYS_EPOLL_CREATE: u64 = 213;
/// `SYS_EPOLL_CTL` — 控制 epoll 关注的事件
pub const SYS_EPOLL_CTL: u64 = 233;
/// `SYS_EPOLL_WAIT` — 等待 epoll 事件
pub const SYS_EPOLL_WAIT: u64 = 232;

// eventfd / signalfd / timerfd (Linux x86_64 标准编号)
/// `SYS_EVENTFD` — 创建事件通知 fd
pub const SYS_EVENTFD: u64 = 284;
/// `SYS_EVENTFD2` — 创建事件通知 fd (带 flags)
pub const SYS_EVENTFD2: u64 = 290;
/// `SYS_SIGNALFD` — 创建信号 fd
pub const SYS_SIGNALFD: u64 = 282;
/// `SYS_SIGNALFD4` — 创建信号 fd (带 flags)
pub const SYS_SIGNALFD4: u64 = 289;
/// `SYS_TIMERFD_CREATE` — 创建定时器 fd
pub const SYS_TIMERFD_CREATE: u64 = 283;
/// `SYS_TIMERFD_SETTIME` — 设置定时器 fd
pub const SYS_TIMERFD_SETTIME: u64 = 286;
/// `SYS_TIMERFD_GETTIME` — 读取定时器 fd 的剩余时间
pub const SYS_TIMERFD_GETTIME: u64 = 287;

// 内存建议 / 锁定 / mincore (Linux x86_64 标准编号)
/// `SYS_MADVISE` — 设置内存区域访问建议
pub const SYS_MADVISE: u64 = 28;
/// `SYS_MINCORE` — 查询内存页的驻留状态
pub const SYS_MINCORE: u64 = 27;
/// `SYS_MLOCK` — 锁定内存页禁止换出
pub const SYS_MLOCK: u64 = 149;
/// `SYS_MUNLOCK` — 解除内存页锁定
pub const SYS_MUNLOCK: u64 = 150;
/// `SYS_MLOCKALL` — 锁定进程全部地址空间
pub const SYS_MLOCKALL: u64 = 151;
/// `SYS_MUNLOCKALL` — 解除进程全部地址空间锁定
pub const SYS_MUNLOCKALL: u64 = 152;

// inotify (Linux x86_64 标准编号)
/// `SYS_INOTIFY_INIT` — 创建 inotify 实例
pub const SYS_INOTIFY_INIT: u64 = 253;
/// `SYS_INOTIFY_ADD_WATCH` — 添加 inotify 监视
pub const SYS_INOTIFY_ADD_WATCH: u64 = 254;
/// `SYS_INOTIFY_RM_WATCH` — 移除 inotify 监视
pub const SYS_INOTIFY_RM_WATCH: u64 = 255;
/// `SYS_INOTIFY_INIT1` — 创建 inotify 实例 (带 flags)
pub const SYS_INOTIFY_INIT1: u64 = 294;

// POSIX Timer (Linux x86_64 标准编号)
/// `SYS_TIMER_CREATE` — 创建 POSIX 定时器
pub const SYS_TIMER_CREATE: u64 = 222;
/// `SYS_TIMER_SETTIME` — 启动 / 调整 / 停止 POSIX 定时器
pub const SYS_TIMER_SETTIME: u64 = 223;
/// `SYS_TIMER_GETTIME` — 读取 POSIX 定时器的剩余时间
pub const SYS_TIMER_GETTIME: u64 = 224;
/// `SYS_TIMER_GETOVERRUN` — 读取 POSIX 定时器的溢出次数
pub const SYS_TIMER_GETOVERRUN: u64 = 225;
/// `SYS_TIMER_DELETE` — 删除 POSIX 定时器
pub const SYS_TIMER_DELETE: u64 = 226;
/// `SYS_CLOCK_GETRES` — 读取指定时钟的分辨率
pub const SYS_CLOCK_GETRES: u64 = 229;

// 熵源 (Linux x86_64 标准编号)
/// `SYS_GETRANDOM` — 从内核熵源获取随机字节
pub const SYS_GETRANDOM: u64 = 318;

// NUMA (Linux x86_64 标准编号)
/// `SYS_MBIND` — 设置内存区域的 NUMA 绑定
pub const SYS_MBIND: u64 = 237;
/// `SYS_SET_MEMPOLICY` — 设置进程默认 NUMA 内存策略
pub const SYS_SET_MEMPOLICY: u64 = 238;
/// `SYS_GET_MEMPOLICY` — 获取 NUMA 内存策略
pub const SYS_GET_MEMPOLICY: u64 = 239;
/// `SYS_MIGRATE_PAGES` — 迁移进程页面到目标 NUMA 节点
pub const SYS_MIGRATE_PAGES: u64 = 256;
/// `SYS_GETCPU` — 获取当前 CPU 与 NUMA 节点
pub const SYS_GETCPU: u64 = 309;

// 文件 I/O 扩展 (Linux x86_64 标准编号)
/// `SYS_READV` — 分散读取 (多段缓冲)
pub const SYS_READV: u64 = 19;
/// `SYS_WRITEV` — 聚集写入 (多段缓冲)
pub const SYS_WRITEV: u64 = 20;
/// `SYS_PREAD64` — 按偏移读取文件
pub const SYS_PREAD64: u64 = 17;
/// `SYS_PWRITE64` — 按偏移写入文件
pub const SYS_PWRITE64: u64 = 18;
/// `SYS_SENDFILE` — 在两个文件描述符之间直接传输数据
pub const SYS_SENDFILE: u64 = 40;
/// `SYS_PREADV` — 按偏移分散读取
pub const SYS_PREADV: u64 = 295;
/// `SYS_PWRITEV` — 按偏移聚集写入
pub const SYS_PWRITEV: u64 = 296;
/// `SYS_PREADV2` — 按偏移分散读取 (带 flags)
pub const SYS_PREADV2: u64 = 327;
/// `SYS_PWRITEV2` — 按偏移聚集写入 (带 flags)
pub const SYS_PWRITEV2: u64 = 328;
/// `SYS_FLOCK` — 对文件施加咨询锁
pub const SYS_FLOCK: u64 = 73;
/// `SYS_FCHMODAT` — 按目录 fd 修改文件权限
pub const SYS_FCHMODAT: u64 = 268;
/// `SYS_FCHOWNAT` — 按目录 fd 修改文件属主与属组
pub const SYS_FCHOWNAT: u64 = 260;
/// `SYS_NEWFSTATAT` — 按目录 fd 获取文件状态
pub const SYS_NEWFSTATAT: u64 = 262;
/// `SYS_UNLINKAT` — 按目录 fd 删除目录项
pub const SYS_UNLINKAT: u64 = 263;
/// `SYS_RENAMEAT` — 按目录 fd 重命名文件
pub const SYS_RENAMEAT: u64 = 264;
/// `SYS_RENAMEAT2` — 按目录 fd 重命名文件 (带 flags)
pub const SYS_RENAMEAT2: u64 = 316;
/// `SYS_LINKAT` — 按目录 fd 创建硬链接
pub const SYS_LINKAT: u64 = 265;
/// `SYS_SYMLINKAT` — 按目录 fd 创建符号链接
pub const SYS_SYMLINKAT: u64 = 266;
/// `SYS_READLINKAT` — 按目录 fd 读取符号链接目标
pub const SYS_READLINKAT: u64 = 267;
/// `SYS_FACCESSAT` — 按目录 fd 检查文件访问权限
pub const SYS_FACCESSAT: u64 = 269;
/// `SYS_FACCESSAT2` — 按目录 fd 检查文件访问权限 (带 flags)
pub const SYS_FACCESSAT2: u64 = 439;
/// `SYS_FCHMODAT2` — 按目录 fd 修改文件权限 (带 flags)
pub const SYS_FCHMODAT2: u64 = 452;
/// `SYS_STATX` — 获取扩展文件状态
pub const SYS_STATX: u64 = 332;
/// `SYS_COPY_FILE_RANGE` — 在两个文件描述符之间复制文件区间
pub const SYS_COPY_FILE_RANGE: u64 = 326;
/// `SYS_NAME_TO_HANDLE_AT` — 由路径获取文件句柄
pub const SYS_NAME_TO_HANDLE_AT: u64 = 303;
/// `SYS_OPEN_BY_HANDLE_AT` — 由文件句柄打开文件
pub const SYS_OPEN_BY_HANDLE_AT: u64 = 304;
/// `SYS_FALLOCATE` — 预分配文件空间
pub const SYS_FALLOCATE: u64 = 285;
/// `SYS_UTIMENSAT` — 按目录 fd 设置文件时间戳
pub const SYS_UTIMENSAT: u64 = 280;
/// `SYS_OPENAT` — 按目录 fd 打开文件
pub const SYS_OPENAT: u64 = 257;
/// `SYS_OPENAT2` — 按目录 fd 打开文件 (可扩展参数)
pub const SYS_OPENAT2: u64 = 437;
/// `SYS_CLOSE_RANGE` — 批量关闭文件描述符区间
pub const SYS_CLOSE_RANGE: u64 = 436;

// FD 扩展 (Linux x86_64 标准编号)
/// `SYS_DUP3` — 复制文件描述符到指定编号 (带 flags)
pub const SYS_DUP3: u64 = 292;
/// `SYS_PIPE2` — 创建匿名管道 (带 flags)
pub const SYS_PIPE2: u64 = 293;
/// `SYS_EPOLL_CREATE1` — 创建 epoll 实例 (带 flags)
pub const SYS_EPOLL_CREATE1: u64 = 291;
/// `SYS_EPOLL_PWAIT` — 等待 epoll 事件 (带信号屏蔽字)
pub const SYS_EPOLL_PWAIT: u64 = 281;
/// `SYS_EPOLL_PWAIT2` — 等待 epoll 事件 (纳秒超时)
pub const SYS_EPOLL_PWAIT2: u64 = 441;

// select / pselect / ppoll (Linux x86_64 标准编号)
/// `SYS_PSELECT6` — 等待文件描述符事件 (带信号屏蔽字)
pub const SYS_PSELECT6: u64 = 270;
/// `SYS_PPOLL` — 等待文件描述符事件 (纳秒超时)
pub const SYS_PPOLL: u64 = 271;

// 进程扩展 (Linux x86_64 标准编号)
/// `SYS_SET_ROBUST_LIST` — 设置 robust futex 链表
pub const SYS_SET_ROBUST_LIST: u64 = 273;
/// `SYS_GET_ROBUST_LIST` — 获取 robust futex 链表
pub const SYS_GET_ROBUST_LIST: u64 = 274;
/// `SYS_PIDFD_OPEN` — 由 PID 获取 pidfd
pub const SYS_PIDFD_OPEN: u64 = 434;
/// `SYS_PIDFD_GETFD` — 由 pidfd 复制目标进程的文件描述符
pub const SYS_PIDFD_GETFD: u64 = 438;
/// `SYS_PIDFD_SEND_SIGNAL` — 经 pidfd 向目标进程发送信号
pub const SYS_PIDFD_SEND_SIGNAL: u64 = 424;
/// `SYS_CLONE3` — 创建进程或线程 (可扩展参数)
pub const SYS_CLONE3: u64 = 435;
/// `SYS_EXECVEAT` — 按目录 fd 加载并执行新程序
pub const SYS_EXECVEAT: u64 = 322;
/// `SYS_WAITID` — 等待子进程状态变化 (扩展信息)
pub const SYS_WAITID: u64 = 247;
/// `SYS_PROCESS_VM_READV` — 跨进程分散读取内存
pub const SYS_PROCESS_VM_READV: u64 = 310;
/// `SYS_PROCESS_VM_WRITEV` — 跨进程聚集写入内存
pub const SYS_PROCESS_VM_WRITEV: u64 = 311;

// 内存扩展 (Linux x86_64 标准编号)
/// `SYS_MEMFD_CREATE` — 创建匿名内存文件
pub const SYS_MEMFD_CREATE: u64 = 319;
/// `SYS_USERFAULTFD` — 创建用户态缺页处理 fd
pub const SYS_USERFAULTFD: u64 = 323;

// 网络扩展 (Linux x86_64 标准编号)
/// `SYS_RECVMMSG` — 批量接收消息
pub const SYS_RECVMMSG: u64 = 299;
/// `SYS_SENDMMSG` — 批量发送消息
pub const SYS_SENDMMSG: u64 = 307;
/// `SYS_SOCKETPAIR` — 创建匿名套接字对
pub const SYS_SOCKETPAIR: u64 = 53;
/// `SYS_ACCEPT4` — 接受连接 (带 flags)
pub const SYS_ACCEPT4: u64 = 288;

// 事件扩展 (Linux x86_64 标准编号)

// Seccomp / prctl (Linux x86_64 标准编号)
/// `SYS_SECCOMP` — 安装 seccomp 过滤器
pub const SYS_SECCOMP: u64 = 317;
/// `SYS_PRCTL` — 进程控制操作
pub const SYS_PRCTL: u64 = 157;
/// `SYS_ARCH_PRCTL` — 架构相关进程控制 (x86_64 TLS 等)
pub const SYS_ARCH_PRCTL: u64 = 158;

// 安全 / 权限 (Linux x86_64 标准编号)
/// `SYS_CAPGET` — 获取进程能力集
pub const SYS_CAPGET: u64 = 125;
/// `SYS_CAPSET` — 设置进程能力集
pub const SYS_CAPSET: u64 = 126;
/// `SYS_PIVOT_ROOT` — 切换根文件系统
pub const SYS_PIVOT_ROOT: u64 = 155;
/// `SYS_CHROOT` — 修改进程根目录
pub const SYS_CHROOT: u64 = 161;

// 时间扩展 (Linux x86_64 标准编号)
/// `SYS_CLOCK_NANOSLEEP` — 按指定时钟高精度休眠
pub const SYS_CLOCK_NANOSLEEP: u64 = 230;
/// `SYS_SETTIMEOFDAY` — 设置系统时间
pub const SYS_SETTIMEOFDAY: u64 = 164;
/// `SYS_ADJTIMEX` — 调整系统时钟
pub const SYS_ADJTIMEX: u64 = 159;

// 杂项 (Linux x86_64 标准编号)
/// `SYS_REBOOT` — 重启或关闭系统
pub const SYS_REBOOT: u64 = 169;
/// `SYS_SETHOSTNAME` — 设置主机名
pub const SYS_SETHOSTNAME: u64 = 170;
/// `SYS_SETDOMAINNAME` — 设置域名
pub const SYS_SETDOMAINNAME: u64 = 171;

// ==================== SGEG 私有 syscall (400-499, 避开 Linux 424-452) ====================
//
// 编号空间: 400-423 + 453-499. 424-452 保留给 Linux 现代扩展 syscall
// (pidfd_send_signal/io_uring/clone3/close_range/openat2/faccessat2/fchmodat2 等),
// 避免 Edgine 私有编号与未来 Linux ABI 冲突 (DECISION-037: 500+ 与 Linux 错开).

// ---------- 400-413: 认证 / 身份 ----------
/// `SYS_SGEG_LOGIN` — SGEG 私有: 用户登录
pub const SYS_SGEG_LOGIN: u64 = 400;
/// `SYS_SGEG_LOGOUT` — SGEG 私有: 用户登出
pub const SYS_SGEG_LOGOUT: u64 = 401;
/// `SYS_SGEG_CREATE_IDENTITY` — SGEG 私有: 创建身份
pub const SYS_SGEG_CREATE_IDENTITY: u64 = 402;
/// `SYS_SGEG_DELETE_IDENTITY` — SGEG 私有: 删除身份
pub const SYS_SGEG_DELETE_IDENTITY: u64 = 403;
/// `SYS_SGEG_IDENTITY_INFO` — SGEG 私有: 查询身份信息
pub const SYS_SGEG_IDENTITY_INFO: u64 = 404;
/// `SYS_SGEG_CHANGE_PASSWORD` — SGEG 私有: 修改身份密码
pub const SYS_SGEG_CHANGE_PASSWORD: u64 = 405;
/// `SYS_SGEG_VERIFY_PASSWORD` — SGEG 私有: 校验身份密码
pub const SYS_SGEG_VERIFY_PASSWORD: u64 = 406;
/// `SYS_SGEG_CREATE_FIRST` — SGEG 私有: 创建首个身份 (系统初始化)
pub const SYS_SGEG_CREATE_FIRST: u64 = 407;
/// `SYS_SGEG_GRANT` — SGEG 私有: 授予能力
pub const SYS_SGEG_GRANT: u64 = 408;
/// `SYS_SGEG_REVOKE` — SGEG 私有: 撤销能力
pub const SYS_SGEG_REVOKE: u64 = 409;
/// `SYS_SGEG_CHECK_CAP` — SGEG 私有: 检查单项能力
pub const SYS_SGEG_CHECK_CAP: u64 = 410;
/// `SYS_SGEG_GET_CAPS` — SGEG 私有: 获取能力集
pub const SYS_SGEG_GET_CAPS: u64 = 411;
/// `SYS_SGEG_GET_PWM` — SGEG 私有: 获取当前登录身份的 PWM 标识
pub const SYS_SGEG_GET_PWM: u64 = 412;
/// `SYS_SGEG_SET_PWM` — SGEG 私有: 设置当前登录身份的 PWM 标识
pub const SYS_SGEG_SET_PWM: u64 = 413;
// 分册 9 批次 4: 域级行为门控 (DomainFlags) 查询 / 设置
/// `SYS_SGEG_GET_DOMAIN_FLAGS` — SGEG 私有: 读取域级行为门控标志
pub const SYS_SGEG_GET_DOMAIN_FLAGS: u64 = 414;
/// `SYS_SGEG_SET_DOMAIN_FLAGS` — SGEG 私有: 设置域级行为门控标志
pub const SYS_SGEG_SET_DOMAIN_FLAGS: u64 = 415;
// 416-419: 保留

// ---------- 420-423: 存储设备 ----------
/// `SYS_SGEG_DISK_LIST` — SGEG 私有: 列出存储设备
pub const SYS_SGEG_DISK_LIST: u64 = 420;
/// `SYS_SGEG_DISK_INFO` — SGEG 私有: 查询存储设备信息
pub const SYS_SGEG_DISK_INFO: u64 = 421;
/// `SYS_SGEG_DISK_FORMAT` — SGEG 私有: 格式化存储设备
pub const SYS_SGEG_DISK_FORMAT: u64 = 422;
/// `SYS_SGEG_DISK_PARTITION` — SGEG 私有: 对存储设备分区
pub const SYS_SGEG_DISK_PARTITION: u64 = 423;

// ---------- 453-463: 存储扩展 + 进程管理 + 系统信息 (避开 424-452) ----------
/// `SYS_SGEG_DISK_INSTALL` — SGEG 私有: 将系统安装到存储设备
pub const SYS_SGEG_DISK_INSTALL: u64 = 453;
/// `SYS_SGEG_FAT_FORMAT` — SGEG 私有: 以 FAT 格式化存储设备
pub const SYS_SGEG_FAT_FORMAT: u64 = 454;
/// `SYS_SGEG_PROC_LIST` — SGEG 私有: 列出进程
pub const SYS_SGEG_PROC_LIST: u64 = 455;
/// `SYS_SGEG_PROC_SETPRI` — SGEG 私有: 设置进程优先级
pub const SYS_SGEG_PROC_SETPRI: u64 = 456;
/// `SYS_SGEG_PROC_SLEEP` — SGEG 私有: 让指定进程休眠
pub const SYS_SGEG_PROC_SLEEP: u64 = 457;
/// `SYS_SGEG_PROC_CPUTIME` — SGEG 私有: 查询进程 CPU 时间
pub const SYS_SGEG_PROC_CPUTIME: u64 = 458;
/// `SYS_SGEG_GETHOSTNAME` — SGEG 私有: 获取主机名
pub const SYS_SGEG_GETHOSTNAME: u64 = 459;
/// `SYS_SGEG_SETHOSTNAME` — SGEG 私有: 设置主机名
pub const SYS_SGEG_SETHOSTNAME: u64 = 460;
/// `SYS_SGEG_BOOT_CHECK` — SGEG 私有: 检查启动完整性
pub const SYS_SGEG_BOOT_CHECK: u64 = 461;
/// `SYS_SGEG_REBOOT` — SGEG 私有: 重启系统
pub const SYS_SGEG_REBOOT: u64 = 462;
/// `SYS_SGEG_HOTPLUG_STATUS` — SGEG 私有: 查询热插拔状态
pub const SYS_SGEG_HOTPLUG_STATUS: u64 = 463;

// ==================== 帧缓冲设备 (Edgine 私有, 与 EG_FB_* 同值) ====================
/// `SYS_FB_OPEN` — 打开帧缓冲设备
pub const SYS_FB_OPEN: u64 = 720;
/// `SYS_FB_MMAP` — 将帧缓冲映射到用户态
pub const SYS_FB_MMAP: u64 = 721;
/// `SYS_FB_RELEASE` — 释放帧缓冲设备
pub const SYS_FB_RELEASE: u64 = 722;

// ============================================================================
// Edgine 原生 syscall 编号 (500+)
//
// 遵循 edgine-naming-standpoint.md:
//   500-599 : 进程 / 内存 / 文件基础
//   600-699 : 网络 / IPC
//   700-799 : 设备 / 系统
//   800-899 : 扩展
//
// 编号原则:
//   - 不抄任何 OS 编号
//   - 按功能分区, 每区留扩展空间
//   - 0-299 直接使用 Linux 标准编号 (SYS_*)
// ============================================================================

// ---------- 500-509: Core I/O ----------

// ---------- 510-519: 内存管理 ----------
// 515-519: reserved (madvise, mlock, munlock, mlockall, munlockall)

// ---------- 520-539: 进程管理 ----------

// ---------- 540-559: 信号 ----------
// 545-559: 保留 (tkill, sigaltstack, rt_sigsuspend, ...)  // syscall 编号预留
// P1-I-45: 接线 sigaltstack 替代栈系统调用

// ---------- 560-579: 文件系统操作 ----------

// ---------- 580-589: FD / 同步 / 挂载 ----------

// ---------- 590-599: 身份 + 文件锁 ----------
// EG_SETREGID 映射到 EG_SETREUID, 由 dispatch 区分

// ---------- 600-619: 网络 ----------
// 614-619: 保留 (socketpair, ...)  // syscall 编号预留

// ---------- 620-639: 同步 / IPC ----------
// 631-639: 保留 (msgqueue, shm, sem)  // syscall 编号预留

// ---------- 640-649: inotify ----------

// ---------- 650-659: sendfile / splice ----------  // 高效拷贝/拼接 syscall

// ---------- 700-709: 系统信息 ----------

// ---------- 710-719: 时间 ----------
// 712: 保留 (settimeofday)  // syscall 编号预留

// ---------- 720-729: 设备 ----------
// B09-17 (2026-09-14): 编号空间归位补齐 (dispatch/测试实际引用的 Linux 编号)
/// `SYS_SIGALTSTACK` — 设置信号处理替代栈
pub const SYS_SIGALTSTACK: u64 = 131;
/// `SYS_SETXATTR` — 设置文件扩展属性
pub const SYS_SETXATTR: u64 = 188;
/// `SYS_GETXATTR` — 读取文件扩展属性
pub const SYS_GETXATTR: u64 = 191;
/// `SYS_LISTXATTR` — 列出文件扩展属性名
pub const SYS_LISTXATTR: u64 = 194;
/// `SYS_REMOVEXATTR` — 删除文件扩展属性
pub const SYS_REMOVEXATTR: u64 = 197;

// ---------- 730-739: 设备固件加载 ----------
/// `EG_FW_LOAD` — 从用户态路径加载固件并附着到设备树节点
pub const EG_FW_LOAD: u64 = 730;
/// `EG_FW_GET` — 按偏移拷贝固件内容到用户态缓冲
pub const EG_FW_GET: u64 = 731;
/// `EG_FW_GET_INFO` — 拷贝固件信息到用户态
pub const EG_FW_GET_INFO: u64 = 732;
/// `EG_FW_DETACH` — 移除设备树节点上的固件
pub const EG_FW_DETACH: u64 = 733;

// ---------- 740-745: POSIX Timer ----------
/// 创建 per-process 定时器 (`timer_create`)
/// 启动 / 调整 / 停止定时器 (`timer_settime`)
/// 查询定时器剩余时间 (`timer_gettime`)
/// 释放定时器 (`timer_delete`)
/// 返回补打次数 (`timer_getoverrun`)
/// 时钟分辨率 (`clock_getres`)

// ---------- 746-747: 熵源 / Stack Canary (P1 #14) ----------
/// 从内核熵源填充用户 buffer (Linux getrandom 兼容)
/// 读取当前进程 8 字节 stack canary (低字节恒为 0)
pub const EG_GET_CANARY: u64 = 747;

// ---------- 760-765: 内存建议与锁定 (madvise / mlock, P1 #15) ----------
/// 设置内存区域访问模式建议 (madvise)
/// 锁定 [addr, addr+len) 物理页禁止换出 (mlock)
/// 解除锁定 (munlock)
/// 进程级锁定所有/未来映射 (mlockall)
/// 解除进程级所有锁定 (munlockall)
/// 查询每页驻留性 (mincore)

// ---------- 800-809: 内核调试 / 跟踪 (ftrace / KGDB) ----------
/// 启用 ftrace 全局开关
pub const EG_FTRACE_ENABLE: u64 = 800;
/// 禁用 ftrace 全局开关
pub const EG_FTRACE_DISABLE: u64 = 801;
/// 从 ftrace ring buffer 读取一条事件到用户缓冲
pub const EG_FTRACE_READ: u64 = 802;
/// 查询 ftrace 状态 (`event_count` / `overflow_count`)
pub const EG_FTRACE_STAT: u64 = 803;
/// KGDB 主动断点 (用户态调试器触发)
pub const EG_KGDB_ENTER: u64 = 804;

// ==================== C7: Seccomp / prctl ====================

/// seccomp — 安装 Seccomp 过滤器
/// prctl — 进程控制 (`Seccomp/no_new_privs` 子集)

// ==================== C5: 路由表 ====================

/// `route_add` — 添加路由条目
pub const EG_ROUTE_ADD: u64 = 807;
/// `route_del` — 删除路由条目
pub const EG_ROUTE_DEL: u64 = 808;
/// `route_query` — 查询路由 (最长前缀匹配)
pub const EG_ROUTE_QUERY: u64 = 809;

// ==================== C5: Netfilter ====================

/// `nf_add_rule` — 添加 Netfilter 规则
pub const EG_NF_ADD_RULE: u64 = 810;
/// `nf_del_rule` — 删除 Netfilter 规则
pub const EG_NF_DEL_RULE: u64 = 811;

// ==================== C4: io_uring ====================

/// `io_uring_setup` — 创建 `io_uring` 实例
/// `io_uring_enter` — 提交/等待完成
/// `io_uring_register` — 注册缓冲区/文件
/// `io_uring_submit_sqe` — 提交单个 SQE (简化版)

// ==================== D1: Namespace ====================

/// unshare — 取消共享指定 namespace
/// setns — 切换到指定 namespace

// ==================== D2: cgroup ====================

/// `cgroup_create` — 创建子 cgroup
pub const EG_CGROUP_CREATE: u64 = 830;
/// `cgroup_destroy` — 删除 cgroup
pub const EG_CGROUP_DESTROY: u64 = 831;
/// `cgroup_attach` — 将进程迁移到 cgroup
pub const EG_CGROUP_ATTACH: u64 = 832;
/// `cgroup_set_limit` — 设置 cgroup 资源限制
pub const EG_CGROUP_SET_LIMIT: u64 = 833;
/// `cgroup_get_stat` — 获取 cgroup 统计信息
pub const EG_CGROUP_GET_STAT: u64 = 834;

// ==================== D3: NUMA ====================

/// `get_mempolicy` — 获取 NUMA 内存策略
/// `set_mempolicy` — 设置 NUMA 内存策略
/// `migrate_pages` — 迁移进程页面到目标节点
/// getcpu — 获取当前 CPU 和 NUMA 节点

// ==================== D4: eBPF ====================

/// bpf — BPF 系统调用多路复用

// ==================== D5: 电源管理 ====================

/// pm — 电源管理系统调用
pub const EG_PM: u64 = 860;

// ==================== D6: 安全启动 + TPM ====================

/// `secure_boot` — 安全启动系统调用
pub const EG_SECURE_BOOT: u64 = 870;

/// tpm — TPM 系统调用
pub const EG_TPM: u64 = 871;

// ==================== D7: Shadow Stack (CET) ====================

/// cet — CET/Shadow Stack 系统调用
pub const EG_CET: u64 = 880;

// ==================== D8: Tickless (NO_HZ) ====================  // 动态时钟节拍模式

/// tickless — Tickless 系统调用
pub const EG_TICKLESS: u64 = 881;

// ==================== D9: NTP/PTP 时钟同步 ====================

/// timesync — 时间同步系统调用
pub const EG_TIMESYNC: u64 = 882;

// ==================== D10: kexec ====================

/// kexec — 直接内核引导系统调用

// ==================== D11: UEFI ====================

/// uefi — UEFI 运行时服务系统调用
pub const EG_UEFI: u64 = 884;

// ==================== D12: 扩展属性 (xattr) ====================

/// setxattr — 设置扩展属性
/// getxattr — 获取扩展属性
/// listxattr — 列出扩展属性
/// removexattr — 删除扩展属性

// ==================== D13: 快照 (snapshot) ====================

/// `io_uring_submit_sqe` — 提交单个 SQE (简化版)
pub const EG_IO_URING_SUBMIT: u64 = 815;

/// `snapshot_create` — 创建快照
pub const EG_SNAPSHOT_CREATE: u64 = 895;
/// `snapshot_destroy` — 销毁快照
pub const EG_SNAPSHOT_DESTROY: u64 = 896;
/// `snapshot_rollback` — 回滚快照
pub const EG_SNAPSHOT_ROLLBACK: u64 = 897;
/// `snapshot_clone` — 从快照创建克隆
pub const EG_SNAPSHOT_CLONE: u64 = 898;

// ==================== POSIX errno (使用 Linux 风格: 返回值 = -errno) ====================
//
// B09-12/DECISION-H13 P0-1: Errno 定义已迁回 privileged (privileged::errno),
// 本处 re-export 保持调用方兼容 (functions→privileged 单向依赖).
pub use crate::privileged::errno::Errno;

// ==================== 辅助类型 ====================

/// syscall 处理函数的统一返回类型: 成功值或 `Errno`
pub type SyscallResult<T> = Result<T, Errno>;

// ============================================================================
// 编译期唯一性断言 (B05-02 防御)
//
// 防止未来新增 syscall 编号时引入重复值, 在编译期即失败.
// 若新增编号命中下方任一断言, 说明与现有 Linux 或私有编号冲突, 须重新分配.
// ============================================================================

/// 编译期断言: Linux 兼容编号 (0-299) 内部无重复
const _: () = {
    // 所有 Linux 标准编号 (0-299 区段)
    const LINUX_NUMS: &[u64] = &[
        SYS_READ,
        SYS_WRITE,
        SYS_OPEN,
        SYS_CLOSE,
        SYS_STAT,
        SYS_FSTAT,
        SYS_LSTAT,
        SYS_POLL,
        SYS_LSEEK,
        SYS_MMAP,
        SYS_MPROTECT,
        SYS_MUNMAP,
        SYS_BRK,
        SYS_RT_SIGACTION,
        SYS_RT_SIGPROCMASK,
        SYS_RT_SIGRETURN,
        SYS_IOCTL,
        SYS_ACCESS,
        SYS_PIPE,
        SYS_SELECT,
        SYS_SCHED_YIELD,
        SYS_MREMAP,
        SYS_DUP,
        SYS_DUP2,
        SYS_NANOSLEEP,
        SYS_GETITIMER,
        SYS_ALARM,
        SYS_SETITIMER,
        SYS_GETPID,
        SYS_SOCKET,
        SYS_CONNECT,
        SYS_ACCEPT,
        SYS_SENDTO,
        SYS_RECVFROM,
        SYS_SENDMSG,
        SYS_RECVMSG,
        SYS_SHUTDOWN,
        SYS_BIND,
        SYS_LISTEN,
        SYS_GETSOCKNAME,
        SYS_GETPEERNAME,
        SYS_SETSOCKOPT,
        SYS_GETSOCKOPT,
        SYS_CLONE,
        SYS_FORK,
        SYS_EXECVE,
        SYS_EXIT,
        SYS_WAIT4,
        SYS_KILL,
        SYS_UNAME,
        SYS_FCNTL,
        SYS_FLOCK,
        SYS_FSYNC,
        SYS_FDATASYNC,
        SYS_TRUNCATE,
        SYS_FTRUNCATE,
        SYS_GETDENTS,
        SYS_GETCWD,
        SYS_CHDIR,
        SYS_RENAME,
        SYS_MKDIR,
        SYS_RMDIR,
        SYS_CREAT,
        SYS_LINK,
        SYS_UNLINK,
        SYS_SYMLINK,
        SYS_READLINK,
        SYS_CHMOD,
        SYS_FCHMOD,
        SYS_CHOWN,
        SYS_FCHOWN,
        SYS_UMASK,
        SYS_GETTIMEOFDAY,
        SYS_GETRLIMIT,
        SYS_GETRUSAGE,
        SYS_SYSINFO,
        SYS_TIMES,
        SYS_GETUID,
        SYS_GETGID,
        SYS_SETUID,
        SYS_SETGID,
        SYS_GETEUID,
        SYS_GETEGID,
        SYS_SETREUID,
        SYS_SETREGID,
        SYS_GETPPID,
        SYS_GETPGID,
        SYS_SETSID,
        SYS_GETSID,
        SYS_SETPGID,
        SYS_GETPRIORITY,
        SYS_SETPRIORITY,
        SYS_SYNC,
        SYS_MOUNT,
        SYS_UMOUNT2,
        SYS_GETTID,
        SYS_TIME,
        SYS_CLOCK_GETTIME,
        SYS_EXIT_GROUP,
        SYS_TGKILL,
        SYS_FUTEX,
        SYS_SCHED_SETAFFINITY,
        SYS_SCHED_GETAFFINITY,
        SYS_EPOLL_CREATE,
        SYS_EPOLL_CTL,
        SYS_EPOLL_WAIT,
        SYS_EVENTFD,
        SYS_EVENTFD2,
        SYS_SIGNALFD,
        SYS_SIGNALFD4,
        SYS_TIMERFD_CREATE,
        SYS_TIMERFD_SETTIME,
        SYS_TIMERFD_GETTIME,
        SYS_MADVISE,
        SYS_MINCORE,
        SYS_MLOCK,
        SYS_MUNLOCK,
        SYS_MLOCKALL,
        SYS_MUNLOCKALL,
        SYS_INOTIFY_INIT,
        SYS_INOTIFY_ADD_WATCH,
        SYS_INOTIFY_RM_WATCH,
        SYS_INOTIFY_INIT1,
        SYS_TIMER_CREATE,
        SYS_TIMER_SETTIME,
        SYS_TIMER_GETTIME,
        SYS_TIMER_GETOVERRUN,
        SYS_TIMER_DELETE,
        SYS_CLOCK_GETRES,
        SYS_GETRANDOM,
        SYS_MBIND,
        SYS_SET_MEMPOLICY,
        SYS_GET_MEMPOLICY,
        SYS_MIGRATE_PAGES,
        SYS_GETCPU,
        SYS_READV,
        SYS_WRITEV,
        SYS_PREAD64,
        SYS_PWRITE64,
        SYS_SENDFILE,
        SYS_PREADV,
        SYS_PWRITEV,
        SYS_PREADV2,
        SYS_PWRITEV2,
        SYS_FCHMODAT,
        SYS_FCHOWNAT,
        SYS_NEWFSTATAT,
        SYS_UNLINKAT,
        SYS_RENAMEAT,
        SYS_RENAMEAT2,
        SYS_LINKAT,
        SYS_SYMLINKAT,
        SYS_READLINKAT,
        SYS_FACCESSAT,
        SYS_FACCESSAT2,
        SYS_FCHMODAT2,
        SYS_STATX,
        SYS_COPY_FILE_RANGE,
        SYS_NAME_TO_HANDLE_AT,
        SYS_OPEN_BY_HANDLE_AT,
        SYS_FALLOCATE,
        SYS_UTIMENSAT,
        SYS_OPENAT,
        SYS_OPENAT2,
        SYS_CLOSE_RANGE,
        SYS_DUP3,
        SYS_PIPE2,
        SYS_EPOLL_CREATE1,
        SYS_EPOLL_PWAIT,
        SYS_EPOLL_PWAIT2,
        SYS_PSELECT6,
        SYS_PPOLL,
        SYS_SET_ROBUST_LIST,
        SYS_GET_ROBUST_LIST,
        SYS_PIDFD_OPEN,
        SYS_PIDFD_GETFD,
        SYS_PIDFD_SEND_SIGNAL,
        SYS_CLONE3,
        SYS_EXECVEAT,
        SYS_WAITID,
        SYS_PROCESS_VM_READV,
        SYS_PROCESS_VM_WRITEV,
        SYS_MEMFD_CREATE,
        SYS_USERFAULTFD,
        SYS_RECVMMSG,
        SYS_SENDMMSG,
        SYS_SOCKETPAIR,
        SYS_ACCEPT4,
        SYS_SECCOMP,
        SYS_PRCTL,
        SYS_ARCH_PRCTL,
        SYS_CAPGET,
        SYS_CAPSET,
        SYS_PIVOT_ROOT,
        SYS_CHROOT,
        SYS_CLOCK_NANOSLEEP,
        SYS_SETTIMEOFDAY,
        SYS_ADJTIMEX,
        SYS_REBOOT,
        SYS_SETHOSTNAME,
        SYS_SETDOMAINNAME,
    ];
    let mut i = 0;
    while i < LINUX_NUMS.len() {
        let mut j = i + 1;
        while j < LINUX_NUMS.len() {
            assert!(LINUX_NUMS[i] != LINUX_NUMS[j]);
            j += 1;
        }
        i += 1;
    }
};

/// 编译期断言: 私有编号区 (400+, 不含 Linux 兼容区) 内部无重复
///
/// 每个逻辑 syscall 只列一个代表值; 下列设计别名 (同编号, 由 dispatch 区分语义)
/// 已从断言中剔除, 避免误判:
///   - `SYS_FB_*` (720-722, 帧缓冲, EG_FB_* 别名已删 B09-17)
///   - `EG_FCHMODAT` == `EG_FCHOWN` (570) / `EG_PIPE2` == `EG_PIPE` (579)
///   - `EG_DUP3` == `EG_DUP2` (581) / `EG_SOCKETPAIR` == `EG_SOCKET` (600)
const _: () = {
    const PRIVATE_NUMS: &[u64] = &[
        SYS_SGEG_LOGIN,
        SYS_SGEG_LOGOUT,
        SYS_SGEG_CREATE_IDENTITY,
        SYS_SGEG_DELETE_IDENTITY,
        SYS_SGEG_IDENTITY_INFO,
        SYS_SGEG_CHANGE_PASSWORD,
        SYS_SGEG_VERIFY_PASSWORD,
        SYS_SGEG_CREATE_FIRST,
        SYS_SGEG_GRANT,
        SYS_SGEG_REVOKE,
        SYS_SGEG_CHECK_CAP,
        SYS_SGEG_GET_CAPS,
        SYS_SGEG_GET_PWM,
        SYS_SGEG_SET_PWM,
        SYS_SGEG_GET_DOMAIN_FLAGS,
        SYS_SGEG_SET_DOMAIN_FLAGS,
        SYS_SGEG_DISK_LIST,
        SYS_SGEG_DISK_INFO,
        SYS_SGEG_DISK_FORMAT,
        SYS_SGEG_DISK_PARTITION,
        SYS_SGEG_DISK_INSTALL,
        SYS_SGEG_FAT_FORMAT,
        SYS_SGEG_PROC_LIST,
        SYS_SGEG_PROC_SETPRI,
        SYS_SGEG_PROC_SLEEP,
        SYS_SGEG_PROC_CPUTIME,
        SYS_SGEG_GETHOSTNAME,
        SYS_SGEG_SETHOSTNAME,
        SYS_SGEG_BOOT_CHECK,
        SYS_SGEG_REBOOT,
        SYS_SGEG_HOTPLUG_STATUS,
        EG_FW_LOAD,
        EG_FW_GET,
        EG_FW_GET_INFO,
        EG_FW_DETACH,
        EG_GET_CANARY,
        EG_FTRACE_ENABLE,
        EG_FTRACE_DISABLE,
        EG_FTRACE_READ,
        EG_FTRACE_STAT,
        EG_KGDB_ENTER,
        EG_ROUTE_ADD,
        EG_ROUTE_DEL,
        EG_ROUTE_QUERY,
        EG_NF_ADD_RULE,
        EG_NF_DEL_RULE,
        EG_IO_URING_SUBMIT,
        EG_CGROUP_CREATE,
        EG_CGROUP_DESTROY,
        EG_CGROUP_ATTACH,
        EG_CGROUP_SET_LIMIT,
        EG_CGROUP_GET_STAT,
        EG_PM,
        EG_SECURE_BOOT,
        EG_TPM,
        EG_CET,
        EG_TICKLESS,
        EG_TIMESYNC,
        EG_UEFI,
        EG_SNAPSHOT_CREATE,
        EG_SNAPSHOT_DESTROY,
        EG_SNAPSHOT_ROLLBACK,
        EG_SNAPSHOT_CLONE,
    ];
    let mut i = 0;
    while i < PRIVATE_NUMS.len() {
        let mut j = i + 1;
        while j < PRIVATE_NUMS.len() {
            assert!(PRIVATE_NUMS[i] != PRIVATE_NUMS[j]);
            j += 1;
        }
        i += 1;
    }
};

// B09-17 (2026-09-14): privileged dispatch 回退层引用的 Linux 编号
/// `SYS_TCGETPGRP` — 获取终端前台进程组
pub const SYS_TCGETPGRP: u64 = 130;
/// `SYS_TCSETPGRP` — 设置终端前台进程组
pub const SYS_TCSETPGRP: u64 = 133;
/// `SYS_SETRLIMIT` — 设置资源限制
pub const SYS_SETRLIMIT: u64 = 160;
/// `SYS_KEXEC_LOAD` — 加载 kexec 内核映像
pub const SYS_KEXEC_LOAD: u64 = 246;
/// `SYS_SPLICE` — 在管道与文件描述符之间拼接数据
pub const SYS_SPLICE: u64 = 275;
/// `SYS_UNSHARE` — 取消共享指定 namespace
pub const SYS_UNSHARE: u64 = 272;
/// `SYS_SETNS` — 加入指定 namespace
pub const SYS_SETNS: u64 = 308;
/// `SYS_BPF` — BPF 程序与映射操作
pub const SYS_BPF: u64 = 321;
/// `SYS_IO_URING_SETUP` — 创建 io_uring 实例
pub const SYS_IO_URING_SETUP: u64 = 425;
/// `SYS_IO_URING_ENTER` — 提交 / 等待 io_uring 完成
pub const SYS_IO_URING_ENTER: u64 = 426;
/// `SYS_IO_URING_REGISTER` — 注册 io_uring 缓冲区或文件
pub const SYS_IO_URING_REGISTER: u64 = 427;
