#![deny(unsafe_code)]
//! Socket 系统调用 — functions 层入口代理
//!
//! ## 职责
//!
//! - 0 unsafe,纯类型安全
//! - 委托 privileged/net/syscall.rs 完成用户空间数据搬运 + smoltcp 协议栈调用
//!
//! ## 与 [`mod@super::socket`] 区别
//!
//! - [`mod@super::socket`] 强类型 API (Domain, `SockType`, `SockAddrIn`, `&[u8]`)
//! - 本模块 syscall 入口 API (i32, u64 用户指针, u32 长度)
//!
//! ## fd 对象模型 (阶段 3 第 4 个迁移子系统)
//!
//! smoltcp socket 与 `AF_UNIX` socket 均由 fd 对象承载并挂在 `OpenFileTable`:
//! 用户 fd 经 per-process fd 表 → `handle_id` → 对象, 对象只持协议栈/策略表
//! 槽位编号. 本模块的每个 socket 系统调用经 `SocketObject::resolve` 快照对象
//! 类型与 `(槽位, 非阻塞)` 并在调用期间以引用计数钉住对象 (槽位不会被并发
//! close 回收), 再按对象类型委托 privileged 入口或 functions UDS 策略.
//! 阻塞语义由 `OpenFile::nonblock` + `MSG_DONTWAIT` 合成.

use super::socket_file::SocketFile;
use super::unix as uds;
use super::unix_socket_file::UnixSocketFile;
use crate::functions::fs::{OPEN_FILE_TABLE, vfs_get_fd_handle};
use crate::functions::syscall::types::{SO_PASSCRED, SOL_SOCKET};
use crate::privileged::net::socket_types::SOCK_NONBLOCK;
use crate::privileged::net::syscall as fw;
use crate::privileged::syscall::Errno;
use crate::privileged::syscall::api;

/// `MSG_DONTWAIT` (Linux 0x40): 仅本次调用非阻塞, 不改变 socket 自身阻塞属性
const MSG_DONTWAIT: i32 = 0x40;

// ============================================================================
// socket fd 对象解析守卫
// ============================================================================

/// 已解析的 socket fd 对象 — 持有对象引用直至本次系统调用结束
///
/// `resolve` 在 `OpenFileTable` 锁内完成对象下转型与 `inc_ref`, `Drop` 时
/// `dec_ref`: 并发 close 只会降引用计数, 槽位在本调用期间不被回收.
enum SocketObject {
    /// smoltcp (`AF_INET` / `AF_INET6`) socket
    Inet {
        /// 对象在 `OpenFileTable` 的句柄
        handle_id: u32,
        /// 协议栈槽位编号
        slot: i32,
        /// fd 级非阻塞标志快照 (`OpenFile::nonblock`)
        nonblock: bool,
    },
    /// `AF_UNIX` socket (functions 层策略表槽位)
    Unix {
        /// 对象在 `OpenFileTable` 的句柄
        handle_id: u32,
        /// 策略表槽位下标
        idx: u8,
        /// fd 级非阻塞标志快照 (`OpenFile::nonblock`)
        nonblock: bool,
    },
}

impl SocketObject {
    /// 由用户 fd 解析对象: fd 无效 → `EBADF`, 非 socket 对象 → `ENOTSOCK`
    fn resolve(fd: i32) -> Result<Self, Errno> {
        let fd_idx = usize::try_from(fd).map_err(|_| Errno::Ebadf)?;
        let Some(handle_id) = vfs_get_fd_handle(fd_idx) else {
            return Err(Errno::Ebadf);
        };
        let resolved = OPEN_FILE_TABLE.with_file(handle_id, |of| {
            let file = of.file();
            let nonblock = of.is_nonblock();
            if let Some(socket) = file.as_any().downcast_ref::<SocketFile>() {
                let slot = socket.slot();
                of.inc_ref();
                return Some(Self::Inet {
                    handle_id,
                    slot,
                    nonblock,
                });
            }
            if let Some(unix) = file.as_any().downcast_ref::<UnixSocketFile>() {
                let idx = unix.idx();
                of.inc_ref();
                return Some(Self::Unix {
                    handle_id,
                    idx,
                    nonblock,
                });
            }
            None
        });
        match resolved {
            None => Err(Errno::Ebadf),
            Some(None) => Err(Errno::Enotsock),
            Some(Some(object)) => Ok(object),
        }
    }

    /// fd 级非阻塞标志快照
    fn nonblock(&self) -> bool {
        match self {
            Self::Inet { nonblock, .. } | Self::Unix { nonblock, .. } => *nonblock,
        }
    }

    /// 本次调用是否允许阻塞: 对象非阻塞位与 `MSG_DONTWAIT` 按与关系合成
    fn blocks(&self, flags: i32) -> bool {
        !self.nonblock() && (flags & MSG_DONTWAIT) == 0
    }

    /// UDS 策略表槽位 (非 UDS 对象返回 `None`)
    fn unix_idx(&self) -> Option<u8> {
        match self {
            Self::Unix { idx, .. } => Some(*idx),
            Self::Inet { .. } => None,
        }
    }

    /// smoltcp 协议栈槽位
    // SIMPLIFIED: UDS 对象未实现 inet-only 操作面 (getsockopt/getsockname/
    // getpeername/shutdown); 迁移前 UDS fd 不在 VFS 表, 同路径返 EBADF.
    // 影响: 现以 ENOTSUP 区分"是 socket 但不支持该命令"; 何时需扩展: UDS 补齐
    // 这些命令的语义时按对象类型分派到 UDS 策略.
    fn inet_slot(&self) -> Result<i32, Errno> {
        match self {
            Self::Inet { slot, .. } => Ok(*slot),
            Self::Unix { .. } => Err(Errno::Enotsup),
        }
    }
}

impl Drop for SocketObject {
    fn drop(&mut self) {
        let handle_id = match self {
            Self::Inet { handle_id, .. } | Self::Unix { handle_id, .. } => *handle_id,
        };
        OPEN_FILE_TABLE.dec_ref(handle_id);
    }
}

/// privileged 原生返回值 (`i64`, 负值为 `-errno`) → `Result<usize, Errno>`
fn ret_to_result(r: i64) -> Result<usize, Errno> {
    if r < 0 {
        Err(Errno::from_ret(r))
    } else {
        Ok(crate::functions::intcast::usize_from_i64(r))
    }
}

/// UDS 策略错误 → errno (`UnixSocketError` 为映射单一来源)
fn uds_errno(e: uds::UdsError) -> Errno {
    super::unix::UnixSocketError::from(e).to_errno()
}

// ============================================================================
// 12 个 Socket Syscall 安全代理
// ============================================================================

/// socket(domain, type, protocol) — 返回新 FD
///
/// 分流: `AF_UNIX(1)` → UDS 子系统; `AF_INET(2)` → smoltcp 协议栈; 其他 → EAFNOSUPPORT
///
/// # Errors
///
/// 当 `AF_UNIX` 的 `sock_type` 非 Stream/Dgram 时返回 `Err(Errno::Einval)`; 不支持的协议族返回
/// `Err(Errno::Eafnosupport)`; 底层创建失败时返回对应的 `Errno`; 对象或 fd 表耗尽返回 `Err(Errno::Emfile)`。
pub fn socket_syscall(domain: i32, sock_type: i32, protocol: i32) -> Result<usize, Errno> {
    let nonblock = (sock_type & SOCK_NONBLOCK) != 0;
    // AF_UNIX 分流
    if domain == 1 {
        let st = uds::UnixSockType::from_i32(sock_type & !SOCK_NONBLOCK).ok_or(Errno::Einval)?;
        return UnixSocketFile::create(st, nonblock, false);
    }
    // AF_INET (smoltcp): privileged 返回槽位编号, fd 由本层分配
    let r = fw::socket_syscall(domain, sock_type, protocol);
    if r < 0 {
        return Err(Errno::from_ret(r));
    }
    let slot = i32::try_from(r).map_err(|_| Errno::Einval)?;
    SocketFile::create(slot, nonblock, false)
}

/// bind(fd, addr, addrlen)
///
/// 分流: 读取 `sun_family` 决定走 UDS 或 smoltcp
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; 用户地址不可读时返回 `Err(Errno::Efault)`; 不支持的协议族返回
/// `Err(Errno::Eafnosupport)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`; 底层 bind 失败时返回对应的 `Errno`。
pub fn bind_syscall(fd: i32, addr_ptr: u64, addrlen: u32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    // Peek sun_family
    let family = fw::raw_read_sun_family(addr_ptr)?;
    match family {
        1 => {
            // AF_UNIX
            let addr = fw::raw_read_sockaddr_un(addr_ptr, addrlen)?;
            let obj = SocketObject::resolve(fd)?;
            let idx = obj.unix_idx().ok_or(Errno::Einval)?;
            uds::uds_bind(idx, addr.path_slice())
                .map(|()| 0)
                .map_err(uds_errno)
        }
        2 | 10 => {
            // AF_INET / AF_INET6 (双栈, DECISION-032)
            let obj = SocketObject::resolve(fd)?;
            ret_to_result(fw::bind_syscall(obj.inet_slot()?, addr_ptr, addrlen))
        }
        _ => Err(Errno::Eafnosupport),
    }
}

/// listen(fd, backlog)
///
/// 分流: UDS FD 走 `uds::listen`, smoltcp FD 走 `fw::listen`
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; 当 `backlog` 为负数时返回 `Err(Errno::Einval)`;
/// fd 非 socket 时返回 `Err(Errno::Enotsock)`; 底层 listen 失败时返回对应的 `Errno`。
pub fn listen_syscall(fd: i32, backlog: i32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    if backlog < 0 {
        return Err(Errno::Einval);
    }
    let obj = SocketObject::resolve(fd)?;
    if let Some(idx) = obj.unix_idx() {
        return uds::uds_listen(idx).map(|()| 0).map_err(uds_errno);
    }
    ret_to_result(fw::listen_syscall(obj.inet_slot()?, backlog))
}

/// accept(fd, addr, addrlen)
///
/// 分流: UDS FD 走 `uds::accept`, smoltcp FD 走 `fw::accept`
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 accept 失败 (如无待处理连接) 时返回对应的 `Errno`。
pub fn accept_syscall(fd: i32, addr_ptr: u64, addrlen_ptr: u64) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let obj = SocketObject::resolve(fd)?;
    if let Some(idx) = obj.unix_idx() {
        // 新连接 fd 不继承监听端的 O_NONBLOCK 与 CLOEXEC (POSIX accept 语义)
        return UnixSocketFile::create_accepted(idx, false, false);
    }
    let r = fw::accept_syscall(obj.inet_slot()?, addr_ptr, addrlen_ptr, !obj.nonblock());
    if r < 0 {
        return Err(Errno::from_ret(r));
    }
    let slot = i32::try_from(r).map_err(|_| Errno::Einval)?;
    // 新连接 fd 不继承监听端的 O_NONBLOCK 与 CLOEXEC (POSIX accept 语义)
    SocketFile::create(slot, false, false)
}

/// connect(fd, addr, addrlen)
///
/// 分流: 读取 `sun_family` 决定走 UDS 或 smoltcp
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; 用户地址不可读时返回 `Err(Errno::Efault)`; 不支持的协议族返回
/// `Err(Errno::Eafnosupport)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`; 底层 connect 失败时返回对应的 `Errno`。
pub fn connect_syscall(fd: i32, addr_ptr: u64, addrlen: u32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let family = fw::raw_read_sun_family(addr_ptr)?;
    match family {
        1 => {
            let addr = fw::raw_read_sockaddr_un(addr_ptr, addrlen)?;
            let obj = SocketObject::resolve(fd)?;
            let idx = obj.unix_idx().ok_or(Errno::Einval)?;
            uds::uds_connect(idx, addr.path_slice())
                .map(|()| 0)
                .map_err(uds_errno)
        }
        2 | 10 => {
            // AF_INET / AF_INET6 (双栈, DECISION-032)
            let obj = SocketObject::resolve(fd)?;
            ret_to_result(fw::connect_syscall(
                obj.inet_slot()?,
                addr_ptr,
                addrlen,
                !obj.nonblock(),
            ))
        }
        _ => Err(Errno::Eafnosupport),
    }
}

/// `sendto` 系统调用 — 签名 `(fd, buf, len, flags, dest_addr, addrlen)`
///
/// 分流: UDS FD + `AF_UNIX` dest → `uds::sendto`; 其他 → `fw::sendto`
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; UDS 路径未提供目标地址时返回 `Err(Errno::Edestaddrreq)`;
/// 用户缓冲区不可读时返回 `Err(Errno::Efault)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 sendto 失败时返回对应的 `Errno`。
pub fn sendto_syscall(
    fd: i32,
    buf_ptr: u64,
    len: u32,
    flags: i32,
    dest_ptr: u64,
    dest_len: u32,
) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let obj = SocketObject::resolve(fd)?;
    if obj.unix_idx().is_some() {
        if dest_ptr == 0 {
            return Err(Errno::Edestaddrreq);
        }
        // copy-in 用户数据
        let data = fw::raw_copy_in(buf_ptr, len)?;
        // 解析 sockaddr_un
        let addr = fw::raw_read_sockaddr_un(dest_ptr, dest_len)?;
        return uds::uds_sendto(&data, addr.path_slice()).map_err(uds_errno);
    }
    ret_to_result(fw::sendto_syscall(
        obj.inet_slot()?,
        buf_ptr,
        len,
        flags,
        dest_ptr,
        dest_len,
        obj.blocks(flags),
    ))
}

/// `recvfrom` 系统调用 — 签名 `(fd, buf, len, flags, src_addr, addrlen)`
///
/// 分流: UDS FD → copy-out 到用户缓冲, 忽略 `src_addr` 填写 (v1 简化)
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; 用户缓冲区无效时返回 `Err(Errno::Efault)`;
/// fd 非 socket 时返回 `Err(Errno::Enotsock)`; 底层 recvfrom 失败时返回对应的 `Errno`。
pub fn recvfrom_syscall(
    fd: i32,
    buf_ptr: u64,
    len: u32,
    flags: i32,
    src_ptr: u64,
    src_len_ptr: u64,
) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    if buf_ptr == 0 || len == 0 {
        return Err(Errno::Efault);
    }
    let obj = SocketObject::resolve(fd)?;
    if let Some(idx) = obj.unix_idx() {
        if !api::validate_user_buf(buf_ptr, u64::from(len)) {
            return Err(Errno::Efault);
        }
        // 栈上缓冲接收, 再 copy-out (走 TCB raw_copy_out, 0 unsafe)
        let mut stack_buf = alloc::vec![0u8; len as usize];
        let sock_type = uds::uds_sock_type(idx).map_err(uds_errno)?;
        let n = match sock_type {
            uds::UnixSockType::Stream => uds::uds_recv(idx, &mut stack_buf),
            uds::UnixSockType::Dgram => uds::uds_recvfrom(idx, &mut stack_buf),
        }
        .map_err(uds_errno)?;
        if n > 0 {
            fw::raw_copy_out(
                buf_ptr,
                crate::functions::intcast::low_u32_from_usize(n),
                &stack_buf[..n],
            )?;
        }
        // v1 简化: 不填 src_addr / addrlen (POSIX 允许)
        let _ = (src_ptr, src_len_ptr);
        return Ok(n);
    }
    ret_to_result(fw::recvfrom_syscall(
        obj.inet_slot()?,
        buf_ptr,
        len,
        flags,
        src_ptr,
        src_len_ptr,
        obj.blocks(flags),
    ))
}

/// setsockopt(fd, level, optname, val, valen)
///
/// `SOL_SOCKET/SO_PASSCRED` 是 UDS 专属选项, 由本层按 fd 分流到
/// [`uds::uds_setsockopt`] (privileged 侧不再持有 UDS 策略钩子);
/// 其余选项解析 socket 对象后按槽位委托 privileged.
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; `SO_PASSCRED` 的取值不可读时返回
/// `Err(Errno::Efault)`、长度不足 4 字节返回 `Err(Errno::Einval)`、fd 非 UDS 时返回
/// `Err(Errno::Ebadf)`; 其他选项在 fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 setsockopt 失败时返回对应的 `Errno`。
pub fn setsockopt_syscall(
    fd: i32,
    level: i32,
    optname: i32,
    val_ptr: u64,
    valen: u32,
) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    // v2 SO_PASSCRED 分流: 保持旧语义次序 — 先 copy-in (EFAULT), 再长度校验
    // (EINVAL), 最后按对象类型分流 (非 UDS socket → EBADF)
    if level == SOL_SOCKET && optname == SO_PASSCRED {
        let len = (valen as usize).min(20);
        let buf = fw::raw_copy_in(val_ptr, crate::functions::intcast::low_u32_from_usize(len))?;
        if len < 4 {
            return Err(Errno::Einval);
        }
        let enable = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) != 0;
        let obj = SocketObject::resolve(fd)?;
        let idx = obj.unix_idx().ok_or(Errno::Ebadf)?;
        let rc = uds::uds_setsockopt(idx, enable);
        return ret_to_result(i64::from(rc));
    }
    let obj = SocketObject::resolve(fd)?;
    ret_to_result(fw::setsockopt_syscall(
        obj.inet_slot()?,
        level,
        optname,
        val_ptr,
        valen,
    ))
}

/// getsockopt(fd, level, optname, val, valen)
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 getsockopt 失败时返回对应的 `Errno`。
pub fn getsockopt_syscall(
    fd: i32,
    level: i32,
    optname: i32,
    val_ptr: u64,
    valen_ptr: u64,
) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let obj = SocketObject::resolve(fd)?;
    ret_to_result(fw::getsockopt_syscall(
        obj.inet_slot()?,
        level,
        optname,
        val_ptr,
        valen_ptr,
    ))
}

/// getsockname(fd, addr, addrlen) — 获取本端地址
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 getsockname 失败时返回对应的 `Errno`。
pub fn getsockname_syscall(fd: i32, addr_ptr: u64, addrlen_ptr: u64) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let obj = SocketObject::resolve(fd)?;
    ret_to_result(fw::getsockname_syscall(
        obj.inet_slot()?,
        addr_ptr,
        addrlen_ptr,
    ))
}

/// getpeername(fd, addr, addrlen) — 获取对端地址
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 getpeername 失败时返回对应的 `Errno`。
pub fn getpeername_syscall(fd: i32, addr_ptr: u64, addrlen_ptr: u64) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let obj = SocketObject::resolve(fd)?;
    ret_to_result(fw::getpeername_syscall(
        obj.inet_slot()?,
        addr_ptr,
        addrlen_ptr,
    ))
}

/// shutdown(fd, how)
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 shutdown 失败时返回对应的 `Errno`。
pub fn shutdown_syscall(fd: i32, how: i32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    let obj = SocketObject::resolve(fd)?;
    ret_to_result(fw::shutdown_syscall(obj.inet_slot()?, how))
}

/// sendmsg(fd, msg, flags) — 真实实现
///
/// v2: UDS 协议 sendmsg 路径 — 在 fw 之前处理 `msg_control` 注入 `SCM_CREDENTIALS`
/// (对端 `SO_PASSCRED` 启用时), 解析 cmsg 头序列处理 `SCM_RIGHTS` (跨进程 fd).
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; `msg_ptr` 或 `iov` 等用户缓冲区无效时返回 `Err(Errno::Efault)`;
/// 参数不合法 (如 `iovlen` 为 0 或超过上限) 时返回 `Err(Errno::Einval)`; fd 非 socket 时返回
/// `Err(Errno::Enotsock)`; 底层 sendmsg 失败时返回对应的 `Errno`。
pub fn sendmsg_syscall(fd: i32, msg_ptr: u64, flags: i32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    if msg_ptr == 0 {
        return Err(Errno::Efault);
    }
    if !api::validate_user_buf(msg_ptr, 56) {
        return Err(Errno::Efault);
    }
    let iov_ptr = api::read_u64_from_user(msg_ptr + 16).ok_or(Errno::Efault)?;
    let iovlen = api::read_u64_from_user(msg_ptr + 24).ok_or(Errno::Efault)?;
    if iovlen == 0 || iovlen > 1024 {
        return Err(Errno::Einval);
    }
    if iov_ptr == 0 {
        return Err(Errno::Einval);
    }
    let iov_bytes = iovlen.checked_mul(16).ok_or(Errno::Einval)?;
    if !api::validate_user_buf(iov_ptr, iov_bytes) {
        return Err(Errno::Efault);
    }
    // v2: UDS cmsg 处理 — 在发送前注入凭据 (如果对端 passcred)
    // cmsg 头格式: cmsghdr { cmsg_len: usize (8B), cmsg_level: i32 (4B), cmsg_type: i32 (4B) }
    // 总 16 字节头, 然后是 data (cmsg_len - 16 字节)
    let msg_control_ptr = api::read_u64_from_user(msg_ptr + 32).ok_or(Errno::Efault)?;
    let msg_controllen_raw = crate::functions::intcast::usize_from_u64(
        api::read_u64_from_user(msg_ptr + 40).ok_or(Errno::Efault)?,
    );
    let control_present = msg_control_ptr != 0 && msg_controllen_raw >= 28;
    if control_present && !api::validate_user_buf(msg_control_ptr, msg_controllen_raw as u64) {
        return Err(Errno::Efault);
    }
    let obj = SocketObject::resolve(fd)?;
    let unix_idx = obj.unix_idx();
    if control_present {
        // SO_PASSCRED 仅 UDS 对象持有; 非 UDS 对象恒不注入
        let local_passcred = unix_idx.is_some_and(|idx| uds::uds_getsockopt_passcred(idx) > 0);
        if local_passcred {
            // 写 SCM_CREDENTIALS cmsghdr (28 字节) 到 msg_control:
            // [0-7]   cmsg_len = 28
            // [8-11]  cmsg_level = 1 (SOL_SOCKET)
            // [12-15] cmsg_type = 2 (SCM 凭据)
            // [16-19] pid (高 32 位) | uid (低 32 位)
            // [24-27] gid
            // B07-02: 使用当前进程真实凭据, 消除硬编码伪造的 root 凭据.
            let pid: u64 = u64::from(crate::privileged::proc::process_get_current_pid());
            let uid: u64 = u64::from(crate::privileged::sgeg::get_current_uid());
            let gid: u64 = u64::from(crate::privileged::sgeg::get_current_gid());
            let _ = api::write_u64_to_user(msg_control_ptr, 28u64);
            let _ = api::write_u64_to_user(msg_control_ptr + 8, (2u64 << 32) | 1u64);
            let _ = api::write_u64_to_user(msg_control_ptr + 16, (pid << 32) | uid);
            let _ = api::write_u64_to_user(msg_control_ptr + 24, gid);
            let _ = api::write_u64_to_user(msg_ptr + 40, 28u64);
        }
        // 解析 cmsg 头序列处理 SCM_RIGHTS
        let mut coff = 0;
        while coff + 16 <= msg_controllen_raw {
            let cmsg_len = crate::functions::intcast::usize_from_u64(
                api::read_u64_from_user(msg_control_ptr + coff as u64).ok_or(Errno::Efault)?,
            );
            let cmsg_level = crate::functions::intcast::low_i32_from_u64(
                api::read_u64_from_user(msg_control_ptr + coff as u64 + 8).ok_or(Errno::Efault)?,
            );
            let cmsg_type = crate::functions::intcast::low_i32_from_u64(
                api::read_u64_from_user(msg_control_ptr + coff as u64 + 12).ok_or(Errno::Efault)?,
            );
            if cmsg_len < 16 || cmsg_len > msg_controllen_raw - coff {
                break;
            }
            if cmsg_level == 1 /* SOL_SOCKET */ && cmsg_type == 1
            /* SCM_RIGHTS */
            {
                // v2: SCM_RIGHTS (fd 跨进程传递) — deferred 到后续 PR.
                // 需对接 fd_alloc::dup_to_process 实现完整跨进程 fd 传递.
                // 当前简化: 仅标记已处理, 不做实际操作.
            }
            coff = (coff + cmsg_len + 3) & !3; // 4 字节对齐
        }
    }

    if let Some(idx) = unix_idx {
        return sendmmsg_uds_entry(idx, msg_ptr);
    }
    ret_to_result(fw::sendmsg_syscall(
        obj.inet_slot()?,
        msg_ptr,
        flags,
        obj.blocks(flags),
    ))
}

/// recvmsg(fd, msg, flags)
///
/// v2: UDS 协议 recvmsg 路径 — 写回对端注入的 `SCM_CREDENTIALS` cmsg
/// 到用户 `msg_control` 区域 (对端 send 时已注入).
///
/// # Errors
///
/// 当 `fd` 为负数时返回 `Err(Errno::Ebadf)`; `msg_ptr` 或 `iov` 等用户缓冲区无效时返回 `Err(Errno::Efault)`;
/// 参数不合法时返回 `Err(Errno::Einval)`; fd 非 socket 时返回 `Err(Errno::Enotsock)`;
/// 底层 recvmsg 失败时返回对应的 `Errno`。
pub fn recvmsg_syscall(fd: i32, msg_ptr: u64, flags: i32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    if msg_ptr == 0 {
        return Err(Errno::Efault);
    }
    if !api::validate_user_buf(msg_ptr, 56) {
        return Err(Errno::Efault);
    }
    let iov_ptr = api::read_u64_from_user(msg_ptr + 16).ok_or(Errno::Efault)?;
    let iovlen = api::read_u64_from_user(msg_ptr + 24).ok_or(Errno::Efault)?;
    if iovlen == 0 || iovlen > 1024 {
        return Err(Errno::Einval);
    }
    if iov_ptr == 0 {
        return Err(Errno::Einval);
    }
    let iov_bytes = iovlen.checked_mul(16).ok_or(Errno::Einval)?;
    if !api::validate_user_buf(iov_ptr, iov_bytes) {
        return Err(Errno::Efault);
    }
    // v2: UDS cmsg 回传 — 写回对端注入的 SCM_CREDENTIALS (对端 passcred 启用时)
    // 框架 fw_recvmsg 把数据搬入 iov, 凭据在 stream/dgram 缓冲里已添加 12 字节.
    // 这里仅在 msg_control 区域写回标准 SCM_CREDENTIALS cmsghdr, 让用户能解析凭据.
    let msg_control_ptr = api::read_u64_from_user(msg_ptr + 32).ok_or(Errno::Efault)?;
    let msg_controllen = crate::functions::intcast::usize_from_u64(
        api::read_u64_from_user(msg_ptr + 40).ok_or(Errno::Efault)?,
    );
    let control_present = msg_control_ptr != 0 && msg_controllen >= 28;
    if control_present && !api::validate_user_buf(msg_control_ptr, msg_controllen as u64) {
        return Err(Errno::Efault);
    }
    let obj = SocketObject::resolve(fd)?;
    let unix_idx = obj.unix_idx();
    if control_present
        && let Some(idx) = unix_idx
        && let Some(cred) = uds::uds_peer_creds(idx)
    {
        // 写回 SCM_CREDENTIALS cmsghdr + 12 字节凭据 (与 send 路径相同编码)
        let _ = api::write_u64_to_user(msg_control_ptr, 28u64);
        let _ = api::write_u64_to_user(msg_control_ptr + 8, (2u64 << 32) | 1u64);
        let _ = api::write_u64_to_user(
            msg_control_ptr + 16,
            (u64::from(cred.pid) << 32) | u64::from(cred.uid),
        );
        let _ = api::write_u64_to_user(msg_control_ptr + 24, u64::from(cred.gid));
        let _ = api::write_u64_to_user(msg_ptr + 40, 28u64);
    }
    if let Some(idx) = unix_idx {
        let st = uds::uds_sock_type(idx).map_err(uds_errno)?;
        return recvmmsg_uds_entry(idx, msg_ptr, st);
    }
    ret_to_result(fw::recvmsg_syscall(
        obj.inet_slot()?,
        msg_ptr,
        flags,
        obj.blocks(flags),
    ))
}

// ============================================================================
// socketpair / sendmmsg / recvmmsg (T1 G3 实装)
// ============================================================================

/// mmsghdr 用户态大小 = msghdr (56B) + `msg_len` (u32) = 64 字节
/// (x86_64 与 aarch64 同布局, 8 字节对齐)
const MMSGHDR_SIZE: u64 = 64;
/// `msghdr.msg_name` 偏移
const MSGHDR_NAME_OFF: u64 = 0;
/// `msghdr.msg_namelen` 偏移
const MSGHDR_NAMELEN_OFF: u64 = 8;
/// `msghdr.msg_iov` 偏移
const MSGHDR_IOV_OFF: u64 = 16;
/// `msghdr.msg_iovlen` 偏移
const MSGHDR_IOVLEN_OFF: u64 = 24;
/// `msghdr.msg_flags` 偏移 (int, 4 字节)
const MSGHDR_FLAGS_OFF: u64 = 48;
/// `mmsghdr.msg_len` 偏移 (unsigned int, 4 字节)
const MMSGHDR_MSGLEN_OFF: u64 = 56;
/// 单条消息 iov 数量上限 (`UIO_MAXIOV`)
const UIO_MAXIOV: u64 = 1024;
/// `MSG_TRUNC` (数据被截断)
const MSG_TRUNC_FLAG: u32 = 0x20;
/// `MSG_DONTWAIT` (本次调用非阻塞)
const MSG_DONTWAIT_FLAG: u32 = 0x40;
/// `MSG_WAITFORONE` (recvmmsg: 首条之后不再阻塞等待)
const MSG_WAITFORONE_FLAG: u32 = 0x1_0000;
/// struct `timespec.tv_nsec` 合法上界 (半开区间)
const NSEC_PER_SEC: i64 = 1_000_000_000;

/// socketpair(domain, `sock_type`, protocol, `sv[2]`) — 创建一对互相连接的套接字
///
/// 仅支持 `AF_UNIX(1)` (两端同类型互为 peer, 无路径绑定), FD 写回用户 `sv` (2 × i32)。
///
/// # Errors
///
/// `sv` 指针无效时返回 `Err(Errno::Efault)`; 非 `AF_UNIX` 或 `sock_type` 非 Stream/Dgram 时
/// 返回 `Err(Errno::Enotsup)`; `protocol` 非 0 时返回 `Err(Errno::Eprotonosupport)`;
/// 创建或写回失败时返回对应 `Errno`。
pub fn socketpair_syscall(
    domain: i32,
    sock_type: i32,
    protocol: i32,
    sv_ptr: u64,
) -> Result<usize, Errno> {
    if sv_ptr == 0 || !api::validate_user_buf(sv_ptr, 8) {
        return Err(Errno::Efault);
    }
    if domain != 1 {
        return Err(Errno::Enotsup);
    }
    if protocol != 0 {
        return Err(Errno::Eprotonosupport);
    }
    let st = uds::UnixSockType::from_i32(sock_type & !SOCK_NONBLOCK).ok_or(Errno::Enotsup)?;
    let nonblock = (sock_type & SOCK_NONBLOCK) != 0;
    let (fd0, fd1) = UnixSocketFile::create_pair(st, nonblock, false)?;
    // sv[0]=fd0, sv[1]=fd1: 拼为主机序 u64 一次写回 (低 4 字节落在 sv[0])
    let packed = (u64::from(crate::functions::intcast::low_u32_from_usize(fd1)) << 32)
        | u64::from(crate::functions::intcast::low_u32_from_usize(fd0));
    if !api::write_u64_to_user(sv_ptr, packed) {
        // 写回失败: 两端 FD 已无用户可见入口, 走 VFS 共享关闭链回收
        let _ = crate::functions::fs::api::vfs_close_safe(
            crate::functions::intcast::low_u32_from_usize(fd0),
        );
        let _ = crate::functions::fs::api::vfs_close_safe(
            crate::functions::intcast::low_u32_from_usize(fd1),
        );
        return Err(Errno::Efault);
    }
    Ok(0)
}

/// sendmmsg(fd, msgvec, vlen, flags) — 批量发送消息
///
/// UDS 分流: 逐 entry 收集 iov 数据, 有 `msg_name` 走路径发送 (与 `sendto` UDS 路径一致),
/// 无则走已连接发送; 非 UDS 逐 entry 委托 [`sendmsg_syscall`] (smoltcp)。
/// 出错时若已发送 ≥1 条则返回已发送条数 (Linux 语义)。
///
/// # Errors
///
/// `fd` 为负时返回 `Err(Errno::Ebadf)`; `vlen` 超过 `UIO_MAXIOV` 时返回 `Err(Errno::Einval)`;
/// entry 或 iov 用户缓冲无效时返回 `Err(Errno::Efault)`; 首条发送失败透传底层 `Errno`。
pub fn sendmmsg_syscall(fd: i32, msgvec_ptr: u64, vlen: u32, flags: u32) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    if vlen == 0 {
        return Ok(0);
    }
    if u64::from(vlen) > UIO_MAXIOV {
        return Err(Errno::Einval);
    }
    let obj = SocketObject::resolve(fd)?;
    let unix_idx = obj.unix_idx();
    let mut sent: usize = 0;
    for i in 0..u64::from(vlen) {
        let entry = msgvec_ptr + i * MMSGHDR_SIZE;
        if !api::validate_user_buf(entry, MMSGHDR_SIZE) {
            if sent > 0 {
                break;
            }
            return Err(Errno::Efault);
        }
        let r = if let Some(idx) = unix_idx {
            sendmmsg_uds_entry(idx, entry)
        } else {
            sendmsg_syscall(fd, entry, flags.cast_signed())
        };
        match r {
            Ok(n) => {
                // msg_len (u32) 写回 entry+56, 4 字节粒度写避免越界下一条 entry
                let len32 = u32::try_from(n).unwrap_or(u32::MAX);
                if fw::raw_copy_out(entry + MMSGHDR_MSGLEN_OFF, 4, &len32.to_ne_bytes()).is_err()
                    && sent == 0
                {
                    return Err(Errno::Efault);
                }
                sent += 1;
            }
            Err(e) => {
                if sent > 0 {
                    break;
                }
                return Err(e);
            }
        }
    }
    Ok(sent)
}

/// sendmmsg 单条 UDS entry 发送: 收集 iov 数据后按有无 `msg_name` 分流
///
/// # Errors
///
/// iov 参数非法时返回 `Err(Errno::Einval)`; 用户缓冲无效时返回 `Err(Errno::Efault)`;
/// 数据报超长时返回 `Err(Errno::Emsgsize)`; 底层 UDS 发送失败透传对应 `Errno`。
fn sendmmsg_uds_entry(idx: u8, entry: u64) -> Result<usize, Errno> {
    let data = gather_entry_iov(entry)?;
    let st = uds::uds_sock_type(idx).map_err(uds_errno)?;
    let name_ptr = api::read_u64_from_user(entry + MSGHDR_NAME_OFF).ok_or(Errno::Efault)?;
    if name_ptr != 0 {
        // 有目标地址: 按路径发送, 目标为 Dgram, 数据超过单报上限拒绝
        if data.len() > uds::UNIX_DGRAM_MAX {
            return Err(Errno::Emsgsize);
        }
        let namelen = api::read_u64_from_user(entry + MSGHDR_NAMELEN_OFF).ok_or(Errno::Efault)?;
        let addr = fw::raw_read_sockaddr_un(
            name_ptr,
            crate::functions::intcast::low_u32_from_u64(namelen),
        )?;
        uds::uds_sendto(&data, addr.path_slice()).map_err(uds_errno)
    } else {
        // 无目标地址: 已连接发送; 已连接 Dgram 单报上限与路径发送一致
        if st == uds::UnixSockType::Dgram && data.len() > uds::UNIX_DGRAM_MAX {
            return Err(Errno::Emsgsize);
        }
        uds::uds_send_connected(idx, &data).map_err(uds_errno)
    }
}

/// 读取 entry 的 iov 链并收集为连续缓冲 (sendmmsg UDS 路径)
///
/// # Errors
///
/// `iovlen` 为 0 或超过 `UIO_MAXIOV` 时返回 `Err(Errno::Einval)`; 用户缓冲无效时返回
/// `Err(Errno::Efault)`。
fn gather_entry_iov(entry: u64) -> Result<alloc::vec::Vec<u8>, Errno> {
    let iov_ptr = api::read_u64_from_user(entry + MSGHDR_IOV_OFF).ok_or(Errno::Efault)?;
    let iovlen = api::read_u64_from_user(entry + MSGHDR_IOVLEN_OFF).ok_or(Errno::Efault)?;
    if iovlen == 0 || iovlen > UIO_MAXIOV || iov_ptr == 0 {
        return Err(Errno::Einval);
    }
    let iov_bytes = iovlen.checked_mul(16).ok_or(Errno::Einval)?;
    if !api::validate_user_buf(iov_ptr, iov_bytes) {
        return Err(Errno::Efault);
    }
    let mut data = alloc::vec::Vec::new();
    for j in 0..iovlen {
        let base = api::read_u64_from_user(iov_ptr + j * 16).ok_or(Errno::Efault)?;
        let seg_len = api::read_u64_from_user(iov_ptr + j * 16 + 8).ok_or(Errno::Efault)?;
        if seg_len == 0 {
            continue;
        }
        if base == 0 || !api::validate_user_buf(base, seg_len) {
            return Err(Errno::Efault);
        }
        // 单段长度需可经 raw_copy_in 的 u32 长度参数表达
        let seg_len32 = u32::try_from(seg_len).map_err(|_| Errno::Einval)?;
        let seg = fw::raw_copy_in(base, seg_len32)?;
        data.extend_from_slice(&seg);
    }
    Ok(data)
}

// SIMPLIFIED: recvmmsg 无 timeout (timeout_ptr==0) 时不阻塞等待, 空数据立即返回 EAGAIN;
// 影响: 阻塞 socket 语义未达成, 调用方需自行轮询或传入 timeout; 忙等已通过 yield 规避;
// 扩展时机: functions wait_queue 阻塞机制就位后接入睡眠等待.
/// recvmmsg(fd, msgvec, vlen, flags, timeout) — 批量接收消息
///
/// 语义 (Linux): deadline = now + timeout, 逐 entry 尝试接收, 每条成功后检查 deadline;
/// ≥1 条后出错返回已收条数; deadline 到期且 0 条返回 `EAGAIN`; `MSG_WAITFORONE` 首条后
/// 不再阻塞等待; `MSG_DONTWAIT` 本次调用非阻塞; UDS 分流按套接字类型选接收原语,
/// 非 UDS 逐 entry 委托 [`recvmsg_syscall`]。
///
/// # Errors
///
/// `fd` 为负时返回 `Err(Errno::Ebadf)`; `vlen` 超过 `UIO_MAXIOV` 或 timeout 字段非法时
/// 返回 `Err(Errno::Einval)`; entry/iov 用户缓冲无效时返回 `Err(Errno::Efault)`;
/// 首条接收失败透传底层 `Errno`。
pub fn recvmmsg_syscall(
    fd: i32,
    msgvec_ptr: u64,
    vlen: u32,
    flags: u32,
    timeout_ptr: u64,
) -> Result<usize, Errno> {
    if fd < 0 {
        return Err(Errno::Ebadf);
    }
    if vlen == 0 {
        return Ok(0);
    }
    if u64::from(vlen) > UIO_MAXIOV {
        return Err(Errno::Einval);
    }
    // timeout 解析: struct timespec { tv_sec: i64, tv_nsec: i64 }, 相对时长
    let mut deadline_ms: u64 = 0;
    if timeout_ptr != 0 {
        if !api::validate_user_buf(timeout_ptr, 16) {
            return Err(Errno::Efault);
        }
        let sec_raw = api::read_u64_from_user(timeout_ptr).ok_or(Errno::Efault)?;
        let nsec_raw = api::read_u64_from_user(timeout_ptr + 8).ok_or(Errno::Efault)?;
        if sec_raw > i64::MAX as u64 || nsec_raw > i64::MAX as u64 {
            return Err(Errno::Einval);
        }
        let (sec, nsec) = (sec_raw.cast_signed(), nsec_raw.cast_signed());
        if sec < 0 || !(0..NSEC_PER_SEC).contains(&nsec) {
            return Err(Errno::Einval);
        }
        let rel_ms = sec
            .cast_unsigned()
            .checked_mul(1000)
            .and_then(|ms| ms.checked_add(nsec.cast_unsigned() / 1_000_000));
        let now = crate::privileged::syscall::api::get_ticks();
        // 相对时长或当前 tick 溢出时饱和为"永不超时"
        deadline_ms = rel_ms.and_then(|r| now.checked_add(r)).unwrap_or(u64::MAX);
    }
    let blocking = timeout_ptr != 0 && (flags & MSG_DONTWAIT_FLAG) == 0;
    let waitforone = (flags & MSG_WAITFORONE_FLAG) != 0;
    // UDS 接收分流: 套接字类型决定 Stream/Dgram 接收原语 (逐 entry 一致)
    let obj = SocketObject::resolve(fd)?;
    let uds_target = match obj.unix_idx() {
        Some(idx) => Some((idx, uds::uds_sock_type(idx).map_err(uds_errno)?)),
        None => None,
    };
    let mut received: usize = 0;
    for i in 0..u64::from(vlen) {
        let entry = msgvec_ptr + i * MMSGHDR_SIZE;
        if !api::validate_user_buf(entry, MMSGHDR_SIZE) {
            if received > 0 {
                break;
            }
            return Err(Errno::Efault);
        }
        // 单条接收, 空数据时按阻塞语义 yield 重试直至 deadline
        let r = loop {
            let attempt = match uds_target {
                Some((idx, t)) => recvmmsg_uds_entry(idx, entry, t),
                None => recvmsg_syscall(fd, entry, flags.cast_signed()),
            };
            match attempt {
                Err(Errno::Eagain) => {
                    let now = crate::privileged::syscall::api::get_ticks();
                    let deadline_passed = timeout_ptr != 0 && now >= deadline_ms;
                    if !blocking || deadline_passed || (waitforone && received > 0) {
                        break Err(Errno::Eagain);
                    }
                    crate::privileged::proc::scheduler_yield();
                }
                other => break other,
            }
        };
        match r {
            Ok(n) => {
                // msg_len (u32) 写回 entry+56 (recvmsg_syscall 不写 msg_len, 此处必须补写)
                let len32 = u32::try_from(n).unwrap_or(u32::MAX);
                if fw::raw_copy_out(entry + MMSGHDR_MSGLEN_OFF, 4, &len32.to_ne_bytes()).is_err()
                    && received == 0
                {
                    return Err(Errno::Efault);
                }
                received += 1;
            }
            Err(e) => {
                if received > 0 {
                    break;
                }
                return Err(e);
            }
        }
    }
    if received == 0 {
        // 0 条成功且以 EAGAIN 终止 (deadline 到期 / 非阻塞语义)
        return Err(Errno::Eagain);
    }
    Ok(received)
}

/// recvmmsg 单条 UDS entry 接收: 按套接字类型选择接收原语, 数据 copy-out 到各 iov 段
///
/// `msg_name` 不回填 (与 `recvfrom_syscall` v1 简化一致); `msg_flags` 逐条写回。
///
/// # Errors
///
/// iov 参数非法时返回 `Err(Errno::Einval)`; 用户缓冲无效时返回 `Err(Errno::Efault)`;
/// 底层 UDS 接收失败透传对应 `Errno`。
fn recvmmsg_uds_entry(idx: u8, entry: u64, st: uds::UnixSockType) -> Result<usize, Errno> {
    let (iov_ptr, total_cap) = read_entry_iov_cap(entry)?;
    // 接收缓冲: 以 iov 总容量为上限, 封顶该类型单端缓冲大小
    let cap_limit = match st {
        uds::UnixSockType::Stream => uds::UNIX_STREAM_BUF,
        uds::UnixSockType::Dgram => uds::UNIX_DGRAM_MAX,
    };
    let cap = crate::functions::intcast::usize_from_u64(total_cap).min(cap_limit);
    let mut buf = alloc::vec![0u8; cap];
    let n = match st {
        uds::UnixSockType::Stream => uds::uds_recv(idx, &mut buf).map_err(uds_errno)?,
        uds::UnixSockType::Dgram => uds::uds_recvfrom(idx, &mut buf).map_err(uds_errno)?,
    };
    copy_out_to_entry_iov(iov_ptr, &buf[..n])?;
    // msg_flags 逐条写回: 收满缓冲视为可能截断
    // SIMPLIFIED: MSG_TRUNC 以"收满接收缓冲"近似判定, Stream 恰满与真截断不可区分;
    // 影响: 满缓冲消息可能被多报 MSG_TRUNC; 扩展时机: UDS 携带真实报文长度时精确判定.
    let out_flags: u32 = if cap > 0 && n >= cap {
        MSG_TRUNC_FLAG
    } else {
        0
    };
    if fw::raw_copy_out(entry + MSGHDR_FLAGS_OFF, 4, &out_flags.to_ne_bytes()).is_err() {
        return Err(Errno::Efault);
    }
    Ok(n)
}

/// 读取 entry 的 iov 链, 返回 (iov 指针, 各段长度之和)
///
/// # Errors
///
/// `iovlen` 为 0 或超过 `UIO_MAXIOV` 时返回 `Err(Errno::Einval)`; 用户缓冲无效时返回
/// `Err(Errno::Efault)`。
fn read_entry_iov_cap(entry: u64) -> Result<(u64, u64), Errno> {
    let iov_ptr = api::read_u64_from_user(entry + MSGHDR_IOV_OFF).ok_or(Errno::Efault)?;
    let iovlen = api::read_u64_from_user(entry + MSGHDR_IOVLEN_OFF).ok_or(Errno::Efault)?;
    if iovlen == 0 || iovlen > UIO_MAXIOV || iov_ptr == 0 {
        return Err(Errno::Einval);
    }
    let iov_bytes = iovlen.checked_mul(16).ok_or(Errno::Einval)?;
    if !api::validate_user_buf(iov_ptr, iov_bytes) {
        return Err(Errno::Efault);
    }
    let mut total: u64 = 0;
    for j in 0..iovlen {
        let seg_len = api::read_u64_from_user(iov_ptr + j * 16 + 8).ok_or(Errno::Efault)?;
        total = total.saturating_add(seg_len);
    }
    Ok((iov_ptr, total))
}

/// 将接收数据按序 copy-out 到 entry 的各 iov 段 (数据量不超过 iov 总容量)
///
/// # Errors
///
/// iov 段指针无效时返回 `Err(Errno::Efault)`。
fn copy_out_to_entry_iov(iov_ptr: u64, data: &[u8]) -> Result<(), Errno> {
    let mut off = 0usize;
    let mut j: u64 = 0;
    while off < data.len() {
        let base = api::read_u64_from_user(iov_ptr + j * 16).ok_or(Errno::Efault)?;
        let seg_len = api::read_u64_from_user(iov_ptr + j * 16 + 8).ok_or(Errno::Efault)?;
        if seg_len == 0 {
            j += 1;
            continue;
        }
        // take 受剩余数据与段长双重约束, 恒不超过缓冲大小 (u32 可表达)
        let take = (data.len() - off).min(crate::functions::intcast::usize_from_u64(seg_len));
        fw::raw_copy_out(
            base,
            crate::functions::intcast::low_u32_from_usize(take),
            &data[off..off + take],
        )?;
        off += take;
        j += 1;
    }
    Ok(())
}
