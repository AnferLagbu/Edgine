#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! Unix Domain Socket (`AF_UNIX`) — functions 层策略主体
//!
//! ## T3-4 迁移记录
//!
//! 原属 privileged/net/unix.rs, 2026-06-16 提取到 functions.
//! 纯策略代码 (socket CRUD + 路径绑定 + STREAM/DGRAM 数据传输), 0 unsafe.
//! privileged 仅保留 re-export.

use alloc::vec::Vec;

use crate::functions::proc::fd_alloc::{FdSubsystem, free_fd};
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// FD 起点, 位于 smoltcp FD 空间 (`[0, 256)`) 之后, 与之不重叠.
pub const UDS_FD_BASE: i32 = crate::privileged::proc::FdPlan::UDS.base;

/// 最大 UDS socket 数量
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
pub enum UdsError {
    BadFd = 9,
    Again = 11,
    NoMem = 12,
    AddrFamily = 97,
    AddrInUse = 98,
    ConnRefused = 111,
    Invalid = 22,
    NotFound = 2,
    NoSys = 38,
}

impl UdsError {
    pub fn as_ret(self) -> i32 {
        -(self as i32)
    }
}

// ============================================================================
// 类型定义
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnixSockType {
    Stream = 1,
    Dgram = 2,
}

impl UnixSockType {
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
pub enum UnixSockState {
    Unbound = 0,
    Bound = 1,
    Listening = 2,
    Connected = 3,
    Closed = 4,
}

#[derive(Debug, Clone)]
pub struct UnixSocket {
    pub id: u32,
    pub sock_type: UnixSockType,
    pub state: UnixSockState,
    pub bound_path: [u8; UNIX_PATH_MAX],
    pub bound_path_len: u16,

    pub listen_pending: [u32; UNIX_LISTEN_BACKLOG],
    pub listen_head: u8,
    pub listen_tail: u8,
    pub listen_count: u8,

    pub peer: Option<u32>,

    pub stream_buf: [u8; UNIX_STREAM_BUF],
    pub stream_len: u32,
    pub peer_closed: bool,

    pub dgram_buf: [u8; UNIX_DGRAM_MAX],
    pub dgram_len: u32,
    pub dgram_pending: bool,

    /// v2: `SO_PASSCRED` 选项 — 对端 socket 设置后, 发送消息自动附加
    /// `SCM_CREDENTIALS` 凭据 (uid/gid/pid).
    pub passcred: bool,
}

impl UnixSocket {
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
pub struct UnixPathBinding {
    pub path: [u8; UNIX_PATH_MAX],
    pub path_len: u16,
    pub sock_idx: u8,
    pub used: bool,
}

impl UnixPathBinding {
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
pub struct UdsState {
    pub sockets: Vec<UnixSocket>,
    pub paths: Vec<UnixPathBinding>,
}

impl UdsState {
    /// 构造空容器 (不含元素), 字段级堆化前置态.
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
                return Some(i as u8);
            }
        }
        None
    }

    /// 查找空闲套接字槽位
    fn find_free_socket(&self) -> Option<u8> {
        for (i, s) in self.sockets.iter().enumerate() {
            if s.id == 0 {
                return Some(i as u8);
            }
        }
        None
    }

    /// 检查是否有空闲套接字槽位
    fn has_free_socket(&self) -> bool {
        self.find_free_socket().is_some()
    }

    /// 获取已使用的套接字数量
    fn used_socket_count(&self) -> usize {
        self.sockets.iter().filter(|s| s.id != 0).count()
    }

    fn find_path(&self, path: &[u8]) -> Option<u8> {
        for (i, p) in self.paths.iter().enumerate() {
            if p.used && p.path_len as usize == path.len() && p.path[..path.len()] == *path {
                return Some(i as u8);
            }
        }
        None
    }

    fn socket_idx_by_id(&self, id: u32) -> Option<u8> {
        for (i, s) in self.sockets.iter().enumerate() {
            if s.id == id {
                return Some(i as u8);
            }
        }
        None
    }
}

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

/// FD → 槽位索引反查 (V3: 使用 `idx_of`, FD 编号由 `fd_at` 计算)
#[inline]
fn fd_to_idx(fd: i32) -> Result<u8, UdsError> {
    match crate::privileged::proc::idx_of(fd) {
        Some((crate::privileged::proc::FdSubsystem::Uds, slot)) => Ok(slot as u8),
        _ => Err(UdsError::BadFd),
    }
}

// ============================================================================
// 公开 API
// ============================================================================

pub fn uds_init() {
    // 惰性堆置: 在堆就绪后填充 sockets/paths 两表 (与旧静态数组语义等价).
    UDS_STATE.with_mut(UdsState::allocate);
    NEXT_SOCK_ID.with_mut(|id| *id = 1);
    // 注册契约 (DECISION-K 统一模式: 机制 init 后注册策略, 第二十六批):
    // privileged sm_setsockopt 的 SO_PASSCRED 路由经此钩子委托本模块,
    // privileged 不再反向依赖 functions::net::unix. 重复注册 Err 忽略 (幂等).
    // cfg 对齐 privileged::net::init 门控 (kernel_test 下 init FFI 层不存在).
    #[cfg(not(feature = "kernel_test"))]
    {
        let _ = crate::privileged::net::init::register_uds_setsockopt_hook(uds_setsockopt);
    }
}

/// 创建一个新的 UDS 套接字, 返回其 FD
///
/// 通过集中分配器申请 FD, 并初始化对应槽位为空闲 (Unbound) 状态。
///
/// # Errors
///
/// 当无空闲套接字槽位或 FD 数量已达 `MAX_UDS_FD` 上限时返回 `Err(UdsError::NoMem)`。
pub fn uds_create(sock_type: UnixSockType) -> Result<i32, UdsError> {
    UDS_STATE.with_mut(|state| {
        // 检查是否有空闲套接字槽位
        if !state.has_free_socket() {
            return Err(UdsError::NoMem);
        }

        // 检查已使用套接字数量 (资源限制)
        let used_count = state.used_socket_count();
        if used_count >= MAX_UDS_FD {
            return Err(UdsError::NoMem);
        }

        // V2: 使用集中分配器获取 FD, 再通过 idx_of 获取槽位索引
        let fd = crate::functions::proc::fd_alloc::alloc_fd(
            crate::functions::proc::fd_alloc::FdSubsystem::Uds,
        )
        .ok_or(UdsError::NoMem)?;

        let (_sub, slot) = crate::functions::proc::fd_alloc::idx_of(fd).ok_or(UdsError::BadFd)?;

        let idx = slot as u8;
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
        Ok(fd)
    })
}

/// 创建一对互相连接的 UDS 套接字 (`socketpair` 机制核心)
///
/// 两端同类型且互设 peer, 均为 Connected 状态, 无路径绑定 (对齐 Linux `socketpair(AF_UNIX)` 语义)。
///
/// # Errors
///
/// 无空闲槽位或 FD 达 `MAX_UDS_FD` 上限时返回 `Err(UdsError::NoMem)`; 第二端创建失败时
/// 回滚关闭第一端; 槽位关联异常时返回 `Err(UdsError::BadFd)` 并回滚两端。
pub fn uds_socketpair(sock_type: UnixSockType) -> Result<(i32, i32), UdsError> {
    let fd0 = uds_create(sock_type)?;
    let fd1 = match uds_create(sock_type) {
        Ok(f) => f,
        Err(e) => {
            let _ = uds_close(fd0);
            return Err(e);
        }
    };
    let linked = UDS_STATE.with_mut(|state| {
        let slot0 = fd_to_idx(fd0)? as usize;
        let slot1 = fd_to_idx(fd1)? as usize;
        if state.sockets[slot0].id == 0 || state.sockets[slot1].id == 0 || slot0 == slot1 {
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
        let _ = uds_close(fd0);
        let _ = uds_close(fd1);
        return Err(e);
    }
    Ok((fd0, fd1))
}

/// 查询 UDS 套接字类型 (`recvmmsg` 分流 Stream/Dgram 接收原语时使用)
///
/// # Errors
///
/// `fd` 无效 (非 UDS 或槽位为空) 时返回 `Err(UdsError::BadFd)`。
pub fn uds_sock_type(fd: i32) -> Result<UnixSockType, UdsError> {
    UDS_STATE.with(|state| {
        let idx = fd_to_idx(fd)? as usize;
        if state.sockets[idx].id == 0 {
            return Err(UdsError::BadFd);
        }
        Ok(state.sockets[idx].sock_type)
    })
}

/// 将 UDS 套接字绑定到指定路径
///
/// # Errors
///
/// 当路径为空或超过 `UNIX_PATH_MAX` 时返回 `Err(UdsError::Invalid)`; `fd` 无效时返回 `Err(UdsError::BadFd)`;
/// 套接字非 Unbound 状态时返回 `Err(UdsError::Invalid)`; 路径已被占用时返回 `Err(UdsError::AddrInUse)`;
/// 路径表无空闲槽位时返回 `Err(UdsError::NoMem)`。
pub fn uds_bind(fd: i32, path: &[u8]) -> Result<(), UdsError> {
    if path.is_empty() || path.len() > UNIX_PATH_MAX {
        return Err(UdsError::Invalid);
    }
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        let s = &state.sockets[idx];
        if s.id == 0 {
            return Err(UdsError::BadFd);
        }
        if s.state != UnixSockState::Unbound {
            return Err(UdsError::Invalid);
        }
        if state.find_path(path).is_some() {
            return Err(UdsError::AddrInUse);
        }
        let pidx = state.find_free_path().ok_or(UdsError::NoMem)?;
        let binding = &mut state.paths[pidx as usize];
        binding.path[..path.len()].copy_from_slice(path);
        binding.path_len = path.len() as u16;
        binding.sock_idx = idx as u8;
        binding.used = true;
        let s = &mut state.sockets[idx];
        s.bound_path[..path.len()].copy_from_slice(path);
        s.bound_path_len = path.len() as u16;
        s.state = UnixSockState::Bound;
        Ok(())
    })
}

/// 将已绑定的流式 UDS 套接字置为监听状态
///
/// # Errors
///
/// 当 `fd` 无效时返回 `Err(UdsError::BadFd)`; 套接字非流式或未处于 Bound 状态时返回 `Err(UdsError::Invalid)`。
pub fn uds_listen(fd: i32) -> Result<(), UdsError> {
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        let s = &state.sockets[idx];
        if s.id == 0 {
            return Err(UdsError::BadFd);
        }
        if s.sock_type != UnixSockType::Stream {
            return Err(UdsError::Invalid);
        }
        if s.state != UnixSockState::Bound {
            return Err(UdsError::Invalid);
        }
        state.sockets[idx].state = UnixSockState::Listening;
        Ok(())
    })
}

/// 从监听套接字的待连接队列中取出一个客户端连接, 返回新连接的 FD
///
/// # Errors
///
/// 当新 FD 分配失败时返回 `Err(UdsError::NoMem)`; `fd` 无效时返回 `Err(UdsError::BadFd)`;
/// 套接字非流式或非 Listening 状态时返回 `Err(UdsError::Invalid)`; 待连接队列为空时返回 `Err(UdsError::Again)`。
pub fn uds_accept(fd: i32) -> Result<i32, UdsError> {
    // V2: 使用集中分配器获取新 FD
    let new_fd = crate::functions::proc::fd_alloc::alloc_fd(
        crate::functions::proc::fd_alloc::FdSubsystem::Uds,
    )
    .ok_or(UdsError::NoMem)?;

    // UT-06 (2026-09-24): 预分配 FD 后所有失败路径必须回收, 否则位图泄漏
    // (实测: 空队列 accept 每次调用泄漏 1 个 UDS FD, 累积至 16 后 alloc_fd 恒 None).
    let Some((_, new_slot)) = crate::functions::proc::fd_alloc::idx_of(new_fd) else {
        let _ = free_fd(FdSubsystem::Uds, new_fd);
        return Err(UdsError::BadFd);
    };

    let accepted = UDS_STATE.with_mut(|state| {
        let listen_idx = fd_to_idx(fd)? as usize;
        let listen = &state.sockets[listen_idx];
        if listen.id == 0 {
            return Err(UdsError::BadFd);
        }
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
        let new_idx = new_slot as usize;
        let id = alloc_socket_id();
        let ns = &mut state.sockets[new_idx];
        ns.id = id;
        ns.sock_type = UnixSockType::Stream;
        ns.state = UnixSockState::Connected;
        ns.peer = Some(client_id);
        ns.stream_len = 0;
        ns.peer_closed = false;
        state.sockets[listen_idx].listen_head =
            (state.sockets[listen_idx].listen_head + 1) % UNIX_LISTEN_BACKLOG as u8;
        state.sockets[listen_idx].listen_count -= 1;
        state.sockets[client_idx].peer = Some(id);
        Ok(new_fd)
    });

    if accepted.is_err() {
        let _ = free_fd(FdSubsystem::Uds, new_fd);
    }
    accepted
}

/// 发起到指定路径 UDS 套接字的连接
///
/// # Errors
///
/// 当路径为空或过长时返回 `Err(UdsError::Invalid)`; `fd` 无效时返回 `Err(UdsError::BadFd)`;
/// 套接字状态非法或两端类型不匹配时返回 `Err(UdsError::Invalid)`; 目标路径不存在时返回 `Err(UdsError::ConnRefused)`;
/// 目标监听队列已满时返回 `Err(UdsError::Again)`。
pub fn uds_connect(fd: i32, path: &[u8]) -> Result<(), UdsError> {
    if path.is_empty() || path.len() > UNIX_PATH_MAX {
        return Err(UdsError::Invalid);
    }
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        if state.sockets[idx].id == 0 {
            return Err(UdsError::BadFd);
        }
        let s = &state.sockets[idx];
        if s.state != UnixSockState::Unbound && s.state != UnixSockState::Bound {
            return Err(UdsError::Invalid);
        }
        let pidx = state.find_path(path).ok_or(UdsError::ConnRefused)? as usize;
        let target_idx = state.paths[pidx].sock_idx as usize;
        let target = &state.sockets[target_idx];
        if target.sock_type != state.sockets[idx].sock_type {
            return Err(UdsError::Invalid);
        }
        match (state.sockets[idx].sock_type, target.state) {
            (UnixSockType::Stream, UnixSockState::Listening) => {
                if target.listen_count as usize >= UNIX_LISTEN_BACKLOG {
                    return Err(UdsError::Again);
                }
                let tail = target.listen_tail as usize;
                let my_id = state.sockets[idx].id;
                state.sockets[target_idx].listen_pending[tail] = my_id;
                state.sockets[target_idx].listen_tail =
                    (state.sockets[target_idx].listen_tail + 1) % UNIX_LISTEN_BACKLOG as u8;
                state.sockets[target_idx].listen_count += 1;
                state.sockets[idx].state = UnixSockState::Connected;
                state.sockets[idx].bound_path_len = 0;
                Ok(())
            }
            (UnixSockType::Dgram, _) => {
                let my_id = state.sockets[idx].id;
                let peer_id = target.id;
                state.sockets[idx].peer = Some(peer_id);
                state.sockets[target_idx].peer = Some(my_id);
                state.sockets[idx].state = UnixSockState::Connected;
                state.sockets[idx].bound_path_len = 0;
                Ok(())
            }
            _ => Err(UdsError::ConnRefused),
        }
    })
}

#[expect(
    clippy::many_single_char_names,
    reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
)]
/// 通过流式 UDS 连接发送数据
///
/// # Errors
///
/// 当 `fd` 无效或非流式套接字时返回 `Err(UdsError::Invalid)`; 未连接或对端已关闭时返回 `Err(UdsError::NotFound)`;
/// 对端缓冲区无剩余空间时返回 `Err(UdsError::Again)`。
pub fn uds_send(fd: i32, data: &[u8]) -> Result<usize, UdsError> {
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        let s = &state.sockets[idx];
        if s.id == 0 || s.sock_type != UnixSockType::Stream {
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
        peer.stream_len += n as u32;
        // v2 SO_PASSCRED: 对端 passcred=true 时附加 SCM_CREDENTIALS.
        // 简化方案: 凭据追加到 stream_buf 末尾 (POSIX 兼容凭据含 pid/uid/gid).
        // B07-01: 使用当前进程真实凭据, 消除硬编码伪造的 root 凭据.
        if peer.passcred && peer.stream_len as usize + 12 <= UNIX_STREAM_BUF {
            // 序列化 ScmCredentials 到 12 字节 (safe: 字段都是 Copy u32)
            let cred = current_scm_credentials();
            let off = peer.stream_len as usize;
            let p = cred.pid.to_ne_bytes();
            let u = cred.uid.to_ne_bytes();
            let g = cred.gid.to_ne_bytes();
            let mut bytes = [0u8; 12];
            bytes[0..4].copy_from_slice(&p);
            bytes[4..8].copy_from_slice(&u);
            bytes[8..12].copy_from_slice(&g);
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
/// 当 `fd` 无效或非流式套接字时返回 `Err(UdsError::Invalid)`; 对端已关闭时返回 `Err(UdsError::NotFound)`;
/// 缓冲为空且对端未关闭时返回 `Err(UdsError::Again)`。
pub fn uds_recv_with_creds(
    fd: i32,
    out: &mut [u8],
) -> Result<(usize, Option<ScmCredentials>), UdsError> {
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        let s = &mut state.sockets[idx];
        if s.id == 0 || s.sock_type != UnixSockType::Stream {
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
/// 当 `fd` 无效或非流式套接字时返回 `Err(UdsError::Invalid)`; 套接字未连接时返回 `Err(UdsError::Invalid)`;
/// 缓冲为空且对端未关闭时返回 `Err(UdsError::Again)` (对端已关闭时返回 `Ok(0)`)。
pub fn uds_recv(fd: i32, out: &mut [u8]) -> Result<usize, UdsError> {
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        let s = &mut state.sockets[idx];
        if s.id == 0 || s.sock_type != UnixSockType::Stream {
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
        s.stream_len = remaining as u32;
        Ok(n)
    })
}

/// 向指定路径的 UDS 数据报套接字发送数据
///
/// # Errors
///
/// 当目标路径不存在时返回 `Err(UdsError::ConnRefused)`; 目标套接字非数据报类型时返回 `Err(UdsError::Invalid)`;
/// 数据超过 `UNIX_DGRAM_MAX` 时返回 `Err(UdsError::Invalid)` (越界写防护)。
pub fn uds_sendto(_fd: i32, data: &[u8], dest_path: &[u8]) -> Result<usize, UdsError> {
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
        target.dgram_len = data.len() as u32;
        target.dgram_pending = true;
        // v2 SO_PASSCRED: 目标 socket 启用时附加 SCM_CREDENTIALS 12 字节
        // B07-01: 使用当前进程真实凭据, 消除硬编码伪造的 root 凭据.
        if target.passcred && target.dgram_len as usize + 12 <= UNIX_DGRAM_MAX {
            // 序列化 ScmCredentials (safe u32 字段拼装)
            let cred = current_scm_credentials();
            let off = target.dgram_len as usize;
            let p = cred.pid.to_ne_bytes();
            let u = cred.uid.to_ne_bytes();
            let g = cred.gid.to_ne_bytes();
            let mut bytes = [0u8; 12];
            bytes[0..4].copy_from_slice(&p);
            bytes[4..8].copy_from_slice(&u);
            bytes[8..12].copy_from_slice(&g);
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
/// `fd` 无效时返回 `Err(UdsError::BadFd)`; 未连接或对端已关闭时返回 `Err(UdsError::NotFound)`;
/// 数据超过 `UNIX_DGRAM_MAX` 时返回 `Err(UdsError::Invalid)`; 对端已有在途数据报时返回
/// `Err(UdsError::Again)`。
pub fn uds_send_connected(fd: i32, data: &[u8]) -> Result<usize, UdsError> {
    let sock_type = uds_sock_type(fd)?;
    match sock_type {
        UnixSockType::Stream => uds_send(fd, data),
        UnixSockType::Dgram => UDS_STATE.with_mut(|state| {
            let idx = fd_to_idx(fd)? as usize;
            if state.sockets[idx].state != UnixSockState::Connected {
                return Err(UdsError::NotFound);
            }
            let peer_id = state.sockets[idx].peer.ok_or(UdsError::NotFound)?;
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
            peer.dgram_len = data.len() as u32;
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

#[expect(
    clippy::used_underscore_binding,
    reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
)]
/// 从 UDS 数据报套接字接收数据
///
/// # Errors
///
/// 传播自 [`uds_recvfrom_with_creds`]: `fd` 无效或非数据报类型时返回 `Err(UdsError::Invalid)`;
/// 无待收数据时返回 `Err(UdsError::Again)`。
pub fn uds_recvfrom(fd: i32, out: &mut [u8]) -> Result<usize, UdsError> {
    let (_n, _cred) = uds_recvfrom_with_creds(fd, out)?;
    Ok(_n)
}

/// v2: DGRAM 接收并提取凭据
///
/// # Errors
///
/// 当 `fd` 无效或非数据报类型时返回 `Err(UdsError::Invalid)`; 无待收数据时返回 `Err(UdsError::Again)`。
pub fn uds_recvfrom_with_creds(
    fd: i32,
    out: &mut [u8],
) -> Result<(usize, Option<ScmCredentials>), UdsError> {
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        let s = &mut state.sockets[idx];
        if s.id == 0 || s.sock_type != UnixSockType::Dgram {
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

/// 关闭 UDS 套接字并清理其路径绑定、对端关系与 fd_alloc 位图位
///
/// # Errors
///
/// 当 `fd` 无效 (非 UDS 或槽位已释放) 时返回 `Err(UdsError::BadFd)`。
pub fn uds_close(fd: i32) -> Result<(), UdsError> {
    UDS_STATE.with_mut(|state| {
        let idx = fd_to_idx(fd)? as usize;
        if state.sockets[idx].id == 0 {
            return Err(UdsError::BadFd);
        }
        let s = &state.sockets[idx];
        let sock_type = s.sock_type;
        let state_val = s.state;
        let bound_path = s.bound_path;
        let bound_path_len = s.bound_path_len;
        let peer_id = s.peer;

        if sock_type == UnixSockType::Stream && state_val == UnixSockState::Connected {
            if let Some(pid) = peer_id {
                if let Some(pidx) = state.socket_idx_by_id(pid) {
                    state.sockets[pidx as usize].peer_closed = true;
                    state.sockets[pidx as usize].peer = None;
                }
            }
        }

        if sock_type == UnixSockType::Stream && state_val == UnixSockState::Listening {
            for i in 0..state.sockets[idx].listen_count {
                let pos = (state.sockets[idx].listen_head + i) % UNIX_LISTEN_BACKLOG as u8;
                let client_id = state.sockets[idx].listen_pending[pos as usize];
                if let Some(ci) = state.socket_idx_by_id(client_id) {
                    state.sockets[ci as usize] = UnixSocket::empty();
                    // pending client 已分配的 fd_alloc 位同步回收 (fd = 基址 + 槽位)
                    let _ = free_fd(FdSubsystem::Uds, UDS_FD_BASE + i32::from(ci));
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

        state.sockets[idx] = UnixSocket::empty();
        // fd_alloc 位图位回收 (对齐 pidfd close 释放模式; 预存泄漏修复)
        let _ = free_fd(FdSubsystem::Uds, fd);
        Ok(())
    })
}

/// 取消指定路径的 UDS 绑定并释放对应套接字
///
/// # Errors
///
/// 当路径为空或超过 `UNIX_PATH_MAX` 时返回 `Err(UdsError::Invalid)`; 路径不存在时返回 `Err(UdsError::NotFound)`。
pub fn uds_unlink(path: &[u8]) -> Result<(), UdsError> {
    if path.is_empty() || path.len() > UNIX_PATH_MAX {
        return Err(UdsError::Invalid);
    }
    UDS_STATE.with_mut(|state| {
        let pidx = state.find_path(path).ok_or(UdsError::NotFound)? as usize;
        let sidx = state.paths[pidx].sock_idx as usize;
        state.paths[pidx] = UnixPathBinding::empty();
        state.sockets[sidx].bound_path_len = 0;
        state.sockets[sidx].state = UnixSockState::Unbound;
        Ok(())
    })
}

// ============================================================================
// v2: SO_PASSCRED / cmsg / 抽象命名空间
// ============================================================================

/// v2: `SO_PASSCRED` 选项 — 设置 socket 是否在 sendmsg 中附加 `SCM_CREDENTIALS`.
///
/// 由 `privileged::net::init::sm_setsockopt` 路由 (`level=SOL_SOCKET`, `optname=SO_PASSCRED`).
/// 仅对 UDS socket 生效, 其他 family 返 ENOPROTOOPT.
pub fn uds_setsockopt(fd: i32, enable: bool) -> i32 {
    UDS_STATE.with_mut(|state| {
        let idx = match fd_to_idx(fd) {
            Ok(i) => i as usize,
            Err(_) => return -9, // EBADF
        };
        if state.sockets[idx].id == 0 {
            return -9; // EBADF
        }
        if !matches!(
            state.sockets[idx].sock_type,
            UnixSockType::Stream | UnixSockType::Dgram
        ) {
            return -92; // ENOPROTOOPT
        }
        state.sockets[idx].passcred = enable;
        0
    })
}

/// v2: `SO_PASSCRED` 查询.
pub fn uds_getsockopt_passcred(fd: i32) -> i32 {
    UDS_STATE.with(|state| {
        let idx = match fd_to_idx(fd) {
            Ok(i) => i as usize,
            Err(_) => return -9,
        };
        if state.sockets[idx].id == 0 {
            return -9;
        }
        i32::from(state.sockets[idx].passcred)
    })
}

/// v2: `SCM_CREDENTIALS` 数据结构 (Linux ABI 12 字节)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScmCredentials {
    pub pid: u32,
    pub uid: u32,
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
/// 供 `recvmsg` 路径回写 `SCM_CREDENTIALS` cmsg 使用; 非 UDS / 未启用
/// `SO_PASSCRED` / 缓冲不足 12 字节时返回 `None` (不伪造凭据).
pub fn uds_peer_creds(fd: i32) -> Option<ScmCredentials> {
    UDS_STATE.with(|state| {
        let idx = fd_to_idx(fd).ok()? as usize;
        let s = &state.sockets[idx];
        if s.id == 0 || !s.passcred {
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
    pub cmsg_len: usize,
    pub cmsg_level: i32,
    pub cmsg_type: i32,
    pub data: [u8; 0], // flex array, 通过指针偏移访问
}

/// 抽象 namespace 路径解析: `path` `[0]` == 0` 标识 abstract namespace.
/// 返回 `&path[1..]` (去掉前导 0 字节), 其余按文件系统路径处理.
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

pub fn uds_reset_for_test() {
    // 惰性堆置等价语义: 直接重建两表 (保证已堆置且全部归零).
    UDS_STATE.with_mut(UdsState::allocate);
    NEXT_SOCK_ID.with_mut(|id| *id = 1);
    // UT-06 (2026-09-24): 同步归零本子系统 FD 位图. 此前仅清 UDS_STATE,
    // 未回收 fd_alloc 位, 导致跨用例位图残留 (占满 16 槽后 alloc_fd 恒 None).
    for i in 0..MAX_UDS_FD as i32 {
        let _ = free_fd(FdSubsystem::Uds, UDS_FD_BASE + i);
    }
}

// ============================================================================
// functions 层安全封装 (保持原有 API 兼容)
// ============================================================================

/// UDS FD 起点
pub const FD_BASE: i32 = UDS_FD_BASE;
/// UDS FD 上限 (不含)
pub const FD_END: i32 = UDS_FD_BASE + MAX_UDS_FD as i32;
/// 路径最大长度
pub const PATH_MAX: usize = UNIX_PATH_MAX;

/// UDS socket 类型 (兼容旧名)
pub use UnixSockType as SockType;

/// UDS 错误 (functions 层映射)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketError {
    PathNotFound,
    Kernel(crate::functions::error::KernelError),
}

impl UnixSocketError {
    pub fn to_errno(self) -> Errno {
        match self {
            Self::PathNotFound => Errno::ENOENT,
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

pub type UnixResult<T> = Result<T, UnixSocketError>;

/// `struct sockaddr_un` 包装
///
/// DECISION-J (2026-09-13): wire 类型迁回 `privileged::net::socket_types` —
/// 由 privileged TCB raw 桥接 (net/syscall.rs::raw_read/write_sockaddr_un)
/// 从用户内存构造, 属机制的安全导出面。此处 re-export 保持 API 兼容。
pub use crate::privileged::net::socket_types::SockAddrUn;

// ============================================================================
// 安全封装 API (保持原有调用方兼容)
// ============================================================================

/// 安全封装: 创建 UDS 套接字, 委托给 [`uds_create`]
///
/// # Errors
///
/// 无空闲槽位或 FD 数量达上限时映射为 `Err(UnixSocketError)`。
pub fn socket(sock_type: SockType) -> UnixResult<i32> {
    uds_create(sock_type).map_err(Into::into)
}

/// 安全封装: 绑定 UDS 套接字, 委托给 [`uds_bind`]
///
/// # Errors
///
/// `fd` 无效、路径非法、已被占用或无空闲槽位时映射为对应的 `Err(UnixSocketError)`。
pub fn bind(fd: i32, addr: &SockAddrUn) -> UnixResult<()> {
    uds_bind(fd, addr.path_slice()).map_err(Into::into)
}

/// 安全封装: 置为监听状态, 委托给 [`uds_listen`]
///
/// # Errors
///
/// `fd` 无效或套接字非流式/未绑定时映射为 `Err(UnixSocketError)`。
pub fn listen(fd: i32) -> UnixResult<()> {
    uds_listen(fd).map_err(Into::into)
}

/// 安全封装: 接受连接, 委托给 [`uds_accept`]
///
/// # Errors
///
/// `fd` 无效、队列为空或新 FD 分配失败时映射为 `Err(UnixSocketError)`。
pub fn accept(fd: i32) -> UnixResult<i32> {
    uds_accept(fd).map_err(Into::into)
}

/// 安全封装: 发起连接, 委托给 [`uds_connect`]
///
/// # Errors
///
/// `fd` 无效、目标路径不存在或连接被拒绝时映射为 `Err(UnixSocketError)`。
pub fn connect(fd: i32, addr: &SockAddrUn) -> UnixResult<()> {
    uds_connect(fd, addr.path_slice()).map_err(Into::into)
}

/// 安全封装: 流式发送数据, 委托给 [`uds_send`]
///
/// # Errors
///
/// `fd` 无效、未连接或对端无空间时映射为 `Err(UnixSocketError)`。
pub fn send(fd: i32, data: &[u8]) -> UnixResult<usize> {
    uds_send(fd, data).map_err(Into::into)
}

/// 安全封装: 流式接收数据, 委托给 [`uds_recv`]
///
/// # Errors
///
/// `fd` 无效、未连接或无数据可读时映射为 `Err(UnixSocketError)`。
pub fn recv(fd: i32, out: &mut [u8]) -> UnixResult<usize> {
    uds_recv(fd, out).map_err(Into::into)
}

/// 安全封装: 数据报发送, 委托给 [`uds_sendto`]
///
/// # Errors
///
/// 目标路径不存在或目标非数据报套接字时映射为 `Err(UnixSocketError)`。
pub fn sendto(fd: i32, data: &[u8], dest: &SockAddrUn) -> UnixResult<usize> {
    uds_sendto(fd, data, dest.path_slice()).map_err(Into::into)
}

/// 安全封装: 创建一对互相连接的 UDS 套接字, 委托给 [`uds_socketpair`]
///
/// # Errors
///
/// 无空闲槽位或 FD 达上限时映射为 `Err(UnixSocketError)`。
pub fn socketpair(sock_type: SockType) -> UnixResult<(i32, i32)> {
    uds_socketpair(sock_type).map_err(Into::into)
}

/// 安全封装: 向已连接套接字发送数据, 委托给 [`uds_send_connected`]
///
/// # Errors
///
/// `fd` 无效、未连接、对端已关闭或对端在途数据报未收走时映射为 `Err(UnixSocketError)`。
pub fn send_connected(fd: i32, data: &[u8]) -> UnixResult<usize> {
    uds_send_connected(fd, data).map_err(Into::into)
}

/// 安全封装: 数据报接收, 委托给 [`uds_recvfrom`]
///
/// # Errors
///
/// `fd` 无效或无可读数据时映射为 `Err(UnixSocketError)`。
pub fn recvfrom(fd: i32, out: &mut [u8]) -> UnixResult<usize> {
    uds_recvfrom(fd, out).map_err(Into::into)
}

/// 安全封装: 关闭套接字, 委托给 [`uds_close`]
///
/// # Errors
///
/// `fd` 无效时映射为 `Err(UnixSocketError)`。
pub fn close(fd: i32) -> UnixResult<()> {
    uds_close(fd).map_err(Into::into)
}

/// 安全封装: 解除路径绑定, 委托给 [`uds_unlink`]
///
/// # Errors
///
/// 路径非法或不存在时映射为 `Err(UnixSocketError)`。
pub fn unlink(addr: &SockAddrUn) -> UnixResult<()> {
    uds_unlink(addr.path_slice()).map_err(Into::into)
}

#[inline]
pub fn is_uds_fd(fd: i32) -> bool {
    (FD_BASE..FD_END).contains(&fd)
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 模块内用例的互斥锁.
    ///
    /// 所有用例共享全局 `UDS_STATE` 与 UDS FD 位图, 且均以 `uds_reset_for_test()`
    /// 开场; 并行 test runner 下重置会破坏其他用例正在使用的 socket, 故必须互斥
    /// (2026-09-24 UT-06, 口径同 privileged/timer/tick.rs 的 FREQ_TEST_LOCK).
    static UDS_TEST_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());

    #[test]
    fn stream_bind_listen_connect_accept_echo() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let srv = uds_create(UnixSockType::Stream).expect("srv create");
        let cli = uds_create(UnixSockType::Stream).expect("cli create");
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
        uds_close(cli).expect("cli close");
        uds_close(accepted).expect("acc close");
        uds_close(srv).expect("srv close");
    }

    #[test]
    fn dgram_bind_connect_echo() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let rx = uds_create(UnixSockType::Dgram).expect("rx create");
        let tx = uds_create(UnixSockType::Dgram).expect("tx create");
        uds_bind(rx, b"/tmp/rx.sock").expect("rx bind");
        uds_connect(tx, b"/tmp/rx.sock").expect("tx connect");
        let n = uds_sendto(tx, b"datagram-payload", b"/tmp/rx.sock").expect("sendto");
        assert_eq!(n, 16);
        let mut buf = [0u8; 32];
        let m = uds_recvfrom(rx, &mut buf).expect("recvfrom");
        assert_eq!(m, 16);
        assert_eq!(&buf[..16], b"datagram-payload");
        uds_close(tx).expect("close tx");
        uds_close(rx).expect("close rx");
    }

    #[test]
    fn eaddrinuse_on_duplicate_bind() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let a = uds_create(UnixSockType::Stream).expect("a");
        let b = uds_create(UnixSockType::Stream).expect("b");
        uds_bind(a, b"/tmp/dup.sock").expect("a bind");
        let r = uds_bind(b, b"/tmp/dup.sock");
        assert_eq!(r, Err(UdsError::AddrInUse));
        uds_close(a).expect("close a");
        uds_close(b).expect("close b");
    }

    #[test]
    fn eagain_on_empty_accept() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let s = uds_create(UnixSockType::Stream).expect("s");
        uds_bind(s, b"/tmp/empty.sock").expect("bind");
        uds_listen(s).expect("listen");
        let r = uds_accept(s);
        assert_eq!(r, Err(UdsError::Again));
        uds_close(s).expect("close");
    }

    #[test]
    fn close_listener_cancels_pending_clients() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        let srv = uds_create(UnixSockType::Stream).expect("srv");
        let cli1 = uds_create(UnixSockType::Stream).expect("c1");
        let cli2 = uds_create(UnixSockType::Stream).expect("c2");
        uds_bind(srv, b"/tmp/cancel.sock").expect("bind");
        uds_listen(srv).expect("listen");
        uds_connect(cli1, b"/tmp/cancel.sock").expect("c1 connect");
        uds_connect(cli2, b"/tmp/cancel.sock").expect("c2 connect");
        uds_close(srv).expect("close srv");
        let r1 = uds_close(cli1);
        let r2 = uds_close(cli2);
        assert_eq!(r1, Err(UdsError::BadFd));
        assert_eq!(r2, Err(UdsError::BadFd));
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
        uds_close(a).expect("close a");
        uds_close(b).expect("close b");
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
        uds_close(a).expect("close a");
        uds_close(b).expect("close b");
    }

    #[test]
    fn socketpair_rollback_on_second_create_failure() {
        let _lock = UDS_TEST_LOCK.lock();
        uds_reset_for_test();
        // 占满 MAX_UDS_FD - 1 个槽位, socketpair 需连续 2 个空槽 → 第二端 NoMem
        let mut filled = 0usize;
        let mut guard = [0i32; MAX_UDS_FD];
        while filled < MAX_UDS_FD - 1 {
            guard[filled] = uds_create(UnixSockType::Stream).expect("fill");
            filled += 1;
        }
        assert_eq!(uds_socketpair(UnixSockType::Stream), Err(UdsError::NoMem));
        // 回滚验证: 第一端已被关闭释放槽位, 单个创建应成功
        let one = uds_create(UnixSockType::Stream).expect("single after rollback");
        let _ = one;
    }
}
