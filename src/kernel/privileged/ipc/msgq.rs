//! 消息队列 (MsgQ) 机制 + FFI 边界 — T6-1 策略已迁移至 functions/ipc/msgq.rs
//!
//! 本模块保留:
//! - `raw` 模块: MessageRef 侵入式链表安全封装 (机制, 含 unsafe)
//! - FFI 函数: 用户空间指针转换, 委托 functions 策略
//!
//! ## SAFETY
//!
//! - `raw::MessageRef` 集中所有 `NonNull<Message>` 的 unsafe 操作.
//! - FFI 函数通过 `UserReadPtr/WritePtr/RefMut` 安全访问用户空间内存.

use super::types::{IpcId, MSG_MAX_SIZE, Message};
use crate::privileged::errno::Errno;
use crate::privileged::ipc::strategy::current_ipc_strategy;
use crate::privileged::proc::process_get_current_pid;
use crate::privileged::userptr::{UserReadPtr, UserRefMut, UserWritePtr};

/// === 消息原始指针特权封装 (Framekernel 模式) ===
///
/// `NonNull<Message>` 是侵入式链表的关键句柄, 所有 unsafe 访问
/// (`Box::from_raw/NonNull::new_unchecked/裸字段读`) 都集中在 `MessageRef` 内部,
/// 业务逻辑 (`send`/`receive`/`free`) 通过安全方法操作。
///
/// T6-1: `pub` 可见性 — functions/ipc/msgq.rs 策略函数需要通过 `MessageRef` 安全方法
/// 操作侵入式链表, 不直接接触 unsafe 内部.
pub mod raw {
    use super::Message;
    use alloc::boxed::Box;
    use core::ptr::NonNull;

    /// `NonNull<Message>` 的安全 newtype 封装
    #[derive(Clone, Copy)]
    pub struct MessageRef(NonNull<Message>);

    impl MessageRef {
        /// 构造一个 `MessageRef` (内部 unsafe 边界)
        ///
        /// # Safety
        ///
        /// `nn` 必须为 `allocate_message` 返回的有效 `NonNull`。
        pub unsafe fn from_non_null(nn: NonNull<Message>) -> Self {
            Self(nn)
        }

        /// 从 `Option<NonNull<Message>>` 安全提升为 `MessageRef`
        ///
        /// - 内部 unsafe: 在 `raw` 子模块内已声明 `from_non_null` 边界
        /// - 调用方只需保证 `nn` 是 Some (从 mq.head/mq.tail 取出)
        pub fn from_some(nn: NonNull<Message>) -> Self {
            // SAFETY: nn 来自 `Option::unwrap` (字段由 `allocate_message` 填充, 满足侵入式链表不变量).
            unsafe { Self::from_non_null(nn) }
        }

        /// 获取底层 `NonNull`
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn as_non_null(self) -> NonNull<Message> {
            self.0
        }

        /// 读 `next` 字段 (侵入式链表) — 返回裸指针 (null = 无后继)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn next(&self) -> *mut Message {
            // SAFETY: 调用方保证 self 指向有效 Message。
            // B03-24: Message.next 为 AtomicPtr, 队列操作在持锁下执行, Relaxed 足够.
            unsafe {
                (*self.0.as_ptr())
                    .next
                    .load(core::sync::atomic::Ordering::Relaxed)
            }
        }

        /// 写 `next` 字段 (null = 无后继)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_next(&self, next: *mut Message) {
            // SAFETY: 同上, self 必须是有效 Message。
            unsafe {
                (*self.0.as_ptr())
                    .next
                    .store(next, core::sync::atomic::Ordering::Relaxed);
            }
        }

        /// 获取 &Message 引用
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn get(&self) -> &Message {
            // SAFETY: self 指向有效 Message。
            unsafe { &*self.0.as_ptr() }
        }

        /// 获取 &mut Message 引用
        #[inline(always)]
        pub fn get_mut(&self) -> &mut Message {
            // SAFETY: self 指向有效 Message, &mut 保证独占。
            unsafe { &mut *self.0.as_ptr() }
        }

        /// 释放消息 (`Box::from_raw` + drop)
        pub fn free(self) {
            // SAFETY: self 来自 allocate_message, 由 Box::into_raw 创建。
            let _ = unsafe { Box::from_raw(self.0.as_ptr()) };
        }
    }

    /// 分配一个新 `Message` 并包装为 `MessageRef` (集中 unsafe 入口)
    pub fn allocate() -> Option<MessageRef> {
        let msg = Box::new(Message::new());
        // Box::into_raw never returns null; if it somehow did, treat as OOM.
        let ptr = NonNull::new(Box::into_raw(msg))?;
        // SAFETY: ptr is non-null and was just produced by Box::into_raw.
        Some(unsafe { MessageRef::from_non_null(ptr) })
    }
}

// ============================================================================
// FFI 导出函数
// ============================================================================

/// FFI: 创建消息队列
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ipc_msgq_create(perm: i32) -> IpcId {
    let ns = super::IPC_NAMESPACE.get_mut();
    let next_id = super::NEXT_IPC_ID.get_mut();
    let pid = process_get_current_pid();
    // DECISION-K: 未注册降级 (返回 0=无效 id, 逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_msgq_create");
        return 0;
    };
    s.msgq_create(ns, next_id, perm, pid).unwrap_or(0)
}

/// FFI: 发送消息。
///
/// # Safety
/// `data` 必须是有效可读指针, 至少 `size` 字节, 内存必须在调用期间保持有效。
/// 由 `sys_msgsnd` 分发, cred 校验已通过。
#[unsafe(no_mangle)]
// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn ipc_msgq_send(id: IpcId, type_: u64, data: *const u8, size: u64) -> i32 {
    let ns = super::IPC_NAMESPACE.get_mut();
    let pid = process_get_current_pid();

    let slice = if data.is_null() || size == 0 {
        None
    } else {
        // SAFETY: caller guarantees data is valid for size bytes in user memory.
        let user_data = unsafe { UserReadPtr::new(data, size as usize) };
        Some(user_data)
    };

    // 将 `UserReadPtr` 转换为 `Option<&[u8]>` 以适配 safe API
    let data_slice = slice
        .as_ref()
        .map(super::super::userptr::UserReadPtr::as_slice);

    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_msgq_send");
        return -(Errno::ENOSYS as i32);
    };
    match s.msgq_send(ns, id, type_, data_slice, size as usize, pid) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// FFI: 接收消息。
///
/// # Safety
/// `data` 必须是有效可写指针, 至少 `size` 字节; `type_out` 用于返回消息类型。
/// 由 `sys_msgrcv` 分发, cred 校验已通过。
#[unsafe(no_mangle)]
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
pub unsafe extern "C" fn ipc_msgq_recv(
    id: IpcId,
    type_out: *mut u64,
    data: *mut u8,
    size_out: *mut u64,
) -> i64 {
    let ns = super::IPC_NAMESPACE.get_mut();

    let mut type_opt = if type_out.is_null() {
        None
    } else {
        // SAFETY: caller guarantees type_out is a valid pointer to u64 in user memory.
        let out = unsafe { UserRefMut::<u64>::new(type_out) };
        Some(out)
    };

    let mut data_opt = if data.is_null() {
        None
    } else {
        // SAFETY: caller guarantees data is valid for MSG_MAX_SIZE bytes in user memory.
        let buf = unsafe { UserWritePtr::new(data, MSG_MAX_SIZE) };
        Some(buf)
    };

    let mut size_opt = if size_out.is_null() {
        None
    } else {
        // SAFETY: caller guarantees size_out is a valid pointer to u64 in user memory.
        let out = unsafe { UserRefMut::<u64>::new(size_out) };
        Some(out)
    };

    // 将 privileged 包装器转换为安全 Rust 类型
    let type_ref = type_opt
        .as_mut()
        .map(super::super::userptr::UserRefMut::as_mut);
    let data_ref = data_opt
        .as_mut()
        .map(super::super::userptr::UserWritePtr::as_mut_slice);
    let size_ref = size_opt
        .as_mut()
        .map(super::super::userptr::UserRefMut::as_mut);

    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_msgq_recv");
        return -(Errno::ENOSYS as i64);
    };
    s.msgq_recv(ns, id, type_ref, data_ref, size_ref)
        .map_or(-1, |n| n as i64)
}

/// FFI: 销毁消息队列
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ipc_msgq_destroy(id: IpcId) -> i32 {
    let ns = super::IPC_NAMESPACE.get_mut();
    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_msgq_destroy");
        return -(Errno::ENOSYS as i32);
    };
    match s.msgq_destroy(ns, id) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
