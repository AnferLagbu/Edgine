//! IPC 子系统 API 层
//!
//! 管道 / 共享内存 / 消息队列 / 信号量 / 信号的统一入口,
//! 等价于 POSIX IPC 函数族 + System V 信号量/消息队列。
//!
//! ## 调用方契约
//! - `syscall::mod` —— 管道/共享内存/消息队列/信号量/信号系统调用入口
//! - `proc::api` —— 进程 fork/exec 时继承/清理 IPC 资源
//! - `functions::ipc::scheduler_integration` —— 阻塞/唤醒与调度器交互 (functions 策略)
//!
//! ## 内部接口
//! - `pipe.rs` —— 管道安全创建/关闭/读写 (privileged FFI 边界, 调 functions::ipc::pipe)
//! - `shm.rs` —— 共享内存安全创建/附加/分离/销毁 (privileged FFI 边界, 调 functions::ipc::shm)
//! - `msgq.rs` —— 消息队列安全创建/发送/接收 (privileged FFI 边界, 调 functions::ipc::msgq)
//! - `types.rs` —— Pipe/ShmSegment/MsgQueue/Semaphore/SignalAction 类型定义
//! - sem/signal 策略与 scheduler_integration/async_ipc 由 `functions::ipc` 提供 (DECISION-J 壳已删)
//!
//! ## 安全约束
//! - 所有 _safe 函数接收 &mut IpcNamespace, 调用方负责命名空间生命周期
//! - 全局 IPC_NAMESPACE 在 ipc_init() 中初始化, 必须单线程调用
//! - 管道/信号量等待队列依赖调度器集成, 不可在中断上下文操作
//! - unsafe 仅存在于 FFI 边界和静态可变变量访问
//!
//! ## 性能特征
//! - 管道读写: O(1) 环形缓冲区, 零拷贝指针
//! - 共享内存: O(1) 页表映射
//! - 消息队列: O(1) 固定槽位
//! - 资源上限: pipes`[64]` / shm`[16]` / msgq`[32]` / sem`[64]`

#![deny(missing_docs)]

// ============================================================================
// 契约 trait: IpcResource — 所有 IPC 资源类型必须实现
// ============================================================================

/// IPC 资源抽象。
///
/// Pipe / `ShmSegment` / `MsgQueue` / Semaphore 均实现此 trait,
/// 使 syscall 层可以用统一模式管理 IPC 文件描述符。
pub trait IpcResource {
    /// 资源标识符 (0 = 未使用)
    fn id(&self) -> u32;

    /// 资源类型
    fn resource_type(&self) -> IpcResourceType;

    /// 释放资源槽位
    fn release(&mut self);
}

/// IPC 资源类型 (管道/共享内存/消息队列/信号量)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcResourceType {
    /// 管道
    Pipe,
    /// 共享内存段
    Shm,
    /// 消息队列
    MsgQueue,
    /// 信号量
    Semaphore,
}

// ============================================================================
// 契约常量
// ============================================================================

// 单源: 一律 re-export types 的机制常量, 避免同值双源漂移
// (曾出现 MSG_MAX_SIZE = 1024 而 types 为 4096 的不一致; 测试模式的缩减门控也随之统一).
pub use super::types::{
    IPC_MAX_MSG_QUEUES, IPC_MAX_PIPES, IPC_MAX_SEMAPHORES, IPC_MAX_SHM_SEGS, MSG_MAX_SIZE,
};

// ============================================================================
// 契约: 生命周期
// ============================================================================

/// 初始化 IPC 子系统。
///
/// # 安全约束
/// - 必须在内核启动早期调用, 单线程环境下
/// - 只能调用一次
pub fn ipc_init() {
    super::ipc_init();
}
