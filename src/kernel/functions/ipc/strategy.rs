#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯策略包装。
//! IPC 默认策略实现 — functions 侧注册
//!
//! ## DECISION-I 记录 (2026-09-12)
//!
//! privileged/ipc FFI 边界 (pipe/shm/msgq) 经 `IpcStrategy` trait 注入调用策略,
//! 消除 privileged→functions 直接引用 (13 处)。本模块为 functions 侧实现:
//! 包装 `functions::ipc::{shm,msgq}` 的 `*_safe` 策略函数, 保持 T6 权威。
//!
//! 管道例外: 匿名管道已纳入 VFS fd 对象模型 (`functions::fs::pipe_fd`),
//! 用户 fd 级语义由 VFS 路径承担, 故 pipe 不再经 `IpcStrategy` 分发.
//!
//! 注册时序: privileged ipc 机制先启 (IPC_NAMESPACE) → functions 经
//! `register_default_ipc_strategy()` 注册 → FFI syscall 使用时经
//! `current_ipc_strategy()` 获取。由 lib.rs 编排 (crate root 双向编排者)。

use crate::functions::error::AlreadyRegistered;
use crate::functions::ipc::{msgq, shm};
use crate::privileged::ipc::IpcStrategy;
use crate::privileged::ipc::types::{IpcId, IpcNamespace};

/// 默认 IPC 策略 — 包装 functions `*_safe` 策略函数
pub struct DefaultIpcStrategy;

impl IpcStrategy for DefaultIpcStrategy {
    // ── SHM ──

    fn shm_create(
        &self,
        ns: &mut IpcNamespace,
        next_id: &mut IpcId,
        size: u64,
        perm: i32,
        pid: u32,
    ) -> Result<IpcId, i32> {
        shm::shm_create_safe(ns, next_id, size, perm, pid)
    }

    fn shm_attach(&self, ns: &mut IpcNamespace, id: IpcId, pid: u32) -> Result<u64, i32> {
        shm::shm_attach_safe(ns, id, pid)
    }

    fn shm_detach(&self, ns: &mut IpcNamespace, id: IpcId, pid: u32) -> Result<(), i32> {
        shm::shm_detach_safe(ns, id, pid)
    }

    fn shm_destroy(&self, ns: &mut IpcNamespace, id: IpcId) -> Result<(), i32> {
        shm::shm_destroy_safe(ns, id)
    }

    // ── MsgQ ──

    fn msgq_create(
        &self,
        ns: &mut IpcNamespace,
        next_id: &mut IpcId,
        perm: i32,
        pid: u32,
    ) -> Result<IpcId, i32> {
        msgq::msgq_create_safe(ns, next_id, perm, pid)
    }

    fn msgq_send(
        &self,
        ns: &mut IpcNamespace,
        id: IpcId,
        type_: u64,
        data: Option<&[u8]>,
        size: usize,
        pid: u32,
    ) -> Result<(), i32> {
        msgq::msgq_send_safe(ns, id, type_, data, size, pid)
    }

    fn msgq_recv(
        &self,
        ns: &mut IpcNamespace,
        id: IpcId,
        type_out: Option<&mut u64>,
        data_out: Option<&mut [u8]>,
        size_out: Option<&mut u64>,
    ) -> Result<usize, i32> {
        msgq::msgq_recv_safe(ns, id, type_out, data_out, size_out)
    }

    fn msgq_destroy(&self, ns: &mut IpcNamespace, id: IpcId) -> Result<(), i32> {
        msgq::msgq_destroy_safe(ns, id)
    }
}

/// 全局默认策略实例
static IPC_STRATEGY: DefaultIpcStrategy = DefaultIpcStrategy;

/// 注册默认 IPC 策略 (由 lib.rs 编排调用, 早于任何 IPC syscall)
///
/// 幂等性: 已注册时返回 `Err(AlreadyRegistered)` (与 pmm/slab/swap policy 注册模式一致).
///
/// # Errors
/// 当策略已注册时返回 `Err(AlreadyRegistered)`.
pub fn register_default_ipc_strategy() -> Result<(), AlreadyRegistered> {
    crate::privileged::ipc::strategy::register_ipc_strategy(&IPC_STRATEGY)
        .map_err(|_| AlreadyRegistered)
}
