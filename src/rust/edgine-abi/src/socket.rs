//! Socket 选项常量（`setsockopt`/`getsockopt` 值域，Linux x86_64 asm-generic）.
//!
//! 内核 `privileged::net::init::sm_fi` 的 FFI 分派与用户态 `userlib::sys`
//! 的公开常量均来源于此. `setsockopt`/`getsockopt` 按 (`level`, `optname`)
//! 整数分派、无符号名映射层, 故两侧数值必须逐一吻合 —— 单源引用是本约束的
//! 结构性保障.
//!
//! 范围界定（本轮工程口径, 见 plan S1）: 仅收录 sockopt 分派直接涉及的
//! level 与选项值. sock 类型回值（`SOCK_STREAM`/`SOCK_DGRAM`）、send/recv
//! flags（`MSG_*`）、地址族（`AF_*`）、poll 事件位（`POLL*`）与 errno 族
//! 不在本轮范围, 后续 ABI 族扩张时按模块平行加入.

/// `SOL_SOCKET`: 套接字层 sockopt 的 level（`<asm-generic/socket.h>`）.
pub const SOL_SOCKET: i32 = 1;
/// `IPPROTO_IP`: IPv4 层 sockopt 的 level（兼作 `socket()` protocol 号, 值为 0）.
pub const IPPROTO_IP: i32 = 0;
/// `IPPROTO_TCP`: TCP 层 sockopt 的 level（IANA 协议号）.
pub const IPPROTO_TCP: i32 = 6;
/// `IPPROTO_UDP`: UDP 协议号（`socket()` protocol 参数用）.
pub const IPPROTO_UDP: i32 = 17;
/// `IPPROTO_IPV6`: IPv6 层 sockopt 的 level（兼作 `socket()` protocol 号）.
///
/// 取 **41** —— IANA 给 IPv6 协议号的永久分配, Linux（`<linux/in.h>` /
/// glibc `<netinet/in.h>`）与 BSD 系一致, 非本内核自造值.
pub const IPPROTO_IPV6: i32 = 41;

/// `SO_REUSEADDR`: 允许绑定处于 `TIME_WAIT` 的本地地址.
pub const SO_REUSEADDR: i32 = 2;
/// `SO_TYPE`: 查询 socket 类型（getsockopt 返回 `SOCK_STREAM`/`SOCK_DGRAM`）.
pub const SO_TYPE: i32 = 3;
/// `SO_ERROR`: 查询并清除 socket 的挂起错误状态.
pub const SO_ERROR: i32 = 4;
/// `SO_BROADCAST`: UDP 广播发送开关.
///
/// 本内核当前"接受但忽略"（setsockopt 返 0 不产生行为; getsockopt 未实装,
/// 返 `-ENOPROTOOPT`, 与 `SO_REUSEADDR` 先例一致）—— 裁定见 plan
/// `socket-option-constants-unification.md`.
pub const SO_BROADCAST: i32 = 6;
/// `SO_KEEPALIVE`: TCP 保活开关（内核侧实装, 7200s 缺省间隔对齐 Linux
/// `tcp_keepalive_time`）.
pub const SO_KEEPALIVE: i32 = 9;
/// `SO_REUSEPORT`: 端口复用开关. 本内核接受但忽略（无端口复用调度需求）.
pub const SO_REUSEPORT: i32 = 15;
/// `SO_PASSCRED`: 对端凭据传递开关（UDS 专用, 路由到 functions UDS 服务层）.
pub const SO_PASSCRED: i32 = 16;
/// `SO_RCVTIMEO`: 接收超时（参数 `struct timeval`, 内核存 ns 于 per-slot）.
pub const SO_RCVTIMEO: i32 = 20;
/// `SO_SNDTIMEO`: 发送超时（参数 `struct timeval`, 内核存 ns 于 per-slot）.
pub const SO_SNDTIMEO: i32 = 21;

/// `TCP_NODELAY`: 关闭 Nagle 算法（`IPPROTO_TCP` 层, `<netinet/tcp.h>`）.
pub const TCP_NODELAY: i32 = 1;

/// `IP_ADD_MEMBERSHIP`: 加入 IPv4 组播组（参数 `struct ip_mreq`）.
pub const IP_ADD_MEMBERSHIP: i32 = 35;
/// `IP_DROP_MEMBERSHIP`: 离开 IPv4 组播组（参数 `struct ip_mreq`）.
pub const IP_DROP_MEMBERSHIP: i32 = 36;
/// `IPV6_ADD_MEMBERSHIP`: 加入 IPv6 组播组（参数 `struct ipv6_mreq`）.
///
/// 取 Linux 口径（glibc 将 BSD 的 `IPV6_JOIN_GROUP` 别名到此值）.
pub const IPV6_ADD_MEMBERSHIP: i32 = 20;
/// `IPV6_DROP_MEMBERSHIP`: 离开 IPv6 组播组（参数 `struct ipv6_mreq`）.
///
/// 取 Linux 口径（glibc 将 BSD 的 `IPV6_LEAVE_GROUP` 别名到此值）.
pub const IPV6_DROP_MEMBERSHIP: i32 = 21;
