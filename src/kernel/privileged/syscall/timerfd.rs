//! timerfd — 定时器槽位机制 (TCB)
//!
//! ## fd 对象模型 (阶段 3 第 6 个迁移子系统)
//!
//! 自 fd 对象模型阶段 3 起, timerfd 的 fd 编号与对象归属不再由本模块持有:
//! `TimerFdFile` 对象 (`functions::fs::timerfd`) 挂在全局 `OpenFileTable`,
//! 用户 fd 由 per-process `FdTable` 分配. 本模块只保留**机制面**:
//!
//! - 全局槽位表 `TFD_SLOTS` (静态存储, 内核生命周期) + 控制面 `TFD_LOCK`
//! - 嵌入槽位的 `HrTimer` 与到期回调
//! - 供对象层调用的安全槽位 API (分配 / 设时 / 取到期 / 轮询 / 释放)
//!
//! 槽位表留在 privileged 静态存储: 回调经静态数组指针反查槽位, 不持有对象
//! 引用; `hrtimer_cancel` 已是阻塞语义 (`Busy` 时等待在途回调退出), 但静态
//! 槽位仍保证 `HrTimer` 地址稳定、回调反查无需额外生命周期协议.
//!
//! ```text
//! TFD_SLOTS (全局静态数组)
//!   └── [TimerFdSlot; TFD_MAX_SLOTS]
//!         ├── timer: HrTimer            (嵌入 hrtimer 对象)
//!         ├── expiry_count: AtomicU64   (到期次数累计)
//!         ├── interval_ns: AtomicU64    (周期间隔, 0=单次)
//!         ├── armed: AtomicBool         (是否已启动)
//!         ├── arm_seq: AtomicU64        (当前激活序号)
//!         ├── used: AtomicBool
//!         └── wake_fd: AtomicI32        (对象挂表时登记的唤醒 fd, -1=未登记)
//! TFD_LOCK (IrqSpinLock<()>)             (只保护控制面复合操作, 回调不取)
//! ```
//!
//! epoll 集成: `expiry_count > 0` → `EPOLLIN`; 回调在槽位表锁释放后用
//! `epoll_pwake(wake_fd)` 唤醒等待者.
//!
//! # Safety
//!
//! - `TimerFdTable` 由 `IrqSpinLock` 保护
//! - `HrTimer` 回调在中断上下文执行 (不可睡眠), 且**直接不取槽位控制面锁**:
//!   槽位经静态数组指针反查, 计数/代际/间隔均为原子量; 回调只做计数、周期重挂
//!   与 epoll 唤醒. 注意 `epoll_pwake` 可能经 timerfd poll 间接取 `TFD_LOCK`,
//!   故 `set_time`/`free_slot` 在等待在途回调前必须先放 `TFD_LOCK`
//! - `HrTimer` 对象嵌入静态槽位, 生命周期与内核一致

#![deny(missing_docs)]

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, InitError, StartOutcome, hrtimer_cancel, hrtimer_clock_read,
    hrtimer_start,
};

// ============================================================================
// 公共类型
// ============================================================================

/// timerfd 最大实例数 (全局槽位表容量, 非 fd 编号空间)
pub const TFD_MAX_SLOTS: usize = 16;

/// 槽位时间读数 `(interval_ns, remaining_ns)`
pub type TimerTime = (u64, u64);

/// `set_time` 失败原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetTimeError {
    /// 槽位下标无效或槽位未使用
    InvalidSlot,
    /// 槽位存在但底层定时器无法进入 `Pending` (未就绪/已 shutdown/初始化失败)
    TimerUnavailable,
}

/// 到期计数出队结果
pub enum TakeExpiry {
    /// 有待消费的到期次数
    Count(u64),
    /// 无待消费事件 (非阻塞读 → `EAGAIN`)
    Empty,
    /// 槽位无效 (fd 已关闭 → `EBADF`)
    Invalid,
}

// ============================================================================
// 数据结构
// ============================================================================

/// timerfd 槽位
///
/// 回调只持 `&TimerFdSlot` (静态槽位地址), 故所有可变状态均为原子量: 槽位表
/// 锁只串行化控制面 (set/get/free), 回调不参与该锁域, 避免 `set_time` 持锁
/// 等待在途回调时与回调形成 AB-BA 死锁.
struct TimerFdSlot {
    /// 嵌入的 hrtimer 对象 (必须是首字段: 回调经 `core::ptr::eq` 在静态数组
    /// 中反查槽位, 不依赖偏移或锁)
    timer: HrTimer,
    /// 到期次数累计 (read 时清零)
    expiry_count: AtomicU64,
    /// 周期间隔 (纳秒, 0 = 单次)
    interval_ns: AtomicU64,
    /// 是否已启动 (armed)
    armed: AtomicBool,
    /// 当前激活序号 (`hrtimer_start` 分配的入队序号; 0 = 无有效激活)
    ///
    /// 回调携带入队时序号, 与此值不一致即说明槽位已释放/复用或已重新设时,
    /// 该回调属迟到条目, 不得计入当前实例.
    arm_seq: AtomicU64,
    /// 是否已使用
    used: AtomicBool,
    /// 控制面互斥 (`set_time`/`free_slot` 在放 `TFD_LOCK` 等待在途回调期间占位,
    /// 防止槽位被并发释放/复用; 回调与只读查询不取)
    control_busy: AtomicBool,
    /// 唤醒用数值 fd (对象挂表时登记; -1 = 未登记)
    wake_fd: AtomicI32,
}

impl TimerFdSlot {
    const fn new() -> Self {
        Self {
            timer: HrTimer::uninit(),
            expiry_count: AtomicU64::new(0),
            interval_ns: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            arm_seq: AtomicU64::new(0),
            used: AtomicBool::new(false),
            control_busy: AtomicBool::new(false),
            wake_fd: AtomicI32::new(-1),
        }
    }

    /// 复位槽位 (调用方必须已取消在途定时器)
    fn reset(&self) -> Result<(), InitError> {
        self.timer.try_init(timerfd_callback)?;
        self.expiry_count.store(0, Ordering::Release);
        self.interval_ns.store(0, Ordering::Release);
        self.armed.store(false, Ordering::Release);
        self.arm_seq.store(0, Ordering::Release);
        self.used.store(false, Ordering::Release);
        self.wake_fd.store(-1, Ordering::Release);
        Ok(())
    }
}

/// 槽位控制面占位守卫: 在 `TFD_LOCK` 外等待在途回调期间防止槽位被释放/复用.
struct SlotGuard {
    slot: &'static TimerFdSlot,
}

impl SlotGuard {
    fn acquire(slot: &'static TimerFdSlot) -> Self {
        while slot
            .control_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self { slot }
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.slot.control_busy.store(false, Ordering::Release);
    }
}

/// 全局 timerfd 槽位表 (静态存储, 生命周期与内核一致)
static TFD_SLOTS: [TimerFdSlot; TFD_MAX_SLOTS] = [const { TimerFdSlot::new() }; TFD_MAX_SLOTS];

/// 槽位控制面锁 (只保护 set/get/free 的复合操作; 回调直接不取, 但其
/// `epoll_pwake` 间接路径可能取, 故等待在途回调前必须放锁)
static TFD_LOCK: Mutex<()> = Mutex::new(());

// ============================================================================
// 槽位 API (供 `TimerFdFile` 对象调用)
// ============================================================================

/// 分配一个空闲槽位并初始化其 `HrTimer`; 表满或槽位初始化失败返回 `None`.
pub fn alloc_slot() -> Option<usize> {
    let _guard = TFD_LOCK.lock();
    for (idx, slot) in TFD_SLOTS.iter().enumerate() {
        if slot.used.load(Ordering::Acquire) {
            continue;
        }
        // `reset` 的运行时拒绝只在槽位仍有活动 timer 时发生; 这种槽位不可
        // 重新分配 (否则会覆盖在途回调的对象), 跳过继续找下一个空闲槽.
        if slot.reset().is_err() {
            continue;
        }
        slot.used.store(true, Ordering::Release);
        return Some(idx);
    }
    None
}

/// 登记唤醒用数值 fd (对象完成 `OpenFileTable` + per-process fd 表挂载后调用).
pub fn set_wake_fd(idx: usize, fd: i32) {
    let _guard = TFD_LOCK.lock();
    if let Some(slot) = TFD_SLOTS.get(idx) {
        slot.wake_fd.store(fd, Ordering::Release);
    }
}

/// 设置定时器, 返回设置前的 `(interval_ns, remaining_ns)`。
///
/// `value_ns == 0` 表示 disarm (与 Linux 一致). `abstime == true` 时
/// `value_ns` 视为绝对到期时刻 (与 `hrtimer_clock_read()` 同一单调时基).
///
/// # Errors
///
/// 槽位无效返回 [`SetTimeError::InvalidSlot`]；底层 timer 无法初始化或入队返回
/// [`SetTimeError::TimerUnavailable`]。后者的已置 armed 状态会回滚，不留半武装槽位。
pub fn set_time(
    idx: usize,
    value_ns: u64,
    interval_ns: u64,
    abstime: bool,
) -> Result<TimerTime, SetTimeError> {
    let Some(slot) = TFD_SLOTS.get(idx) else {
        return Err(SetTimeError::InvalidSlot);
    };
    // 控制面占位必须在取 TFD_LOCK 之前: 若先持表锁再等 control_busy, 另一
    // set_time/free_slot 可能正持 control_busy 等表锁 → AB-BA.
    let _busy = SlotGuard::acquire(slot);
    let mut guard = TFD_LOCK.lock();
    if !slot.used.load(Ordering::Acquire) {
        return Err(SetTimeError::InvalidSlot);
    }

    let now = hrtimer_clock_read();
    let remaining = if slot.armed.load(Ordering::Acquire) {
        slot.timer.expiry_ns().saturating_sub(now)
    } else {
        0
    };
    let old = (slot.interval_ns.load(Ordering::Acquire), remaining);

    if slot.armed.load(Ordering::Acquire) {
        // 关键: 放 TFD_LOCK 再等待. 回调虽不取 TFD_LOCK, 但它调用的
        // epoll_pwake → check_fd_ready → timerfd poll 会取 TFD_LOCK; 持锁等待
        // 会与该路径形成自死锁. control_busy 保证放锁期间槽位不被释放/复用.
        drop(guard);
        hrtimer_cancel(&slot.timer);
        guard = TFD_LOCK.lock();
        if !slot.used.load(Ordering::Acquire) {
            return Err(SetTimeError::InvalidSlot);
        }
        slot.armed.store(false, Ordering::Release);
        slot.arm_seq.store(0, Ordering::Release);
    }
    slot.expiry_count.store(0, Ordering::Release);

    if value_ns == 0 {
        slot.interval_ns.store(0, Ordering::Release);
        return Ok(old);
    }

    slot.interval_ns.store(interval_ns, Ordering::Release);
    // 取消后槽位可能停在 Inactive；try_init 同时在队列锁内拒绝 Pending/Running,
    // 失败时不得继续 start，也不能留下 armed=true 的半武装状态.
    if slot.timer.try_init(timerfd_callback).is_err() {
        slot.armed.store(false, Ordering::Release);
        slot.arm_seq.store(0, Ordering::Release);
        return Err(SetTimeError::TimerUnavailable);
    }
    let expiry_ns = if abstime {
        value_ns
    } else {
        now.saturating_add(value_ns)
    };
    // 先置 armed、arm_seq=0, 再入队: 若回调在 start 返回前抢到该激活, 允许它
    // 以 seq 认领 (arm_seq==0 + armed 是「新激活尚未回写序号」的唯一窗口).
    // 回写用 CAS, 避免覆盖回调周期重挂写入的更新序号.
    slot.arm_seq.store(0, Ordering::Release);
    slot.armed.store(true, Ordering::Release);
    match hrtimer_start(&slot.timer, expiry_ns) {
        StartOutcome::Started(seq) => {
            let _ = slot
                .arm_seq
                .compare_exchange(0, seq, Ordering::Release, Ordering::Relaxed);
            drop(guard);
            Ok(old)
        }
        StartOutcome::NotReady | StartOutcome::NotInitialized | StartOutcome::Shutdown => {
            slot.armed.store(false, Ordering::Release);
            slot.arm_seq.store(0, Ordering::Release);
            drop(guard);
            Err(SetTimeError::TimerUnavailable)
        }
    }
}

/// 读取当前 `(interval_ns, remaining_ns)`; 槽位无效返回 `None`.
pub fn get_time(idx: usize) -> Option<TimerTime> {
    let _guard = TFD_LOCK.lock();
    let slot = TFD_SLOTS.get(idx)?;
    if !slot.used.load(Ordering::Acquire) {
        return None;
    }
    let remaining = if slot.armed.load(Ordering::Acquire) {
        slot.timer.expiry_ns().saturating_sub(hrtimer_clock_read())
    } else {
        0
    };
    Some((slot.interval_ns.load(Ordering::Acquire), remaining))
}

/// 取出到期次数 (读后清零).
pub fn take_expiry(idx: usize) -> TakeExpiry {
    let _guard = TFD_LOCK.lock();
    let Some(slot) = TFD_SLOTS.get(idx) else {
        return TakeExpiry::Invalid;
    };
    if !slot.used.load(Ordering::Acquire) {
        return TakeExpiry::Invalid;
    }
    let count = slot.expiry_count.swap(0, Ordering::AcqRel);
    if count == 0 {
        return TakeExpiry::Empty;
    }
    TakeExpiry::Count(count)
}

/// 查询槽位是否就绪 (到期计数非空); 槽位无效返回 `None`.
pub fn poll_ready(idx: usize) -> Option<bool> {
    let _guard = TFD_LOCK.lock();
    let slot = TFD_SLOTS.get(idx)?;
    if !slot.used.load(Ordering::Acquire) {
        return None;
    }
    Some(slot.expiry_count.load(Ordering::Acquire) > 0)
}

/// 释放槽位 (最后一个 fd 引用关闭时调用).
pub fn free_slot(idx: usize) {
    let Some(slot) = TFD_SLOTS.get(idx) else {
        return;
    };
    let _busy = SlotGuard::acquire(slot);
    let used = {
        let _guard = TFD_LOCK.lock();
        slot.used.load(Ordering::Acquire)
    };
    if used {
        // 放 TFD_LOCK 等待: 回调经 epoll_pwake → timerfd poll 会取 TFD_LOCK.
        hrtimer_cancel(&slot.timer);
        let _guard = TFD_LOCK.lock();
        // 取消成功后 reset 理论上必成功。若运行时仍拒绝（有在途回调/状态未收敛），
        // 宁可把槽位保持 used 泄漏，也不能把它交给新对象覆盖活动 timer.
        let _ = slot.reset();
    }
}

// ============================================================================
// HrTimer 回调
// ============================================================================

/// timerfd 定时器回调
///
/// 在中断上下文执行: 代际校验 → 递增到期计数 → 周期重挂 (锁内) → 锁外
/// 唤醒 epoll. 槽位经 `timer` 地址在静态槽位表中反查 (不依赖裸指针偏移假设).
///
/// `seq` 为本次激活的入队序号. 槽位释放/复用或重新设时后, 旧激活的迟到
/// 回调携带的序号与 `arm_seq` 不一致, 直接丢弃 — 不得计入新实例.
fn timerfd_callback(timer: &HrTimer, seq: u64) -> HrTimerRestart {
    // 槽位表是静态数组, 回调只做指针反查, 不取 TFD_LOCK: 控制面可持锁等待
    // 在途回调 (hrtimer_cancel), 不会与回调形成 AB-BA.
    let Some(slot) = TFD_SLOTS
        .iter()
        .find(|s| core::ptr::eq(&raw const s.timer, timer))
    else {
        return HrTimerRestart::OneShot;
    };

    // 代际校验: 槽位释放/复用或重新设时后, 旧激活的迟到回调直接丢弃.
    // `arm_seq==0 && armed` 是新激活在 `hrtimer_start` 返回前被回调抢到的
    // 唯一窗口 (见 `set_time`): 认领该 seq, 不丢首个 tick.
    if !slot.used.load(Ordering::Acquire) {
        return HrTimerRestart::OneShot;
    }
    let arm_seq = slot.arm_seq.load(Ordering::Acquire);
    if arm_seq != 0 && arm_seq != seq {
        return HrTimerRestart::OneShot;
    }
    if arm_seq == 0 {
        if !slot.armed.load(Ordering::Acquire) {
            return HrTimerRestart::OneShot;
        }
        let _ = slot
            .arm_seq
            .compare_exchange(0, seq, Ordering::Release, Ordering::Relaxed);
    }

    slot.expiry_count.fetch_add(1, Ordering::Relaxed);
    let wake_fd = slot.wake_fd.load(Ordering::Acquire);
    let interval_ns = slot.interval_ns.load(Ordering::Acquire);

    if interval_ns == 0 {
        // 单次定时器到期后自动解除 armed
        slot.armed.store(false, Ordering::Release);
        slot.arm_seq.store(0, Ordering::Release);
    } else {
        // 周期定时器: 相对原到期时刻推进, 落后于当前时刻则从当前时刻重新计时.
        // 回调内自重挂 (hrtimer 允许), 并回写新激活序号供下一次回调校验.
        let now = hrtimer_clock_read();
        let next = timer.expiry_ns().saturating_add(interval_ns);
        let next = if next <= now {
            now.saturating_add(interval_ns)
        } else {
            next
        };
        let restart = hrtimer_start(timer, next);
        match restart {
            StartOutcome::Started(seq) => {
                slot.armed.store(true, Ordering::Release);
                slot.arm_seq.store(seq, Ordering::Release);
            }
            StartOutcome::NotReady | StartOutcome::NotInitialized | StartOutcome::Shutdown => {
                // 重挂失败（尤其 shutdown）: 必须解除 armed，不能留下
                // “读路径以为仍会到期”的假状态.
                slot.armed.store(false, Ordering::Release);
                slot.arm_seq.store(0, Ordering::Release);
            }
        }
    }

    if wake_fd >= 0 {
        crate::privileged::syscall::epoll::epoll_pwake(wake_fd);
    }

    HrTimerRestart::OneShot
}

// ============================================================================
// 单元测试 (host-side, 不依赖硬件时基)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 取槽位内 `HrTimer` 的裸指针 (回调经静态数组反查槽位).
    fn timer_ptr(idx: usize) -> *const HrTimer {
        core::ptr::from_ref(&TFD_SLOTS[idx].timer)
    }

    #[test]
    fn stale_callback_after_slot_reuse_is_ignored() {
        let idx = alloc_slot().expect("分配槽位");
        let old_seq = 7_u64;
        {
            let _guard = TFD_LOCK.lock();
            let slot = &TFD_SLOTS[idx];
            slot.armed.store(true, Ordering::Release);
            slot.interval_ns.store(0, Ordering::Release);
            slot.arm_seq.store(old_seq, Ordering::Release);
        }
        // SAFETY: 指针指向静态槽位表内的 `HrTimer`, 表生命周期覆盖进程;
        // 本测试独占该槽位, 期间无并发释放.
        let timer = unsafe { &*timer_ptr(idx) };
        let _ = timerfd_callback(timer, old_seq);
        assert!(matches!(take_expiry(idx), TakeExpiry::Count(1)));

        // 释放并复用槽位 (代际变更)
        free_slot(idx);
        let new_seq = 8_u64;
        {
            let _guard = TFD_LOCK.lock();
            let slot = &TFD_SLOTS[idx];
            slot.used.store(true, Ordering::Release);
            slot.armed.store(true, Ordering::Release);
            slot.interval_ns.store(0, Ordering::Release);
            slot.arm_seq.store(new_seq, Ordering::Release);
        }
        // SAFETY: 同上, 静态槽位指针在整个测试期间有效.
        let timer = unsafe { &*timer_ptr(idx) };
        // 旧代际的迟到回调不得计入新实例
        let _ = timerfd_callback(timer, old_seq);
        assert!(matches!(take_expiry(idx), TakeExpiry::Empty));
        // 新代际回调正常计数
        let _ = timerfd_callback(timer, new_seq);
        assert!(matches!(take_expiry(idx), TakeExpiry::Count(1)));
        free_slot(idx);
    }
}
