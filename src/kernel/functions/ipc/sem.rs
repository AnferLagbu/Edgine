#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。纯策略实现。
//! 信号量 (Semaphore) 实现 — functions 层策略主体
//!
//! ## T6-9 迁移记录
//!
//! 原属 privileged/ipc/sem.rs, 2026-06-16 提取到 functions.
//! 纯策略代码 (信号量 CRUD + 操作), 0 unsafe.
//! privileged 仅保留 re-export.
//!
//! 提供进程间同步原语
//! 功能等价于 POSIX semaphores

use super::types::{IpcId, IpcNamespace, Semaphore};
use crate::privileged::proc::process_get_current_pid;

/// 查找空闲信号量槽位
fn sem_find_free(namespace: &mut IpcNamespace) -> Option<&mut Semaphore> {
    namespace.semaphores.iter_mut().find(|s| s.id == 0)
}

/// 根据 ID 查找信号量
fn sem_find_by_id(namespace: &mut IpcNamespace, id: IpcId) -> Option<&mut Semaphore> {
    namespace.semaphores.iter_mut().find(|s| s.id == id)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 创建信号量 (Rust 安全接口)
///
/// # Arguments
/// * `namespace` - IPC 命名空间引用
/// * `next_id` - 全局 ID 分配器
/// * `count` - 初始计数值
/// * `max_count` - 最大计数值
/// * `current_pid` - 当前进程 PID
///
/// # Returns
/// * Ok(IpcId) - 成功，返回信号量 ID
/// * Err(i32) - 失败 (-1: 无可用槽位)
///
/// # Errors
/// 当信号量表已满、无空闲槽位时返回 `Err(-1)`.
pub fn sem_create_safe(
    namespace: &mut IpcNamespace,
    next_id: &mut IpcId,
    count: u32,
    max_count: u32,
    current_pid: u32,
) -> Result<IpcId, i32> {
    let sem = match sem_find_free(namespace) {
        Some(s) => s,
        None => return Err(-1),
    };

    // 初始化信号量
    sem.id = *next_id;
    *next_id += 1;

    sem.owner = current_pid;
    sem.count = count as i32;
    sem.max_count = max_count;
    sem.flags = 0;
    sem.perm = 0o666; // 默认权限

    sem.wait.init();

    Ok(sem.id)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 等待/获取信号量 (P 操作) (Rust 安全接口)
///
/// 如果计数 > 0，则减少计数并立即返回；
/// 否则阻塞等待直到有资源可用。
///
/// # Arguments
/// * `namespace` - IPC 命名空间引用
/// * `id` - 信号量 ID
///
/// # Returns
/// * Ok(()) - 成功获取
/// * Err(i32) - 错误码 (-1: 无效 ID)
///
/// # Errors
/// 当信号量 ID 无效、不存在时返回 `Err(-1)`.
pub fn sem_wait_safe(namespace: &mut IpcNamespace, id: IpcId) -> Result<(), i32> {
    let sem = match sem_find_by_id(namespace, id) {
        Some(s) => s,
        None => return Err(-1),
    };

    // 等待直到计数 > 0 (B07-15: 真实阻塞替代忙等待)
    // sem.count 由另一个执行上下文（sem_post）修改，这是信号量同步原语
    while sem.count <= 0 {
        // B07-15: 阻塞当前线程到信号量等待队列, 由 sem_post 唤醒.
        // 原实现为忙等待 (spin_loop), 单核下 starve 唤醒方.
        match super::scheduler_integration::block_current_thread(&mut sem.wait, 0) {
            Ok(()) => {} // 被唤醒, 循环重新检查计数
            Err(_) => {
                // 中断上下文或无效线程: 退回忙等待 (保守).
                core::hint::spin_loop();
            }
        }
    }

    sem.count -= 1;

    Ok(())
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 释放/通知信号量 (V 操作) (Rust 安全接口)
///
/// 增加计数并唤醒一个等待的线程。
///
/// # Arguments
/// * `namespace` - IPC 命名空间引用
/// * `id` - 信号量 ID
///
/// # Returns
/// * Ok(()) - 成功
/// * Err(i32) - 错误码 (-1: 无效 ID)
///
/// # Errors
/// 当信号量 ID 无效、不存在时返回 `Err(-1)`.
pub fn sem_post_safe(namespace: &mut IpcNamespace, id: IpcId) -> Result<(), i32> {
    let sem = match sem_find_by_id(namespace, id) {
        Some(s) => s,
        None => return Err(-1),
    };

    // 增加计数 (不超过最大值)
    if (sem.count as u32) < sem.max_count {
        sem.count += 1;
    }

    // 唤醒一个等待的线程 (B07-15: 经调度器真实唤醒, 中断上下文安全)
    if sem.wait.count() > 0 {
        super::scheduler_integration::wake_one_thread(&mut sem.wait);
    }

    Ok(())
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 销毁信号量 (Rust 安全接口)
///
/// # Arguments
/// * `namespace` - IPC 命名空间引用
/// * `id` - 信号量 ID
///
/// # Returns
/// * Ok(()) - 成功
/// * Err(i32) - 错误码 (-1: 无效 ID)
///
/// # Errors
/// 当信号量 ID 无效、不存在时返回 `Err(-1)`.
pub fn sem_destroy_safe(namespace: &mut IpcNamespace, id: IpcId) -> Result<(), i32> {
    let sem = match sem_find_by_id(namespace, id) {
        Some(s) => s,
        None => return Err(-1),
    };

    // 唤醒所有等待的线程 (B07-15: 经调度器真实唤醒)
    super::scheduler_integration::wake_all_threads(&mut sem.wait);

    // 清理结构体
    sem.id = 0;

    Ok(())
}

// ============================================================================
// FFI 导出函数
// ============================================================================

/// FFI: 创建信号量
pub fn ipc_sem_create(count: u32, max_count: u32) -> IpcId {
    let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
    let next_id = crate::privileged::ipc::NEXT_IPC_ID.get_mut();
    let pid = process_get_current_pid();
    sem_create_safe(ns, next_id, count, max_count, pid).unwrap_or(0)
}

/// FFI: 等待信号量 (P 操作)
pub fn ipc_sem_wait(id: IpcId) -> i32 {
    let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
    match sem_wait_safe(ns, id) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// FFI: 释放信号量 (V 操作)
pub fn ipc_sem_post(id: IpcId) -> i32 {
    let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
    match sem_post_safe(ns, id) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// FFI: 销毁信号量
pub fn ipc_sem_destroy(id: IpcId) -> i32 {
    let ns = crate::privileged::ipc::IPC_NAMESPACE.get_mut();
    match sem_destroy_safe(ns, id) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
