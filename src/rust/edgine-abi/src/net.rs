//! 网络地址解析私有 syscall ABI 常量与结果结构.
//!
//! 值域为 Edgine 私有功能段 `812-814`：Linux 的 `getaddrinfo` 是 libc 函数
//! (经 socket/sendto/recvfrom 访问 DNS, 并读 resolv.conf / hosts / NSS), 并无
//! 对应的系统调用号. 占用 0-299 的 Linux 号位既无兼容收益, 又破坏"0-299 ==
//! Linux"的承重不变式, 故按私有段取值 (与 `edgine_abi::wasm` 同构).
//!
//! 内核 `privileged::syscall::types` 的编号定义、`privileged::net::syscall`
//! 的结果写出与用户态入口桩 (`userlib::sys`) 均引用本模块, 结构上避免
//! "内核与用户态各写一份常量"的漂移.

/// `EG_GETADDRINFO` — 双栈 DNS 解析 (返回单个 [`EgAddrInfo`], IPv6 优先).
///
/// 参数: (`name_ptr`: u64 指向主机名字节, `name_len`: u64 名字长度,
/// `out_ptr`: u64 指向 [`EgAddrInfo`]). 成功返 0 并写出结果; 名字无法解析
/// 返 `-ENOENT`; 用户缓冲非法返 `-EFAULT`; 名字非 UTF-8 返 `-EINVAL`.
/// 解析采 IPv6 优先 (AAAA 成功返 V6, 否则 A 返 V4), 与内核 `dns_resolve` 同源.
pub const EG_GETADDRINFO: u64 = 812;

/// 地址族标签: IPv4 (`AF_INET`), 与 Linux `asm-generic` 值域一致.
pub const EG_AF_INET: u32 = 2;

/// 地址族标签: IPv6 (`AF_INET6`), 与 Linux `asm-generic` 值域一致.
pub const EG_AF_INET6: u32 = 10;

/// DNS 解析结果 (`EG_GETADDRINFO` 的写出结构, 双栈单地址).
///
/// 内核 `dns_resolve` 每次返单个 `IpAddr` (smoltcp `DNS_MAX_RESULT_COUNT=1`,
/// IPv6 优先), 故本结构承载单地址而非 POSIX `addrinfo` 链表. `addr` 按网络序
/// 存放: IPv4 用前 4 字节 (余 12 字节为 0), IPv6 用全 16 字节.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EgAddrInfo {
    /// 地址族: [`EG_AF_INET`] (2) = IPv4, [`EG_AF_INET6`] (10) = IPv6.
    pub family: u32,
    /// 地址字节 (网络序): IPv4 前 4 字节有效, IPv6 全 16 字节有效.
    pub addr: [u8; 16],
}
