//! Socket 系统调用 — privileged TCB 入口
//!
//! 服务于 `functions::net::syscall` 调用的 raw unsafe 桥接。
//! 实际完成:
//! - 用户空间数据 copy-in/copy-out (依赖 `userptr::validate_user_buf`)
//! - 调 smoltcp 协议栈 (`privileged::net::init::sm`_*)
//!
//! functions 层通过 privileged 层接口访问,本模块是 functions/net/syscall.rs 的 TCB 后端.

use crate::privileged::errno::Errno;
use crate::privileged::mm::{
    copy_from_user as safe_copy_from_user, copy_to_user as safe_copy_to_user,
};
use crate::privileged::net_socket;
use crate::privileged::userptr;

use crate::privileged::net::socket_types::{
    Domain, SOCK_NONBLOCK, SockAddrIn, SockAddrUn, SockType,
};

// ============================================================================
// 用户空间数据搬运 (TCB)
// ============================================================================

/// 从用户空间读 8 字节 `sockaddr_in,返回` (`SockAddrIn`, Errno)
///
/// # Errors
/// 当 `ptr` 无效或无法从用户空间拷贝数据时返回 `Errno::EFAULT`;
/// 当地址族不是 `AF_INET` (2) 时返回 `Errno::EAFNOSUPPORT`.
pub fn raw_read_sockaddr_in(ptr: u64) -> Result<SockAddrIn, Errno> {
    if ptr == 0 || !userptr::validate_user_buf(ptr, 8) {
        return Err(Errno::EFAULT);
    }
    let mut buf = [0u8; 8];
    // P0-I-37 修复: 走异常表保护版 copy_from_user, 用户 munmap 缓冲区时
    // 返回 EFAULT 而非 panic.
    if safe_copy_from_user(&mut buf, ptr, 8).is_err() {
        return Err(Errno::EFAULT);
    }
    // 双栈 (DECISION-032): sin_family 用主机序 (NE) 读取, 与 Linux sa_family_t
    // 一致, 且与 sm_fi.rs::parse_endpoint_trait 的 read_unaligned 读取契约对齐.
    // sin_port 保持 BE (POSIX 网络字节序), 与 sm_fi.rs::parse_endpoint_trait
    // 的 u16::from_be(sin.sin_port) 一致.
    let family = u16::from_ne_bytes([buf[0], buf[1]]);
    if family != 2 {
        return Err(Errno::EAFNOSUPPORT);
    }
    let port = u16::from_be_bytes([buf[2], buf[3]]);
    let mut ip = [0u8; 4];
    ip.copy_from_slice(&buf[4..8]);
    Ok(SockAddrIn { port, ip })
}

/// 从用户空间读 28 字节 `sockaddr_in6` (双栈, DECISION-032), 返回原始字节.
///
/// 调用方 (`sm_bind/sm_connect/sm_sendto`) 内部通过 `parse_endpoint_trait`
/// 按 family 分支解析, 此处仅做 copy-in 与边界校验. 族字段校验由
/// 调用方在分流时经 `raw_read_sun_family` 完成, 与本函数保持一致.
///
/// # Errors
/// 当 `ptr` 无效或无法从用户空间拷贝 28 字节时返回 `Errno::EFAULT`.
pub fn raw_read_sockaddr_in6(ptr: u64) -> Result<[u8; 28], Errno> {
    if ptr == 0 || !userptr::validate_user_buf(ptr, 28) {
        return Err(Errno::EFAULT);
    }
    let mut buf = [0u8; 28];
    if safe_copy_from_user(&mut buf, ptr, 28).is_err() {
        return Err(Errno::EFAULT);
    }
    Ok(buf)
}

/// copy-in 用户空间数据到 `alloc::vec::Vec`
///
/// # Errors
/// 当 `ptr` 无效 (`ptr == 0` 或未通过用户缓冲区校验) 或拷贝失败时返回 `Errno::EFAULT`.
pub fn raw_copy_in(ptr: u64, len: u32) -> Result<alloc::vec::Vec<u8>, Errno> {
    if len == 0 {
        return Ok(alloc::vec::Vec::new());
    }
    if ptr == 0 || !userptr::validate_user_buf(ptr, u64::from(len)) {
        return Err(Errno::EFAULT);
    }
    let mut buf = alloc::vec![0u8; len as usize];
    // P0-I-37 修复: 走异常表保护版
    if safe_copy_from_user(&mut buf, ptr, len as usize).is_err() {
        return Err(Errno::EFAULT);
    }
    Ok(buf)
}

/// copy-out 内核数据到用户空间,返回实际写入字节数
///
/// # Errors
/// 当 `ptr` 无效 (`ptr == 0` 或未通过用户缓冲区校验) 或写入失败时返回 `Errno::EFAULT`.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn raw_copy_out(ptr: u64, len: u32, data: &[u8]) -> Result<u32, Errno> {
    if len == 0 {
        return Ok(0);
    }
    if ptr == 0 || !userptr::validate_user_buf(ptr, u64::from(len)) {
        return Err(Errno::EFAULT);
    }
    let n = data.len().min(len as usize);
    // P0-I-37 修复: 走异常表保护版
    if safe_copy_to_user(ptr, &data[..n], n).is_err() {
        return Err(Errno::EFAULT);
    }
    Ok(n as u32)
}

/// 读取 4 字节 u32
///
/// # Errors
/// 当 `ptr` 无效或未通过用户缓冲区校验时返回 `Errno::EFAULT`.
pub fn raw_read_u32(ptr: u64) -> Result<u32, Errno> {
    if ptr == 0 || !userptr::validate_user_buf(ptr, 4) {
        return Err(Errno::EFAULT);
    }
    // SAFETY: ptr 由 check_user_buf 验证为可读 4 字节
    let v = unsafe { core::ptr::read_unaligned(ptr as *const u32) };
    Ok(v)
}

/// 写入 4 字节 u32
///
/// # Errors
/// 当 `ptr` 无效或未通过用户缓冲区校验时返回 `Errno::EFAULT`.
pub fn raw_write_u32(ptr: u64, v: u32) -> Result<(), Errno> {
    if ptr == 0 || !userptr::validate_user_buf(ptr, 4) {
        return Err(Errno::EFAULT);
    }
    // SAFETY: ptr 由 check_user_buf 验证为可写
    unsafe {
        core::ptr::write_unaligned(ptr as *mut u32, v);
    }
    Ok(())
}

// ============================================================================
// sockaddr_un 解析 (Phase C.3 UDS)
// ============================================================================

/// 从用户指针读 2 字节 `sun_family` (大端 u16)
///
/// # Errors
/// 当 `ptr` 无效或未通过用户缓冲区校验时返回 `Errno::EFAULT`.
pub fn raw_read_sun_family(ptr: u64) -> Result<u16, Errno> {
    if ptr == 0 || !userptr::validate_user_buf(ptr, 2) {
        return Err(Errno::EFAULT);
    }
    // SAFETY: ptr 由 check_user_buf 验证为可读 2 字节
    let lo = unsafe { core::ptr::read_volatile(ptr as *const u8) };
    let hi = unsafe { core::ptr::read_volatile((ptr + 1) as *const u8) };
    Ok(u16::from_be_bytes([hi, lo]))
}

/// 从用户指针读 `sockaddr_un` (110 字节布局: family u16 + path`[108]`)
///
/// 返回 (`path_bytes`, `path_len)。若用户提供的` addrlen 不足 2 字节返回 EFAULT。
///
/// # Errors
/// 当 `ptr` 无效、`addrlen` 不足 2 字节或拷贝失败时返回 `Errno::EFAULT`;
/// 当 `sun_family` 不是 `AF_UNIX` (1) 时返回 `Errno::EAFNOSUPPORT`;
/// 当路径以 NUL 开头 (空路径) 时返回 `Errno::EINVAL`.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn raw_read_sockaddr_un(ptr: u64, addrlen: u32) -> Result<SockAddrUn, Errno> {
    if ptr == 0 || addrlen < 2 {
        return Err(Errno::EFAULT);
    }
    if !userptr::validate_user_buf(ptr, u64::from(addrlen)) {
        return Err(Errno::EFAULT);
    }
    let mut buf = [0u8; 110];
    let copy_len = (addrlen as usize).min(110);
    // P0-I-37 修复: 走异常表保护版
    if safe_copy_from_user(&mut buf[..copy_len], ptr, copy_len).is_err() {
        return Err(Errno::EFAULT);
    }
    // 校验 family = AF_UNIX = 1
    let family = u16::from_be_bytes([buf[0], buf[1]]);
    if family != 1 {
        return Err(Errno::EAFNOSUPPORT);
    }
    // 路径从 offset 2 开始, 到第一个 NUL 或末尾
    let path_bytes = &buf[2..copy_len];
    let nul_pos = path_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(path_bytes.len());
    if nul_pos == 0 {
        return Err(Errno::EINVAL);
    }
    let mut path = [0u8; 108];
    path[..nul_pos].copy_from_slice(&path_bytes[..nul_pos]);
    Ok(SockAddrUn {
        path,
        path_len: nul_pos as u16,
    })
}

/// 向用户指针写 `sockaddr_un` (110 字节布局)
///
/// # Errors
/// 当 `ptr`/`addrlen_ptr` 无效或任一用户缓冲区校验失败时返回 `Errno::EFAULT`.
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn raw_write_sockaddr_un(ptr: u64, addrlen_ptr: u64, addr: &SockAddrUn) -> Result<(), Errno> {
    if ptr == 0 || addrlen_ptr == 0 {
        return Err(Errno::EFAULT);
    }
    if !userptr::validate_user_buf(ptr, 110) {
        return Err(Errno::EFAULT);
    }
    if !userptr::validate_user_buf(addrlen_ptr, 4) {
        return Err(Errno::EFAULT);
    }
    let mut buf = [0u8; 110];
    buf[0..2].copy_from_slice(&(1u16).to_be_bytes()); // AF_UNIX
    let n = addr.path_len as usize;
    buf[2..2 + n].copy_from_slice(&addr.path[..n]);
    // P0-I-37 修复: 走异常表保护版
    if safe_copy_to_user(ptr, &buf, 110).is_err() {
        return Err(Errno::EFAULT);
    }
    // 写回 addrlen = 110
    let total = (n + 2) as u32;
    let total_bytes = total.to_ne_bytes();
    if safe_copy_to_user(addrlen_ptr, &total_bytes, 4).is_err() {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

// ============================================================================
// Socket 12 Syscall TCB 实现
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// socket — 创建 socket
pub fn socket_syscall(domain: i32, sock_type: i32, _protocol: i32) -> i64 {
    let d = match Domain::from_i32(domain) {
        Some(x) => x,
        None => return Errno::EAFNOSUPPORT.as_ret(),
    };
    // D10 (DECISION-092): SOCK_NONBLOCK 是 type 参数的标志位, 非 socket 类型本身.
    // 校验前剥离本位, 只对基础类型过 SockType; 创建成功后据此置 per-slot 非阻塞标志.
    let nonblock = sock_type & SOCK_NONBLOCK != 0;
    let base_type = sock_type & !SOCK_NONBLOCK;
    let t = match SockType::from_i32(base_type) {
        Some(x) => x,
        None => return Errno::EINVAL.as_ret(),
    };
    let fd = net_socket::sm_socket(d as i32, t as i32, 0);
    if fd < 0 {
        Errno::EINVAL.as_ret()
    } else {
        if nonblock {
            net_socket::sm_set_nonblocking(fd, true);
        }
        i64::from(fd)
    }
}

/// bind — 绑定本地地址到 socket (按 sockaddr 族分流 IPv4/IPv6)
pub fn bind_syscall(fd: i32, addr_ptr: u64, _addrlen: u32) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    // 双栈 (DECISION-032): 按 sockaddr 族分流 — 2 = AF_INET, 10 = AF_INET6
    let family = match raw_read_sun_family(addr_ptr) {
        Ok(f) => f,
        Err(e) => return e.as_ret(),
    };
    let rc = match family {
        2 => {
            let addr = match raw_read_sockaddr_in(addr_ptr) {
                Ok(a) => a,
                Err(e) => return e.as_ret(),
            };
            // 双栈 (DECISION-032): sin_family 用主机序 (NE), 与 sm_fi.rs
            // parse_endpoint_trait 的 read_unaligned 读取一致. sin_port 保持
            // BE (POSIX), 与 sm_fi.rs 的 u16::from_be(sin.sin_port) 一致.
            let mut bytes = [0u8; 8];
            bytes[0..2].copy_from_slice(&(2u16).to_ne_bytes());
            bytes[2..4].copy_from_slice(&addr.port.to_be_bytes());
            bytes[4..8].copy_from_slice(&addr.ip);
            net_socket::sm_bind(fd, bytes.as_ptr(), 8)
        }
        10 => {
            let buf = match raw_read_sockaddr_in6(addr_ptr) {
                Ok(b) => b,
                Err(e) => return e.as_ret(),
            };
            net_socket::sm_bind(fd, buf.as_ptr(), 28)
        }
        _ => return Errno::EAFNOSUPPORT.as_ret(),
    };
    i64::from(rc)
}

/// listen — 将 socket 置为监听状态并设置连接队列长度
pub fn listen_syscall(fd: i32, backlog: i32) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    if backlog < 0 {
        return Errno::EINVAL.as_ret();
    }
    let rc = net_socket::sm_listen(fd, backlog);
    i64::from(rc)
}

/// accept — 接受连接, 并把对端地址回写到 `addr`/`addrlen` (二者可为 0)。
pub fn accept_syscall(fd: i32, addr_ptr: u64, addrlen_ptr: u64) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    // addr/addrlen 非 0 时须为有效可写用户指针; sm_accept 校验并直写 (同 getsockname 约定).
    let rc = net_socket::sm_accept(fd, addr_ptr as *mut u8, addrlen_ptr as *mut u32);
    i64::from(rc)
}

/// connect — 连接远端地址 (按 sockaddr 族分流 IPv4/IPv6)
pub fn connect_syscall(fd: i32, addr_ptr: u64, _addrlen: u32) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    // 双栈 (DECISION-032): 按 sockaddr 族分流 — 2 = AF_INET, 10 = AF_INET6
    let family = match raw_read_sun_family(addr_ptr) {
        Ok(f) => f,
        Err(e) => return e.as_ret(),
    };
    let rc = match family {
        2 => {
            let addr = match raw_read_sockaddr_in(addr_ptr) {
                Ok(a) => a,
                Err(e) => return e.as_ret(),
            };
            // 双栈 (DECISION-032): sin_family 用主机序 (NE), sin_port 保持 BE.
            let mut bytes = [0u8; 8];
            bytes[0..2].copy_from_slice(&(2u16).to_ne_bytes());
            bytes[2..4].copy_from_slice(&addr.port.to_be_bytes());
            bytes[4..8].copy_from_slice(&addr.ip);
            net_socket::sm_connect(fd, bytes.as_ptr(), 8)
        }
        10 => {
            let buf = match raw_read_sockaddr_in6(addr_ptr) {
                Ok(b) => b,
                Err(e) => return e.as_ret(),
            };
            net_socket::sm_connect(fd, buf.as_ptr(), 28)
        }
        _ => return Errno::EAFNOSUPPORT.as_ret(),
    };
    i64::from(rc)
}

/// sendto / send — 发送数据, 指定目标地址时按族分流, 否则走已连接路径
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn sendto_syscall(
    fd: i32,
    buf_ptr: u64,
    len: u32,
    _flags: i32,
    dest_ptr: u64,
    _dest_len: u32,
) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    let data = match raw_copy_in(buf_ptr, len) {
        Ok(v) => v,
        Err(e) => return e.as_ret(),
    };
    let rc = if dest_ptr == 0 {
        net_socket::sm_send(fd, data.as_ptr(), data.len() as u32, 0)
    } else {
        // 双栈 (DECISION-032): 按 sockaddr 族分流 — 2 = AF_INET, 10 = AF_INET6
        let family = match raw_read_sun_family(dest_ptr) {
            Ok(f) => f,
            Err(e) => return e.as_ret(),
        };
        match family {
            2 => {
                let dest = match raw_read_sockaddr_in(dest_ptr) {
                    Ok(a) => a,
                    Err(e) => return e.as_ret(),
                };
                // 双栈 (DECISION-032): sin_family 用主机序 (NE), sin_port 保持 BE.
                let mut bytes = [0u8; 8];
                bytes[0..2].copy_from_slice(&(2u16).to_ne_bytes());
                bytes[2..4].copy_from_slice(&dest.port.to_be_bytes());
                bytes[4..8].copy_from_slice(&dest.ip);
                net_socket::sm_sendto(fd, data.as_ptr(), data.len() as u32, 0, bytes.as_ptr(), 8)
            }
            10 => {
                let buf = match raw_read_sockaddr_in6(dest_ptr) {
                    Ok(b) => b,
                    Err(e) => return e.as_ret(),
                };
                net_socket::sm_sendto(fd, data.as_ptr(), data.len() as u32, 0, buf.as_ptr(), 28)
            }
            _ => return Errno::EAFNOSUPPORT.as_ret(),
        }
    };
    i64::from(rc)
}

/// recvfrom / recv — 接收数据到用户缓冲区, 并回写对端地址 (D9).
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn recvfrom_syscall(
    fd: i32,
    buf_ptr: u64,
    len: u32,
    _flags: i32,
    src_ptr: u64,
    src_len_ptr: u64,
) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    if buf_ptr == 0 || len == 0 {
        return Errno::EFAULT.as_ret();
    }
    if !userptr::validate_user_buf(buf_ptr, u64::from(len)) {
        return Errno::EFAULT.as_ret();
    }
    // 在栈上准备临时缓冲
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    const MAX: usize = 4096;
    let want = (len as usize).min(MAX);
    let mut stack_buf = [0u8; MAX];
    // D9: 改调 sm_recvfrom 取回对端地址; data 仍走内核栈 bounce (保 P0-I-37 异常表保护),
    // 对端 sockaddr 按 accept/getsockname 约定由 sm 层直写用户 src_ptr (可为 0, null-guard 已备).
    let n = net_socket::sm_recvfrom(
        fd,
        stack_buf.as_mut_ptr(),
        want as u32,
        0,
        src_ptr as *mut u8,
        src_len_ptr as *mut u32,
    );
    if n < 0 {
        return i64::from(n);
    }
    // P0-I-37 修复: 走异常表保护版
    if safe_copy_to_user(buf_ptr, &stack_buf[..n as usize], n as usize).is_err() {
        return Errno::EFAULT.as_ret();
    }
    i64::from(n)
}

/// setsockopt — 设置 socket 选项 (按 valen 全长 copy-in, 覆盖 i32 与 timeval 选项)
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub fn setsockopt_syscall(fd: i32, level: i32, optname: i32, val_ptr: u64, valen: u32) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    // P5d (DECISION-096): 按 valen 全长 copy-in (上限 16 字节 — 覆盖 i32 选项 (4B)
    // 与 SO_RCVTIMEO/SO_SNDTIMEO 的 struct timeval (16B)); 旧实现硬读 4 字节, 无法
    // 承载 timeval. 定长内核栈缓冲, 免堆分配.
    let len = (valen as usize).min(16);
    let mut buf = [0u8; 16];
    if len > 0 {
        if val_ptr == 0 || !userptr::validate_user_buf(val_ptr, len as u64) {
            return Errno::EFAULT.as_ret();
        }
        // copy_from_user 内部走异常表保护 (P0-I-37), 用户 munmap 时返 EFAULT 而非 panic.
        if safe_copy_from_user(&mut buf[..len], val_ptr, len).is_err() {
            return Errno::EFAULT.as_ret();
        }
    }
    let rc = net_socket::sm_setsockopt(fd, level, optname, buf.as_ptr(), len as u32);
    i64::from(rc)
}

/// getsockopt — 获取 socket 选项 (按用户 *optlen 容量回填, 覆盖 i32 与 timeval 选项)
pub fn getsockopt_syscall(fd: i32, level: i32, optname: i32, val_ptr: u64, optlen_ptr: u64) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    // P5d (DECISION-096): 长度感知 — 读用户 *optlen (socklen_t) 作缓冲容量, 上限
    // 16 字节 (i32 选项 4B / SO_RCVTIMEO/SNDTIMEO timeval 16B). 旧实现硬写 4 字节,
    // 无法回填 timeval.
    let cap = match raw_read_u32(optlen_ptr) {
        Ok(x) => x,
        Err(e) => return e.as_ret(),
    };
    let in_len = cap.min(16);
    if in_len == 0 {
        return Errno::EINVAL.as_ret();
    }
    let mut buf = [0u8; 16];
    let mut out_len = in_len;
    let rc = net_socket::sm_getsockopt(
        fd,
        level,
        optname,
        buf.as_mut_ptr(),
        core::ptr::from_mut(&mut out_len),
    );
    if rc != 0 {
        return i64::from(rc);
    }
    // copy-out 实际长度 (out_len ≤ 16) 到用户缓冲区; raw_copy_out 内 validate + 异常表保护.
    if let Err(e) = raw_copy_out(val_ptr, out_len, &buf) {
        return e.as_ret();
    }
    // 回写 *optlen = 实际选项长度 (POSIX 语义).
    if let Err(e) = raw_write_u32(optlen_ptr, out_len) {
        return e.as_ret();
    }
    0
}

/// shutdown(fd, how) — 半关闭 socket (D7: 透传 how 到 sm_shutdown)
pub fn shutdown_syscall(fd: i32, how: i32) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    let rc = net_socket::sm_shutdown(fd, how);
    i64::from(rc)
}

/// sendmsg(fd, msg, flags) — functions 层入口
pub fn sendmsg_syscall(fd: i32, msg_ptr: u64, _flags: i32) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    let rc = net_socket::sm_sendmsg(fd, msg_ptr as *const u8, 0);
    i64::from(rc)
}

/// recvmsg(fd, msg, flags) — functions 层入口
pub fn recvmsg_syscall(fd: i32, msg_ptr: u64, _flags: i32) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    let rc = net_socket::sm_recvmsg(fd, msg_ptr as *mut u8, 0);
    i64::from(rc)
}

/// getsockname(fd, addr, addrlen) — 获取本端地址
pub fn getsockname_syscall(fd: i32, addr_ptr: u64, addrlen_ptr: u64) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    let rc = net_socket::sm_getsockname(fd, addr_ptr as *mut u8, addrlen_ptr as *mut u32);
    i64::from(rc)
}

/// getpeername(fd, addr, addrlen) — 获取对端地址
pub fn getpeername_syscall(fd: i32, addr_ptr: u64, addrlen_ptr: u64) -> i64 {
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }
    let rc = net_socket::sm_getpeername(fd, addr_ptr as *mut u8, addrlen_ptr as *mut u32);
    i64::from(rc)
}
