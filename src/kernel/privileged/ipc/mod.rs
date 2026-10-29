//! IPC 子系统 (Inter-Process Communication)
//!
//! 提供进程间通信的核心机制，包括：
//! - **管道 (Pipe)**: 字节流通信，支持阻塞读写
//! - **信号 (Signal)**: 异步通知机制
//! - **共享内存 (SHM)**: 高效大数据传输
//! - **消息队列 (`MsgQ`)**: 结构化消息传递
//! - **信号量 (Semaphore)**: 同步原语
//!
//! ## 架构设计
//!
//! ```text
//! IPC Subsystem
//! ├── Namespace (全局资源容器)
//! │   ├── pipes`[64]`        // 管道数组
//! │   ├── shm_segs`[16]`     // 共享内存段数组
//! │   ├── msg_queues`[32]`   // 消息队列数组
//! │   └── semaphores`[64]`   // 信号量数组
//!
//! ├── Pipe Buffer (类型层) // 管道环形缓冲 (用户 fd 级语义在 functions/fs/pipe_fd)
//! ├── Shm Module          // 共享内存管理
//! ├── MsgQ Module         // 消息队列管理
//! ├── Semaphore Module    // 信号量管理
//! └── Signal Module       // 信号机制
//! ```
//!
//! ## 设计理念
//!
//! - **功能等价**: 与 C 版本 API 完全兼容
//! - **类型安全**: 利用 Rust 类型系统消除空指针和缓冲区溢出
//! - **零成本抽象**: 关键路径性能与 C 版本相当
//! - **FFI 兼容**: 同时提供 Rust 安全接口和 C FFI 接口

// ============================================================================
// 子模块声明
// ============================================================================

/// API 层
pub mod api;

/// 数据类型定义
pub mod types;

/// 共享内存实现
pub mod shm;

/// 消息队列实现
pub mod msgq;

/// 动态 IPC 命名空间
pub mod dynamic;

/// IPC 策略 trait (DECISION-I: privileged 定义契约, functions 注册实现)
pub mod strategy;

// ============================================================================
// 全局状态
// ============================================================================

use crate::privileged::racy_cell::RacyCell;
use types::{IPC_MAX_MSG_QUEUES, IPC_MAX_PIPES, IPC_MAX_SEMAPHORES, IpcNamespace};

/// IPC 命名空间 (全局资源容器)
///
/// 存储所有 IPC 资源。资源数组在 kmalloc 堆上惰性分配,
/// 由 `ipc_init()` 在初始化时填充槽位。
pub static IPC_NAMESPACE: RacyCell<IpcNamespace> = RacyCell::new(IpcNamespace::empty());

/// 全局 ID 分配器
///
/// 确保每个 IPC 资源有唯一标识符。
/// 从 1 开始递增，0 表示"未使用/无效"。
pub static NEXT_IPC_ID: RacyCell<IpcId> = RacyCell::new(1);

// ============================================================================
// 初始化函数
// ============================================================================

/// 初始化 IPC 子系统
///
/// 必须在内核启动早期调用，初始化所有资源槽位。
/// 此函数只能调用一次，且必须在多核启动前完成。
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ipc_init() {
    // 重置 ID 分配器
    NEXT_IPC_ID.map_mut(|id| *id = 1);

    IPC_NAMESPACE.map_mut(|ns| {
        // 在 kmalloc 堆上分配全部资源槽位
        ns.allocate();

        // 初始化管道等待队列
        for pipe in &mut ns.pipes[..IPC_MAX_PIPES] {
            pipe.id = 0;
            pipe.read_wait.init();
            pipe.write_wait.init();
        }

        // 初始化消息队列等待队列
        for queue in &mut ns.msg_queues[..IPC_MAX_MSG_QUEUES] {
            queue.id = 0;
            queue.send_wait.init();
            queue.recv_wait.init();
        }

        // 初始化信号量等待队列
        for sem in &mut ns.semaphores[..IPC_MAX_SEMAPHORES] {
            sem.id = 0;
            sem.wait.init();
        }
    });
}

// ============================================================================
// 重新导出常用类型
// ============================================================================

pub use types::{
    IpcId, IpcType, Message, MsgQueue, Pipe, Semaphore, ShmSegment, SignalAction, SignalNum,
    WaitQueue, WaitQueueItem,
};

// 调度器集成 (block_current_thread/block_with_timeout/wake_*) 与异步 IPC
// (AsyncMsgSender/Receiver 等) 为 functions 策略项, privileged 侧壳已删 (DECISION-J),
// 由 `functions::ipc::{scheduler_integration, async_ipc}` 提供。

// DECISION-I: IpcStrategy trait 与注册/获取入口 (FFI 边界与 functions 注册共用)
pub use strategy::{IpcStrategy, current_ipc_strategy, register_ipc_strategy};

// P-71: 原 `#[cfg(test)] mod stress_tests` 与 `#[cfg(test)] mod tests` 已上移出
// TCB —— 两者经 `crate::functions::ipc` 访问业务层策略, privileged 独立成 crate
// 后不允许反向依赖 functions. 现位于 `kernel_tests/test_ipc_inline.rs`.
