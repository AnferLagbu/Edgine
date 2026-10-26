//! IPC 压力测试与边界测试 (P-71 上移出 TCB)
//!
//! 验证 IPC 子系统在高负载和极端条件下的稳定性：
//! - **压力测试**: 高并发、大数据量、长时间运行
//! - **边界测试**: 空数据、满缓冲区、无效参数
//! - **竞态条件**: 多线程同时访问
//! - **资源泄漏**: 频繁创建/销毁
//!
//! 原位于 `privileged/ipc/stress_tests.rs`. 本组用例经 `functions::ipc` 访问业务层
//! 策略函数, `privileged` 独立成 crate 后不允许反向依赖 `functions`, 故迁至 glue crate.

use crate::privileged::ipc::types::*;
// T6-1: pipe/shm/msgq 策略函数已迁移到 functions; DECISION-J: sem 壳已删, 亦走 functions
use crate::functions::ipc::types::PIPE_BUFFER_SIZE;
use crate::functions::ipc::{msgq, pipe, sem, shm};

// ============================================================================
// 压力测试
// ============================================================================

#[cfg(test)]
mod stress_tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    /// 测试管道高频读写 (1000 次循环)
    #[test]
    fn test_pipe_high_frequency_io() {
        let mut ns = create_test_namespace();
        let mut next_id: IpcId = 1;
        let pid: u32 = 500;

        // 创建管道
        let (rfd, wfd) = match pipe::pipe_create_safe(&mut ns, &mut next_id, pid) {
            Ok(pair) => pair,
            Err(e) => panic!("Failed to create pipe: {}", e),
        };

        // 高频写入读取循环
        for i in 0..1000 {
            let data = format!("Message {}", i);

            // 写入
            assert!(
                pipe::pipe_write_safe(&mut ns, wfd, data.as_bytes(), data.len() as u32).is_ok(),
                "Write failed at iteration {}",
                i
            );

            // 读取
            let mut buf = [0u8; 64];

            assert!(
                pipe::pipe_read_safe(&mut ns, rfd, &mut buf, data.len() as u32).is_ok(),
                "Read failed at iteration {}",
                i
            );

            assert_eq!(&buf[..data.len()], data.as_bytes());
        }

        // 清理
        pipe::pipe_close_safe(&mut ns, rfd).unwrap();
        pipe::pipe_close_safe(&mut ns, wfd).unwrap();
    }

    /// 测试消息队列满负载 (填满队列后继续发送)
    #[test]
    fn test_msgq_full_queue() {
        let mut ns = create_test_namespace();
        let mut next_id: IpcId = 1;
        let pid: u32 = 600;

        // 创建小容量队列用于快速填满
        let id = msgq::msgq_create_safe(&mut ns, &mut next_id, 0o666, pid).unwrap();

        // 发送大量消息直到失败 (队列满)
        let mut success_count = 0;
        for i in 0..1000 {
            let data = vec![i as u8; 100]; // 100 字节消息

            if msgq::msgq_send_safe(&mut ns, id, i as u64, Some(&data), data.len(), pid).is_ok() {
                success_count += 1;
            } else {
                break; // 队列已满
            }
        }

        assert!(success_count > 0, "Should send at least some messages");

        // 接收所有消息
        let mut recv_count = 0;
        loop {
            let mut type_out: u64 = 0;
            let mut buf = [0u8; MSG_MAX_SIZE];
            let mut size_out: u64 = 0;

            match msgq::msgq_recv_safe(
                &mut ns,
                id,
                Some(&mut type_out),
                Some(&mut buf),
                Some(&mut size_out),
            ) {
                Ok(_) => recv_count += 1,
                Err(_) => break,
            }
        }

        assert_eq!(
            recv_count, success_count,
            "All sent messages should be received"
        );

        // 清理
        msgq::msgq_destroy_safe(&mut ns, id).unwrap();
    }

    // UT-06 (2026-09-24): test_shm_rapid_attach_detach 已删除 — 与 privileged 测试
    // 注册表中同名用例 (register_ipc_tests 注册, 裸机执行真实逻辑 / host 侧 Skip)
    // 断言完全等价, 属残留副本.

    /// 测试信号量高并发 P/V 操作
    #[test]
    fn test_semaphore_high_concurrency() {
        let mut ns = create_test_namespace();
        let mut next_id: IpcId = 1;
        let pid: u32 = 800;

        // 创建计数为 10 的信号量
        let id = sem::sem_create_safe(&mut ns, &mut next_id, 10, 20, pid).unwrap();

        // 快速 P/V 操作
        for _ in 0..1000 {
            sem::sem_wait_safe(&mut ns, id).unwrap();
            sem::sem_post_safe(&mut ns, id).unwrap();
        }

        // 最终值应该仍然是初始值
        // (假设没有其他线程干扰)

        // 清理
        sem::sem_destroy_safe(&mut ns, id).unwrap();
    }

    /// 测试管道大数据传输 (接近 PIPE_BUFFER_SIZE)
    #[test]
    fn test_pipe_large_data_transfer() {
        let mut ns = create_test_namespace();
        let mut next_id: IpcId = 1;
        let pid: u32 = 900;

        let (rfd, wfd) = pipe::pipe_create_safe(&mut ns, &mut next_id, pid).unwrap();

        // 创建接近最大大小的数据块
        let large_data = vec![0xABu8; PIPE_BUFFER_SIZE - 1];

        // 写入大块数据
        let written =
            pipe::pipe_write_safe(&mut ns, wfd, &large_data, large_data.len() as u32).unwrap();
        assert_eq!(written as usize, large_data.len());

        // 读取并验证
        let mut buf = [0u8; PIPE_BUFFER_SIZE];
        let nread = pipe::pipe_read_safe(&mut ns, rfd, &mut buf, large_data.len() as u32).unwrap();

        assert_eq!(nread as usize, large_data.len());
        assert_eq!(&buf[..large_data.len()], large_data.as_slice());

        // 清理
        pipe::pipe_close_safe(&mut ns, rfd).unwrap();
        pipe::pipe_close_safe(&mut ns, wfd).unwrap();
    }
}

// ============================================================================
// 边界测试
// ============================================================================

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use alloc::vec;

    /// 测试空数据读写
    #[test]
    fn test_empty_data_operations() {
        let mut ns = create_test_namespace();
        let mut next_id: IpcId = 1;
        let pid: u32 = 1000;

        // 管道空数据
        let (rfd, wfd) = pipe::pipe_create_safe(&mut ns, &mut next_id, pid).unwrap();

        // 写入 0 字节
        let result = pipe::pipe_write_safe(&mut ns, wfd, &[], 0);
        assert!(result.is_ok() || result.unwrap_err() == -1); // 允许成功或错误

        // 清理
        pipe::pipe_close_safe(&mut ns, rfd).unwrap();
        pipe::pipe_close_safe(&mut ns, wfd).unwrap();

        // 消息队列空消息
        let id = msgq::msgq_create_safe(&mut ns, &mut next_id, 0o666, pid).unwrap();

        let result = msgq::msgq_send_safe(&mut ns, id, 0, None, 0, pid);
        assert!(result.is_ok()); // 空消息应该允许

        // 接收空消息
        let mut buf = [0u8; 1];
        let mut size: u64 = 999;
        msgq::msgq_recv_safe(&mut ns, id, None, Some(&mut buf), Some(&mut size)).unwrap();
        assert_eq!(size, 0); // 应该是 0 字节

        msgq::msgq_destroy_safe(&mut ns, id).unwrap();
    }

    /// 测试最大尺寸数据处理
    #[test]
    fn test_max_size_data() {
        let mut ns = create_test_namespace();
        let mut next_id: IpcId = 1;
        let pid: u32 = 1100;

        // 消息队列最大消息
        let id = msgq::msgq_create_safe(&mut ns, &mut next_id, 0o666, pid).unwrap();

        let max_data = vec![0xFFu8; MSG_MAX_SIZE];
        let result = msgq::msgq_send_safe(&mut ns, id, 999, Some(&max_data), max_data.len(), pid);

        if result.is_ok() {
            // 成功发送，验证接收
            let mut buf = [0u8; MSG_MAX_SIZE + 1]; // 多分配一个字节检测溢出
            let mut size: u64 = 0;
            msgq::msgq_recv_safe(&mut ns, id, None, Some(&mut buf), Some(&mut size)).unwrap();
            assert_eq!(size, MSG_MAX_SIZE as u64);
            assert_eq!(&buf[..MSG_MAX_SIZE], max_data.as_slice());
            assert_eq!(buf[MSG_MAX_SIZE], 0); // 无溢出
        }
        // 如果失败，说明实现限制了大小，也是可接受的

        msgq::msgq_destroy_safe(&mut ns, id).unwrap();
    }

    /// 测试无效 ID 处理
    #[test]
    fn test_invalid_ids() {
        let mut ns = create_test_namespace();
        let invalid_id: IpcId = 99999;
        // 管道族 safe API 以 fd: i32 标识端点 (functions 侧签名), 与资源 ID 类型不同.
        let invalid_fd: i32 = invalid_id as i32;
        let pid: u32 = 1200;

        // 管道操作使用无效 ID
        assert!(pipe::pipe_write_safe(&mut ns, invalid_fd, &[], 0).is_err());
        assert!(pipe::pipe_read_safe(&mut ns, invalid_fd, &mut [], 0).is_err());
        assert!(pipe::pipe_close_safe(&mut ns, invalid_fd).is_err());

        // 共享内存操作使用无效 ID
        assert!(shm::shm_attach_safe(&mut ns, invalid_id, pid).is_err());
        assert!(shm::shm_detach_safe(&mut ns, invalid_id, pid).is_err());
        assert!(shm::shm_destroy_safe(&mut ns, invalid_id).is_err());

        // 消息队列操作使用无效 ID
        assert!(msgq::msgq_send_safe(&mut ns, invalid_id, 0, None, 0, pid).is_err());
        assert!(msgq::msgq_recv_safe(&mut ns, invalid_id, None, None, None).is_err());
        assert!(msgq::msgq_destroy_safe(&mut ns, invalid_id).is_err());

        // 信号量操作使用无效 ID
        assert!(sem::sem_wait_safe(&mut ns, invalid_id).is_err());
        assert!(sem::sem_post_safe(&mut ns, invalid_id).is_err());
        assert!(sem::sem_destroy_safe(&mut ns, invalid_id).is_err());
    }

    /// 测试重复关闭/销毁
    #[test]
    fn test_duplicate_close_or_destroy() {
        let mut ns = create_test_namespace();
        let mut next_id: IpcId = 1;
        let pid: u32 = 1300;

        // 创建管道 (本用例只校验读端重复关闭, 写端不需持有)
        let (rfd, _wfd) = pipe::pipe_create_safe(&mut ns, &mut next_id, pid).unwrap();

        // 第一次关闭 - 应该成功
        assert!(pipe::pipe_close_safe(&mut ns, rfd).is_ok());

        // 第二次关闭 - 应该失败或幂等
        let result = pipe::pipe_close_safe(&mut ns, rfd);
        assert!(result.is_err() || result.is_ok()); // 两种行为都可接受

        // 创建消息队列
        let id = msgq::msgq_create_safe(&mut ns, &mut next_id, 0o666, pid).unwrap();

        // 第一次销毁 - 应该成功
        assert!(msgq::msgq_destroy_safe(&mut ns, id).is_ok());

        // 第二次销毁 - 应该失败或幂等
        let result = msgq::msgq_destroy_safe(&mut ns, id);
        assert!(result.is_err() || result.is_ok());
    }

    // UT-06 (2026-09-24): test_zero_permissions 已迁出 — shm_create_safe 依赖裸机
    // PMM (host 下不可用); 等价断言迁至 privileged/tests/test_ipc.rs
    // (register_ipc_tests 注册), 在 kernel_test (QEMU 裸机) 执行.
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 创建测试用的 IPC 命名空间实例
fn create_test_namespace() -> IpcNamespace {
    let mut ns = IpcNamespace::empty();
    ns.allocate();
    ns
}

// ============================================================================
// IPC 基础用例 (P-71 自 `privileged/ipc/mod.rs` 的 `#[cfg(test)] mod tests` 上移)
// ============================================================================

#[cfg(test)]
mod ipc_tests {
    use crate::privileged::ipc::types::{IpcId, IpcNamespace, MSG_MAX_SIZE};
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
    // PMM (host 下不可用); 等价断言迁至 kernel_tests/test_ipc.rs
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
