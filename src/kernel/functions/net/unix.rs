#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! Unix Domain Socket (`AF_UNIX`) — functions 层策略主体
//!
//! ## T3-4 迁移记录
//!
//! 原属 privileged/net/unix.rs, 2026-06-16 提取到 functions.
//! 纯策略代码 (socket CRUD + 路径绑定 + STREAM/DGRAM 数据传输), 0 unsafe.
//! privileged 仅保留 re-export.
//!
//! ## fd 对象模型 (阶段 3)
//!
//! 本模块是**策略表**: 对外只暴露槽位下标 (`u8`), 不感知 fd 编号. 用户可见
//! fd 由 `net::unix_socket_file::UnixSocketFile` 对象挂在全局 `OpenFileTable`
//! 上分配; 槽位生灭由对象的 `release()` 驱动 ([`uds_free`]), 与 VFS / Smoltcp
//! socket fd 同构. 编号区间 `[1000, 1016)` 与 `fd_alloc` 位图已退役.

use alloc::vec::Vec;

use crate::functions::fs::file::PollEvents;
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// 最大 UDS socket 数量 (策略表槽位容量)
pub const MAX_UDS_FD: usize = 16;

/// POSIX `sun_path` 最大长度
///
/// DECISION-J (2026-09-13): wire 常量迁回 `privileged::net::socket_types`,
/// 此处 re-export 保持 API 兼容。
pub use crate::privileged::net::socket_types::UNIX_PATH_MAX;

/// 路径绑定表容量
pub const UNIX_MAX_BINDINGS: usize = 32;

/// `SOCK_STREAM` 单端接收缓冲大小
pub const UNIX_STREAM_BUF: usize = 8192;

/// `SOCK_DGRAM` 单消息最大长度
pub const UNIX_DGRAM_MAX: usize = 8192;

/// `listen()` 固定 backlog
pub const UNIX_LISTEN_BACKLOG: usize = 5;

// ============================================================================
// 错误码
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
/// UDS 内部错误码, 取 POSIX errno 数值以便直接转成系统调用返回值.
pub enum UdsError {
    /// 文件描述符无效或不属于 UDS 子系统 (EBADF).
    BadFd = 9,
    /// 非阻塞操作当前无法完成 (EAGAIN).
    Again = 11,
    /// 内存或槽位不足 (ENOMEM).
    NoMem = 12,
    /// 地址族不支持 (EAFNOSUPPORT).
    AddrFamily = 97,
    /// 路径已被其他套接字绑定 (EADDRINUSE).
    AddrInUse = 98,
    /// 对端拒绝连接 (ECONNREFUSED).
    ConnRefused = 111,
    /// 参数非法 (EINVAL).
    Invalid = 22,
    /// 目标路径或套接字不存在 (ENOENT).
    NotFound = 2,
    /// 当前实现不支持该操作 (ENOSYS).
    NoSys = 38,
}

impl UdsError {
    /// 转为系统调用返回值形式, 即对应 errno 的负值.
    #[must_use]
    pub fn as_ret(self) -> i32 {
        -(self as i32)
    }
}

// ============================================================================
// 类型定义
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// UDS 套接字类型.
pub enum UnixSockType {
    /// 面向连接的字节流 (`SOCK_STREAM`).
    Stream = 1,
    /// 保留消息边界的数据报 (`SOCK_DGRAM`).
    Dgram = 2,
}

impl UnixSockType {
    /// 由 `socket()` 传入的类型值解析; 非 1 / 2 返回 `None`.
    #[must_use]
    pub fn from_i32(t: i32) -> Option<Self> {
        match t {
            1 => Some(Self::Stream),
            2 => Some(Self::Dgram),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// UDS 套接字生命周期状态.
pub enum UnixSockState {
    /// 尚未绑定路径.
    Unbound = 0,
    /// 已绑定路径, 尚未监听.
    Bound = 1,
    /// 处于监听态, 可接受连接.
    Listening = 2,
    /// 已与对端建立连接.
    Connected = 3,
    /// 已关闭, 等待回收.
    Closed = 4,
}

#[derive(Debug, Clone)]
/// 单个 UDS 套接字: 绑定路径、监听队列、连接对端与收发缓冲.
pub struct UnixSocket {
    /// 套接字内部编号; 0 表示槽位空闲.
    pub id: u32,
    /// 套接字类型 (流 / 数据报).
    pub sock_type: UnixSockType,
    /// 当前生命周期状态.
    pub state: UnixSockState,
    /// 绑定的路径字节 (未绑定时为全零).
    pub bound_path: [u8; UNIX_PATH_MAX],
    /// 绑定路径的有效字节数.
    pub bound_path_len: u16,

    /// 监听队列中待 accept 的套接字编号 (环形缓冲).
    pub listen_pending: [u32; UNIX_LISTEN_BACKLOG],
    /// 监听队列队头下标.
    pub listen_head: u8,
    /// 监听队列队尾下标.
    pub listen_tail: u8,
    /// 监听队列当前长度.
    pub listen_count: u8,

    /// 已连接对端的套接字编号.
    pub peer: Option<u32>,

    /// 流式套接字的接收缓冲.
    pub stream_buf: [u8; UNIX_STREAM_BUF],
    /// 流式缓冲中的有效字节数.
    pub stream_len: u32,
    /// 对端是否已关闭写方向.
    pub peer_closed: bool,

    /// 数据报套接字的接收缓冲 (仅保留一条消息).
    pub dgram_buf: [u8; UNIX_DGRAM_MAX],
    /// 数据报缓冲中的有效字节数.
    pub dgram_len: u32,
    /// 数据报缓冲中是否有待读取消息.
    pub dgram_pending: bool,

    /// v2: `SO_PASSCRED` 选项 — 对端 socket 设置后, 发送消息自动附加
    /// `SCM_CREDENTIALS` 凭据 (uid/gid/pid).
    pub passcred: bool,
}

impl UnixSocket {
    /// 构造全零的空套接字 (类型默认流式, 状态未绑定).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            id: 0,
            sock_type: UnixSockType::Stream,
            state: UnixSockState::Unbound,
            bound_path: [0u8; UNIX_PATH_MAX],
            bound_path_len: 0,
            listen_pending: [0u32; UNIX_LISTEN_BACKLOG],
            listen_head: 0,
            listen_tail: 0,
            listen_count: 0,
            peer: None,
            stream_buf: [0u8; UNIX_STREAM_BUF],
            stream_len: 0,
            peer_closed: false,
            dgram_buf: [0u8; UNIX_DGRAM_MAX],
            dgram_len: 0,
            dgram_pending: false,
            passcred: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// 路径绑定表项: 记录一条已绑定路径及其所属套接字槽位.
pub struct UnixPathBinding {
    /// 绑定路径字节.
    pub path: [u8; UNIX_PATH_MAX],
    /// 路径的有效字节数.
    pub path_len: u16,
    /// 占用该路径的套接字槽位下标.
    pub sock_idx: u8,
    /// 表项是否在使用中.
    pub used: bool,
}

impl UnixPathBinding {
    /// 构造全零的空表项.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            path: [0u8; UNIX_PATH_MAX],
            path_len: 0,
            sock_idx: 0,
            used: false,
        }
    }
}

// ============================================================================
// 全局状态
// ============================================================================

#[derive(Debug)]
/// UDS 子系统的全局状态: 套接字表与路径绑定表.
pub struct UdsState {
    /// 全部套接字槽位.
    pub sockets: Vec<UnixSocket>,
    /// 全部路径绑定表项.
    pub paths: Vec<UnixPathBinding>,
}

impl UdsState {
    /// 构造空容器 (不含元素), 字段级堆化前置态.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            sockets: Vec::new(),
            paths: Vec::new(),
        }
    }

    /// 惰性堆置: 将两个表填满到各自容量 (与旧静态数组语义等价).
    ///
    /// 在 `uds_init()` 时调用 (堆已就绪), 避免 BSS 常驻大数组.
    pub fn allocate(&mut self) {
        self.sockets = (0..MAX_UDS_FD).map(|_| UnixSocket::empty()).collect();
        self.paths = (0..UNIX_MAX_BINDINGS)
            .map(|_| UnixPathBinding::empty())
            .collect();
    }

    fn find_free_path(&self) -> Option<u8> {
        for (i, p) in self.paths.iter().enumerate() {
            if !p.used {
                return Some(crate::functions::intcast::low_u8_from_usize(i));
            }
        }
        None
    }

    /// 查找空闲套接字槽位
    fn find_free_socket(&self) -> Option<u8> {
        for (i, s) in self.sockets.iter().enumerate() {
            if s.id == 0 {
                return Some(crate::functions::intcast::low_u8_from_usize(i));
            }
        }
        None
    }

    fn find_path(&self, path: &[u8]) -> Option<u8> {
        for (i, p) in self.paths.iter().enumerate() {
            if p.used && p.path_len as usize == path.len() && p.path[..path.len()] == *path {
                return Some(crate::functions::intcast::low_u8_from_usize(i));
            }
        }
        None
    }

    fn socket_idx_by_id(&self, id: u32) -> Option<u8> {
        for (i, s) in self.sockets.iter().enumerate() {
            if s.id == id {
                return Some(crate::functions::intcast::low_u8_from_usize(i));
            }
        }
        None
    }
}

/// UDS 全局状态 (受中断自旋锁保护).
pub static UDS_STATE: IrqSpinLock<UdsState> = IrqSpinLock::new(UdsState::empty());

static NEXT_SOCK_ID: IrqSpinLock<u32> = IrqSpinLock::new(1);

// ============================================================================
// 内部辅助
// ============================================================================

fn alloc_socket_id() -> u32 {
    let mut guard = NEXT_SOCK_ID.lock();
    let id = *guard;
    *guard = if id == u32::MAX { 1 } else { id + 1 };
    id
}

/// 槽位下标合法性检查: 越界或槽位空闲统一视为无效对象 (`BadFd`)
#[inline]
fn check_slot(state: &UdsState, idx: u8) -> Result<usize, UdsError> {
    let i = idx as usize;
    if i >= state.sockets.len() || state.sockets[i].id == 0 {
        return Err(UdsError::BadFd);
    }
    Ok(i)
}

/// 在锁内分配一个策略表槽位并初始化为指定类型的空闲 (Unbound) 状态
fn alloc_socket_locked(state: &mut UdsState, sock_type: UnixSockType) -> Result<u8, UdsError> {
    let idx = state.find_free_socket().ok_or(UdsError::NoMem)?;
    let id = alloc_socket_id();
    let s = &mut state.sockets[idx as usize];
    s.id = id;
    s.sock_type = sock_type;
    s.state = UnixSockState::Unbound;
    s.bound_path_len = 0;
    s.listen_count = 0;
    s.listen_head = 0;
    s.listen_tail = 0;
    s.peer = None;
    s.stream_len = 0;
    s.peer_closed = false;
    s.dgram_len = 0;
    s.dgram_pending = false;
    Ok(idx)
}

// ============================================================================
// 公开 API
// ============================================================================

/// 初始化 UDS 子系统: 惰性堆置两张表并重置编号.
pub fn uds_init() {
    // 惰性堆置: 在堆就绪后填充 sockets/paths 两表 (与旧静态数组语义等价).
    UDS_STATE.with_mut(UdsState::allocate);
    NEXT_SOCK_ID.with_mut(|id| *id = 1);
}

/// 分配一个 UDS socket 策略槽位, 返回槽位下标
///
/// fd 对象 (`UnixSocketFile`) 持有该下标并独占槽位所有权; 对象 `release()`
/// 经 [`uds_free`] 归还.
///
/// # Errors
///
/// 无空闲槽位 (`MAX_UDS_FD` 上限) 时返回 `Err(UdsError::NoMem)`。
pub fn uds_alloc(sock_type: UnixSockType) -> Result<u8, UdsError> {
    UDS_STATE.with_mut(|state| alloc_socket_locked(state, sock_type))
}

/// 创建一对互相连接的 UDS 套接字 (`socketpair` 机制核心)
///
/// 两端同类型且互设 peer, 均为 Connected 状态, 无路径绑定 (对齐 Linux `socketpair(AF_UNIX)` 语义)。
///
/// # Errors
///
/// 无空闲槽位或达 `MAX_UDS_FD` 上限时返回 `Err(UdsError::NoMem)`; 第二端创建失败时
/// 回滚释放第一端; 槽位关联异常时返回 `Err(UdsError::BadFd)` 并回滚两端。
pub fn uds_socketpair(sock_type: UnixSockType) -> Result<(u8, u8), UdsError> {
    let idx0 = uds_alloc(sock_type)?;
    let idx1 = match uds_alloc(sock_type) {
        Ok(i) => i,
        Err(e) => {
            let _ = uds_free(idx0);
            return Err(e);
        }
    };
    let linked = UDS_STATE.with_mut(|state| {
        let slot0 = check_slot(state, idx0)?;
        let slot1 = check_slot(state, idx1)?;
        if slot0 == slot1 {
            return Err(UdsError::BadFd);
        }
        // 互设 peer 并置 Connected (参照 uds_connect Dgram 分支的双向 peer 模式)
        state.sockets[slot0].peer = Some(state.sockets[slot1].id);
        state.sockets[slot1].peer = Some(state.sockets[slot0].id);
        state.sockets[slot0].state = UnixSockState::Connected;
        state.sockets[slot1].state = UnixSockState::Connected;
        Ok(())
    });
    if let Err(e) = linked {
        let _ = uds_free(idx0);
        let _ = uds_free(idx1);
        return Err(e);
    }
    Ok((idx0, idx1))
}

/// 查询 UDS 套接字类型 (`recvmmsg` / `read_at` 分流 Stream/Dgram 原语时使用)
///
/// # Errors
///
/// `idx` 越界或槽位空闲时返回 `Err(UdsError::BadFd)`。
pub fn uds_sock_type(idx: u8) -> Result<UnixSockType, UdsError> {
    UDS_STATE.with(|state| {
        let i = check_slot(state, idx)?;
        Ok(state.sockets[i].sock_type)
    })
}

/// 将 UDS 套接字绑定到指定路径
///
/// # Errors
///
/// 当路径为空或超过 `UNIX_PATH_MAX` 时返回 `Err(UdsError::Invalid)`; `idx` 无效时返回 `Err(UdsError::BadFd)`;
/// 套接字非 Unbound 状态时返回 `Err(UdsError::Invalid)`; 路径已被占用时返回 `Err(UdsError::AddrInUse)`;
/// 路径表无空闲槽位时返回 `Err(UdsError::NoMem)`。
pub fn uds_bind(idx: u8, path: &[u8]) -> Result<(), UdsError> {
    if path.is_empty() || path.len() > UNIX_PATH_MAX {
        return Err(UdsError::Invalid);
    }
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let s = &state.sockets[i];
        if s.state != UnixSockState::Unbound {
            return Err(UdsError::Invalid);
        }
        if state.find_path(path).is_some() {
            return Err(UdsError::AddrInUse);
        }
        let pidx = state.find_free_path().ok_or(UdsError::NoMem)?;
        let binding = &mut state.paths[pidx as usize];
        binding.path[..path.len()].copy_from_slice(path);
        binding.path_len = crate::functions::intcast::low_u16_from_usize(path.len());
        binding.sock_idx = crate::functions::intcast::low_u8_from_usize(i);
        binding.used = true;
        let s = &mut state.sockets[i];
        s.bound_path[..path.len()].copy_from_slice(path);
        s.bound_path_len = crate::functions::intcast::low_u16_from_usize(path.len());
        s.state = UnixSockState::Bound;
        Ok(())
    })
}

/// 将已绑定的流式 UDS 套接字置为监听状态
///
/// # Errors
///
/// 当 `idx` 无效时返回 `Err(UdsError::BadFd)`; 套接字非流式或未处于 Bound 状态时返回 `Err(UdsError::Invalid)`。
pub fn uds_listen(idx: u8) -> Result<(), UdsError> {
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let s = &state.sockets[i];
        if s.sock_type != UnixSockType::Stream {
            return Err(UdsError::Invalid);
        }
        if s.state != UnixSockState::Bound {
            return Err(UdsError::Invalid);
        }
        state.sockets[i].state = UnixSockState::Listening;
        Ok(())
    })
}

/// 从监听套接字的待连接队列中取出一个客户端连接, 返回新连接的槽位下标
///
/// 新槽位所有权移交调用方 (`UnixSocketFile`), 由对象 `release()` 归还.
///
/// # Errors
///
/// 当策略表无空闲槽位时返回 `Err(UdsError::NoMem)`; `listen_idx` 无效时返回
/// `Err(UdsError::BadFd)`; 套接字非流式或非 Listening 状态时返回
/// `Err(UdsError::Invalid)`; 待连接队列为空时返回 `Err(UdsError::Again)`。
pub fn uds_accept(listen_idx: u8) -> Result<u8, UdsError> {
    UDS_STATE.with_mut(|state| {
        let listen_slot = check_slot(state, listen_idx)?;
        let listen = &state.sockets[listen_slot];
        if listen.sock_type != UnixSockType::Stream {
            return Err(UdsError::Invalid);
        }
        if listen.state != UnixSockState::Listening {
            return Err(UdsError::Invalid);
        }
        if listen.listen_count == 0 {
            return Err(UdsError::Again);
        }
        let client_id = listen.listen_pending[listen.listen_head as usize];
        let client_idx = state.socket_idx_by_id(client_id).ok_or(UdsError::NoMem)? as usize;
        let new_idx = state.find_free_socket().ok_or(UdsError::NoMem)?;
        let id = alloc_socket_id();
        let ns = &mut state.sockets[new_idx as usize];
        ns.id = id;
        ns.sock_type = UnixSockType::Stream;
        ns.state = UnixSockState::Connected;
        ns.peer = Some(client_id);
        ns.stream_len = 0;
        ns.peer_closed = false;
        state.sockets[listen_slot].listen_head = (state.sockets[listen_slot].listen_head + 1)
            % crate::functions::intcast::low_u8_from_usize(UNIX_LISTEN_BACKLOG);
        state.sockets[listen_slot].listen_count -= 1;
        state.sockets[client_idx].peer = Some(id);
        Ok(new_idx)
    })
}

/// 发起到指定路径 UDS 套接字的连接
///
/// # Errors
///
/// 当路径为空或过长时返回 `Err(UdsError::Invalid)`; `idx` 无效时返回 `Err(UdsError::BadFd)`;
/// 套接字状态非法或两端类型不匹配时返回 `Err(UdsError::Invalid)`; 目标路径不存在时返回 `Err(UdsError::ConnRefused)`;
/// 目标监听队列已满时返回 `Err(UdsError::Again)`。
pub fn uds_connect(idx: u8, path: &[u8]) -> Result<(), UdsError> {
    if path.is_empty() || path.len() > UNIX_PATH_MAX {
        return Err(UdsError::Invalid);
    }
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let s = &state.sockets[i];
        if s.state != UnixSockState::Unbound && s.state != UnixSockState::Bound {
            return Err(UdsError::Invalid);
        }
        let pidx = state.find_path(path).ok_or(UdsError::ConnRefused)? as usize;
        let target_idx = state.paths[pidx].sock_idx as usize;
        let target = &state.sockets[target_idx];
        if target.sock_type != state.sockets[i].sock_type {
            return Err(UdsError::Invalid);
        }
        match (state.sockets[i].sock_type, target.state) {
            (UnixSockType::Stream, UnixSockState::Listening) => {
                if target.listen_count as usize >= UNIX_LISTEN_BACKLOG {
                    return Err(UdsError::Again);
                }
                let tail = target.listen_tail as usize;
                let my_id = state.sockets[i].id;
                state.sockets[target_idx].listen_pending[tail] = my_id;
                state.sockets[target_idx].listen_tail = (state.sockets[target_idx].listen_tail + 1)
                    % crate::functions::intcast::low_u8_from_usize(UNIX_LISTEN_BACKLOG);
                state.sockets[target_idx].listen_count += 1;
                state.sockets[i].state = UnixSockState::Connected;
                state.sockets[i].bound_path_len = 0;
                Ok(())
            }
            (UnixSockType::Dgram, _) => {
                let my_id = state.sockets[i].id;
                let peer_id = target.id;
                state.sockets[i].peer = Some(peer_id);
                state.sockets[target_idx].peer = Some(my_id);
                state.sockets[i].state = UnixSockState::Connected;
                state.sockets[i].bound_path_len = 0;
                Ok(())
            }
            _ => Err(UdsError::ConnRefused),
        }
    })
}

/// 通过流式 UDS 连接发送数据
///
/// # Errors
///
/// 当 `idx` 无效时返回 `Err(UdsError::BadFd)`; 非流式套接字时返回 `Err(UdsError::Invalid)`; 未连接或对端已关闭时返回 `Err(UdsError::NotFound)`;
/// 对端缓冲区无剩余空间时返回 `Err(UdsError::Again)`。
pub fn uds_send(idx: u8, data: &[u8]) -> Result<usize, UdsError> {
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let s = &state.sockets[i];
        if s.sock_type != UnixSockType::Stream {
            return Err(UdsError::Invalid);
        }
        if s.state != UnixSockState::Connected {
            return Err(UdsError::NotFound);
        }
        let peer_id = s.peer.ok_or(UdsError::NotFound)?;
        let peer_idx = state.socket_idx_by_id(peer_id).ok_or(UdsError::NotFound)? as usize;
        let peer = &mut state.sockets[peer_idx];
        if peer.peer_closed {
            return Err(UdsError::NotFound);
        }
        let space = UNIX_STREAM_BUF - peer.stream_len as usize;
        let n = data.len().min(space);
        if n == 0 {
            return Err(UdsError::Again);
        }
        peer.stream_buf[peer.stream_len as usize..peer.stream_len as usize + n]
            .copy_from_slice(&data[..n]);
        peer.stream_len += crate::functions::intcast::low_u32_from_usize(n);
        // v2 SO_PASSCRED: 对端 passcred=true 时附加 SCM_CREDENTIALS.
        // 简化方案: 凭据追加到 stream_buf 末尾 (POSIX 兼容凭据含 pid/uid/gid).
        // B07-01: 使用当前进程真实凭据, 消除硬编码伪造的 root 凭据.
        if peer.passcred && peer.stream_len as usize + 12 <= UNIX_STREAM_BUF {
            // 序列化 ScmCredentials 到 12 字节 (safe: 字段都是 Copy u32)
            let cred = current_scm_credentials();
            let off = peer.stream_len as usize;
            let pid_bytes = cred.pid.to_ne_bytes();
            let uid_bytes = cred.uid.to_ne_bytes();
            let gid_bytes = cred.gid.to_ne_bytes();
            let mut bytes = [0u8; 12];
            bytes[0..4].copy_from_slice(&pid_bytes);
            bytes[4..8].copy_from_slice(&uid_bytes);
            bytes[8..12].copy_from_slice(&gid_bytes);
            peer.stream_buf[off..off + 12].copy_from_slice(&bytes);
            peer.stream_len += 12;
        }
        Ok(n)
    })
}

/// v2: 接收 stream 消息并提取凭据 (若对端发送时附加了 `SCM_CREDENTIALS`).
/// 返回 (字节数, 凭据或 None). 调用方分别处理数据和凭据.
///
/// # Errors
///
/// 当 `idx` 无效时返回 `Err(UdsError::BadFd)`; 非流式套接字时返回 `Err(UdsError::Invalid)`; 对端已关闭时返回 `Err(UdsError::NotFound)`;
/// 缓冲为空且对端未关闭时返回 `Err(UdsError::Again)`。
pub fn uds_recv_with_creds(
    idx: u8,
    out: &mut [u8],
) -> Result<(usize, Option<ScmCredentials>), UdsError> {
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let s = &mut state.sockets[i];
        if s.sock_type != UnixSockType::Stream {
            return Err(UdsError::Invalid);
        }
        if s.stream_len == 0 {
            if s.peer_closed {
                return Err(UdsError::NotFound);
            }
            return Err(UdsError::Again);
        }
        // v2 简化: 凭据固定 12 字节追加在数据末尾 (本端发送逻辑约定).
        // 检测方式: 如果 stream_len >= 数据 + 12 且末 12 字节以 SCM magic 开头, 提取凭据.
        // 简化: 客户端实现约定凭据在最后 12 字节. 我们不实现 magic 校验, 简单尝试读取.
        let total = s.stream_len as usize;
        let cred_size = 12usize;
        let (data_len, cred) = if total >= cred_size && s.passcred {
            // B07-01: 反序列化末尾 12 字节为发送方真实凭据 (发送侧按
            // `[pid 4B][uid 4B][gid 4B]` 小端写入), 不再返回占位全零.
            let off = total - cred_size;
            let mut pid = [0u8; 4];
            let mut uid = [0u8; 4];
            let mut gid = [0u8; 4];
            pid.copy_from_slice(&s.stream_buf[off..off + 4]);
            uid.copy_from_slice(&s.stream_buf[off + 4..off + 8]);
            gid.copy_from_slice(&s.stream_buf[off + 8..off + 12]);
            (
                total - cred_size,
                Some(ScmCredentials {
                    pid: u32::from_ne_bytes(pid),
                    uid: u32::from_ne_bytes(uid),
                    gid: u32::from_ne_bytes(gid),
                }),
            )
        } else {
            (total, None)
        };
        let n = data_len.min(out.len());
        out[..n].copy_from_slice(&s.stream_buf[..n]);
        // 重置 stream_len (简化: 全清空, 不支持粘包)
        s.stream_len = 0;
        Ok((n, cred))
    })
}

/// 通过流式 UDS 连接接收数据
///
/// # Errors
///
/// 当 `idx` 无效时返回 `Err(UdsError::BadFd)`; 非流式套接字时返回 `Err(UdsError::Invalid)`; 套接字未连接时返回 `Err(UdsError::Invalid)`;
/// 缓冲为空且对端未关闭时返回 `Err(UdsError::Again)` (对端已关闭时返回 `Ok(0)`)。
pub fn uds_recv(idx: u8, out: &mut [u8]) -> Result<usize, UdsError> {
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let s = &mut state.sockets[i];
        if s.sock_type != UnixSockType::Stream {
            return Err(UdsError::Invalid);
        }
        if s.state != UnixSockState::Connected {
            return Err(UdsError::Invalid);
        }
        if s.stream_len == 0 {
            if s.peer_closed {
                return Ok(0);
            }
            return Err(UdsError::Again);
        }
        let n = (s.stream_len as usize).min(out.len());
        out[..n].copy_from_slice(&s.stream_buf[..n]);
        let remaining = s.stream_len as usize - n;
        if remaining > 0 {
            s.stream_buf.copy_within(n..n + remaining, 0);
        }
        s.stream_len = crate::functions::intcast::low_u32_from_usize(remaining);
        Ok(n)
    })
}

/// 向指定路径的 UDS 数据报套接字发送数据
///
/// # Errors
///
/// 当目标路径不存在时返回 `Err(UdsError::ConnRefused)`; 目标套接字非数据报类型时返回 `Err(UdsError::Invalid)`;
/// 数据超过 `UNIX_DGRAM_MAX` 时返回 `Err(UdsError::Invalid)` (越界写防护)。
pub fn uds_sendto(data: &[u8], dest_path: &[u8]) -> Result<usize, UdsError> {
    if data.len() > UNIX_DGRAM_MAX {
        return Err(UdsError::Invalid);
    }
    UDS_STATE.with_mut(|state| {
        let pidx = state.find_path(dest_path).ok_or(UdsError::ConnRefused)? as usize;
        let target_idx = state.paths[pidx].sock_idx as usize;
        let target = &mut state.sockets[target_idx];
        if target.sock_type != UnixSockType::Dgram {
            return Err(UdsError::Invalid);
        }
        target.dgram_buf[..data.len()].copy_from_slice(data);
        target.dgram_len = crate::functions::intcast::low_u32_from_usize(data.len());
        target.dgram_pending = true;
        // v2 SO_PASSCRED: 目标 socket 启用时附加 SCM_CREDENTIALS 12 字节
        // B07-01: 使用当前进程真实凭据, 消除硬编码伪造的 root 凭据.
        if target.passcred && target.dgram_len as usize + 12 <= UNIX_DGRAM_MAX {
            // 序列化 ScmCredentials (safe u32 字段拼装)
            let cred = current_scm_credentials();
            let off = target.dgram_len as usize;
            let pid_bytes = cred.pid.to_ne_bytes();
            let uid_bytes = cred.uid.to_ne_bytes();
            let gid_bytes = cred.gid.to_ne_bytes();
            let mut bytes = [0u8; 12];
            bytes[0..4].copy_from_slice(&pid_bytes);
            bytes[4..8].copy_from_slice(&uid_bytes);
            bytes[8..12].copy_from_slice(&gid_bytes);
            target.dgram_buf[off..off + 12].copy_from_slice(&bytes);
            target.dgram_len += 12;
        }
        Ok(data.len())
    })
}

// SIMPLIFIED: Dgram 已连接发送为单条在途非排队 (Linux 为可靠排队缓冲);
// 影响: 对端未收走时再次发送返回 EAGAIN 而非入队, 高吞吐场景可能放大 EAGAIN;
// 扩展时机: dgram 缓冲队列化 (环形队列 + 容量配置) 时改写本函数为追加队尾.
/// 向已连接的 UDS 套接字发送数据 (`sendmsg`/`sendmmsg` 无目标地址路径)
///
/// Stream: 委托 [`uds_send`] (要求 Connected, 语义一致)。Dgram: 需互设 peer
/// (socketpair 或 `uds_connect`), 写入对端 dgram 缓冲并附加凭据 (同 [`uds_sendto`])。
///
/// # Errors
///
/// `idx` 无效时返回 `Err(UdsError::BadFd)`; 未连接或对端已关闭时返回 `Err(UdsError::NotFound)`;
/// 数据超过 `UNIX_DGRAM_MAX` 时返回 `Err(UdsError::Invalid)`; 对端已有在途数据报时返回
/// `Err(UdsError::Again)`。
pub fn uds_send_connected(idx: u8, data: &[u8]) -> Result<usize, UdsError> {
    let sock_type = uds_sock_type(idx)?;
    match sock_type {
        UnixSockType::Stream => uds_send(idx, data),
        UnixSockType::Dgram => UDS_STATE.with_mut(|state| {
            let i = check_slot(state, idx)?;
            if state.sockets[i].state != UnixSockState::Connected {
                return Err(UdsError::NotFound);
            }
            let peer_id = state.sockets[i].peer.ok_or(UdsError::NotFound)?;
            let peer_idx = state.socket_idx_by_id(peer_id).ok_or(UdsError::NotFound)? as usize;
            let peer = &mut state.sockets[peer_idx];
            if peer.peer_closed {
                return Err(UdsError::NotFound);
            }
            if data.len() > UNIX_DGRAM_MAX {
                return Err(UdsError::Invalid);
            }
            if peer.dgram_pending {
                return Err(UdsError::Again);
            }
            peer.dgram_buf[..data.len()].copy_from_slice(data);
            peer.dgram_len = crate::functions::intcast::low_u32_from_usize(data.len());
            peer.dgram_pending = true;
            // v2 SO_PASSCRED: 同 uds_sendto, 对端启用时附加 SCM_CREDENTIALS 12 字节
            if peer.passcred && peer.dgram_len as usize + 12 <= UNIX_DGRAM_MAX {
                let cred = current_scm_credentials();
                let off = peer.dgram_len as usize;
                let p = cred.pid.to_ne_bytes();
                let u = cred.uid.to_ne_bytes();
                let g = cred.gid.to_ne_bytes();
                let mut bytes = [0u8; 12];
                bytes[0..4].copy_from_slice(&p);
                bytes[4..8].copy_from_slice(&u);
                bytes[8..12].copy_from_slice(&g);
                peer.dgram_buf[off..off + 12].copy_from_slice(&bytes);
                peer.dgram_len += 12;
            }
            Ok(data.len())
        }),
    }
}

/// 从 UDS 数据报套接字接收数据
///
/// # Errors
///
/// 传播自 [`uds_recvfrom_with_creds`]: `idx` 无效或非数据报类型时返回 `Err(UdsError::Invalid)`;
/// 无待收数据时返回 `Err(UdsError::Again)`。
pub fn uds_recvfrom(idx: u8, out: &mut [u8]) -> Result<usize, UdsError> {
    let (n, _cred) = uds_recvfrom_with_creds(idx, out)?;
    Ok(n)
}

/// v2: DGRAM 接收并提取凭据
///
/// # Errors
///
/// 当 `idx` 无效或非数据报类型时返回 `Err(UdsError::Invalid)`; 无待收数据时返回 `Err(UdsError::Again)`。
pub fn uds_recvfrom_with_creds(
    idx: u8,
    out: &mut [u8],
) -> Result<(usize, Option<ScmCredentials>), UdsError> {
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let s = &mut state.sockets[i];
        if s.sock_type != UnixSockType::Dgram {
            return Err(UdsError::Invalid);
        }
        if !s.dgram_pending {
            return Err(UdsError::Again);
        }
        let total = s.dgram_len as usize;
        let cred_size = 12usize;
        let (data_len, cred) = if s.passcred && total >= cred_size {
            // B07-01: 反序列化末尾 12 字节为发送方真实凭据, 不再返回占位全零.
            let off = total - cred_size;
            let mut pid = [0u8; 4];
            let mut uid = [0u8; 4];
            let mut gid = [0u8; 4];
            pid.copy_from_slice(&s.dgram_buf[off..off + 4]);
            uid.copy_from_slice(&s.dgram_buf[off + 4..off + 8]);
            gid.copy_from_slice(&s.dgram_buf[off + 8..off + 12]);
            (
                total - cred_size,
                Some(ScmCredentials {
                    pid: u32::from_ne_bytes(pid),
                    uid: u32::from_ne_bytes(uid),
                    gid: u32::from_ne_bytes(gid),
                }),
            )
        } else {
            (total, None)
        };
        let n = data_len.min(out.len());
        out[..n].copy_from_slice(&s.dgram_buf[..n]);
        s.dgram_pending = false;
        s.dgram_len = 0;
        Ok((n, cred))
    })
}

/// 释放 UDS 套接字槽位并清理其路径绑定与对端关系
///
/// 由 fd 对象的 `release()` 在最后一个引用关闭时调用, 因此槽位回收即对象
/// 生命周期终点. 监听端关闭时**不回收**待连接客户端的槽位 (其所有权仍在
/// 各自 fd 对象上), 只标记对端已关闭 — 避免槽位被新 socket 复用后, 陈旧
/// 客户端的释放误伤新持有者.
///
/// # Errors
///
/// 当 `idx` 越界或槽位已释放时返回 `Err(UdsError::BadFd)`。
pub fn uds_free(idx: u8) -> Result<(), UdsError> {
    UDS_STATE.with_mut(|state| {
        let i = check_slot(state, idx)?;
        let sock_type = state.sockets[i].sock_type;
        let state_val = state.sockets[i].state;
        let bound_path = state.sockets[i].bound_path;
        let bound_path_len = state.sockets[i].bound_path_len;
        let peer_id = state.sockets[i].peer;

        if sock_type == UnixSockType::Stream
            && state_val == UnixSockState::Connected
            && let Some(pid) = peer_id
            && let Some(pidx) = state.socket_idx_by_id(pid)
        {
            state.sockets[pidx as usize].peer_closed = true;
            state.sockets[pidx as usize].peer = None;
        }

        if sock_type == UnixSockType::Stream && state_val == UnixSockState::Listening {
            let head = state.sockets[i].listen_head;
            let count = state.sockets[i].listen_count;
            for k in 0..count {
                let pos =
                    (head + k) % crate::functions::intcast::low_u8_from_usize(UNIX_LISTEN_BACKLOG);
                let client_id = state.sockets[i].listen_pending[pos as usize];
                if let Some(ci) = state.socket_idx_by_id(client_id) {
                    state.sockets[ci as usize].peer_closed = true;
                    state.sockets[ci as usize].peer = None;
                }
            }
        }

        if bound_path_len > 0 {
            for i in 0..state.paths.len() {
                if state.paths[i].used
                    && state.paths[i].path_len == bound_path_len
                    && state.paths[i].path[..bound_path_len as usize]
                        == bound_path[..bound_path_len as usize]
                {
                    state.paths[i].used = false;
                    state.paths[i].sock_idx = 0;
                    state.paths[i].path_len = 0;
                    break;
                }
            }
        }

        state.sockets[i] = UnixSocket::empty();
        Ok(())
    })
}

// ============================================================================
// poll 就绪查询
// ============================================================================

/// 查询 UDS 套接字的就绪事件 (`File::poll` 的实现面, 非阻塞可重复调用)
///
/// 返回请求位的子集, 可附加 `HUP` (对端已关闭). 槽位无效/已释放时返回空集
/// — `File::poll` 契约不允许返回未请求的 `NVAL`.
#[must_use]
pub fn uds_poll(idx: u8, events: PollEvents) -> PollEvents {
    let mut ready = PollEvents::empty();
    UDS_STATE.with(|state| {
        let Ok(i) = check_slot(state, idx) else {
            return;
        };
        let s = &state.sockets[i];
        match s.sock_type {
            UnixSockType::Stream => {
                if s.state == UnixSockState::Listening {
                    if s.listen_count > 0 {
                        ready |= PollEvents::IN;
                    }
                } else {
                    if s.stream_len > 0 || s.peer_closed {
                        ready |= PollEvents::IN;
                    }
                    if s.peer_closed {
                        ready |= PollEvents::HUP;
                    }
                    if s.state == UnixSockState::Connected {
                        ready |= PollEvents::OUT;
                    }
                }
            }
            UnixSockType::Dgram => {
                if s.dgram_pending {
                    ready |= PollEvents::IN;
                }
                if matches!(s.state, UnixSockState::Bound | UnixSockState::Connected) {
                    ready |= PollEvents::OUT;
                }
            }
        }
    });
    (ready & events) | (ready & (PollEvents::ERR | PollEvents::HUP))
}

// ============================================================================
// v2: SO_PASSCRED / cmsg / 抽象命名空间
// ============================================================================

/// v2: `SO_PASSCRED` 选项 — 设置 socket 是否在 sendmsg 中附加 `SCM_CREDENTIALS`.
///
/// 由 `functions::net::syscall::setsockopt_syscall` 在 `level=SOL_SOCKET` /
/// `optname=SO_PASSCRED` 且 fd 对象为 `UnixSocketFile` 时分流至此. 仅对 UDS
/// socket 生效, 其他 family 返 ENOPROTOOPT.
pub fn uds_setsockopt(idx: u8, enable: bool) -> i32 {
    UDS_STATE.with_mut(|state| {
        let Ok(i) = check_slot(state, idx) else {
            return -9; // EBADF
        };
        if !matches!(
            state.sockets[i].sock_type,
            UnixSockType::Stream | UnixSockType::Dgram
        ) {
            return -92; // ENOPROTOOPT
        }
        state.sockets[i].passcred = enable;
        0
    })
}

/// v2: `SO_PASSCRED` 查询.
pub fn uds_getsockopt_passcred(idx: u8) -> i32 {
    UDS_STATE.with(|state| {
        let Ok(i) = check_slot(state, idx) else {
            return -9;
        };
        i32::from(state.sockets[i].passcred)
    })
}

/// v2: `SCM_CREDENTIALS` 数据结构 (Linux ABI 12 字节)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScmCredentials {
    /// 发送方进程号.
    pub pid: u32,
    /// 发送方用户号.
    pub uid: u32,
    /// 发送方组号.
    pub gid: u32,
}

/// B07-01: 获取当前进程的真实凭据 (消除硬编码伪造的 root 凭据).
///
/// `pid`/`uid`/`gid` 取自 privileged 的权威凭据源:
/// `proc::process_get_current_pid()` + `sgeg::session::{get_current_uid, get_current_gid}`.
fn current_scm_credentials() -> ScmCredentials {
    ScmCredentials {
        pid: crate::privileged::proc::process_get_current_pid(),
        uid: crate::privileged::sgeg::get_current_uid(),
        gid: crate::privileged::sgeg::get_current_gid(),
    }
}

/// B07-02: 读取本端接收缓冲末尾 12 字节的发送方凭据 (不消费缓冲).
///
/// 供 `recvmsg` 路径回写 `SCM_CREDENTIALS` cmsg 使用; 槽位无效 / 未启用
/// `SO_PASSCRED` / 缓冲不足 12 字节时返回 `None` (不伪造凭据).
pub fn uds_peer_creds(idx: u8) -> Option<ScmCredentials> {
    UDS_STATE.with(|state| {
        let i = check_slot(state, idx).ok()?;
        let s = &state.sockets[i];
        if !s.passcred {
            return None;
        }
        let (buf, total): (&[u8], usize) = if s.sock_type == UnixSockType::Stream {
            (&s.stream_buf, s.stream_len as usize)
        } else {
            (&s.dgram_buf, s.dgram_len as usize)
        };
        if total < 12 {
            return None;
        }
        let off = total - 12;
        Some(ScmCredentials {
            pid: u32::from_ne_bytes(buf[off..off + 4].try_into().ok()?),
            uid: u32::from_ne_bytes(buf[off + 4..off + 8].try_into().ok()?),
            gid: u32::from_ne_bytes(buf[off + 8..off + 12].try_into().ok()?),
        })
    })
}

/// v2: cmsg 头 (Linux msghdr ancillary data, 16 字节)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Cmsghdr {
    /// 整个 cmsg (含头部) 的字节长度.
    pub cmsg_len: usize,
    /// 控制消息所属协议层级.
    pub cmsg_level: i32,
    /// 控制消息类型.
    pub cmsg_type: i32,
    /// 变长数据区起点, 通过指针偏移访问.
    pub data: [u8; 0], // flex array, 通过指针偏移访问
}

/// 抽象 namespace 路径解析: `path[0]` == 0 标识 abstract namespace.
/// 返回 `&path[1..]` (去掉前导 0 字节), 其余按文件系统路径处理.
#[must_use]
pub fn uds_parse_path(path: &[u8]) -> Option<(&[u8], bool)> {
    if path.is_empty() {
        return None;
    }
    if path[0] == 0 {
        // 抽象 namespace
        Some((&path[1..], true))
    } else {
        // 文件系统路径
        Some((path, false))
    }
}

/// 重置 UDS 子系统到初始态 (仅供测试使用).
pub fn uds_reset_for_test() {
    // 惰性堆置等价语义: 直接重建两表 (保证已堆置且全部归零).
    // fd 由 per-process `FdTable` 持有, 测试侧经 `uds_free` 归还槽位, 无需
    // 再同步任何全局位图 (编号区间与 `fd_alloc` 位图已随对象模型退役).
    UDS_STATE.with_mut(UdsState::allocate);
    NEXT_SOCK_ID.with_mut(|id| *id = 1);
}

// ============================================================================
// 错误映射
// ============================================================================

/// UDS 错误 (functions 层映射)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketError {
    /// 目标路径不存在.
    PathNotFound,
    /// 内核通用错误.
    Kernel(crate::functions::error::KernelError),
}

impl UnixSocketError {
    /// 映射为系统调用错误码.
    #[must_use]
    pub fn to_errno(self) -> Errno {
        match self {
            Self::PathNotFound => Errno::Enoent,
            Self::Kernel(e) => e.as_errno(),
        }
    }
}

impl From<UdsError> for UnixSocketError {
    fn from(e: UdsError) -> Self {
        use crate::functions::error::KernelError as K;
        match e {
            UdsError::NotFound => Self::PathNotFound,
            UdsError::BadFd => Self::Kernel(K::BadFd),
            UdsError::Again => Self::Kernel(K::WouldBlock),
            UdsError::NoMem => Self::Kernel(K::NoMemory),
            UdsError::AddrFamily => Self::Kernel(K::AddrFamilyNotSupported),
            UdsError::AddrInUse => Self::Kernel(K::AddrInUse),
            UdsError::ConnRefused => Self::Kernel(K::ConnectionRefused),
            UdsError::Invalid => Self::Kernel(K::InvalidArgument),
            UdsError::NoSys => Self::Kernel(K::NotSupported),
        }
    }
}

impl From<crate::functions::error::KernelError> for UnixSocketError {
    fn from(e: crate::functions::error::KernelError) -> Self {
        Self::Kernel(e)
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 模块内用例的互斥锁.
    ///
    /// 所有用例共享全局 `UDS_STATE`, 且均以 `uds_reset_for_test()` 开场; 并行
    /// test runner 下重置会破坏其他用例正在使用的 socket, 故必须互斥
    /// (2026-09-24 UT-06, 口径同 privileged/timer/tick.rs 的 FREQ_TEST_LOCK).
    static UDS_TEST_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

    #[test]
    fn stream_bind_listen_connect_accept_echo() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let srv = uds_alloc(UnixSockType::Stream).expect("srv alloc");
        let cli = uds_alloc(UnixSockType::Stream).expect("cli alloc");
        uds_bind(srv, b"/tmp/test.sock").expect("srv bind");
        uds_listen(srv).expect("srv listen");
        uds_connect(cli, b"/tmp/test.sock").expect("cli connect");
        let accepted = uds_accept(srv).expect("accept");
        assert_ne!(accepted, srv);
        let n = uds_send(cli, b"hello").expect("send");
        assert_eq!(n, 5);
        let mut buf = [0u8; 16];
        let m = uds_recv(accepted, &mut buf).expect("recv");
        assert_eq!(m, 5);
        assert_eq!(&buf[..5], b"hello");
        let k = uds_send(accepted, b"world").expect("send back");
        assert_eq!(k, 5);
        let mut buf2 = [0u8; 16];
        let p = uds_recv(cli, &mut buf2).expect("recv back");
        assert_eq!(p, 5);
        assert_eq!(&buf2[..5], b"world");
        uds_free(cli).expect("cli free");
        uds_free(accepted).expect("acc free");
        uds_free(srv).expect("srv free");
    }

    #[test]
    fn dgram_bind_connect_echo() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let rx = uds_alloc(UnixSockType::Dgram).expect("rx alloc");
        let tx = uds_alloc(UnixSockType::Dgram).expect("tx alloc");
        uds_bind(rx, b"/tmp/rx.sock").expect("rx bind");
        uds_connect(tx, b"/tmp/rx.sock").expect("tx connect");
        let n = uds_sendto(b"datagram-payload", b"/tmp/rx.sock").expect("sendto");
        assert_eq!(n, 16);
        let mut buf = [0u8; 32];
        let m = uds_recvfrom(rx, &mut buf).expect("recvfrom");
        assert_eq!(m, 16);
        assert_eq!(&buf[..16], b"datagram-payload");
        uds_free(tx).expect("free tx");
        uds_free(rx).expect("free rx");
    }

    #[test]
    fn eaddrinuse_on_duplicate_bind() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let a = uds_alloc(UnixSockType::Stream).expect("a");
        let b = uds_alloc(UnixSockType::Stream).expect("b");
        uds_bind(a, b"/tmp/dup.sock").expect("a bind");
        let r = uds_bind(b, b"/tmp/dup.sock");
        assert_eq!(r, Err(UdsError::AddrInUse));
        uds_free(a).expect("free a");
        uds_free(b).expect("free b");
    }

    #[test]
    fn eagain_on_empty_accept() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let s = uds_alloc(UnixSockType::Stream).expect("s");
        uds_bind(s, b"/tmp/empty.sock").expect("bind");
        uds_listen(s).expect("listen");
        let r = uds_accept(s);
        assert_eq!(r, Err(UdsError::Again));
        uds_free(s).expect("free");
    }

    #[test]
    fn close_listener_keeps_client_slot_ownership() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let srv = uds_alloc(UnixSockType::Stream).expect("srv");
        let cli1 = uds_alloc(UnixSockType::Stream).expect("c1");
        let cli2 = uds_alloc(UnixSockType::Stream).expect("c2");
        uds_bind(srv, b"/tmp/cancel.sock").expect("bind");
        uds_listen(srv).expect("listen");
        uds_connect(cli1, b"/tmp/cancel.sock").expect("c1 connect");
        uds_connect(cli2, b"/tmp/cancel.sock").expect("c2 connect");
        uds_free(srv).expect("free srv");
        // 待连接客户端槽位所有权仍在其 fd 对象上: 释放必须成功, 不得被
        // 监听端回收后误判 BadFd; 对端已关闭 → 发失败 / 收 EOF.
        assert_eq!(uds_send(cli1, b"x"), Err(UdsError::NotFound));
        let mut buf = [0u8; 4];
        assert_eq!(uds_recv(cli1, &mut buf), Ok(0));
        uds_free(cli1).expect("free c1");
        uds_free(cli2).expect("free c2");
    }

    #[test]
    fn socketpair_stream_bidirectional() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let (a, b) = uds_socketpair(UnixSockType::Stream).expect("socketpair");
        assert_ne!(a, b);
        let n = uds_send(a, b"ping").expect("send a->b");
        assert_eq!(n, 4);
        let mut buf = [0u8; 8];
        let m = uds_recv(b, &mut buf).expect("recv b");
        assert_eq!(m, 4);
        assert_eq!(&buf[..4], b"ping");
        let k = uds_send(b, b"pong").expect("send b->a");
        assert_eq!(k, 4);
        let mut buf2 = [0u8; 8];
        let p = uds_recv(a, &mut buf2).expect("recv a");
        assert_eq!(p, 4);
        assert_eq!(&buf2[..4], b"pong");
        uds_free(a).expect("free a");
        uds_free(b).expect("free b");
    }

    #[test]
    fn socketpair_dgram_connected_send() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let (a, b) = uds_socketpair(UnixSockType::Dgram).expect("socketpair");
        // 已连接 Dgram 无路径发送走 uds_send_connected, 接收走 uds_recvfrom
        let n = uds_send_connected(a, b"dg").expect("send_connected");
        assert_eq!(n, 2);
        let mut buf = [0u8; 8];
        let m = uds_recvfrom(b, &mut buf).expect("recvfrom");
        assert_eq!(m, 2);
        assert_eq!(&buf[..2], b"dg");
        // 对端在途未收走时再发 → EAGAIN (单条在途简化)
        let _ = uds_send_connected(a, b"first").expect("first");
        assert_eq!(uds_send_connected(a, b"second"), Err(UdsError::Again));
        let _ = uds_recvfrom(b, &mut buf).expect("drain");
        uds_free(a).expect("free a");
        uds_free(b).expect("free b");
    }

    #[test]
    fn socketpair_rollback_on_second_create_failure() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        // 占满 MAX_UDS_FD - 1 个槽位, socketpair 需连续 2 个空槽 → 第二端 NoMem
        let mut guard = [0u8; MAX_UDS_FD];
        for slot in guard.iter_mut().take(MAX_UDS_FD - 1) {
            *slot = uds_alloc(UnixSockType::Stream).expect("fill");
        }
        assert_eq!(uds_socketpair(UnixSockType::Stream), Err(UdsError::NoMem));
        // 回滚验证: 第一端已被释放归还槽位, 单个分配应成功
        let one = uds_alloc(UnixSockType::Stream).expect("single after rollback");
        uds_free(one).expect("free one");
    }

    #[test]
    fn poll_readiness_by_state() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let srv = uds_alloc(UnixSockType::Stream).expect("srv");
        uds_bind(srv, b"/tmp/poll.sock").expect("bind");
        uds_listen(srv).expect("listen");
        // 空监听队列: 不可读
        assert!(!uds_poll(srv, PollEvents::IN).contains(PollEvents::IN));
        let cli = uds_alloc(UnixSockType::Stream).expect("cli");
        uds_connect(cli, b"/tmp/poll.sock").expect("connect");
        // 待连接 → 监听端可读
        assert!(uds_poll(srv, PollEvents::IN).contains(PollEvents::IN));
        let acc = uds_accept(srv).expect("accept");
        // 已连接 → 两端可写
        assert!(uds_poll(cli, PollEvents::OUT).contains(PollEvents::OUT));
        assert!(uds_poll(acc, PollEvents::OUT).contains(PollEvents::OUT));
        uds_send(cli, b"hi").expect("send");
        assert!(uds_poll(acc, PollEvents::IN).contains(PollEvents::IN));
        // 关闭一端 → 对端挂断 (HUP 不受请求位限制)
        uds_free(cli).expect("free cli");
        assert!(uds_poll(acc, PollEvents::empty()).contains(PollEvents::HUP));
        uds_free(acc).expect("free acc");
        uds_free(srv).expect("free srv");
    }

    #[test]
    fn invalid_slot_is_rejected_not_panicking() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let bad = crate::functions::intcast::low_u8_from_usize(MAX_UDS_FD);
        assert_eq!(uds_sock_type(bad), Err(UdsError::BadFd));
        assert_eq!(uds_free(bad), Err(UdsError::BadFd));
        assert_eq!(uds_poll(bad, PollEvents::IN), PollEvents::empty());
    }
}
