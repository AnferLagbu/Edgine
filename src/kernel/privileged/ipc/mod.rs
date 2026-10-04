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
//! ├── Pipe Module         // 管道管理
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

/// 管道实现
pub mod pipe;

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
        for i in 0..IPC_MAX_PIPES {
            ns.pipes[i].id = 0;
            ns.pipes[i].read_wait.init();
            ns.pipes[i].write_wait.init();
        }

        // 初始化消息队列等待队列
        for i in 0..IPC_MAX_MSG_QUEUES {
            ns.msg_queues[i].id = 0;
            ns.msg_queues[i].send_wait.init();
            ns.msg_queues[i].recv_wait.init();
        }

        // 初始化信号量等待队列
        for i in 0..IPC_MAX_SEMAPHORES {
            ns.semaphores[i].id = 0;
            ns.semaphores[i].wait.init();
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

// ============================================================================
// 压力测试与边界测试
// ============================================================================

#[cfg(test)]
mod stress_tests;

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    // 消息体上限常量定义于 types (框架消息队列的权威来源).
    use super::types::MSG_MAX_SIZE;
    // DECISION-J: sem/signal 壳已删, tests 经 functions 公共 API 访问 (测试代码)
    use crate::functions::ipc::{sem, signal};

    #[test]
    fn test_pipe_create_and_close() {
        let mut ns = IpcNamespace::empty();
        ns.allocate();

        let mut next_id: IpcId = 1;
        let pid: u32 = 100;

        // 测试创建管道 (T6-1: 委托 functions 策略)
        match crate::functions::ipc::pipe::pipe_create_safe(&mut ns, &mut next_id, pid) {
            Ok((rfd, wfd)) => {
                assert!(rfd > 0);
                assert!(wfd > 0);
                assert_eq!(wfd, rfd + 1);

                // 测试关闭读端
                assert!(crate::functions::ipc::pipe::pipe_close_safe(&mut ns, rfd).is_ok());

                // 测试关闭写端
                assert!(crate::functions::ipc::pipe::pipe_close_safe(&mut ns, wfd).is_ok());
            }
            Err(e) => panic!("Failed to create pipe: {}", e),
        }
    }

    // UT-06 (2026-09-24): test_shm_lifecycle 已迁出 — shm_create_safe 依赖裸机
    // PMM (host 下不可用); 等价断言迁至 privileged/tests/test_ipc.rs
    // (register_ipc_tests 注册), 在 kernel_test (QEMU 裸机) 执行.

    #[test]
    fn test_msgq_send_recv() {
        let mut ns = IpcNamespace::empty();
        ns.allocate();

        let mut next_id: IpcId = 1;
        let pid: u32 = 300;

        // 创建消息队列 (T6-1: 委托 functions 策略)
        let id = match crate::functions::ipc::msgq::msgq_create_safe(
            &mut ns,
            &mut next_id,
            0o666,
            pid,
        ) {
            Ok(id) => id,
            Err(e) => panic!("Failed to create MsgQ: {}", e),
        };

        // 发送消息
        let data = b"Hello, IPC!";
        assert!(
            crate::functions::ipc::msgq::msgq_send_safe(
                &mut ns,
                id,
                42,
                Some(data),
                data.len(),
                pid
            )
            .is_ok()
        );

        // 接收消息
        let mut type_out: u64 = 0;
        let mut buf = [0u8; MSG_MAX_SIZE];
        let mut size_out: u64 = 0;

        let read_size = match crate::functions::ipc::msgq::msgq_recv_safe(
            &mut ns,
            id,
            Some(&mut type_out),
            Some(&mut buf),
            Some(&mut size_out),
        ) {
            Ok(n) => n,
            Err(e) => panic!("Failed to receive message: {}", e),
        };

        assert_eq!(type_out, 42);
        assert_eq!(read_size, data.len());
        assert_eq!(&buf[..data.len()], data);

        // 销毁队列
        assert!(crate::functions::ipc::msgq::msgq_destroy_safe(&mut ns, id).is_ok());
    }

    #[test]
    fn test_semaphore_operations() {
        let mut ns = IpcNamespace::empty();
        ns.allocate();

        let mut next_id: IpcId = 1;
        let pid: u32 = 400;

        // 创建计数为 1 的信号量 (互斥锁)
        let id = match sem::sem_create_safe(&mut ns, &mut next_id, 1, 10, pid) {
            Ok(id) => id,
            Err(e) => panic!("Failed to create semaphore: {}", e),
        };

        // P 操作 (应该成功)
        assert!(sem::sem_wait_safe(&mut ns, id).is_ok());

        // V 操作
        assert!(sem::sem_post_safe(&mut ns, id).is_ok());

        // 销毁信号量
        assert!(sem::sem_destroy_safe(&mut ns, id).is_ok());
    }

    #[test]
    fn test_signal_validation() {
        // 测试有效信号. 注: signal_send_safe 需目标进程存在才返回 Ok, host 与
        // kernel_test 均无进程表 → 环境容差接受 Err(-2) (口径同 privileged/proc/
        // signal.rs 的 kill_broadcast 用例), 但不得为范围错误 Err(-1).
        let sent = signal::signal_send_safe(1, 100); // SIGINT
        assert!(sent.is_ok() || sent == Err(-2), "got {:?}", sent);
        assert!(signal::signal_register_safe(1, None, 0).is_ok());
        assert!(signal::signal_block_safe(1).is_ok());
        assert!(signal::signal_unblock_safe(1).is_ok());

        // 测试无效信号 (超出范围)
        assert!(signal::signal_send_safe(0, 100).is_err()); // 信号 0 无效
        assert!(signal::signal_send_safe(33, 100).is_err()); // 超出最大值
        assert!(signal::signal_register_safe(0, None, 0).is_err());
        assert!(signal::signal_block_safe(0).is_err());
        assert!(signal::signal_unblock_safe(34).is_err());
    }
}

#[cfg(feature = "kernel_test")]
pub fn register_ipc_tests() {
    crate::privileged::tests::test_ipc::register_ipc_tests();
}
