use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::thread::Thread;
pub use super::types::{
    SCHED_BOOST_INTERVAL, SCHED_LEVEL_0_QUANTUM, SCHED_LEVEL_1_QUANTUM, SCHED_LEVEL_2_QUANTUM,
    SCHED_LEVEL_3_QUANTUM, SCHED_RT_WATCHDOG_TICKS, ThreadPriority, ThreadState,
};

// === 特权层: 线程裸指针安全访问封装 ===
//
// 该子模块是 scheduler_ex.rs 内 `unsafe` 集中地。所有 `unsafe { (*ptr).field }`
// 形式只在 `raw` 子模块内出现, 本模块的其余业务逻辑 (RunQueue/SchedulerEx)
// 全部为安全 Rust, 调用 `raw::ThreadRef` 的安全方法。
//
// 这等价于 framework 特权层模式: unsafe 集中在 inner module,
// 外层接口是 100% safe Rust。
pub(crate) mod raw {
    use super::{Ordering, Thread, ThreadState};

    // === 线程安全访问封装 (Framekernel privilege wrapper) ===
    //
    // `*mut Thread` 在调度器中作为侵入式链表指针使用。将其封装为 `ThreadRef`
    // newtype 后, 所有 `unsafe { (*ptr).field }` 集中在 `ThreadRef` 的内部方法中。
    //
    // # SAFETY invariant
    // - 调用方必须保证 `*mut Thread` 指向一个有效的 `Thread` 分配 (alloc::alloc::alloc)。
    // - 同一时刻只有一个调度器可以持有指向该 `Thread` 的引用 (调度器串行化)。
    #[derive(Clone, Copy)]
    pub struct ThreadRef(*mut Thread);

    impl ThreadRef {
        /// 从裸指针构造, 要求调用方提供 SAFETY 保证。
        ///
        /// # Safety
        /// - `ptr` 必须为非空, 指向有效 `Thread` 分配
        /// - 在 `ThreadRef` 存活期间, 不会被释放
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub unsafe fn new_unchecked(ptr: *mut Thread) -> Self {
            Self(ptr)
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn as_ptr(self) -> *mut Thread {
            self.0
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn is_null(self) -> bool {
            self.0.is_null()
        }

        /// 获取/设置 next 链指针
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn next(&self) -> *mut Thread {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读 next 链指针 (Acquire 同步后续访问)
            unsafe { (*self.0).next.load(Ordering::Acquire) as *mut Thread }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_next(&self, p: *mut Thread) {
            // SAFETY: `self` 由 new_unchecked 保证有效, 写 next 链指针 (Release 同步可见性)
            unsafe { (*self.0).next.store(p as u64, Ordering::Release) };
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn prev(&self) -> *mut Thread {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读 prev 链指针 (Acquire 同步后续访问)
            unsafe { (*self.0).prev.load(Ordering::Acquire) as *mut Thread }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_prev(&self, p: *mut Thread) {
            // SAFETY: `self` 由 new_unchecked 保证有效, 写 prev 链指针 (Release 同步可见性)
            unsafe { (*self.0).prev.store(p as u64, Ordering::Release) };
        }

        /// 加载/存储/修改 调度状态字段
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn get_state(&self) -> ThreadState {
            // SAFETY: `self` 由 new_unchecked 保证有效, get_state 内部原子加载 state
            unsafe { (*self.0).get_state() }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn set_state(&self, s: ThreadState) -> Result<(), &'static str> {
            // SAFETY: `self` 由 new_unchecked 保证有效, set_state_safe 内部校验合法转换
            unsafe { (*self.0).set_state_safe(s) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn load_state_raw(&self) -> u32 {
            // SAFETY: `self` 由 new_unchecked 保证有效, 原子读 state.u32 表示
            unsafe { (*self.0).state.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_state(&self, v: u32) {
            // SAFETY: `self` 由 new_unchecked 保证有效, 原子写 state (调试/测试路径)
            unsafe { (*self.0).state.store(v, Ordering::SeqCst) };
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn priority_raw(&self) -> u32 {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读 priority (Acquire 同步优先级变化)
            unsafe { (*self.0).priority.load(Ordering::Acquire) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_priority(&self, v: u32) {
            // SAFETY: `self` 由 new_unchecked 保证有效, 写 priority (SeqCst 跨 CPU 强一致)
            unsafe { (*self.0).priority.store(v, Ordering::SeqCst) };
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn time_slice(&self) -> u32 {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读时间片剩余
            unsafe { (*self.0).time_slice.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn fetch_sub_time_slice(&self) -> u32 {
            // SAFETY: `self` 由 new_unchecked 保证有效, RMW 减少时间片 (SeqCst 保证全局一致)
            unsafe { (*self.0).time_slice.fetch_sub(1, Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_time_slice(&self, v: u32) {
            // SAFETY: `self` 由 new_unchecked 保证有效, 重置时间片 (用于线程唤醒/优先级提升)
            unsafe { (*self.0).time_slice.store(v, Ordering::SeqCst) };
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn fetch_add_cpu_time(&self) -> u64 {
            // SAFETY: `self` 由 new_unchecked 保证有效, RMW 累计 CPU 时间 (调度统计)
            unsafe { (*self.0).cpu_time.fetch_add(1, Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn load_sleep_until(&self) -> u64 {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读 sleep_until 截止时间戳
            unsafe { (*self.0).sleep_until.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn store_sleep_until(&self, v: u64) {
            // SAFETY: `self` 由 new_unchecked 保证有效, 写 sleep_until (用于睡眠/唤醒)
            unsafe { (*self.0).sleep_until.store(v, Ordering::SeqCst) };
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn kernel_stack(&self) -> u64 {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读内核栈顶 (上下文切换关键)
            unsafe { (*self.0).kernel_stack.load(Ordering::SeqCst) }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn context_ptr(&self) -> *mut super::super::types::ProcessContext {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读 context_ptr (上下文切换关键)
            unsafe {
                (*self.0).context_ptr.load(Ordering::SeqCst)
                    as *mut super::super::types::ProcessContext
            }
        }

        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn tid(&self) -> u32 {
            // SAFETY: `self` 由 new_unchecked 保证有效, 读 tid 不可变字段 (alloc 时已固化)
            unsafe { (*self.0).tid }
        }

        /// 检查线程是否可冻结 (不在 Running/Zombie 状态)
        #[inline(always)]
        #[expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )]
        pub fn can_freeze(&self) -> bool {
            // SAFETY: `self` 由 new_unchecked 保证有效, can_freeze 内部读 state
            unsafe { (*self.0).can_freeze() }
        }

        /// 写入退出码
        #[inline(always)]
        pub fn store_exit_code(&self, code: u32) {
            // SAFETY: `self` 由 new_unchecked 保证有效, 写 exit_code 供父进程 waitpid 读取
            unsafe { (*self.0).exit_code.store(code, Ordering::SeqCst) };
        }
    }
}

use raw::ThreadRef;

// === 环形双向就绪队列 ===
pub struct RunQueue {
    head: AtomicU64,
    count: AtomicU32,
}

// 所有字段 (AtomicU64, AtomicU32) 自动实现 Send + Sync.

impl RunQueue {
    const fn new() -> Self {
        Self {
            head: AtomicU64::new(0),
            count: AtomicU32::new(0),
        }
    }

    /// 添加到队列尾部
    pub fn push_back(&self, thread: *mut Thread) {
        if thread.is_null() {
            return;
        }
        // SAFETY: 调用方保证 thread 有效, 持续到本函数返回
        let t = unsafe { ThreadRef::new_unchecked(thread) };
        let head_ptr = self.head.load(Ordering::Acquire);
        if head_ptr == 0 {
            t.set_next(thread);
            t.set_prev(thread);
            self.head.store(thread as u64, Ordering::Release);
        } else {
            // SAFETY: head_ptr 由本队列管理, 始终指向有效 Thread
            let head = unsafe { ThreadRef::new_unchecked(head_ptr as *mut Thread) };
            let tail_ptr = head.prev();
            // SAFETY: tail_ptr 与 head 互相指向有效 Thread
            let tail = unsafe { ThreadRef::new_unchecked(tail_ptr) };
            t.set_next(head_ptr as *mut Thread);
            t.set_prev(tail_ptr);
            tail.set_next(thread);
            head.set_prev(thread);
        }
        self.count.fetch_add(1, Ordering::Release);
    }

    /// 从队列头部取出
    pub fn pop_front(&self) -> Option<*mut Thread> {
        let head_ptr = self.head.load(Ordering::Acquire) as *mut Thread;
        if head_ptr.is_null() {
            return None;
        }
        // SAFETY: head_ptr 由本队列管理, 始终有效
        let head = unsafe { ThreadRef::new_unchecked(head_ptr) };
        let count = self.count.load(Ordering::Acquire);
        let result = if count == 1 {
            self.head.store(0, Ordering::Release);
            head.set_next(core::ptr::null_mut());
            head.set_prev(core::ptr::null_mut());
            Some(head_ptr)
        } else {
            let new_head_ptr = head.next();
            let tail_ptr = head.prev();
            if !new_head_ptr.is_null() {
                // SAFETY: 由环形链表结构保证
                let new_head = unsafe { ThreadRef::new_unchecked(new_head_ptr) };
                let tail = unsafe { ThreadRef::new_unchecked(tail_ptr) };
                new_head.set_prev(tail_ptr);
                tail.set_next(new_head_ptr);
                head.set_next(core::ptr::null_mut());
                head.set_prev(core::ptr::null_mut());
                self.head.store(new_head_ptr as u64, Ordering::Release);
            }
            Some(head_ptr)
        };

        if result.is_some() {
            self.count.fetch_sub(1, Ordering::Release);
        }
        result
    }

    /// 从队列中移除指定线程
    pub fn remove(&self, thread: *mut Thread) -> bool {
        if thread.is_null() {
            return false;
        }
        // SAFETY: 调用方保证 thread 有效, push_back 串行
        let t = unsafe { ThreadRef::new_unchecked(thread) };
        let head_ptr = self.head.load(Ordering::Acquire) as *mut Thread;
        if head_ptr.is_null() {
            return false;
        }
        // SAFETY: head_ptr 由本队列管理
        let _head = unsafe { ThreadRef::new_unchecked(head_ptr) };
        let count = self.count.load(Ordering::Acquire);
        let addr = thread as u64;

        if count == 1 {
            if head_ptr as u64 == addr {
                self.head.store(0, Ordering::Release);
                t.set_next(core::ptr::null_mut());
                t.set_prev(core::ptr::null_mut());
                self.count.fetch_sub(1, Ordering::Release);
                return true;
            }
            return false;
        }

        // 遍历链表查找
        let mut current = head_ptr;
        for _ in 0..count {
            if current as u64 == addr {
                // SAFETY: 闭环链表节点
                let prev = t.prev();
                let next = t.next();
                if !prev.is_null() {
                    let p = unsafe { ThreadRef::new_unchecked(prev) };
                    p.set_next(next);
                }
                if !next.is_null() {
                    let n = unsafe { ThreadRef::new_unchecked(next) };
                    n.set_prev(prev);
                }
                if head_ptr as u64 == addr {
                    self.head.store(next as u64, Ordering::Release);
                }
                t.set_next(core::ptr::null_mut());
                t.set_prev(core::ptr::null_mut());
                self.count.fetch_sub(1, Ordering::Release);
                return true;
            }
            // SAFETY: 链表内 next 节点有效
            let n = unsafe { ThreadRef::new_unchecked(current) };
            let n_next = n.next();
            if n_next == head_ptr {
                break;
            } // 循环一周
            current = n_next;
        }

        false
    }

    #[expect(
        clippy::iter_without_into_iter,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    pub fn iter(&self) -> RunQueueIter {
        RunQueueIter {
            head: self.head.load(Ordering::Acquire) as *mut Thread,
            current: 0,
            count: self.count.load(Ordering::Acquire),
            visited: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count.load(Ordering::Acquire) == 0
    }

    pub fn len(&self) -> u32 {
        self.count.load(Ordering::Acquire)
    }
}

pub struct RunQueueIter {
    head: *mut Thread,
    current: u64,
    count: u32,
    visited: u32,
}

impl Iterator for RunQueueIter {
    type Item = *mut Thread;

    fn next(&mut self) -> Option<Self::Item> {
        if self.visited >= self.count || self.head.is_null() {
            return None;
        }

        let item = if self.current == 0 {
            self.head as u64
        } else {
            self.current
        };

        let t = item as *mut Thread;
        if t.is_null() {
            return None;
        }

        // SAFETY: t 来自环形链表, 调度器串行访问, 节点有效
        let tr = unsafe { ThreadRef::new_unchecked(t) };
        self.current = tr.next() as u64;
        self.visited += 1;

        Some(t)
    }
}

// === 调度器统计 ===
pub struct SchedulerStats {
    pub total_switches: AtomicU64,
    pub total_ticks: AtomicU64,
    pub frozen_count: AtomicU32,
    pub zombie_reaped: AtomicU64,
    pub priority_boosts: AtomicU64,
}

impl SchedulerStats {
    const fn new() -> Self {
        Self {
            total_switches: AtomicU64::new(0),
            total_ticks: AtomicU64::new(0),
            frozen_count: AtomicU32::new(0),
            zombie_reaped: AtomicU64::new(0),
            priority_boosts: AtomicU64::new(0),
        }
    }
}

// === 线程级调度器 (SchedulerEx) ===
pub struct SchedulerEx {
    pub run_queues: [RunQueue; 5],
    pub frozen_queue: RunQueue,
    pub current: AtomicU64,
    pub idle_thread: AtomicU64,
    pub stats: SchedulerStats,
    pub tick_count: AtomicU64,
    pub last_boost: AtomicU64,
    pub need_reschedule: AtomicU64,
    pub rt_watchdog: AtomicU64,
    pub is_frozen_global: AtomicBool,
}

// 所有字段 (RunQueue 包含 Atomic*, AtomicU64, AtomicBool, 普通统计) 自动实现 Send + Sync.

impl SchedulerEx {
    pub const fn new() -> Self {
        Self {
            run_queues: [
                RunQueue::new(),
                RunQueue::new(),
                RunQueue::new(),
                RunQueue::new(),
                RunQueue::new(),
            ],
            frozen_queue: RunQueue::new(),
            current: AtomicU64::new(0),
            idle_thread: AtomicU64::new(0),
            stats: SchedulerStats::new(),
            tick_count: AtomicU64::new(0),
            last_boost: AtomicU64::new(0),
            need_reschedule: AtomicU64::new(0),
            rt_watchdog: AtomicU64::new(0),
            is_frozen_global: AtomicBool::new(false),
        }
    }

    fn queue_idx(priority: ThreadPriority) -> usize {
        priority as usize
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    pub fn init(&self) {
        // SAFETY: 分配 0 号 (idle) Thread, 立即写入有效值
        let idle = unsafe {
            let layout = alloc::alloc::Layout::new::<Thread>();
            let raw = alloc::alloc::alloc(layout) as *mut Thread;
            core::ptr::write(raw, Thread::new(0, 0));
            raw
        };
        if !idle.is_null() {
            // SAFETY: idle 由本调用方才分配的 alloc, 必有效
            let idle_ref = unsafe { ThreadRef::new_unchecked(idle) };
            idle_ref.store_state(ThreadState::Ready as u32);
            idle_ref.store_priority(ThreadPriority::Idle as u32);
            idle_ref.store_time_slice(u32::MAX);
            self.idle_thread.store(idle as u64, Ordering::SeqCst);
            self.current.store(idle as u64, Ordering::SeqCst);
            self.run_queues[ThreadPriority::Idle as usize].push_back(idle);
        }
    }

    /// ✅ 添加线程到就绪队列 (类型安全: 直接使用 Thread)
    pub fn add_thread(&self, thread: *mut Thread) {
        if thread.is_null() {
            return;
        }

        // SAFETY: 调用方保证 thread 有效, push_back 串行
        let t = unsafe { ThreadRef::new_unchecked(thread) };
        let state = t.get_state();
        if state == ThreadState::Ready || state == ThreadState::Created {
            t.store_state(ThreadState::Ready as u32);
            let prio = ThreadPriority::from_u32(t.priority_raw());
            let idx = Self::queue_idx(prio);
            self.run_queues[idx].push_back(thread);
        }
    }

    /// 从就绪队列中按策略取出最高优先级线程
    fn pop_highest(&self) -> Option<*mut Thread> {
        let lengths = [
            self.run_queues[0].len(),
            self.run_queues[1].len(),
            self.run_queues[2].len(),
            self.run_queues[3].len(),
            self.run_queues[4].len(),
        ];
        let prio = super::sched_trait::current_sched_decision().pick_next_priority(lengths)?;
        self.run_queues[prio].pop_front()
    }

    /// ✅ 纯记账 tick (不做调度决策)
    pub fn tick_accounting(&self) {
        self.tick_count.fetch_add(1, Ordering::SeqCst);
        self.stats.total_ticks.fetch_add(1, Ordering::SeqCst);

        let current = self.current.load(Ordering::SeqCst);
        if current != 0 {
            // SAFETY: current 由调度器自管理, 必指向有效 Thread
            let thread = unsafe { ThreadRef::new_unchecked(current as *mut Thread) };
            let time_slice = thread.fetch_sub_time_slice();
            thread.fetch_add_cpu_time();

            // TD-10: 按 in_kern 状态记账到 user_time / sys_time.
            // user_time 持续累加, sys_time 仅在 syscall 期间累加.
            let in_kern = crate::framework::proc::proc_get_in_kern();
            crate::framework::proc::proc_account_tick(in_kern);

            let sleep_until = thread.load_sleep_until();
            if sleep_until != 0 {
                let ticks = crate::framework::timer::get_ticks();
                if ticks >= sleep_until {
                    thread.store_sleep_until(0);
                    let _ = thread.set_state(ThreadState::Ready);
                }
            }

            if super::sched_trait::current_sched_decision().should_reschedule(time_slice) {
                self.need_reschedule.store(1, Ordering::SeqCst);
            }
        }

        let tick_count = self.tick_count.load(Ordering::SeqCst);
        let last_boost = self.last_boost.load(Ordering::SeqCst);
        if super::sched_trait::current_sched_decision().should_boost(tick_count, last_boost) {
            self.boost_all();
            self.last_boost.store(tick_count, Ordering::SeqCst);
        }
    }

    /// 兼容旧接口 — 用于独立测试
    pub fn tick(&self) {
        self.tick_accounting();
        if self.need_reschedule.load(Ordering::SeqCst) != 0 {
            self.schedule();
        }
    }

    pub fn yield_current(&self) {
        self.need_reschedule.store(1, Ordering::SeqCst);
        self.schedule();
    }

    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 线程级调度
    pub fn schedule(&self) {
        self.stats.total_switches.fetch_add(1, Ordering::SeqCst);

        let prev = self.current.load(Ordering::SeqCst) as *mut Thread;

        // 清理 zombie 线程
        self.reap_zombies();

        let next = self.pop_highest().or_else(|| {
            let idle = self.idle_thread.load(Ordering::SeqCst) as *mut Thread;
            if idle.is_null() { None } else { Some(idle) }
        });

        let next = match next {
            Some(t) => t,
            None => return,
        };

        if !prev.is_null() && prev as u64 != next as u64 {
            // SAFETY: prev 由调度器自管理, 必有效
            let prev_ref = unsafe { ThreadRef::new_unchecked(prev) };
            let prev_state = prev_ref.get_state();
            if prev_state.is_alive() && prev_state != ThreadState::Zombie {
                let _ = prev_ref.set_state(ThreadState::Ready);
                let prio = ThreadPriority::from_u32(prev_ref.priority_raw());
                self.run_queues[Self::queue_idx(prio)].push_back(prev);
            }
        }

        // SAFETY: next 由调度器自管理
        let next_ref = unsafe { ThreadRef::new_unchecked(next) };
        let _ = next_ref.set_state(ThreadState::Running);

        self.current.store(next as u64, Ordering::SeqCst);

        // 更新 TSS 内核栈
        let kernel_stack = next_ref.kernel_stack();
        if kernel_stack != 0 {
            crate::framework::cpu::arch::set_kernel_stack(kernel_stack);
        }

        // 硬件上下文切换
        if !prev.is_null() && prev as u64 != next as u64 {
            // SAFETY: prev/next 由调度器自管理
            let prev_ref2 = unsafe { ThreadRef::new_unchecked(prev) };
            let next_ref2 = next_ref;
            let prev_ctx = prev_ref2.context_ptr();
            let next_ctx = next_ref2.context_ptr();

            if !prev_ctx.is_null() && !next_ctx.is_null() {
                // ✅ 栈顶 canary 检测
                let ks = prev_ref2.kernel_stack();
                // SAFETY: canary 地址由本调度器维护, 始终位于已映射内核栈内
                let canary_addr = ks + super::types::KERNEL_STACK_SIZE as u64 - 8;
                let canary = unsafe { *(canary_addr as *const u64) };
                if canary != 0xDEADBEEF_CAFEBABE_u64 {
                    // SAFETY: klog_ffi_info 是 framework 提供的 C ABI 日志接口
                    unsafe extern "C" {
                        fn klog_ffi_info(msg: *const u8);
                    }
                    // SAFETY: klog_ffi_info 是 unsafe extern "C" FFI 函数
                    unsafe {
                        klog_ffi_info(b"[SCHED_EX] KERNEL STACK CANARY CORRUPTED\0".as_ptr());
                    }
                }

                crate::arch!(context_switch(prev_ctx as *mut u8, next_ctx as *const u8));
            }
        }

        crate::framework::sync::rcu::rcu_note_quiescent_state();

        self.need_reschedule.store(0, Ordering::SeqCst);
    }

    /// 冻结单个线程
    pub fn freeze_thread(&self, thread: *mut Thread) -> bool {
        if thread.is_null() {
            return false;
        }

        // SAFETY: 调用方保证 thread 有效, push_back 串行
        let t = unsafe { ThreadRef::new_unchecked(thread) };
        if !t.can_freeze() {
            return false;
        }

        for i in 0..5 {
            if self.run_queues[i].remove(thread) {
                let _ = t.set_state(ThreadState::Frozen);
                self.frozen_queue.push_back(thread);
                self.stats.frozen_count.fetch_add(1, Ordering::SeqCst);
                return true;
            }
        }
        false
    }

    /// 解冻线程
    pub fn thaw_thread(&self, thread: *mut Thread) -> bool {
        if thread.is_null() {
            return false;
        }

        // SAFETY: 调用方保证 thread 有效, push_back 串行
        let t = unsafe { ThreadRef::new_unchecked(thread) };
        if t.get_state() != ThreadState::Frozen {
            return false;
        }

        if !self.frozen_queue.remove(thread) {
            return false;
        }

        let _ = t.set_state(ThreadState::Ready);
        let prio = ThreadPriority::from_u32(t.priority_raw());
        self.run_queues[Self::queue_idx(prio)].push_back(thread);
        self.stats.frozen_count.fetch_sub(1, Ordering::SeqCst);
        true
    }

    /// 全局冻结所有非当前线程
    pub fn freeze_all(&self) {
        self.is_frozen_global.store(true, Ordering::SeqCst);
        let current = self.current.load(Ordering::SeqCst);

        let mut to_freeze: [u64; 640] = [0; 640];
        let mut count = 0;

        for i in 0..5 {
            for t in self.run_queues[i].iter() {
                if t.is_null() || t as u64 == current {
                    continue;
                }
                if count < 640 {
                    to_freeze[count] = t as u64;
                    count += 1;
                }
            }
        }

        for i in 0..count {
            let t = to_freeze[i] as *mut Thread;
            self.freeze_thread(t);
        }
    }

    /// 全局解冻
    pub fn thaw_all(&self) {
        self.is_frozen_global.store(false, Ordering::SeqCst);

        let mut to_thaw: [u64; 128] = [0; 128];
        let mut count = 0;

        for t in self.frozen_queue.iter() {
            if t.is_null() {
                continue;
            }
            if count < 128 {
                to_thaw[count] = t as u64;
                count += 1;
            }
        }

        for i in 0..count {
            let t = to_thaw[i] as *mut Thread;
            self.thaw_thread(t);
        }
    }

    /// 线程退出
    pub fn exit_thread(&self, exit_code: u32) {
        let current = self.current.load(Ordering::SeqCst) as *mut Thread;
        if current.is_null() {
            return;
        }

        // SAFETY: current 由调度器自管理
        let t = unsafe { ThreadRef::new_unchecked(current) };
        t.store_exit_code(exit_code);
        let _ = t.set_state(ThreadState::Zombie);

        self.need_reschedule.store(1, Ordering::SeqCst);
        self.schedule();
    }

    /// 回收僵尸线程
    fn reap_zombies(&self) {
        let mut to_reap: [u64; 32] = [0; 32];
        let mut count = 0;

        for i in 0..5 {
            for t in self.run_queues[i].iter() {
                if t.is_null() || count >= 32 {
                    break;
                }
                // SAFETY: t 来自本调度器队列
                let tr = unsafe { ThreadRef::new_unchecked(t) };
                if tr.get_state() == ThreadState::Zombie {
                    to_reap[count] = t as u64;
                    count += 1;
                }
            }
        }

        for i in 0..count {
            let t = to_reap[i] as *mut Thread;
            for j in 0..5 {
                if self.run_queues[j].remove(t) {
                    break;
                }
            }
            // SAFETY: t 来自本调度器
            let tr = unsafe { ThreadRef::new_unchecked(t) };
            if tr.tid() != 0 {
                let _ = tr.set_state(ThreadState::Terminated);
                self.stats.zombie_reaped.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// 优先级 boost: 按策略提升所有线程
    fn boost_all(&self) {
        self.stats.priority_boosts.fetch_add(1, Ordering::SeqCst);
        let target = super::sched_trait::current_sched_decision().boost_target();
        let target_idx = target as usize;
        let ts = super::sched_trait::current_sched_decision().time_slice_for(target);

        for src in 0..4 {
            // 跳过目标队列自身: boost_target() 返回 High (=3), 若继续对其 drain
            // 并把元素 push_back 回同一队列, while let 永不终止 (活锁) —
            // 2026-09-24 UT-06 实测修复 (原实现遍历 0..4 含目标队列).
            if src == target_idx {
                continue;
            }
            while let Some(t) = self.run_queues[src].pop_front() {
                // SAFETY: t 来自本调度器
                let tr = unsafe { ThreadRef::new_unchecked(t) };
                tr.store_priority(target as u32);
                tr.store_time_slice(ts);
                self.run_queues[target_idx].push_back(t);
            }
        }
    }
}

pub static SCHEDULER_EX: SchedulerEx = SchedulerEx::new();

pub fn init() {
    SCHEDULER_EX.init();
}

#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
/// 打印线程调试信息 (诊断用途)
pub fn thread_dump_info(thread: ThreadRef) {
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    unsafe extern "C" {
        fn klog_ffi_info(msg: *const u8);
    }

    if thread.is_null() {
        // SAFETY: klog_ffi_info 是 C ABI 函数
        unsafe {
            klog_ffi_info(b"[SCHED] thread_dump: null thread\0".as_ptr());
        }
        return;
    }

    let ptr = thread.as_ptr();
    // SAFETY: ptr 来自 ThreadRef, 保证有效
    let (pid, state, prio, ts) = unsafe {
        (
            (*ptr).pid,
            (*ptr).state.load(Ordering::SeqCst),
            (*ptr).priority.load(Ordering::Acquire),
            thread.time_slice(),
        )
    };

    // 使用栈上缓冲区格式化输出
    let mut buf = [0u8; 128];
    let msg = b"[SCHED] thread_dump: pid=\0";
    buf[..msg.len()].copy_from_slice(msg);

    // 简单数字转字符串
    let mut pos = msg.len();
    let mut val = pid;
    if val == 0 {
        buf[pos] = b'0';
        pos += 1;
    } else {
        let mut digits = [0u8; 10];
        let mut i = 0;
        while val > 0 {
            digits[i] = b'0' + (val % 10) as u8;
            val /= 10;
            i += 1;
        }
        while i > 0 {
            i -= 1;
            buf[pos] = digits[i];
            pos += 1;
        }
    }

    let suffix = b" state=\0";
    buf[pos..pos + suffix.len()].copy_from_slice(suffix);
    pos += suffix.len();

    let mut val2 = state;
    if val2 == 0 {
        buf[pos] = b'0';
        pos += 1;
    } else {
        let mut digits = [0u8; 10];
        let mut i = 0;
        while val2 > 0 {
            digits[i] = b'0' + (val2 % 10) as u8;
            val2 /= 10;
            i += 1;
        }
        while i > 0 {
            i -= 1;
            buf[pos] = digits[i];
            pos += 1;
        }
    }

    let suffix2 = b" prio=\0";
    buf[pos..pos + suffix2.len()].copy_from_slice(suffix2);
    pos += suffix2.len();

    let mut val3 = prio;
    if val3 == 0 {
        buf[pos] = b'0';
        pos += 1;
    } else {
        let mut digits = [0u8; 10];
        let mut i = 0;
        while val3 > 0 {
            digits[i] = b'0' + (val3 % 10) as u8;
            val3 /= 10;
            i += 1;
        }
        while i > 0 {
            i -= 1;
            buf[pos] = digits[i];
            pos += 1;
        }
    }

    let suffix3 = b" ts=\0";
    buf[pos..pos + suffix3.len()].copy_from_slice(suffix3);
    pos += suffix3.len();

    let mut val4 = ts;
    if val4 == 0 {
        buf[pos] = b'0';
        pos += 1;
    } else {
        let mut digits = [0u8; 10];
        let mut i = 0;
        while val4 > 0 {
            digits[i] = b'0' + (val4 % 10) as u8;
            val4 /= 10;
            i += 1;
        }
        while i > 0 {
            i -= 1;
            buf[pos] = digits[i];
            pos += 1;
        }
    }

    buf[pos] = b'\n';
    pos += 1;

    // SAFETY: klog_ffi_info 是 C ABI 函数, buf 内容有效
    unsafe {
        klog_ffi_info(buf[..pos].as_ptr());
    }
}

// ============================================================
// 单元测试
// ============================================================
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;

    /// 辅助: 创建测试线程
    ///
    /// # Safety
    /// 返回原始指针, 调用方在 `Box::from_raw` 之后确保唯一所有权。
    /// 为安全 Rust 接口, 测试需要 `unsafe` 包装 (无法避免原始指针)。
    fn make_test_thread(tid: u32, pid: u32, priority: ThreadPriority) -> *mut Thread {
        // `Box::into_raw` 为安全接口: 转移所有权到调用方, 返回原始指针
        let t = Box::into_raw(Box::new(Thread::new(tid, pid)));
        // SAFETY: t 来自 Box::into_raw 立即调用, 分配有效
        let tr = unsafe { ThreadRef::new_unchecked(t) };
        tr.store_priority(priority as u32);
        tr.store_state(ThreadState::Ready as u32);
        t
    }

    /// 辅助: 释放测试线程
    ///
    /// # Safety
    /// - `t` 必须由 `make_test_thread` 产生且未被释放
    unsafe fn free_test_thread(t: *mut Thread) {
        if !t.is_null() {
            // SAFETY: 由调用方保证 `t` 由 make_test_thread 产生且未被释放 (见本函数 # Safety 契约).
            unsafe {
                drop(Box::from_raw(t));
            }
        }
    }

    #[test]
    fn test_run_queue_push_pop() {
        let q = RunQueue::new();
        assert!(q.is_empty());

        let t1 = make_test_thread(1, 1, ThreadPriority::Normal);
        let t2 = make_test_thread(2, 1, ThreadPriority::High);

        q.push_back(t1);
        q.push_back(t2);
        assert_eq!(q.len(), 2);

        let popped = q.pop_front();
        assert!(popped.is_some());
        assert_eq!(q.len(), 1);

        let popped2 = q.pop_front();
        assert!(popped2.is_some());
        assert!(q.is_empty());

        // SAFETY: 测试分配由 make_test_thread 负责回收, 单线程测试无竞争
        unsafe {
            if let Some(p) = popped {
                free_test_thread(p);
            }
            if let Some(p) = popped2 {
                free_test_thread(p);
            }
        }
    }

    #[test]
    fn test_run_queue_remove() {
        let q = RunQueue::new();
        let t1 = make_test_thread(1, 1, ThreadPriority::Normal);
        let t2 = make_test_thread(2, 1, ThreadPriority::High);
        let t3 = make_test_thread(3, 1, ThreadPriority::Low);

        q.push_back(t1);
        q.push_back(t2);
        q.push_back(t3);
        assert_eq!(q.len(), 3);

        assert!(q.remove(t2));
        assert_eq!(q.len(), 2);

        // t1 与 t3 应该仍在
        let p1 = q.pop_front().unwrap();
        let p2 = q.pop_front().unwrap();
        // SAFETY: p1/p2 由 push_back 路径插入, pop_front 后未再被引用
        unsafe {
            free_test_thread(p1);
            free_test_thread(p2);
        }
        assert!(q.is_empty());
    }

    #[test]
    fn test_run_queue_empty_pop() {
        let q = RunQueue::new();
        assert!(q.pop_front().is_none());
    }

    #[test]
    fn test_scheduler_ex_add_thread() {
        // 为隔离测试创建全新的调度器实例
        let sched = SchedulerEx::new();
        sched.init();

        let t = make_test_thread(10, 1, ThreadPriority::Normal);
        sched.add_thread(t);
        // idle(0) + test(2=Normal)
        assert_eq!(sched.run_queues[ThreadPriority::Idle as usize].len(), 1);
        assert_eq!(sched.run_queues[ThreadPriority::Normal as usize].len(), 1);

        // SAFETY: t 由 make_test_thread 分配, 测试结束时无其他引用
        unsafe {
            free_test_thread(t);
        }
    }

    #[test]
    fn test_pop_highest() {
        let sched = SchedulerEx::new();
        let idle = make_test_thread(0, 0, ThreadPriority::Idle);
        sched.idle_thread.store(idle as u64, Ordering::SeqCst);
        sched.current.store(idle as u64, Ordering::SeqCst);

        let t1 = make_test_thread(1, 1, ThreadPriority::Low);
        let t2 = make_test_thread(2, 1, ThreadPriority::High);
        let t3 = make_test_thread(3, 1, ThreadPriority::Normal);

        sched.run_queues[ThreadPriority::Low as usize].push_back(t1);
        sched.run_queues[ThreadPriority::High as usize].push_back(t2);
        sched.run_queues[ThreadPriority::Normal as usize].push_back(t3);

        let best = sched.pop_highest();
        assert!(best.is_some());
        // SAFETY: best 来自 pop_highest
        let best_tr = unsafe { ThreadRef::new_unchecked(best.unwrap()) };
        assert_eq!(best_tr.tid(), 2); // highest priority
        unsafe {
            free_test_thread(t1);
            free_test_thread(t2);
            free_test_thread(t3);
            free_test_thread(idle);
        }
    }

    #[test]
    fn test_thread_state_transitions() {
        let t = make_test_thread(1, 1, ThreadPriority::Normal);

        // SAFETY: t 由 make_test_thread 分配, 立即构造 ThreadRef
        let tr = unsafe { ThreadRef::new_unchecked(t) };

        // make_test_thread 初值即 Ready (store_state(Ready)), 故 Ready → Running 合法
        assert!(tr.set_state(ThreadState::Running).is_ok());
        assert_eq!(tr.load_state_raw(), ThreadState::Running as u32);

        // Running → Ready 合法 (时间片耗尽)
        assert!(tr.set_state(ThreadState::Ready).is_ok());
        assert_eq!(tr.load_state_raw(), ThreadState::Ready as u32);

        // 重置为 Created: Created → Ready 在白名单内, 应成功
        // (原断言认为它非法, 与 Thread::set_state_safe 白名单不符; 2026-09-24 UT-06 修正)
        tr.store_state(ThreadState::Created as u32);
        assert!(tr.set_state(ThreadState::Ready).is_ok());
        assert_eq!(tr.load_state_raw(), ThreadState::Ready as u32);

        // 非法转换: Created → Running 不在白名单内, 应拒绝且不改状态
        tr.store_state(ThreadState::Created as u32);
        assert!(tr.set_state(ThreadState::Running).is_err());
        assert_eq!(tr.load_state_raw(), ThreadState::Created as u32);

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            free_test_thread(t);
        }
    }

    #[test]
    fn test_freeze_thaw() {
        let sched = SchedulerEx::new();
        let t = make_test_thread(1, 1, ThreadPriority::Normal);
        // SAFETY: t 由 make_test_thread 分配
        let tr = unsafe { ThreadRef::new_unchecked(t) };
        tr.store_state(ThreadState::Ready as u32);

        sched.run_queues[ThreadPriority::Normal as usize].push_back(t);
        assert!(sched.freeze_thread(t));
        assert_eq!(sched.run_queues[ThreadPriority::Normal as usize].len(), 0);
        assert_eq!(sched.frozen_queue.len(), 1);

        assert!(sched.thaw_thread(t));
        assert_eq!(sched.run_queues[ThreadPriority::Normal as usize].len(), 1);
        assert_eq!(sched.frozen_queue.len(), 0);

        // SAFETY: t 经 freeze/thaw 往返后回到 ready 队列, 测试末释放
        unsafe {
            free_test_thread(t);
        }
    }

    #[test]
    fn test_tick_accounting() {
        let sched = SchedulerEx::new();
        let t = make_test_thread(100, 1, ThreadPriority::Normal);
        // SAFETY: t 由 make_test_thread 分配
        let tr = unsafe { ThreadRef::new_unchecked(t) };
        tr.store_time_slice(10);
        tr.store_state(ThreadState::Ready as u32);

        sched.run_queues[ThreadPriority::Normal as usize].push_back(t);
        sched.current.store(t as u64, Ordering::SeqCst);

        for _ in 0..5 {
            sched.tick_accounting();
        }

        let ts = tr.time_slice();
        assert_eq!(ts, 5); // 10 - 5 = 5
        // SAFETY: t 由本测试 make_test_thread 分配, tick_accounting 不持有引用
        unsafe {
            free_test_thread(t);
        }
    }

    #[test]
    fn test_need_reschedule_flag() {
        let sched = SchedulerEx::new();
        let t = make_test_thread(100, 1, ThreadPriority::Normal);
        // SAFETY: t 由 make_test_thread 分配
        let tr = unsafe { ThreadRef::new_unchecked(t) };
        tr.store_time_slice(1);
        tr.store_state(ThreadState::Ready as u32);

        sched.run_queues[ThreadPriority::Normal as usize].push_back(t);
        sched.current.store(t as u64, Ordering::SeqCst);

        sched.tick_accounting();
        assert_eq!(sched.need_reschedule.load(Ordering::SeqCst), 1);

        // SAFETY: t 经 push_back + tick_accounting 验证后, 测试末释放
        unsafe {
            free_test_thread(t);
        }
    }

    #[test]
    fn test_tick_then_schedule() {
        let sched = SchedulerEx::new();
        let idle = make_test_thread(0, 0, ThreadPriority::Idle);
        sched.idle_thread.store(idle as u64, Ordering::SeqCst);
        sched.current.store(idle as u64, Ordering::SeqCst);

        let t = make_test_thread(1, 1, ThreadPriority::Normal);
        // SAFETY: t 由 make_test_thread 分配
        let tr = unsafe { ThreadRef::new_unchecked(t) };
        tr.store_state(ThreadState::Ready as u32);
        sched.add_thread(t);
        sched.need_reschedule.store(1, Ordering::SeqCst);

        sched.schedule();
        let current = sched.current.load(Ordering::SeqCst);
        // 应该已切到 t (优先级高于 idle)
        assert!(current != 0);

        // SAFETY: t/idle 在 schedule() 后未再被引用, 测试末释放
        unsafe {
            free_test_thread(t);
            free_test_thread(idle);
        }
    }

    #[test]
    fn test_zombie_reap() {
        let sched = SchedulerEx::new();
        let idle = make_test_thread(0, 0, ThreadPriority::Idle);
        sched.idle_thread.store(idle as u64, Ordering::SeqCst);
        sched.current.store(idle as u64, Ordering::SeqCst);

        let zombie = make_test_thread(99, 1, ThreadPriority::Low);
        // SAFETY: zombie 由 make_test_thread 分配, 直接原子写 state 模拟僵尸态
        unsafe {
            (*zombie)
                .state
                .store(ThreadState::Zombie as u32, Ordering::SeqCst);
        }
        sched.run_queues[ThreadPriority::Low as usize].push_back(zombie);

        sched.schedule();
        // zombie 原本在 queue[1=Low], reap 后应为空
        assert!(sched.run_queues[ThreadPriority::Low as usize].is_empty());

        // SAFETY: zombie 经 reap 后, idle 经 schedule 后, 测试末释放
        unsafe {
            free_test_thread(zombie);
            free_test_thread(idle);
        }
    }

    #[test]
    fn test_priority_boost() {
        let sched = SchedulerEx::new();
        let t1 = make_test_thread(1, 1, ThreadPriority::Low);
        let t2 = make_test_thread(2, 1, ThreadPriority::Low);

        sched.run_queues[ThreadPriority::Low as usize].push_back(t1);
        sched.run_queues[ThreadPriority::Low as usize].push_back(t2);

        // 手动触发提升
        sched.boost_all();

        // SAFETY: t1/t2 由本测试 make_test_thread 分配, boost_all 后读 priority 验证
        unsafe {
            assert_eq!(
                (*t1).priority.load(Ordering::SeqCst),
                ThreadPriority::High as u32
            );
            assert_eq!(
                (*t2).priority.load(Ordering::SeqCst),
                ThreadPriority::High as u32
            );
            free_test_thread(t1);
            free_test_thread(t2);
        }
    }

    #[test]
    fn test_threadstate_from_u32() {
        assert_eq!(ThreadState::from_u32(0), ThreadState::Created);
        assert_eq!(ThreadState::from_u32(1), ThreadState::Ready);
        assert_eq!(ThreadState::from_u32(2), ThreadState::Running);
        assert_eq!(ThreadState::from_u32(3), ThreadState::Blocked);
        assert_eq!(ThreadState::from_u32(4), ThreadState::Zombie);
        assert_eq!(ThreadState::from_u32(5), ThreadState::Terminated);
        assert_eq!(ThreadState::from_u32(6), ThreadState::Frozen);
        assert_eq!(ThreadState::from_u32(99), ThreadState::Created); // fallback
    }

    #[test]
    fn test_threadstate_runnable() {
        assert!(ThreadState::Ready.is_runnable());
        assert!(ThreadState::Running.is_runnable());
        assert!(!ThreadState::Created.is_runnable());
        assert!(!ThreadState::Blocked.is_runnable());
        assert!(!ThreadState::Zombie.is_runnable());
        assert!(!ThreadState::Frozen.is_runnable());
    }

    #[test]
    fn test_threadstate_alive() {
        assert!(ThreadState::Created.is_alive());
        assert!(ThreadState::Ready.is_alive());
        assert!(ThreadState::Running.is_alive());
        assert!(ThreadState::Blocked.is_alive());
        assert!(!ThreadState::Zombie.is_alive());
        assert!(!ThreadState::Terminated.is_alive());
        assert!(ThreadState::Frozen.is_alive());
    }

    #[test]
    fn test_threadstate_can_freeze() {
        assert!(ThreadState::Running.can_freeze());
        assert!(ThreadState::Ready.can_freeze());
        assert!(ThreadState::Blocked.can_freeze());
        assert!(!ThreadState::Created.can_freeze());
        assert!(!ThreadState::Zombie.can_freeze());
        assert!(!ThreadState::Frozen.can_freeze());
    }
}
