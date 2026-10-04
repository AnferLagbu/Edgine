//! Socket 协议 wire 类型 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 原定义于 `functions::net::socket` / `functions::net::unix`。按 DECISION-F 服务
//! 对象准则 + DECISION-J 统一判据反转：这些类型是**用户态 ABI 协议 wire 类型**
//! (`AF_INET=2` / `SOCK_STREAM=1` / `struct sockaddr_in` / `struct sockaddr_un`
//! 字节布局), 由 privileged TCB raw 桥接 (`net/syscall.rs::raw_read_sockaddr_in`
//! / `raw_read_sockaddr_un` / `raw_write_sockaddr_un`) 从用户内存 copy-in/copy-out
//! 直接构造并返回——属"机制的安全导出面" (机制的嘴), 留 privileged。
//!
//! 与 fd_alloc/madvise_mlock 同型：privileged 机制持有 + 消费, functions 侧改
//! re-export 保持 API 兼容 (functions→privileged 合法方向)。本文件 0 unsafe。
//!
//! ## 内容
//!
//! - [`Domain`]: socket 协议族 (AF_UNIX/AF_INET/AF_INET6)
//! - [`SockType`]: socket 类型 (SOCK_STREAM/SOCK_DGRAM)
//! - [`SockAddrIn`]: IPv4 地址 (port + 4 字节 IP)
//! - [`SockAddrUn`]: UDS 地址 (sun_path 包装)
//! - [`UNIX_PATH_MAX`]: POSIX `sun_path` 最大长度

// ============================================================================
// 协议族 / 类型
// ============================================================================

/// Socket 协议族
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Domain {
    /// Unix Domain (`AF_UNIX = 1`) — Phase C.3 新增
    Unix = 1,
    /// IPv4 (`AF_INET = 2`)
    Inet = 2,
    /// IPv6 (`AF_INET6 = 10`) — 双栈 (DECISION-032)
    Inet6 = 10,
}

impl Domain {
    pub fn from_i32(d: i32) -> Option<Self> {
        match d {
            1 => Some(Self::Unix),
            2 => Some(Self::Inet),
            10 => Some(Self::Inet6),
            _ => None,
        }
    }
}

/// Socket 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum SockType {
    /// TCP 流 (`SOCK_STREAM = 1`)
    Stream = 1,
    /// UDP 数据报 (`SOCK_DGRAM = 2`)
    Dgram = 2,
}

impl SockType {
    pub fn from_i32(t: i32) -> Option<Self> {
        match t {
            1 => Some(Self::Stream),
            2 => Some(Self::Dgram),
            _ => None,
        }
    }
}

// ============================================================================
// 地址 wire 类型
// ============================================================================

/// IPv4 Socket 地址 (端口 + 4 字节 IP)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SockAddrIn {
    pub port: u16,
    pub ip: [u8; 4],
}

impl SockAddrIn {
    pub fn new(port: u16, ip: [u8; 4]) -> Self {
        Self { port, ip }
    }
}

/// POSIX `sun_path` 最大长度
pub const UNIX_PATH_MAX: usize = 108;

/// `struct sockaddr_un` 包装
#[derive(Debug, Clone, Copy)]
pub struct SockAddrUn {
    pub path: [u8; UNIX_PATH_MAX],
    pub path_len: u16,
}

impl SockAddrUn {
    pub fn new(path: &[u8]) -> Option<Self> {
        if path.is_empty() || path.len() > UNIX_PATH_MAX {
            return None;
        }
        let mut p = [0u8; UNIX_PATH_MAX];
        p[..path.len()].copy_from_slice(path);
        Some(Self {
            path: p,
            path_len: path.len() as u16,
        })
    }

    pub fn path_slice(&self) -> &[u8] {
        &self.path[..self.path_len as usize]
    }
}
