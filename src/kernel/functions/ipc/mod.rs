#![deny(unsafe_code)]
//! IPC — 进程间通信
//!
//! ## 真实状态 (v2.7, 2026-06-12)
//!
//! 全部 4 子系统已完成 functions 层 safe 迁移 (I-54):
//! - pipe — 创建/读取/写入/关闭
//! - shm  — 创建/附加/分离/销毁
//! - msgq — 创建/发送/接收/销毁
//! - sem  — 创建/等待/唤醒/销毁
//!
//! 全部走 `privileged::ipc` 的 safe 入口 (`*_safe` 系列), 模块顶部
//! `#![deny(unsafe_code)]` 拒绝任何 unsafe 块 (由 audit_functions_boundary.py
//! 强约束). 静态契约测试见 host-tests/tests/functions_ipc_complete_test.rs.

// T6-1: 策略函数已从 privileged 迁移到 functions 本地
pub mod msgq;
pub mod pipe;
pub mod shm;
// T6-9: sem 已迁移到 functions 本地 (原 privileged/ipc/sem.rs)

// ============================================================================
// 错误
// ============================================================================

/// IPC 错误 — TD-20: 收敛到 `KernelError`, 1 字段 IPC 特有 + 1 共享包装.
///
/// 字段说明:
///   - `InvalidOp`: IPC 句柄类型不匹配 (写读端 / 读写端) 走 EBADF
///   - `Kernel(KernelError)`: 共享错误 (NoResources→WouldBlock / `BadFd` /
///     未找到→无此进程 / 会阻塞 / 权限不足 / 参数非法) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// 无效操作 (写读端 / 读写端)
    InvalidOp,
    /// 共享 `KernelError` 包装
    Kernel(crate::functions::error::KernelError),
}

impl IpcError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::InvalidOp => E::EBADF,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    pub fn from_i32(rc: i32) -> Self {
        use crate::functions::error::KernelError as K;
        match rc {
            -1 => Self::Kernel(K::WouldBlock),
            -2 => Self::InvalidOp,
            -3 => Self::Kernel(K::NoSuchProcess),
            -4 => Self::Kernel(K::WouldBlock),
            -9 | -77 => Self::Kernel(K::BadFd),
            -13 => Self::Kernel(K::PermissionDenied),
            -22 => Self::Kernel(K::InvalidArgument),
            rc => Self::Kernel(K::Other(rc)),
        }
    }
}

use crate::privileged::syscall::Errno;

// ============================================================================
// 句柄
// ============================================================================

// ============================================================================
// 类型 (从本地 types 模块 re-export)
// ============================================================================

/// 异步 IPC 基础设施 (原 privileged/ipc/async_ipc.rs, 2026-06-18 迁移)
#[cfg(feature = "async")]
pub mod async_ipc;
/// T6-9: 调度器集成 (原 privileged/ipc/scheduler_integration.rs)
pub mod scheduler_integration;
/// T6-9: 信号量实现 (原 privileged/ipc/sem.rs)
pub mod sem;
/// 信号机制实现 (原 privileged/ipc/signal.rs)
pub mod signal;
/// DECISION-I: IpcStrategy trait 默认实现与注册
pub mod strategy;
pub mod types;

/// IPC 资源 ID (functions 层视图, 内核 `IpcId = u32` 的包装)
pub use types::IpcId;

/// 管道文件描述符
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeFd {
    pub fd: i32,
}

/// 共享内存段句柄
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShmHandle {
    /// 共享内存 ID
    pub id: IpcId,
    /// 物理地址 (attach 时获得)
    pub phys_addr: u64,
}

/// 消息队列句柄
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgqHandle {
    pub id: IpcId,
}

/// 信号量句柄
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemHandle {
    pub id: IpcId,
}

// ============================================================================
// 全局命名空间
// ============================================================================

// I-16: 替换 spin::Once → 项目自研 functions::sync::once::OnceCell (统一 OnceCell 抽象, 不绕过框架同步层)
use crate::functions::sync::once::OnceCell;
static GLOBAL_IPC: OnceCell<IpcNamespaceRef> = OnceCell::new();

/// 初始化全局 IPC 命名空间
pub fn init_global() {
    let _ = GLOBAL_IPC.get_or_init(|slot| {
        slot.write(IpcNamespaceRef::new());
    });

    // T1-2: 注册 functions 层信号决策策略
    signal::init_signal_decision();
}

/// 获取全局 IPC 引用
///
/// # Panics
/// 当 `init_global()` 尚未被调用、全局实例未初始化时发生 panic (内部使用 `expect`).
pub fn global() -> &'static IpcNamespaceRef {
    GLOBAL_IPC
        .get()
        .expect("ipc::global() called before init_global()")
}

/// IPC 命名空间的安全视图
pub struct IpcNamespaceRef;

impl IpcNamespaceRef {
    pub fn new() -> Self {
        Self
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 获取命名空间锁 (spin, 短临界区)
    pub fn lock(&self) -> IpcLock {
        IpcLock
    }
}

/// IPC 命名空间临界区守卫
pub struct IpcLock;

impl IpcLock {
    // ── Pipe ──

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 创建管道
    ///
    /// # Errors
    /// 当底层创建失败 (如资源耗尽等) 时以对应的 `IpcError` 返回.
    pub fn pipe_create(&self, current_pid: u32) -> Result<(PipeFd, PipeFd), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        let next_id = crate::privileged::ipc::NEXT_IPC_ID.get_mut();
        self::pipe::pipe_create_safe(ns, next_id, current_pid)
            .map(|(r, w)| (PipeFd { fd: r }, PipeFd { fd: w }))
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 管道读
    ///
    /// # Errors
    /// 当管道未打开、无数据可读或底层读失败时以对应的 `IpcError` 返回.
    pub fn pipe_read(&self, fd: PipeFd, buf: &mut [u8]) -> Result<usize, IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::pipe::pipe_read_safe(ns, fd.fd, buf, buf.len() as u32)
            .map(|n| n as usize)
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 管道写
    ///
    /// # Errors
    /// 当管道未打开、缓冲区非法或底层写失败时以对应的 `IpcError` 返回.
    pub fn pipe_write(&self, fd: PipeFd, buf: &[u8]) -> Result<usize, IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::pipe::pipe_write_safe(ns, fd.fd, buf, buf.len() as u32)
            .map(|n| n as usize)
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 关闭管道
    ///
    /// # Errors
    /// 当管道未打开或底层关闭失败时以对应的 `IpcError` 返回.
    pub fn pipe_close(&self, fd: PipeFd) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::pipe::pipe_close_safe(ns, fd.fd).map_err(IpcError::from_i32)
    }

    // ── SHM ──

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 创建共享内存段
    ///
    /// # Errors
    /// 当底层创建失败 (如资源耗尽等) 时以对应的 `IpcError` 返回.
    pub fn shm_create(&self, current_pid: u32, size: usize) -> Result<ShmHandle, IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        let next_id = crate::privileged::ipc::NEXT_IPC_ID.get_mut();
        self::shm::shm_create_safe(ns, next_id, size as u64, 0, current_pid)
            .map(|id| ShmHandle { id, phys_addr: 0 })
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 附加共享内存段
    ///
    /// # Errors
    /// 当共享内存段不存在、无权限或底层附加失败时以对应的 `IpcError` 返回.
    pub fn shm_attach(&self, id: IpcId, current_pid: u32) -> Result<ShmHandle, IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::shm::shm_attach_safe(ns, id, current_pid)
            .map(|phys_addr| ShmHandle { id, phys_addr })
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 分离共享内存段
    ///
    /// # Errors
    /// 当句柄无效、无权限或底层分离失败时以对应的 `IpcError` 返回.
    pub fn shm_detach(&self, handle: ShmHandle, current_pid: u32) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::shm::shm_detach_safe(ns, handle.id, current_pid).map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 删除共享内存段
    ///
    /// # Errors
    /// 当共享内存段不存在或底层删除失败时以对应的 `IpcError` 返回.
    pub fn shm_destroy(&self, id: IpcId) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::shm::shm_destroy_safe(ns, id).map_err(IpcError::from_i32)
    }

    // ── MsgQ ──

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 创建消息队列
    ///
    /// # Errors
    /// 当底层创建失败 (如资源耗尽等) 时以对应的 `IpcError` 返回.
    pub fn msgq_create(&self, current_pid: u32) -> Result<MsgqHandle, IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        let next_id = crate::privileged::ipc::NEXT_IPC_ID.get_mut();
        self::msgq::msgq_create_safe(ns, next_id, 0, current_pid)
            .map(MsgqHandle::from)
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 发送消息
    ///
    /// # Errors
    /// 当队列不存在、队列已满、数据非法或底层发送失败时以对应的 `IpcError` 返回.
    pub fn msgq_send(&self, q: MsgqHandle, data: &[u8], current_pid: u32) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::msgq::msgq_send_safe(ns, q.id, 0, Some(data), data.len(), current_pid)
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 接收消息
    ///
    /// # Errors
    /// 当队列不存在、队列为空或底层接收失败时以对应的 `IpcError` 返回.
    pub fn msgq_recv(&self, q: MsgqHandle, buf: &mut [u8]) -> Result<usize, IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        let mut type_buf = 0u64;
        let mut size_buf = 0u64;
        self::msgq::msgq_recv_safe(
            ns,
            q.id,
            Some(&mut type_buf),
            Some(buf),
            Some(&mut size_buf),
        )
        .map(|n| n as usize)
        .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 销毁消息队列
    ///
    /// # Errors
    /// 当队列不存在或底层销毁失败时以对应的 `IpcError` 返回.
    pub fn msgq_destroy(&self, q: MsgqHandle) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        self::msgq::msgq_destroy_safe(ns, q.id).map_err(IpcError::from_i32)
    }

    // ── Semaphore ──

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 创建信号量
    ///
    /// # Errors
    /// 当参数非法或底层创建失败 (如资源耗尽等) 时以对应的 `IpcError` 返回.
    pub fn sem_create(
        &self,
        initial: u32,
        max_count: u32,
        current_pid: u32,
    ) -> Result<SemHandle, IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        let next_id = crate::privileged::ipc::NEXT_IPC_ID.get_mut();
        sem::sem_create_safe(ns, next_id, initial, max_count, current_pid)
            .map(SemHandle::from)
            .map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// P 操作 (wait)
    ///
    /// # Errors
    /// 当信号量不存在或底层等待失败时以对应的 `IpcError` 返回.
    pub fn sem_wait(&self, s: SemHandle) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        sem::sem_wait_safe(ns, s.id).map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// V 操作 (signal/post)
    ///
    /// # Errors
    /// 当信号量不存在或底层释放失败时以对应的 `IpcError` 返回.
    pub fn sem_post(&self, s: SemHandle) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        sem::sem_post_safe(ns, s.id).map_err(IpcError::from_i32)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 销毁信号量
    ///
    /// # Errors
    /// 当信号量不存在或底层销毁失败时以对应的 `IpcError` 返回.
    pub fn sem_destroy(&self, s: SemHandle) -> Result<(), IpcError> {
        let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
        sem::sem_destroy_safe(ns, s.id).map_err(IpcError::from_i32)
    }
}

// ============================================================================
// 句柄类型转换
// ============================================================================

impl ShmHandle {
    /// 从 ID + 物理地址构造
    pub fn from_id_and_addr(id: IpcId, phys_addr: u64) -> Self {
        Self { id, phys_addr }
    }
}

impl MsgqHandle {
    /// 从内核 `IpcId` 构造
    pub fn from(id: IpcId) -> Self {
        Self { id }
    }
}

impl SemHandle {
    /// 从内核 `IpcId` 构造
    pub fn from(id: IpcId) -> Self {
        Self { id }
    }
}

// ============================================================================
// 便利函数 (顶层)
// ============================================================================

/// 创建管道
///
/// # Errors
/// 错误条件与 [`IpcLock::pipe_create`] 相同, 参见其 `# Errors` 段.
pub fn pipe_create(current_pid: u32) -> Result<(PipeFd, PipeFd), IpcError> {
    global().lock().pipe_create(current_pid)
}

/// 管道读
///
/// # Errors
/// 错误条件与 [`IpcLock::pipe_read`] 相同, 参见其 `# Errors` 段.
pub fn pipe_read(fd: PipeFd, buf: &mut [u8]) -> Result<usize, IpcError> {
    global().lock().pipe_read(fd, buf)
}

/// 管道写
///
/// # Errors
/// 错误条件与 [`IpcLock::pipe_write`] 相同, 参见其 `# Errors` 段.
pub fn pipe_write(fd: PipeFd, buf: &[u8]) -> Result<usize, IpcError> {
    global().lock().pipe_write(fd, buf)
}

/// 关闭管道
///
/// # Errors
/// 错误条件与 [`IpcLock::pipe_close`] 相同, 参见其 `# Errors` 段.
pub fn pipe_close(fd: PipeFd) -> Result<(), IpcError> {
    global().lock().pipe_close(fd)
}

// ============================================================================
// 便利函数 (顶层 shm/msgq/sem)
// ============================================================================

/// 创建共享内存段
///
/// # Errors
/// 错误条件与 [`IpcLock::shm_create`] 相同, 参见其 `# Errors` 段.
pub fn shm_create(current_pid: u32, size: usize) -> Result<ShmHandle, IpcError> {
    global().lock().shm_create(current_pid, size)
}

/// 附加共享内存段
///
/// # Errors
/// 错误条件与 [`IpcLock::shm_attach`] 相同, 参见其 `# Errors` 段.
pub fn shm_attach(id: IpcId, current_pid: u32) -> Result<ShmHandle, IpcError> {
    global().lock().shm_attach(id, current_pid)
}

/// 分离共享内存段
///
/// # Errors
/// 错误条件与 [`IpcLock::shm_detach`] 相同, 参见其 `# Errors` 段.
pub fn shm_detach(handle: ShmHandle, current_pid: u32) -> Result<(), IpcError> {
    global().lock().shm_detach(handle, current_pid)
}

/// 删除共享内存段
///
/// # Errors
/// 错误条件与 [`IpcLock::shm_destroy`] 相同, 参见其 `# Errors` 段.
pub fn shm_destroy(id: IpcId) -> Result<(), IpcError> {
    global().lock().shm_destroy(id)
}

/// 创建消息队列
///
/// # Errors
/// 错误条件与 [`IpcLock::msgq_create`] 相同, 参见其 `# Errors` 段.
pub fn msgq_create(current_pid: u32) -> Result<MsgqHandle, IpcError> {
    global().lock().msgq_create(current_pid)
}

/// 发送消息
///
/// # Errors
/// 错误条件与 [`IpcLock::msgq_send`] 相同, 参见其 `# Errors` 段.
pub fn msgq_send(q: MsgqHandle, data: &[u8], current_pid: u32) -> Result<(), IpcError> {
    global().lock().msgq_send(q, data, current_pid)
}

/// 接收消息
///
/// # Errors
/// 错误条件与 [`IpcLock::msgq_recv`] 相同, 参见其 `# Errors` 段.
pub fn msgq_recv(q: MsgqHandle, buf: &mut [u8]) -> Result<usize, IpcError> {
    global().lock().msgq_recv(q, buf)
}

/// 销毁消息队列
///
/// # Errors
/// 错误条件与 [`IpcLock::msgq_destroy`] 相同, 参见其 `# Errors` 段.
pub fn msgq_destroy(q: MsgqHandle) -> Result<(), IpcError> {
    global().lock().msgq_destroy(q)
}

/// 创建信号量
///
/// # Errors
/// 错误条件与 [`IpcLock::sem_create`] 相同, 参见其 `# Errors` 段.
pub fn sem_create(initial: u32, max_count: u32, current_pid: u32) -> Result<SemHandle, IpcError> {
    global().lock().sem_create(initial, max_count, current_pid)
}

/// P 操作 (wait)
///
/// # Errors
/// 错误条件与 [`IpcLock::sem_wait`] 相同, 参见其 `# Errors` 段.
pub fn sem_wait(s: SemHandle) -> Result<(), IpcError> {
    global().lock().sem_wait(s)
}

/// V 操作 (signal/post)
///
/// # Errors
/// 错误条件与 [`IpcLock::sem_post`] 相同, 参见其 `# Errors` 段.
pub fn sem_post(s: SemHandle) -> Result<(), IpcError> {
    global().lock().sem_post(s)
}

/// 销毁信号量
///
/// # Errors
/// 错误条件与 [`IpcLock::sem_destroy`] 相同, 参见其 `# Errors` 段.
pub fn sem_destroy(s: SemHandle) -> Result<(), IpcError> {
    global().lock().sem_destroy(s)
}

// ============================================================================
