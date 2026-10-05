//! 动态 IPC 命名空间 (Dynamic IPC Namespace)
//!
//! 替换现有静态数组为 `Vec` 动态分配，消除编译期容量上限。
//!
//! ## 与旧版区别
//!
//! | 特性 | 静态 (IpcNamespace) | 动态 (DynIpcNamespace) |
//! |------|---------------------|------------------------|
//! | 管道上限 | `IPC_MAX_PIPES` (64) | 物理内存限制 |
//! | 消息队列上限 | `IPC_MAX_MSG_QUEUES` (32) | 物理内存限制 |
//! | 共享内存段上限 | `IPC_MAX_SHM_SEGS` (16) | 物理内存限制 |
//! | 信号量上限 | `IPC_MAX_SEMAPHORES` (64) | 物理内存限制 |
//! | 扩容方式 | 不可扩容 | 按需分配 |
//!
//! ## 迁移策略
//!
//! 旧版 `IpcNamespace` 保持不变（向后兼容）。
//! 新代码使用 `DynIpcNamespace`，通过 FFI 桥接暴露给 C。

use super::types::{
    IpcId, Message, MsgQueue, Pipe, SHM_MAX_SIZE, Semaphore, ShmSegment, WaitQueue,
};
use crate::privileged::mm::PAGE_SIZE;
use crate::privileged::sync::IrqSpinLock as Mutex;
use alloc::vec::Vec;
use core::ptr::NonNull;
use core::sync::atomic::AtomicPtr;

/// === Message 原始指针特权封装 (Framekernel 模式) ===
///
/// 与 `msgq.rs` 中的 `MessageRef` 同样思路: 把侵入式链表的 `NonNull`
/// 操作 (`Box::from_raw` / `*msg_nn.as_ptr()`) 集中到 `raw` 子模块,
/// 业务代码 (destroy 释放) 走安全接口。
pub(crate) mod raw {
    use super::Message;
    use alloc::boxed::Box;
    use core::ptr::NonNull;

    /// `NonNull<Message>` 的安全 newtype
    #[derive(Clone, Copy)]
    pub struct MessageRef(NonNull<Message>);

    impl MessageRef {
        /// # Safety (内部)
        /// nn 必须是 `Box::into_raw` 产生的有效 `NonNull`。
        pub(crate) unsafe fn from_non_null(nn: NonNull<Message>) -> Self {
            Self(nn)
        }

        /// 读取 `next` 字段 (侵入式链表) — 返回裸指针 (null = 无后继)
        pub fn next(&self) -> *mut Message {
            // SAFETY: self 来自 `from_non_null`, 指向有效 Message。
            // B03-24: Message.next 为 AtomicPtr, 队列操作在持锁下执行, Relaxed 足够.
            unsafe {
                (*self.0.as_ptr())
                    .next
                    .load(core::sync::atomic::Ordering::Relaxed)
            }
        }

        /// 通过 `Box::from_raw` 释放, 触发 Drop
        pub fn free(self) {
            // SAFETY: self 来自 `Box::into_raw`。
            let _ = unsafe { Box::from_raw(self.0.as_ptr()) };
        }
    }
}

use crate::privileged::racy_cell::RacyCell;
use raw::MessageRef;

/// 动态 IPC 命名空间, 持有管道/共享内存/消息队列/信号量集合与 ID 分配器.
pub struct DynIpcNamespace {
    pub pipes: Mutex<Vec<Pipe>>,
    pub shm_segs: Mutex<Vec<ShmSegment>>,
    pub msg_queues: Mutex<Vec<MsgQueue>>,
    pub semaphores: Mutex<Vec<Semaphore>>,
    pub next_id: Mutex<IpcId>,
}

impl DynIpcNamespace {
    pub fn new() -> Self {
        Self {
            pipes: Mutex::new(Vec::new()),
            shm_segs: Mutex::new(Vec::new()),
            msg_queues: Mutex::new(Vec::new()),
            semaphores: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
        }
    }

    fn allocate_id(&self) -> IpcId {
        let mut next = self.next_id.lock();
        let id = *next;
        *next = id.wrapping_add(1);
        id
    }

    // ─── Pipe ────────────────────────────────────────────────

    pub fn pipe_create(&self, read_pid: u32, write_pid: u32) -> IpcId {
        let id = self.allocate_id();
        let mut pipe = Pipe::new();
        pipe.id = id;
        pipe.read_pid = read_pid;
        pipe.write_pid = write_pid;
        pipe.readers = 1;
        pipe.writers = 1;
        self.pipes.lock().push(pipe);
        id
    }

    /// 销毁指定管道。
    /// # Errors
    /// 管道 ID 不存在时返回 Err。
    pub fn pipe_destroy(&self, id: IpcId) -> Result<(), i32> {
        let mut pipes = self.pipes.lock();
        let pos = pipes.iter().position(|p| p.id == id).ok_or(-1)?;
        pipes.remove(pos);
        Ok(())
    }

    pub fn pipe_exists(&self, id: IpcId) -> bool {
        self.pipes.lock().iter().any(|p| p.id == id)
    }

    pub fn pipe_count(&self) -> usize {
        self.pipes.lock().len()
    }

    // ─── Shared Memory ─────────────────────────────────────

    /// 创建共享内存段并返回其 IPC ID。
    /// # Errors
    /// 大小为 0 或超过最大限制时返回 Err, 物理页分配失败时返回 Err。
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn shm_create(&self, owner_pid: u32, size: u64) -> Result<IpcId, i32> {
        if size == 0 || size > SHM_MAX_SIZE {
            return Err(-1);
        }

        let pages = (size as usize).div_ceil(PAGE_SIZE as usize);
        let phys = crate::privileged::mm::pmm_alloc_pages(pages);
        if phys.is_null() {
            return Err(-3);
        }

        let id = self.allocate_id();
        let seg = ShmSegment {
            id,
            creator: owner_pid,
            phys_addr: phys as u64,
            size,
            perm: 0o666,
            ref_count: 0,
            attach_count: 0,
            flags: 0,
            attached_pids: [0u32; 16],
        };
        self.shm_segs.lock().push(seg);
        Ok(id)
    }

    /// 销毁指定共享内存段并释放其物理页。
    /// # Errors
    /// 共享内存段 ID 不存在时返回 Err。
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn shm_destroy(&self, id: IpcId) -> Result<(), i32> {
        let mut segs = self.shm_segs.lock();
        let pos = segs.iter().position(|s| s.id == id).ok_or(-1)?;
        let seg = segs.remove(pos);
        let pages = (seg.size as usize).div_ceil(PAGE_SIZE as usize);
        crate::privileged::mm::pmm_free_pages(seg.phys_addr as *mut u8, pages);
        Ok(())
    }

    pub fn shm_count(&self) -> usize {
        self.shm_segs.lock().len()
    }

    // ─── Message Queue ─────────────────────────────────────

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 创建消息队列并返回其 IPC ID。
    /// # Errors
    /// 队列创建失败时返回 Err。
    pub fn msgq_create(&self, owner_pid: u32, max_msgs: u32, max_size: u32) -> Result<IpcId, i32> {
        let id = self.allocate_id();
        let mq = MsgQueue {
            id,
            owner: owner_pid,
            head: AtomicPtr::new(core::ptr::null_mut()),
            tail: AtomicPtr::new(core::ptr::null_mut()),
            count: 0,
            max_msgs,
            max_size,
            send_wait: WaitQueue::new(),
            recv_wait: WaitQueue::new(),
            flags: 0,
            perm: 0o666,
        };
        self.msg_queues.lock().push(mq);
        Ok(id)
    }

    /// 销毁指定消息队列并释放其全部待处理消息。
    /// # Errors
    /// 消息队列 ID 不存在时返回 Err。
    pub fn msgq_destroy(&self, id: IpcId) -> Result<(), i32> {
        let mut queues = self.msg_queues.lock();
        let pos = queues.iter().position(|q| q.id == id).ok_or(-1)?;

        let mq = queues.remove(pos);

        let mut cur = mq.head.load(core::sync::atomic::Ordering::Relaxed);
        while !cur.is_null() {
            // SAFETY: cur 是经 Box::into_raw 分配的 Box<Message> 派生出的有效指针.
            // 消息链表只在持有 msg_queues 锁时被修改.
            let msg_nn = unsafe { NonNull::new_unchecked(cur) };
            let msg_ref = unsafe { MessageRef::from_non_null(msg_nn) };
            cur = msg_ref.next();
            // SAFETY: msg_nn 由 Box::into_raw 分配, 此处重新构造 Box 以
            // drop 释放内存.
            msg_ref.free();
        }

        Ok(())
    }

    pub fn msgq_exists(&self, id: IpcId) -> bool {
        self.msg_queues.lock().iter().any(|q| q.id == id)
    }

    pub fn msgq_count(&self) -> usize {
        self.msg_queues.lock().len()
    }

    // ─── Semaphore ─────────────────────────────────────────

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
    /// 创建信号量并返回其 IPC ID。
    /// # Errors
    /// 信号量创建失败时返回 Err。
    pub fn sem_create(&self, owner_pid: u32, count: u32, max_count: u32) -> Result<IpcId, i32> {
        let id = self.allocate_id();
        let sem = Semaphore {
            id,
            owner: owner_pid,
            count: count as i32,
            max_count,
            wait: WaitQueue::new(),
            flags: 0,
            perm: 0o666,
        };
        self.semaphores.lock().push(sem);
        Ok(id)
    }

    /// 销毁指定信号量。
    /// # Errors
    /// 信号量 ID 不存在时返回 Err。
    pub fn sem_destroy(&self, id: IpcId) -> Result<(), i32> {
        let mut sems = self.semaphores.lock();
        let pos = sems.iter().position(|s| s.id == id).ok_or(-1)?;
        sems.remove(pos);
        Ok(())
    }

    pub fn sem_count(&self) -> usize {
        self.semaphores.lock().len()
    }

    pub fn total_count(&self) -> usize {
        self.pipe_count() + self.shm_count() + self.msgq_count() + self.sem_count()
    }
}

/// 动态 IPC 命名空间全局实例
///
/// 使用 `RacyCell` 提供安全访问，替代 `static mut`。
/// 在内核启动单线程阶段通过 `dyn_ipc_init()` 初始化，
/// 之后只读访问，无需额外同步。
static DYN_IPC: RacyCell<Option<DynIpcNamespace>> = RacyCell::new(None);

fn dyn_ipc_init_impl() {
    // SAFETY: 单线程启动路径; 一次性初始化.
    // RacyCell::get_mut() 在此安全, 因启动期在并发访问前, 调用方保证独占访问.
    *DYN_IPC.get_mut() = Some(DynIpcNamespace::new());
}

/// 获取全局动态 IPC 命名空间的只读引用 (要求已由 `dyn_ipc_init` 初始化).
pub fn get_dyn_ipc() -> &'static DynIpcNamespace {
    // SAFETY 集中在 privileged::RacyCell::get_ref 内部;
    // 调用方保证 DYN_IPC 在 dyn_ipc_init() 中初始化, 此后只读。
    DYN_IPC.get_ref()
}

#[unsafe(no_mangle)]
pub extern "C" fn dyn_ipc_init() {
    dyn_ipc_init_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn dyn_ipc_pipe_create(read_pid: u32, write_pid: u32) -> u32 {
    get_dyn_ipc().pipe_create(read_pid, write_pid)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn dyn_ipc_pipe_destroy(id: u32) -> i32 {
    match get_dyn_ipc().pipe_destroy(id) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn dyn_ipc_msgq_create(owner_pid: u32, max_msgs: u32, max_size: u32) -> u32 {
    get_dyn_ipc()
        .msgq_create(owner_pid, max_msgs, max_size)
        .unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn dyn_ipc_shm_create(owner_pid: u32, size: u64) -> u32 {
    get_dyn_ipc().shm_create(owner_pid, size).unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn dyn_ipc_sem_create(owner_pid: u32, count: u32, max_count: u32) -> u32 {
    get_dyn_ipc()
        .sem_create(owner_pid, count, max_count)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dyn_pipe_no_limit() {
        let ns = DynIpcNamespace::new();

        let mut ids = Vec::new();
        for _ in 0..200 {
            let id = ns.pipe_create(1000, 2000);
            assert_ne!(id, 0);
            ids.push(id);
        }
        assert_eq!(ids.len(), 200);
        assert_eq!(ns.pipe_count(), 200);

        for id in ids {
            ns.pipe_destroy(id).unwrap();
        }
        assert_eq!(ns.pipe_count(), 0);
    }

    #[test]
    fn test_dyn_msgq_growth() {
        let ns = DynIpcNamespace::new();

        for _ in 0..100 {
            let id = ns.msgq_create(1000, 64, 4096).unwrap();
            assert!(ns.msgq_exists(id));
            assert_eq!(ns.msgq_count(), 1);
            ns.msgq_destroy(id).unwrap();
            assert_eq!(ns.msgq_count(), 0);
        }
    }

    // UT-06 (2026-09-24): test_dyn_shm_alloc_and_free 已迁出 — shm_create 依赖
    // 裸机 PMM (host 下 panic); 等价断言迁至 privileged/tests/test_ipc.rs
    // (register_ipc_tests 注册), 在 kernel_test (QEMU 裸机) 执行.
}
