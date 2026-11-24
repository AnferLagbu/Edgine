//! timerfd — per-context 定时器对象 (TCB)
//!
//! ## fd 对象模型
//!
//! `TimerFdFile` 对象 (`functions::fs::timerfd`) 挂在全局 `OpenFileTable`,
//! 用户 fd 由 per-process `FdTable` 分配. 本模块只保留**机制面**:
//!
//! - 堆上 `Arc<TimerFdContext>` 持有嵌入式 `HrTimer` 与全部到期状态
//! - 回调经 `container_of` 从 `HrTimer` 指针 O(1) 定位宿主 context
//! - 供对象层调用的安全 API (创建 / 设时 / 取到期 / 轮询 / 释放)
//!
//! 对象生命周期由 `Arc` 与 fd 引用计数承担: `TimerFdFile` 持有
//! `Arc<TimerFdContext>`, 最后一个 fd 引用关闭时对象层调用 [`TimerFdContext::release`],
//! 先 `hrtimer_shutdown` 等待在途回调退出, 之后 `Arc` 才能归零. 回调只借
//! `&HrTimer`, 不克隆 `Arc`; `TimerFdContext` 发布后不再移动, 故 `container_of`
//! 不会悬垂.
//!
//! ```text
//! TimerFdContext (Arc, 堆)
//!   ├── timer: HrTimer              (首字段; container_of 偏移 0)
//!   ├── ticks: AtomicU64            (到期次数累计, read 清零)
//!   ├── interval_ns: AtomicU64      (周期间隔, 0=单次)
//!   ├── armed: AtomicBool           (是否已启动)
//!   ├── generation: AtomicU64       (当前激活序号)
//!   ├── last_expiry_ns: AtomicU64   (上次计划到期时刻, read 重挂基准)
//!   ├── tfd_ctx: IrqSpinLock<()>    (TFD_CTX 控制面锁; 回调与 poll 不取)
//!   ├── waiters: IrqSpinLock<WaitQueue> (TFD_WAIT 阻塞 read 等待队列)
//!   ├── closed: AtomicBool          (最后一个 fd 关闭后拒绝新等待)
//!   └── wake_token(): 对象地址令牌    (方法而非字段; epoll 兴趣按此唤醒, 不存裸 fd)
//! ```
//!
//! ## 周期语义
//!
//! 回调只累计 ticks、置 `armed=false`、按对象 token 唤醒 epoll, 返回 `OneShot`:
//! **不在回调内重挂**. 周期 timerfd 在首次到期后自然停住; `read` 消费 ticks 后
//! 以 `last_expiry + interval` 为基准重挂下一次到期. 无人 `read` 的周期对象
//! 不会在硬中断里持续点火.
//!
//! # Safety
//!
//! - 回调在中断上下文执行 (不可睡眠), 只做原子读写、取走 read 等待者并
//!   `scheduler_unblock`、`epoll_pwake_token`; 不取 `TFD_CTX`, 避免
//!   `epoll_pwake_token → 对象 poll` 回取控制锁.
//! - 控制面锁在等待 `hrtimer_cancel`/`hrtimer_shutdown` 期间保护 context 不被
//!   并发 `release`; `Arc` 保证对象本身存活, 锁保证状态操作串行.
//! - `container_of` 依赖 `timer` 为 `#[repr(C)]` 结构的首字段, 且 context 在
//!   回调在途期间不会释放 (`release` 的 shutdown 语义).

#![deny(missing_docs)]

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::privileged::ipc::{WaitQueue, WaitQueueItem};
use crate::privileged::proc::{
    BlockReason, process_block_finish, process_block_prepare, process_get_current_pid,
    scheduler_unblock,
};
use crate::privileged::sync::{
    IrqSpinLock, IrqSpinLockGuard, disable_interrupts, restore_interrupts,
};
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, StartOutcome, hrtimer_cancel, hrtimer_clock_read, hrtimer_shutdown,
    hrtimer_start,
};

// ============================================================================
// 公共类型
// ============================================================================

/// timerfd 时间读数 `(interval_ns, remaining_ns)`
pub type TimerTime = (u64, u64);

/// `set_time` 失败原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetTimeError {
    /// 底层定时器未就绪/已 shutdown, 无法进入 `Pending`
    TimerUnavailable,
}

/// 到期计数出队结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeExpiry {
    /// 有待消费的到期次数
    Count(u64),
    /// 无待消费事件 (非阻塞读 → `EAGAIN`)
    Empty,
}

// ============================================================================
// 数据结构
// ============================================================================

/// timerfd 的 per-context 状态
///
/// `timer` 必须是首字段 (配合 `#[repr(C)]`), 回调据此用 `container_of` 从
/// `&HrTimer` 反推宿主. 其余状态均为原子量: 回调不取控制面占位, 控制面只
/// 串行化 `set_time`/`read`/`release` 的复合操作.
#[repr(C)]
pub struct TimerFdContext {
    /// 嵌入的 hrtimer 对象 (首字段; 地址即 context 地址)
    timer: HrTimer,
    /// 到期次数累计 (read 时清零)
    ticks: AtomicU64,
    /// 周期间隔 (纳秒, 0 = 单次)
    interval_ns: AtomicU64,
    /// 是否已启动 (armed)
    armed: AtomicBool,
    /// 当前激活序号 (`hrtimer_start` 分配; 0 = 新激活尚未回写)
    generation: AtomicU64,
    /// 上次计划到期时刻 (纳秒); `read` 重挂的基准
    last_expiry_ns: AtomicU64,
    /// 控制面锁 (`TFD_CTX`；`set_time`/`take_expiry`/`get_time`/`release` 串行;
    /// 回调与 `poll` 不取)
    tfd_ctx: IrqSpinLock<()>,
    /// 阻塞 `read` 等待队列 (`TFD_WAIT`；回调与 `release` 取出 pid 后唤醒)
    waiters: IrqSpinLock<WaitQueue>,
    /// 最后一个 fd 引用关闭; 阻止关闭后新等待者入队
    closed: AtomicBool,
}

impl TimerFdContext {
    /// 创建一个已初始化 `HrTimer` 的 context
    ///
    /// 堆分配由内核全局分配器承担; 新建 `HrTimer` 处于 `Uninit`, 初始化回调不会
    /// 被活动定时器拒绝. 失败时保留 `Uninit`, 后续 `set_time` 会返回
    /// [`SetTimeError::TimerUnavailable`], 不会留下半武装对象.
    #[must_use]
    pub fn create() -> Arc<Self> {
        let ctx = Arc::new(Self {
            timer: HrTimer::uninit(),
            ticks: AtomicU64::new(0),
            interval_ns: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            last_expiry_ns: AtomicU64::new(0),
            tfd_ctx: IrqSpinLock::named("TFD_CTX.tfd_ctx", ()),
            waiters: IrqSpinLock::named("TFD_WAIT.waiters", WaitQueue::new()),
            closed: AtomicBool::new(false),
        });
        if ctx.timer.try_init(timerfd_callback).is_err() {
            debug_assert!(false, "新建 timerfd 上下文的 HrTimer 初始化被拒绝");
        }
        ctx
    }

    /// 对象级稳定唤醒 token (非 0)
    ///
    /// 只暴露对象地址作为身份, 不据此解引用; epoll 兴趣按它唤醒, 因而
    /// `dup` 别名与多个 epoll 实例共享同一唤醒源. 最后一个 fd 关闭时
    /// [`Self::release`] 会从所有 epoll 兴趣中移除该 token, 防止地址复用后
    /// 误唤醒新对象.
    #[must_use]
    pub fn wake_token(&self) -> u64 {
        core::ptr::from_ref(self) as usize as u64
    }

    /// 阻塞等待至少一个到期计数.
    ///
    /// 在 `TFD_WAIT` 内完成"检查 ticks → 入队 → 置 Blocked"三步, 再把锁外
    /// `process_block_finish` 切换, 与 futex 的锁域顺序一致: 唤醒方在
    /// `TFD_WAIT` 内看到 Blocked 后才投递, 不会丢唤醒. 被关闭或队列满时返回
    /// `false`, 调用方保持 `EAGAIN`; 被信号/伪唤醒且无 ticks 时重新入队.
    pub fn wait_for_ticks(&self) -> bool {
        let pid = process_get_current_pid();
        if pid == 0 {
            return false;
        }
        loop {
            let mut waiters = self.waiters.lock();
            if self.closed.load(Ordering::Acquire) {
                return false;
            }
            if self.ticks.load(Ordering::Acquire) > 0 {
                return true;
            }
            if waiters.count() >= 4 {
                return false;
            }
            let flags = disable_interrupts();
            if !process_block_prepare(pid, BlockReason::WaitingForIo, 0) {
                drop(waiters);
                restore_interrupts(flags);
                return false;
            }
            if !waiters.add(WaitQueueItem { pid }) {
                drop(waiters);
                restore_interrupts(flags);
                scheduler_unblock(pid);
                return false;
            }
            drop(waiters);
            restore_interrupts(flags);
            process_block_finish(pid);
        }
    }

    /// 取出全部阻塞 read 等待者 (最多队列容量), 不调用调度器.
    fn drain_waiters(&self) -> ([u32; 4], usize) {
        let mut pids = [0u32; 4];
        let mut count = 0;
        let mut waiters = self.waiters.lock();
        while count < pids.len() {
            let Some(item) = waiters.wake_one() else {
                break;
            };
            pids[count] = item.pid;
            count += 1;
        }
        (pids, count)
    }

    /// 设置定时器, 返回设置前的 `(interval_ns, remaining_ns)`
    ///
    /// `value_ns == 0` 表示 disarm (与 Linux 一致). `abstime == true` 时
    /// `value_ns` 视为绝对到期时刻 (与 [`hrtimer_clock_read`] 同一单调时基).
    ///
    /// # Errors
    ///
    /// 底层 timer 无法入队时返回 [`SetTimeError::TimerUnavailable`]; 已写入的
    /// armed 状态会回滚, 不留半武装 context.
    pub fn set_time(
        &self,
        value_ns: u64,
        interval_ns: u64,
        abstime: bool,
    ) -> Result<TimerTime, SetTimeError> {
        let _control = ControlGuard::acquire(self);
        let now = hrtimer_clock_read();
        let remaining = if self.armed.load(Ordering::Acquire) {
            self.timer.expiry_ns().saturating_sub(now)
        } else {
            0
        };
        let old = (self.interval_ns.load(Ordering::Acquire), remaining);

        if self.armed.load(Ordering::Acquire) {
            // 控制面占位已取得; 回调不取该占位, 故等待在途回调不会自锁.
            hrtimer_cancel(&self.timer);
        }
        self.armed.store(false, Ordering::Release);
        self.generation.store(0, Ordering::Release);
        self.ticks.store(0, Ordering::Release);

        if value_ns == 0 {
            self.interval_ns.store(0, Ordering::Release);
            self.last_expiry_ns.store(0, Ordering::Release);
            return Ok(old);
        }

        let expiry_ns = if abstime {
            value_ns
        } else {
            now.saturating_add(value_ns)
        };
        self.interval_ns.store(interval_ns, Ordering::Release);
        self.last_expiry_ns.store(expiry_ns, Ordering::Release);
        // 新激活先以 generation=0 + armed=true 发布; 回调若在 start 返回前抢到,
        // 允许它以 seq 认领 (唯一合法窗口). 回写用 CAS, 不覆盖回调已写入的值.
        self.generation.store(0, Ordering::Release);
        self.armed.store(true, Ordering::Release);
        match hrtimer_start(&self.timer, expiry_ns) {
            StartOutcome::Started(seq) => {
                let _ =
                    self.generation
                        .compare_exchange(0, seq, Ordering::Release, Ordering::Relaxed);
                Ok(old)
            }
            StartOutcome::NotReady | StartOutcome::NotInitialized | StartOutcome::Shutdown => {
                self.armed.store(false, Ordering::Release);
                self.generation.store(0, Ordering::Release);
                Err(SetTimeError::TimerUnavailable)
            }
        }
    }

    /// 读取当前 `(interval_ns, remaining_ns)`
    ///
    /// `armed=false` (含“已到期但尚未 read 重挂”) 时 remaining 为 0, 对应 Linux
    /// timerfd 的到期后读数语义.
    #[must_use]
    pub fn get_time(&self) -> TimerTime {
        let _control = ControlGuard::acquire(self);
        let remaining = if self.armed.load(Ordering::Acquire) {
            self.timer.expiry_ns().saturating_sub(hrtimer_clock_read())
        } else {
            0
        };
        (self.interval_ns.load(Ordering::Acquire), remaining)
    }

    /// 取出到期次数 (读后清零), 并在周期对象上重挂下一次到期
    ///
    /// 重挂只在 `read` 路径发生: 回调用 `OneShot` 收敛后不自行续期, 无人 `read`
    /// 的周期对象在首次到期后自然停住. 重挂失败不影响本次已消费的 ticks,
    /// 只把 `armed` 回滚为 false.
    pub fn take_expiry(&self) -> TakeExpiry {
        let _control = ControlGuard::acquire(self);
        let count = self.ticks.swap(0, Ordering::AcqRel);
        if count == 0 {
            return TakeExpiry::Empty;
        }

        let interval_ns = self.interval_ns.load(Ordering::Acquire);
        if interval_ns > 0 {
            let now = hrtimer_clock_read();
            let base = self.last_expiry_ns.load(Ordering::Acquire);
            let next = if base == 0 {
                now.saturating_add(interval_ns)
            } else {
                base.saturating_add(interval_ns)
            };
            // 落后于当前时刻则从当前时刻重新计时, 避免补发一串过期 tick.
            let next = if next <= now {
                now.saturating_add(interval_ns)
            } else {
                next
            };
            self.last_expiry_ns.store(next, Ordering::Release);
            self.generation.store(0, Ordering::Release);
            self.armed.store(true, Ordering::Release);
            match hrtimer_start(&self.timer, next) {
                StartOutcome::Started(seq) => {
                    let _ = self.generation.compare_exchange(
                        0,
                        seq,
                        Ordering::Release,
                        Ordering::Relaxed,
                    );
                }
                StartOutcome::NotReady | StartOutcome::NotInitialized | StartOutcome::Shutdown => {
                    self.armed.store(false, Ordering::Release);
                    self.generation.store(0, Ordering::Release);
                }
            }
        }
        TakeExpiry::Count(count)
    }

    /// 查询是否就绪 (到期计数非空)
    ///
    /// poll 只读 `ticks`, 不取控制面占位; 否则回调唤醒 epoll 后的间接 poll
    /// 会回取同一占位, 形成自死锁.
    #[must_use]
    pub fn poll_ready(&self) -> bool {
        self.ticks.load(Ordering::Acquire) > 0
    }

    /// 释放 context: 关闭定时器、等待在途回调退出, 再清空到期状态
    ///
    /// 由对象层在最后一个 fd 引用关闭时调用. 返回后不再有本 context 的回调,
    /// `Arc` 可以安全归零.
    pub fn release(&self) {
        let (pids, count) = {
            let _control = ControlGuard::acquire(self);
            hrtimer_shutdown(&self.timer);
            self.closed.store(true, Ordering::Release);
            self.armed.store(false, Ordering::Release);
            self.generation.store(0, Ordering::Release);
            self.ticks.store(0, Ordering::Release);
            self.interval_ns.store(0, Ordering::Release);
            self.last_expiry_ns.store(0, Ordering::Release);
            self.drain_waiters()
        };
        crate::privileged::syscall::epoll::epoll_forget_token(self.wake_token());
        for pid in &pids[..count] {
            scheduler_unblock(*pid);
        }
    }
}

impl Drop for TimerFdContext {
    fn drop(&mut self) {
        // 兜底: 即使对象层漏掉 release, 也不能在仍有 Pending/Running 激活时释放
        // context 内存, 否则回调的 container_of 会悬垂. shutdown 会等待在途回调.
        hrtimer_shutdown(&self.timer);
        self.closed.store(true, Ordering::Release);
        let (pids, count) = self.drain_waiters();
        crate::privileged::syscall::epoll::epoll_forget_token(self.wake_token());
        for pid in &pids[..count] {
            scheduler_unblock(*pid);
        }
    }
}

/// 控制面守卫: 持有 `TFD_CTX` 串行化 `set_time`/`take_expiry`/`get_time`/`release`
///
/// 回调与 `poll` 不取该锁: 回调只写原子量并按对象 token 唤醒, `poll` 只读 ticks.
/// 因此持锁等待在途回调不会与回调路径形成 AB-BA.
struct ControlGuard<'a> {
    _guard: IrqSpinLockGuard<'a, ()>,
}

impl<'a> ControlGuard<'a> {
    fn acquire(ctx: &'a TimerFdContext) -> Self {
        Self {
            _guard: ctx.tfd_ctx.lock(),
        }
    }
}

// ============================================================================
// HrTimer 回调
// ============================================================================

/// timerfd 定时器回调
///
/// 在中断上下文执行: 代际校验 → 累计 ticks → 置 `armed=false` → 唤醒 epoll.
/// **不做周期重挂**; 周期续期由 [`TimerFdContext::take_expiry`] 在 read 路径完成.
///
/// `seq` 为本次激活的入队序号; 与 `generation` 不一致说明是旧激活的迟到回调,
/// 直接丢弃. `generation == 0 && armed` 是新激活在 `hrtimer_start` 返回前被
/// 回调抢到的唯一窗口, 允许认领该 seq.
fn timerfd_callback(timer: &HrTimer, seq: u64) -> HrTimerRestart {
    let timer_ptr = core::ptr::from_ref(timer);
    // SAFETY: `TimerFdContext` 是 `#[repr(C)]` 且 `timer` 为首字段, 故 `&HrTimer`
    // 与宿主 context 同地址; context 由 `Arc` 持有, `release` 的 shutdown 会等待
    // 本回调退出后才允许 Arc 归零, 因此回调期间该地址始终有效.
    let ctx = unsafe { &*timer_ptr.cast::<TimerFdContext>() };

    if !ctx.armed.load(Ordering::Acquire) {
        return HrTimerRestart::OneShot;
    }
    let generation = ctx.generation.load(Ordering::Acquire);
    if generation != 0 && generation != seq {
        return HrTimerRestart::OneShot;
    }
    if generation == 0 {
        let _ = ctx
            .generation
            .compare_exchange(0, seq, Ordering::Release, Ordering::Relaxed);
    }

    // 先收敛状态、记录到期时刻, 再发布 ticks: read 路径只在看到 ticks 后才重挂,
    // 因此不会与回调写 armed/generation/last_expiry 竞争.
    let last_expiry = ctx.timer.expiry_ns();
    ctx.last_expiry_ns.store(last_expiry, Ordering::Release);
    ctx.armed.store(false, Ordering::Release);
    ctx.generation.store(0, Ordering::Release);
    ctx.ticks.fetch_add(1, Ordering::Release);

    let (pids, count) = ctx.drain_waiters();
    for pid in &pids[..count] {
        scheduler_unblock(*pid);
    }

    let wake_token = ctx.wake_token();
    crate::privileged::syscall::epoll::epoll_pwake_token(wake_token);

    HrTimerRestart::OneShot
}

// ============================================================================
// 单元测试 (host-side, 不依赖硬件时基)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn mark_armed(ctx: &TimerFdContext, interval_ns: u64, generation: u64) {
        ctx.interval_ns.store(interval_ns, Ordering::Release);
        ctx.last_expiry_ns.store(0, Ordering::Release);
        ctx.armed.store(true, Ordering::Release);
        ctx.generation.store(generation, Ordering::Release);
    }

    #[test]
    fn callback_accumulates_ticks_without_rearming() {
        let ctx = TimerFdContext::create();
        mark_armed(&ctx, 5_000_000, 7);
        let _ = timerfd_callback(&ctx.timer, 7);
        assert!(ctx.poll_ready(), "回调应累计 ticks");
        assert_eq!(ctx.take_expiry(), TakeExpiry::Count(1), "read 应消费 ticks");
        assert!(!ctx.poll_ready(), "回调不得自行重挂并持续产生 tick");
        ctx.release();
    }

    #[test]
    fn stale_generation_is_ignored() {
        let ctx = TimerFdContext::create();
        mark_armed(&ctx, 0, 9);
        let _ = timerfd_callback(&ctx.timer, 8);
        assert!(!ctx.poll_ready(), "旧激活回调不得计入");
        let _ = timerfd_callback(&ctx.timer, 9);
        assert!(ctx.poll_ready(), "当前激活回调应计入");
        ctx.release();
    }

    #[test]
    fn drain_waiters_returns_all_pids_without_scheduling() {
        let ctx = TimerFdContext::create();
        {
            let mut waiters = ctx.waiters.lock();
            assert!(waiters.add(WaitQueueItem { pid: 0x1111 }));
            assert!(waiters.add(WaitQueueItem { pid: 0x2222 }));
        }
        let (pids, count) = ctx.drain_waiters();
        assert_eq!(count, 2, "drain 必须取出全部等待者");
        assert_eq!((pids[0], pids[1]), (0x1111, 0x2222), "pid 顺序必须保持");
        assert_eq!(ctx.waiters.lock().count(), 0, "drain 后队列必须为空");
    }

    #[test]
    fn closed_context_rejects_new_waiters() {
        let ctx = TimerFdContext::create();
        ctx.release();
        assert!(!ctx.wait_for_ticks(), "关闭后的 context 不得再阻塞");
        assert_eq!(ctx.waiters.lock().count(), 0, "关闭后不得残留等待者");
    }

    #[test]
    fn container_of_address_matches_context() {
        let ctx = TimerFdContext::create();
        let ctx_addr = Arc::as_ptr(&ctx) as *const u8;
        let timer_addr = (&raw const ctx.timer).cast::<u8>();
        assert_eq!(ctx_addr, timer_addr, "timer 必须是 context 首字段");
    }

    #[test]
    fn callback_wakes_all_object_token_interests() {
        let ctx = TimerFdContext::create();
        let token = ctx.wake_token();
        assert_ne!(token, 0, "对象唤醒 token 必须非零");
        assert_eq!(token, ctx.wake_token(), "token 必须稳定");

        let ep1 = super::super::epoll::test_register_token(token, 0x001, 0x111);
        let ep2 = super::super::epoll::test_register_token(token, 0x001, 0x222);
        mark_armed(&ctx, 0, 7);
        let _ = timerfd_callback(&ctx.timer, 7);

        assert!(
            super::super::epoll::test_token_ready(ep1, 0x111),
            "第一个 epoll 兴趣必须被 token 唤醒"
        );
        assert!(
            super::super::epoll::test_token_ready(ep2, 0x222),
            "多个 epoll 兴趣必须同时被 token 唤醒"
        );
        assert!(ctx.poll_ready(), "token 唤醒不得吞掉 ticks");

        ctx.release();
        assert!(
            !super::super::epoll::test_token_registered(ep1, token),
            "release 必须移除第一个 epoll 兴趣"
        );
        assert!(
            !super::super::epoll::test_token_registered(ep2, token),
            "release 必须移除所有 epoll 兴趣"
        );
        super::super::epoll::epoll_destroy(ep1.cast_unsigned());
        super::super::epoll::epoll_destroy(ep2.cast_unsigned());
    }

    #[test]
    fn context_count_is_not_capped_to_static_slots() {
        let mut contexts = alloc::vec::Vec::new();
        for _ in 0..32 {
            contexts.push(TimerFdContext::create());
        }
        assert_eq!(contexts.len(), 32, "timerfd 数量不再受 16 静态槽位限制");
    }
}
