//! timerfd — 定时器槽位机制 (TCB)
//!
//! ## fd 对象模型 (阶段 3 第 6 个迁移子系统)
//!
//! 自 fd 对象模型阶段 3 起, timerfd 的 fd 编号与对象归属不再由本模块持有:
//! `TimerFdFile` 对象 (`functions::fs::timerfd`) 挂在全局 `OpenFileTable`,
//! 用户 fd 由 per-process `FdTable` 分配. 本模块只保留**机制面**:
//!
//! - 全局槽位表 `TFD_TABLE` (静态存储, 内核生命周期)
//! - 嵌入槽位的 `HrTimer` 与到期回调
//! - 供对象层调用的安全槽位 API (分配 / 设时 / 取到期 / 轮询 / 释放)
//!
//! 槽位表必须留在 privileged 静态存储: `hrtimer_cancel` 是异步的 (仅置
//! Inactive, 队列条目延迟到下次 `hrtimer_run_queues` 移除), 若 `HrTimer`
//! 随堆上 `Arc<dyn File>` 释放, 迟到回调会触发 use-after-free. 静态槽位
//! 保证 `HrTimer` 指针在整个内核生命周期有效.
//!
//! ```text
//! TimerFdTable (全局, IrqSpinLock 保护)
//!   └── [TimerFdSlot; TFD_MAX_SLOTS]
//!         ├── timer: HrTimer       (嵌入 hrtimer 对象)
//!         ├── expiry_count: u64    (到期次数累计)
//!         ├── interval_ns: u64     (周期间隔, 0=单次)
//!         ├── armed: bool          (是否已启动)
//!         ├── used: bool
//!         └── wake_fd: i32         (对象挂表时登记的唤醒 fd, -1=未登记)
//! ```
//!
//! epoll 集成: `expiry_count > 0` → `EPOLLIN`; 回调在槽位表锁释放后用
//! `epoll_pwake(wake_fd)` 唤醒等待者.
//!
//! # Safety
//!
//! - `TimerFdTable` 由 `IrqSpinLock` 保护
//! - `HrTimer` 回调在中断上下文执行 (不可睡眠); 回调内先取槽位表锁, 释放
//!   后再唤醒 epoll / 重新入队, 不嵌套其它锁
//! - `HrTimer` 对象嵌入静态槽位, 生命周期与内核一致

#![deny(missing_docs)]

use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, hrtimer_cancel, hrtimer_clock_read, hrtimer_start,
};

// ============================================================================
// 公共类型
// ============================================================================

/// timerfd 最大实例数 (全局槽位表容量, 非 fd 编号空间)
pub const TFD_MAX_SLOTS: usize = 16;

/// 槽位时间读数 `(interval_ns, remaining_ns)`
pub type TimerTime = (u64, u64);

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
struct TimerFdSlot {
    /// 嵌入的 hrtimer 对象
    timer: HrTimer,
    /// 到期次数累计 (read 时清零)
    expiry_count: u64,
    /// 周期间隔 (纳秒, 0 = 单次)
    interval_ns: u64,
    /// 是否已启动 (armed)
    armed: bool,
    /// 是否已使用
    used: bool,
    /// 唤醒用数值 fd (对象挂表时登记; -1 = 未登记)
    wake_fd: i32,
}

impl TimerFdSlot {
    const fn new() -> Self {
        Self {
            timer: HrTimer::uninit(),
            expiry_count: 0,
            interval_ns: 0,
            armed: false,
            used: false,
            wake_fd: -1,
        }
    }

    /// 复位槽位 (对象 `release()` 驱动)
    fn reset(&mut self) {
        self.timer.init(timerfd_callback);
        self.expiry_count = 0;
        self.interval_ns = 0;
        self.armed = false;
        self.used = false;
        self.wake_fd = -1;
    }
}

/// timerfd 全局表
struct TimerFdTable {
    slots: [TimerFdSlot; TFD_MAX_SLOTS],
}

impl TimerFdTable {
    const fn new() -> Self {
        Self {
            slots: [const { TimerFdSlot::new() }; TFD_MAX_SLOTS],
        }
    }
}

/// 全局 timerfd 表
static TFD_TABLE: Mutex<TimerFdTable> = Mutex::new(TimerFdTable::new());

// ============================================================================
// 槽位 API (供 `TimerFdFile` 对象调用)
// ============================================================================

/// 分配一个空闲槽位并初始化其 `HrTimer`; 表满返回 `None`.
pub fn alloc_slot() -> Option<usize> {
    let mut table = TFD_TABLE.lock();
    let (idx, slot) = table.slots.iter_mut().enumerate().find(|(_, s)| !s.used)?;
    slot.reset();
    slot.used = true;
    Some(idx)
}

/// 登记唤醒用数值 fd (对象完成 `OpenFileTable` + per-process fd 表挂载后调用).
pub fn set_wake_fd(idx: usize, fd: i32) {
    let mut table = TFD_TABLE.lock();
    if let Some(slot) = table.slots.get_mut(idx) {
        slot.wake_fd = fd;
    }
}

/// 设置定时器, 返回设置前的 `(interval_ns, remaining_ns)`; 槽位无效返回 `None`.
///
/// `value_ns == 0` 表示 disarm (与 Linux 一致). `abstime == true` 时
/// `value_ns` 视为绝对到期时刻 (与 `hrtimer_clock_read()` 同一单调时基).
pub fn set_time(idx: usize, value_ns: u64, interval_ns: u64, abstime: bool) -> Option<TimerTime> {
    let mut table = TFD_TABLE.lock();
    let slot = table.slots.get_mut(idx)?;
    if !slot.used {
        return None;
    }

    let now = hrtimer_clock_read();
    let remaining = if slot.armed {
        slot.timer.expiry_ns().saturating_sub(now)
    } else {
        0
    };
    let old = (slot.interval_ns, remaining);

    if slot.armed {
        hrtimer_cancel(&slot.timer);
        slot.armed = false;
    }
    slot.expiry_count = 0;

    if value_ns == 0 {
        slot.interval_ns = 0;
        return Some(old);
    }

    slot.interval_ns = interval_ns;
    slot.timer.init(timerfd_callback);
    let expiry_ns = if abstime {
        value_ns
    } else {
        now.saturating_add(value_ns)
    };
    hrtimer_start(&slot.timer, expiry_ns);
    slot.armed = true;
    Some(old)
}

/// 读取当前 `(interval_ns, remaining_ns)`; 槽位无效返回 `None`.
pub fn get_time(idx: usize) -> Option<TimerTime> {
    let table = TFD_TABLE.lock();
    let slot = table.slots.get(idx)?;
    if !slot.used {
        return None;
    }
    let remaining = if slot.armed {
        slot.timer.expiry_ns().saturating_sub(hrtimer_clock_read())
    } else {
        0
    };
    Some((slot.interval_ns, remaining))
}

/// 取出到期次数 (读后清零).
pub fn take_expiry(idx: usize) -> TakeExpiry {
    let mut table = TFD_TABLE.lock();
    let Some(slot) = table.slots.get_mut(idx) else {
        return TakeExpiry::Invalid;
    };
    if !slot.used {
        return TakeExpiry::Invalid;
    }
    if slot.expiry_count == 0 {
        return TakeExpiry::Empty;
    }
    let count = slot.expiry_count;
    slot.expiry_count = 0;
    TakeExpiry::Count(count)
}

/// 查询槽位是否就绪 (到期计数非空); 槽位无效返回 `None`.
pub fn poll_ready(idx: usize) -> Option<bool> {
    let table = TFD_TABLE.lock();
    let slot = table.slots.get(idx)?;
    if !slot.used {
        return None;
    }
    Some(slot.expiry_count > 0)
}

/// 释放槽位 (最后一个 fd 引用关闭时调用).
pub fn free_slot(idx: usize) {
    let mut table = TFD_TABLE.lock();
    let Some(slot) = table.slots.get_mut(idx) else {
        return;
    };
    if slot.used {
        slot.reset();
    }
}

// ============================================================================
// HrTimer 回调
// ============================================================================

/// timerfd 定时器回调
///
/// 在中断上下文执行: 递增到期计数, 唤醒 epoll; 周期定时器手动重新入队.
/// 槽位经 `timer` 地址在静态槽位表中反查 (不依赖裸指针偏移假设).
fn timerfd_callback(timer: &HrTimer) -> HrTimerRestart {
    let mut table = TFD_TABLE.lock();
    let Some(idx) = table
        .slots
        .iter()
        .position(|s| core::ptr::eq(&raw const s.timer, timer))
    else {
        return HrTimerRestart::OneShot;
    };
    let slot = &mut table.slots[idx];
    let wake_fd = slot.wake_fd;
    let interval_ns = slot.interval_ns;
    if slot.used {
        slot.expiry_count = slot.expiry_count.saturating_add(1);
        if interval_ns == 0 {
            // 单次定时器到期后自动解除 armed
            slot.armed = false;
        }
    }
    drop(table);

    if wake_fd >= 0 {
        crate::privileged::syscall::epoll::epoll_pwake(wake_fd);
    }

    // 周期定时器: 相对原到期时刻推进, 落后于当前时刻则从当前时刻重新计时
    if interval_ns > 0 {
        let now = hrtimer_clock_read();
        let next = timer.expiry_ns().saturating_add(interval_ns);
        let next = if next <= now {
            now.saturating_add(interval_ns)
        } else {
            next
        };
        hrtimer_start(timer, next);
    }

    HrTimerRestart::OneShot
}
