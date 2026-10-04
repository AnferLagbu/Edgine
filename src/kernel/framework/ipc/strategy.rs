//! IPC 策略 trait — 策略-机制分离接口
//!
//! 本模块落实「framekernel 范式落实」工程 DECISION-I: framework/ipc 的
//! `pipe.rs`/`shm.rs`/`msgq.rs` FFI 边界直接调用 `services::ipc::*_safe`
//! (framework→services 反向依赖 13 处), 改为经 `IpcStrategy` trait 注入:
//! framework 定义契约, services 实现并注册, FFI 边界经 `current_ipc_strategy()`
//! 调用策略方法。
//!
//! ## 设计 (对齐 PageFaultPolicy/SwapPolicy 既有模式)
//!
//! - trait 定义在 framework (引用 framework 类型 `IpcNamespace`/`IpcId`)
//! - 实现在 services (100% safe Rust, `#![deny(unsafe_code)]`)
//! - services 通过 `register_ipc_strategy()` 注册; FFI 边界经 `current_ipc_strategy()`
//!   获取策略
//! - **无内建回退 + 不 panic (DECISION-K)**: 策略方法依赖 services 实现, framework
//!   无法安全回退; `current_ipc_strategy()` 返回 `Option`, 未注册时调用点降级返回
//!   `ENOSYS` + 日志 (逻辑错误降级原则, 不进 freg 恢复流程), 开发期由时序门禁
//!   host-test 捕获。注册点前置契约: 紧随 framework `ipc_init()` 后立即 (kernel_init
//!   早期), 与调用点 (用户态 syscall) 分离 — 注册零依赖 (零字段构造 + static 零初始化)

use super::types::{IpcId, IpcNamespace};

/// IPC 策略接口 — services 实现, framework FFI 边界调用
///
/// 所有方法为纯策略逻辑 (参数校验 + 资源操作), 操作由调用方提供的
/// `&mut IpcNamespace` (framework 机制全局状态), 不涉及硬件或 unsafe.
pub trait IpcStrategy: Send + Sync {
    // ── Pipe ──

    /// 判断 fd 是否为 pipe fd (供 sendfile/splice 使用)
    fn is_pipe_fd(&self, fd: i32) -> bool;

    /// 创建管道 (策略: 分配槽位 + 初始化等待队列)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (槽位耗尽或参数非法).
    fn pipe_create(
        &self,
        ns: &mut IpcNamespace,
        next_id: &mut IpcId,
        pid: u32,
    ) -> Result<(i32, i32), i32>;

    /// 管道读 (策略: 缓冲复制 + 唤醒写端)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (fd 非法/已关闭).
    fn pipe_read(
        &self,
        ns: &mut IpcNamespace,
        fd: i32,
        buf: &mut [u8],
        count: u32,
    ) -> Result<u32, i32>;

    /// 管道写 (策略: 缓冲复制 + 唤醒读端)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (fd 非法/已关闭/缓冲不足).
    fn pipe_write(
        &self,
        ns: &mut IpcNamespace,
        fd: i32,
        buf: &[u8],
        count: u32,
    ) -> Result<u32, i32>;

    /// 关闭管道 (策略: 释放槽位)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (fd 非法).
    fn pipe_close(&self, ns: &mut IpcNamespace, fd: i32) -> Result<(), i32>;

    // ── SHM ──

    /// 创建共享内存段 (策略: 分配槽位 + 物理页)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (槽位/物理页耗尽).
    fn shm_create(
        &self,
        ns: &mut IpcNamespace,
        next_id: &mut IpcId,
        size: u64,
        perm: i32,
        pid: u32,
    ) -> Result<IpcId, i32>;

    /// 附加共享内存段 (策略: 页表映射, 返回物理地址)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (id 非法/内存不足).
    fn shm_attach(&self, ns: &mut IpcNamespace, id: IpcId, pid: u32) -> Result<u64, i32>;

    /// 分离共享内存段 (策略: 解除映射)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (id 非法).
    fn shm_detach(&self, ns: &mut IpcNamespace, id: IpcId, pid: u32) -> Result<(), i32>;

    /// 销毁共享内存段 (策略: 释放资源)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (id 非法).
    fn shm_destroy(&self, ns: &mut IpcNamespace, id: IpcId) -> Result<(), i32>;

    // ── MsgQ ──

    /// 创建消息队列 (策略: 分配槽位 + 初始化链表)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (槽位耗尽或参数非法).
    fn msgq_create(
        &self,
        ns: &mut IpcNamespace,
        next_id: &mut IpcId,
        perm: i32,
        pid: u32,
    ) -> Result<IpcId, i32>;

    /// 发送消息 (策略: 参数校验 + 容量检查 + 入队 + 唤醒)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (id 非法/队列满/无效消息).
    fn msgq_send(
        &self,
        ns: &mut IpcNamespace,
        id: IpcId,
        type_: u64,
        data: Option<&[u8]>,
        size: usize,
        pid: u32,
    ) -> Result<(), i32>;

    /// 接收消息 (策略: 出队 + 数据复制)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (id 非法或空队列).
    fn msgq_recv(
        &self,
        ns: &mut IpcNamespace,
        id: IpcId,
        type_out: Option<&mut u64>,
        data_out: Option<&mut [u8]>,
        size_out: Option<&mut u64>,
    ) -> Result<usize, i32>;

    /// 销毁消息队列 (策略: 释放消息链表 + 槽位)
    ///
    /// # Errors
    /// 失败返回 `Err(errno)` (id 非法).
    fn msgq_destroy(&self, ns: &mut IpcNamespace, id: IpcId) -> Result<(), i32>;
}

// ============================================================================
// 全局注册表
// ============================================================================

/// 全局 IPC 策略注册表 — services 通过 `register_ipc_strategy` 注册
static IPC_STRATEGY: crate::framework::sync::OnceLock<&'static dyn IpcStrategy> =
    crate::framework::sync::OnceLock::new();

/// 注册 IPC 策略 (由 `services::ipc::strategy` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_ipc_strategy(
    strategy: &'static dyn IpcStrategy,
) -> Result<(), &'static dyn IpcStrategy> {
    match IPC_STRATEGY.set(strategy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的 IPC 策略.
///
/// 未注册时返回 `None` — **DECISION-K 修订 (2026-09-12)**: 不 panic。
/// 依据: EG panic 触发 freg 系统级恢复 (`panic!()→PANIC_FLAG→int 0x82`),
/// "策略未注册"是确定性逻辑错误 (启动顺序 bug), 非可恢复故障 — 统一原则:
/// **逻辑错误一律降级 + 日志, 不进恢复流程**。调用点对 `None` 返回 `ENOSYS`
/// 并打日志; 开发期由时序门禁 host-test 捕获。
pub fn current_ipc_strategy() -> Option<&'static dyn IpcStrategy> {
    IPC_STRATEGY.get().copied()
}
