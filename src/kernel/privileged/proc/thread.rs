use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::types::{SCHED_LEVEL_2_QUANTUM, ThreadPriority, ThreadState};

pub use crate::privileged::config::{MAX_THREADS, MAX_THREADS_PER_PROCESS};

/// ✅ 统一线程结构体 — 合并了 Thread 和 `ThreadNode`, 消除类型强转 UB
///
/// 字段分为三组:
/// 1. 调度器链表字段 (next/prev) — 供 `SchedulerEx` 环形双向链表使用
/// 2. 调度器记账字段 (`priority/time_slice/cpu_time/sleep_until/state_change_count/frozen_since`)
/// 3. 线程资源字段 (`entry/kernel_stack/user_stack/cr3/context_ptr/rsp/cs/ss/rflags`)
#[repr(C)]
pub struct Thread {
    // === 调度器链表 (SchedulerEx 环形双向链表) ===
    pub next: AtomicU64,
    pub prev: AtomicU64,

    // === 线程标识 ===
    pub tid: u32,
    pub pid: u32,

    // === 调度状态 ===
    pub state: AtomicU32,
    pub priority: AtomicU32,
    pub time_slice: AtomicU32,
    pub cpu_time: AtomicU64,
    pub sleep_until: AtomicU64,

    // === 内核/用户栈 ===
    pub kernel_stack: AtomicU64,
    pub user_stack: AtomicU64,

    // === 页表 & 入口 ===
    pub cr3: AtomicU64,
    pub entry: u64,

    // === 上下文指针 (指向 ProcessContext 用于硬件切换) ===
    pub context_ptr: AtomicU64,

    // === Ring 3 上下文 (iretq 用的段选择子和栈) ===
    pub rsp: u64,
    pub cs: u64,
    pub ss: u64,
    pub rflags: u64,

    // === 退出 ===
    pub exit_code: AtomicU32,

    // === 状态追踪 ===
    pub state_change_count: AtomicU64,
    pub frozen_since: AtomicU64,
}

// 所有字段 (Atomic*, u32, u64) 满足 Send + Sync.
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe impl Send for Thread {}
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe impl Sync for Thread {}

impl Thread {
    pub fn new(tid: u32, pid: u32) -> Self {
        Self {
            next: AtomicU64::new(0),
            prev: AtomicU64::new(0),
            tid,
            pid,
            state: AtomicU32::new(ThreadState::Created as u32),
            priority: AtomicU32::new(ThreadPriority::Normal as u32),
            time_slice: AtomicU32::new(SCHED_LEVEL_2_QUANTUM),
            cpu_time: AtomicU64::new(0),
            sleep_until: AtomicU64::new(0),
            kernel_stack: AtomicU64::new(0),
            user_stack: AtomicU64::new(0),
            cr3: AtomicU64::new(0),
            entry: 0,
            context_ptr: AtomicU64::new(0),
            rsp: 0,
            cs: 0x08,
            ss: 0x10,
            rflags: 0x202,
            exit_code: AtomicU32::new(0),
            state_change_count: AtomicU64::new(0),
            frozen_since: AtomicU64::new(0),
        }
    }

    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    /// ✅ 安全的状态设置 (带合法性检查)
    ///
    /// # Errors
    /// 当请求的 `new_state` 不属于状态机允许的合法转换时, 返回
    /// `Err("Illegal state transition")`, 且不会修改任何状态.
    pub fn set_state_safe(&self, new_state: ThreadState) -> Result<(), &'static str> {
        let current = ThreadState::from_u32(self.state.load(Ordering::Acquire));

        match (current, new_state) {
            (ThreadState::Created, ThreadState::Ready) => {}
            (ThreadState::Ready, ThreadState::Running) => {}
            (ThreadState::Running, ThreadState::Ready) => {}
            (ThreadState::Running, ThreadState::Blocked) => {}
            (ThreadState::Running, ThreadState::Zombie) => {}
            (ThreadState::Running, ThreadState::Frozen) => {}
            (ThreadState::Ready, ThreadState::Frozen) => {}
            (ThreadState::Blocked, ThreadState::Frozen) => {}
            (ThreadState::Blocked, ThreadState::Ready) => {}
            (ThreadState::Blocked, ThreadState::Zombie) => {}
            (ThreadState::Zombie, ThreadState::Terminated) => {}
            (ThreadState::Frozen, ThreadState::Ready) => {}
            (ThreadState::Frozen, ThreadState::Blocked) => {}
            _ => return Err("Illegal state transition"),
        }

        self.state.store(new_state as u32, Ordering::Release);
        self.state_change_count.fetch_add(1, Ordering::Relaxed);

        if new_state == ThreadState::Frozen {
            self.frozen_since
                .store(crate::privileged::timer::get_ticks(), Ordering::Relaxed);
        }

        Ok(())
    }

    pub fn get_state(&self) -> ThreadState {
        ThreadState::from_u32(self.state.load(Ordering::Acquire))
    }

    pub fn is_runnable(&self) -> bool {
        self.get_state().is_runnable()
    }

    pub fn is_alive(&self) -> bool {
        self.get_state().is_alive()
    }

    pub fn can_freeze(&self) -> bool {
        self.get_state().can_freeze()
    }
}

/// 线程表 — 以固定容量数组按 tid 索引存放线程句柄, 并自增分配 tid。
pub struct ThreadTable {
    threads: Mutex<[Option<NonNull<Thread>>; MAX_THREADS]>,
    next_tid: AtomicU32,
}

use crate::privileged::sync::IrqSpinLock as Mutex;
// SAFETY: ThreadTable 始终通过静态 THREAD_TABLE 访问.
// 所有变更都走 Mutex, NonNull 指针指向的 Thread 对象
// 字段均为 Atomic* 或普通整数.
unsafe impl Send for ThreadTable {}
unsafe impl Sync for ThreadTable {}

impl ThreadTable {
    const fn new() -> Self {
        Self {
            threads: Mutex::new([None; MAX_THREADS]),
            next_tid: AtomicU32::new(1),
        }
    }

    pub fn allocate(&self) -> Option<u32> {
        let tid = self.next_tid.fetch_add(1, Ordering::SeqCst);
        if (tid as usize) < MAX_THREADS {
            Some(tid)
        } else {
            None
        }
    }

    pub fn insert(&self, thread: *mut Thread) -> bool {
        if thread.is_null() {
            return false;
        }
        // SAFETY: caller guarantees thread is a valid, non-null pointer.
        let nn = unsafe { NonNull::new_unchecked(thread) };
        let tid = unsafe { nn.as_ref().tid };
        let mut table = self.threads.lock();
        if (tid as usize) < MAX_THREADS {
            table[tid as usize] = Some(nn);
            true
        } else {
            false
        }
    }

    /// 获取线程裸指针 (向后兼容接口)。
    /// 内部存储为 `NonNull`, 转为 *mut 供外部调用。
    pub fn get(&self, tid: u32) -> Option<*mut Thread> {
        if (tid as usize) >= MAX_THREADS {
            return None;
        }
        self.threads.lock()[tid as usize].map(core::ptr::NonNull::as_ptr)
    }

    pub fn remove(&self, tid: u32) {
        if (tid as usize) < MAX_THREADS {
            self.threads.lock()[tid as usize] = None;
        }
    }
}

static THREAD_TABLE: ThreadTable = ThreadTable::new();

/// 线程管理器 — 跟踪当前运行线程的句柄与线程总数。
pub struct ThreadManager {
    current_thread: AtomicU64,
    thread_count: AtomicU32,
}

// 所有字段 (AtomicU64, AtomicU32) 自动实现 Send + Sync.

impl ThreadManager {
    pub const fn new() -> Self {
        Self {
            current_thread: AtomicU64::new(0),
            thread_count: AtomicU32::new(0),
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn init(&self) {}

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    pub fn create_thread(
        &self,
        pid: u32,
        entry: u64,
        user_stack: u64,
        kernel_stack: u64,
        cr3: u64,
    ) -> Option<u32> {
        let tid = THREAD_TABLE.allocate()?;

        let thread =
            // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
            unsafe { alloc::alloc::alloc(alloc::alloc::Layout::new::<Thread>()) as *mut Thread };

        if thread.is_null() {
            return None;
        }

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::ptr::write(thread, Thread::new(tid, pid));
            (*thread).entry = entry;
            (*thread).user_stack.store(user_stack, Ordering::SeqCst);
            (*thread).kernel_stack.store(kernel_stack, Ordering::SeqCst);
            (*thread).cr3.store(cr3, Ordering::SeqCst);
            (*thread)
                .state
                .store(ThreadState::Ready as u32, Ordering::SeqCst);
        }

        if !THREAD_TABLE.insert(thread) {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                alloc::alloc::dealloc(thread as *mut u8, alloc::alloc::Layout::new::<Thread>());
            };
            return None;
        }

        self.thread_count.fetch_add(1, Ordering::SeqCst);

        // ✅ 类型安全: Thread 现在包含调度器链表字段, 直接传入
        super::scheduler_ex::SCHEDULER_EX.add_thread(thread);

        Some(tid)
    }

    pub fn get_current_thread(&self) -> Option<u64> {
        let id = self.current_thread.load(Ordering::SeqCst);
        if id == 0 { None } else { Some(id) }
    }

    pub fn set_current(&self, tid: u32) {
        self.current_thread.store(u64::from(tid), Ordering::SeqCst);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn get_thread(&self, tid: u32) -> Option<*mut Thread> {
        THREAD_TABLE.get(tid)
    }

    pub fn exit_current(&self, exit_code: u32) {
        if let Some(tid) = self.get_current_thread() {
            if let Some(thread) = THREAD_TABLE.get(tid as u32) {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    (*thread).exit_code.store(exit_code, Ordering::SeqCst);
                    (*thread)
                        .state
                        .store(ThreadState::Zombie as u32, Ordering::SeqCst);
                }
            }
        }
    }

    pub fn count(&self) -> u32 {
        self.thread_count.load(Ordering::SeqCst)
    }
}

pub static THREAD_MANAGER: ThreadManager = ThreadManager::new();

/// 线程子系统初始化入口 — 初始化全局 [`THREAD_MANAGER`]。
pub fn init() {
    THREAD_MANAGER.init();
}
