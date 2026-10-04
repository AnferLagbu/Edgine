#![deny(unsafe_code)]
//! 消息队列策略 — T6-1 从 privileged/ipc/msgq.rs 提取
//!
//! 纯策略逻辑: 参数校验、资源查找、状态管理、链表操作.
//! 所有 unsafe 操作通过 `privileged::ipc::msgq::raw` (`MessageRef`) 安全方法完成.

use crate::privileged::ipc::msgq::raw::{self, MessageRef};
use crate::privileged::ipc::types::{
    IpcId, IpcNamespace, MSG_MAX_SIZE, MSG_QUEUE_MAX_MSGS, MsgQueue,
};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};

/// 查找空闲消息队列槽位
pub fn msgq_find_free(namespace: &mut IpcNamespace) -> Option<&mut MsgQueue> {
    namespace.msg_queues.iter_mut().find(|q| q.id == 0)
}

/// 根据 ID 查找消息队列
pub fn msgq_find_by_id(namespace: &mut IpcNamespace, id: IpcId) -> Option<&mut MsgQueue> {
    namespace.msg_queues.iter_mut().find(|q| q.id == id)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 创建消息队列 (策略: 槽位分配 + 初始化)
///
/// # Errors
/// 当消息队列表已满、无空闲槽位时返回 `Err(-1)`.
pub fn msgq_create_safe(
    namespace: &mut IpcNamespace,
    next_id: &mut IpcId,
    perm: i32,
    current_pid: u32,
) -> Result<IpcId, i32> {
    let mq = match msgq_find_free(namespace) {
        Some(q) => q,
        None => return Err(-1),
    };

    // 初始化消息队列
    mq.id = *next_id;
    *next_id += 1;

    mq.owner = current_pid;
    mq.head = AtomicPtr::new(core::ptr::null_mut());
    mq.tail = AtomicPtr::new(core::ptr::null_mut());
    mq.count = 0;
    mq.max_msgs = MSG_QUEUE_MAX_MSGS;
    mq.max_size = MSG_MAX_SIZE as u32;
    mq.flags = 0;
    mq.perm = perm;

    mq.send_wait.init();
    mq.recv_wait.init();

    Ok(mq.id)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 向消息队列发送消息 (策略: 参数校验 + 容量检查 + 入队 + 唤醒)
///
/// # Errors
/// 当队列不存在时返回 `Err(-1)`; 当消息过大时返回 `Err(-2)`;
/// 当队列已满时返回 `Err(-3)`; 当消息结构体分配失败时返回 `Err(-4)`.
///
/// # Panics
/// 当队列尾指针 `tail` 本应为 `Some` 却为 `None` 时发生 panic (内部使用 `expect`).
pub fn msgq_send_safe(
    namespace: &mut IpcNamespace,
    id: IpcId,
    type_: u64,
    data: Option<&[u8]>,
    size: usize,
    current_pid: u32,
) -> Result<(), i32> {
    // 参数校验
    if size > MSG_MAX_SIZE {
        return Err(-2);
    }

    let mq = match msgq_find_by_id(namespace, id) {
        Some(q) => q,
        None => return Err(-1),
    };

    // 检查队列是否已满
    if mq.count >= mq.max_msgs {
        return Err(-3);
    }

    // 分配消息结构体 (委托 privileged 机制)
    let msg_nn = match raw::allocate() {
        Some(m) => m,
        None => return Err(-4),
    };

    // SAFETY: msg 刚由 `allocate` 分配, 非空, 后续由
    // `msgq_recv_safe` 或 `msgq_destroy_safe` 释放.
    let msg_ref = msg_nn.get_mut();
    msg_ref.type_ = type_;
    msg_ref.sender = u64::from(current_pid);
    msg_ref.size = size as u64;
    msg_ref.next.store(core::ptr::null_mut(), Ordering::Relaxed);

    // 复制数据 (如果有)
    if let Some(src) = data {
        if size > 0 && !src.is_empty() {
            msg_ref.data[..size].copy_from_slice(&src[..size]);
        }
    }

    // 入队 (尾插法)
    let msg_ptr = msg_nn.as_non_null().as_ptr();
    if mq.tail.load(Ordering::Relaxed).is_null() {
        mq.head.store(msg_ptr, Ordering::Relaxed);
        mq.tail.store(msg_ptr, Ordering::Relaxed);
    } else {
        // 队列非空: 取尾节点链接新消息
        let tail_ptr = mq.tail.load(Ordering::Relaxed);
        let tail_nn = NonNull::new(tail_ptr).expect("msgq: tail 应为非空");
        let tail_ref = MessageRef::from_some(tail_nn);
        tail_ref.set_next(msg_ptr);
        mq.tail.store(msg_ptr, Ordering::Relaxed);
    }
    mq.count += 1;

    // 唤醒等待接收的线程 (B07-15: 经调度器真实唤醒, 中断上下文安全)
    if mq.recv_wait.count() > 0 {
        super::scheduler_integration::wake_one_thread(&mut mq.recv_wait);
    }

    Ok(())
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 从消息队列接收消息 (策略: 出队 + 数据拷贝 + 释放 + 唤醒)
///
/// # Errors
/// 当队列不存在时返回 `Err(-1)`; 当队列为空时返回 `Err(-2)`.
///
/// # Panics
/// 当队列头指针 `head` 本应为 `Some` 却为 `None` 时发生 panic (内部使用 `expect`).
pub fn msgq_recv_safe(
    namespace: &mut IpcNamespace,
    id: IpcId,
    type_out: Option<&mut u64>,
    data_out: Option<&mut [u8]>,
    size_out: Option<&mut u64>,
) -> Result<usize, i32> {
    let mq = match msgq_find_by_id(namespace, id) {
        Some(q) => q,
        None => return Err(-1),
    };

    // 检查队列是否为空
    if mq.head.load(Ordering::Relaxed).is_null() {
        return Err(-2);
    }

    // 出队 (头删法)
    let head_ptr = mq.head.load(Ordering::Relaxed);
    let msg_nn = NonNull::new(head_ptr).expect("msgq: head 应为非空");
    let msg_ref = MessageRef::from_some(msg_nn);
    mq.head.store(msg_ref.next(), Ordering::Relaxed);

    if mq.head.load(Ordering::Relaxed).is_null() {
        mq.tail.store(core::ptr::null_mut(), Ordering::Relaxed);
    }
    mq.count -= 1;

    // 通过 MessageRef 安全读取字段
    let read_size = msg_ref.get().size as usize;
    let msg_type = msg_ref.get().type_;
    let msg_data = msg_ref.get().data;
    let msg_size = msg_ref.get().size;

    if let Some(t) = type_out {
        *t = msg_type;
    }

    if let Some(buf) = data_out {
        let copy_len = read_size.min(buf.len());
        if read_size > 0 {
            buf[..copy_len].copy_from_slice(&msg_data[..copy_len]);
        }
    }

    if let Some(s) = size_out {
        *s = msg_size;
    }

    // 通过 MessageRef 释放内存
    msg_ref.free();

    // 唤醒等待发送的线程 (B07-15: 经调度器真实唤醒, 中断上下文安全)
    if mq.send_wait.count() > 0 {
        super::scheduler_integration::wake_one_thread(&mut mq.send_wait);
    }

    Ok(read_size)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
#[expect(
    clippy::missing_panics_doc,
    reason = "missing_panics_doc: msgq_destroy_safe 释放消息链表时若 head 指针本应非空却为 null 会 panic (链表不变量被破坏, 见函数内 expect)"
)]
/// 销毁消息队列 (策略: 释放所有消息 + 清理结构体)
///
/// # Errors
/// 当队列不存在时返回 `Err(-1)`.
pub fn msgq_destroy_safe(namespace: &mut IpcNamespace, id: IpcId) -> Result<(), i32> {
    let mq = match msgq_find_by_id(namespace, id) {
        Some(q) => q,
        None => return Err(-1),
    };

    // 释放所有剩余消息
    let mut head_ptr = mq.head.load(Ordering::Relaxed);
    while !head_ptr.is_null() {
        let msg_nn = NonNull::new(head_ptr).expect("msgq_destroy: head 应为非空");
        let msg_ref = MessageRef::from_some(msg_nn);
        head_ptr = msg_ref.next();
        mq.head.store(head_ptr, Ordering::Relaxed);
        msg_ref.free();
    }

    // 清理结构体
    mq.id = 0;

    Ok(())
}
