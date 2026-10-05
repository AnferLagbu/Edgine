//! timerfd — 定时器文件描述符 (TCB)
//!
//! 实现 Linux timerfd API: `timerfd_create` / `timerfd_settime` / `timerfd_gettime`.
//!
//! ## 架构
//!
//! ```text
//! TimerFdTable (全局, IrqSpinLock 保护)
//!   └── [TimerFdSlot; TFD_MAX_SLOTS]
//!         ├── timer: HrTimer       (嵌入 hrtimer 对象)
//!         ├── expiry_count: u64    (到期次数累计)
//!         ├── interval_ns: u64     (周期间隔, 0=单次)
//!         ├── clockid: i32         (CLOCK_MONOTONIC / CLOCK_REALTIME)
//!         ├── armed: bool          (是否已启动)
//!         └── used: bool
//!
//! FD 空间: [240, 240 + TFD_MAX_SLOTS)
//!
//! read(fd): 返回 expiry_count 并清零; 未到期 → EAGAIN
//!
//! timerfd_settime: 设置到期时间和间隔, 启动 hrtimer
//! timerfd_gettime: 获取剩余时间和间隔
//!
//! epoll 集成: expiry_count > 0 → EPOLLIN
//! ```
//!
//! # Safety
//!
//! - `TimerFdTable` 由 `IrqSpinLock` 保护
//! - `HrTimer` 回调在中断上下文执行, 仅递增 `expiry_count` 和唤醒 epoll
//! - `HrTimer` 对象嵌入 `TimerFdSlot`, 生命周期与槽位一致

use core::sync::atomic::{AtomicU32, Ordering};

use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::syscall::Errno;
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, hrtimer_cancel, hrtimer_clock_read, hrtimer_start,
};

// ============================================================================
// 常量
// ============================================================================

/// timerfd 最大实例数
pub const TFD_MAX_SLOTS: usize = 16;
/// TD-15: FD 空间基址来源已迁移至 `privileged::proc::FdPlan::TIMER_FD` 单一来源 (1160),
/// 不再硬编码 240 (旧值与 smoltcp [0, 256) 重叠).
pub const TFD_FD_BASE: i32 = crate::privileged::proc::FdPlan::TIMER_FD.base;
/// `TFD_CLOEXEC`
pub const TFD_CLOEXEC: i32 = 0o2000000;
/// `TFD_NONBLOCK`
pub const TFD_NONBLOCK: i32 = 0o4000;
/// `TFD_TIMER_ABSTIME`
pub const TFD_TIMER_ABSTIME: i32 = 1;

/// `CLOCK_MONOTONIC`
pub const CLOCK_MONOTONIC: i32 = 1;
/// `CLOCK_REALTIME`
pub const CLOCK_REALTIME: i32 = 0;

/// itimerspec 结构体 (与 Linux 兼容)
#[repr(C)]
pub struct Itimerspec {
    /// 间隔时间
    pub it_interval_sec: i64,
    pub it_interval_nsec: i64,
    /// 初次到期时间
    pub it_value_sec: i64,
    pub it_value_nsec: i64,
}

impl Itimerspec {
    pub const fn zeroed() -> Self {
        Self {
            it_interval_sec: 0,
            it_interval_nsec: 0,
            it_value_sec: 0,
            it_value_nsec: 0,
        }
    }
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
    /// 周期间隔 (纳秒)
    interval_ns: u64,
    /// 时钟 ID
    clockid: i32,
    /// 是否已启动 (armed)
    armed: bool,
    /// 是否已使用
    used: bool,
    /// 对应的 fd (回调中需要)
    fd: i32,
}

impl TimerFdSlot {
    const fn new() -> Self {
        Self {
            timer: HrTimer::uninit(),
            expiry_count: 0,
            interval_ns: 0,
            clockid: CLOCK_MONOTONIC,
            armed: false,
            used: false,
            fd: 0,
        }
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

/// 已分配的 timerfd 数量
static TFD_COUNT: AtomicU32 = AtomicU32::new(0);

// ============================================================================
// 系统调用实现
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `timerfd_create` — 创建 timerfd 实例
///
/// `clockid`: `CLOCK_MONOTONIC` 或 `CLOCK_REALTIME`
/// `flags`: `TFD_CLOEXEC` | `TFD_NONBLOCK`
/// 返回 fd (≥ 240), 或负 errno
pub fn sys_timerfd_create(clockid: i32, flags: i32) -> i64 {
    // clockid 校验
    if clockid != CLOCK_MONOTONIC && clockid != CLOCK_REALTIME {
        return Errno::EINVAL.as_ret();
    }

    // flags 校验
    let valid_flags = TFD_CLOEXEC | TFD_NONBLOCK;
    if flags & !valid_flags != 0 {
        return Errno::EINVAL.as_ret();
    }

    // V2: 使用集中分配器获取 FD
    let fd = match crate::privileged::proc::fd_alloc::alloc_fd(
        crate::privileged::proc::fd_alloc::FdSubsystem::TimerFd,
    ) {
        Some(f) => f,
        None => return Errno::EMFILE.as_ret(),
    };

    let slot = match crate::privileged::proc::fd_alloc::idx_of(fd) {
        Some((_sub, s)) => s,
        None => return Errno::EBADF.as_ret(),
    };

    let mut table = TFD_TABLE.lock();
    let s = &mut table.slots[slot];
    s.used = true;
    s.fd = fd;
    s.clockid = clockid;
    s.armed = false;
    s.expiry_count = 0;
    s.interval_ns = 0;

    // 初始化 HrTimer, 回调使用 slot index 编码
    // SAFETY: timer 嵌入在 slot 中, 生命周期与 slot 一致
    s.timer.init(timerfd_callback);

    TFD_COUNT.fetch_add(1, Ordering::Relaxed);

    crate::klog_debug!(Sync, "[timerfd] Created fd={} clockid={}", fd, clockid);
    i64::from(fd)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `timerfd_settime` — 设置定时器
///
/// `fd`: timerfd 文件描述符
/// `flags`: 0 或 `TFD_TIMER_ABSTIME`
/// `new_value_ptr`: 指向 itimerspec 的用户空间指针
/// `old_value_ptr`: 指向旧 itimerspec 的用户空间指针 (可为 null)
/// 返回 0 或负 errno
pub fn sys_timerfd_settime(fd: i32, flags: i32, new_value_ptr: u64, old_value_ptr: u64) -> i64 {
    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    // flags 校验
    if flags & !TFD_TIMER_ABSTIME != 0 {
        return Errno::EINVAL.as_ret();
    }

    if new_value_ptr == 0 {
        return Errno::EFAULT.as_ret();
    }

    // 读取新值
    // SAFETY: new_value_ptr 由 syscall 入口验证
    let new_value = unsafe { core::ptr::read(new_value_ptr as *const Itimerspec) };

    // 校验: it_value 必须非零才能启动 (it_value 全零 = disarm)
    let value_ns = new_value.it_value_sec as u64 * 1_000_000_000 + new_value.it_value_nsec as u64;
    let interval_ns =
        new_value.it_interval_sec as u64 * 1_000_000_000 + new_value.it_interval_nsec as u64;

    let mut table = TFD_TABLE.lock();
    let slot = &mut table.slots[idx];

    if !slot.used {
        return Errno::EBADF.as_ret();
    }

    // 保存旧值到 old_value_ptr
    if old_value_ptr != 0 {
        let remaining = if slot.armed {
            let now = hrtimer_clock_read();
            slot.timer.expiry_ns().saturating_sub(now)
        } else {
            0
        };
        let old = Itimerspec {
            it_interval_sec: (slot.interval_ns / 1_000_000_000) as i64,
            it_interval_nsec: (slot.interval_ns % 1_000_000_000) as i64,
            it_value_sec: (remaining / 1_000_000_000) as i64,
            it_value_nsec: (remaining % 1_000_000_000) as i64,
        };
        // SAFETY: old_value_ptr 由 syscall 入口验证
        unsafe {
            core::ptr::write(old_value_ptr as *mut Itimerspec, old);
        }
    }

    // 取消旧定时器
    if slot.armed {
        hrtimer_cancel(&slot.timer);
        slot.armed = false;
    }

    // it_value 全零 = disarm
    if value_ns == 0 {
        slot.expiry_count = 0;
        slot.interval_ns = 0;
        crate::klog_debug!(Sync, "[timerfd] Disarm fd={}", fd);
        return 0;
    }

    // 启动新定时器
    slot.interval_ns = interval_ns;
    slot.expiry_count = 0;

    let expiry_ns = if (flags & TFD_TIMER_ABSTIME) != 0 {
        value_ns // 绝对时间
    } else {
        hrtimer_clock_read() + value_ns // 相对时间
    };

    if interval_ns > 0 {
        // 周期定时器: 使用 hrtimer_start_periodic 语义
        // 但我们需要自定义回调行为 (递增 expiry_count 而非重新入队)
        // 所以手动设置 interval 并启动
        slot.timer.init(timerfd_callback);
        hrtimer_start(&slot.timer, expiry_ns);
        // 注意: hrtimer 周期重启在回调中处理
    } else {
        slot.timer.init(timerfd_callback);
        hrtimer_start(&slot.timer, expiry_ns);
    }

    slot.armed = true;

    crate::klog_debug!(
        Sync,
        "[timerfd] Settime fd={} value_ns={} interval_ns={} abstime={}",
        fd,
        value_ns,
        interval_ns,
        (flags & TFD_TIMER_ABSTIME) != 0
    );
    0
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `timerfd_gettime` — 获取定时器状态
///
/// `fd`: timerfd 文件描述符
/// `curr_value_ptr`: 指向 itimerspec 的用户空间指针
/// 返回 0 或负 errno
pub fn sys_timerfd_gettime(fd: i32, curr_value_ptr: u64) -> i64 {
    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    if curr_value_ptr == 0 {
        return Errno::EFAULT.as_ret();
    }

    let table = TFD_TABLE.lock();
    let slot = &table.slots[idx];

    if !slot.used {
        return Errno::EBADF.as_ret();
    }

    let remaining = if slot.armed {
        let now = hrtimer_clock_read();
        slot.timer.expiry_ns().saturating_sub(now)
    } else {
        0
    };
    let curr = Itimerspec {
        it_interval_sec: (slot.interval_ns / 1_000_000_000) as i64,
        it_interval_nsec: (slot.interval_ns % 1_000_000_000) as i64,
        it_value_sec: (remaining / 1_000_000_000) as i64,
        it_value_nsec: (remaining % 1_000_000_000) as i64,
    };

    // SAFETY: curr_value_ptr 由 syscall 入口验证
    unsafe {
        core::ptr::write(curr_value_ptr as *mut Itimerspec, curr);
    }

    0
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// timerfd read — 读取到期次数
///
/// `fd`: timerfd 文件描述符
/// `buf`: 用户空间 8 字节缓冲区
/// 返回 8 (成功), 或负 errno
pub fn sys_timerfd_read(fd: i32, buf: u64) -> i64 {
    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    let mut table = TFD_TABLE.lock();
    let slot = &mut table.slots[idx];

    if !slot.used {
        return Errno::EBADF.as_ret();
    }

    if slot.expiry_count == 0 {
        return Errno::EAGAIN.as_ret();
    }

    let count = slot.expiry_count;
    slot.expiry_count = 0;

    drop(table);

    if buf == 0 {
        return Errno::EFAULT.as_ret();
    }
    // SAFETY: buf 由 syscall 入口验证
    unsafe {
        core::ptr::write(buf as *mut u64, count);
    }

    crate::klog_debug!(Sync, "[timerfd] Read fd={} count={}", fd, count);
    8
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// timerfd close — 关闭 timerfd
pub fn sys_timerfd_close(fd: i32) -> i64 {
    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    let mut table = TFD_TABLE.lock();
    let slot = &mut table.slots[idx];

    if !slot.used {
        return Errno::EBADF.as_ret();
    }

    if slot.armed {
        hrtimer_cancel(&slot.timer);
    }

    slot.used = false;
    slot.armed = false;
    slot.expiry_count = 0;
    slot.interval_ns = 0;
    slot.fd = 0;
    TFD_COUNT.fetch_sub(1, Ordering::Relaxed);

    crate::klog_debug!(Sync, "[timerfd] Closed fd={}", fd);
    0
}

// ============================================================================
// HrTimer 回调
// ============================================================================

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// timerfd 定时器回调
///
/// 在中断上下文执行: 递增 `expiry_count`, 唤醒 epoll.
/// 周期定时器: 手动重新入队.
fn timerfd_callback(timer: &HrTimer) -> HrTimerRestart {
    // 从 timer 指针反推 slot index
    // HrTimer 嵌入在 TimerFdSlot 中, 偏移量为 0 (第一个字段)
    let slot_ptr = timer as *const HrTimer as *const TimerFdSlot;
    // SAFETY: timer 嵌入在 slot 中, 指针有效
    let slot = unsafe { &*slot_ptr };

    let fd = slot.fd;
    let interval_ns = slot.interval_ns;

    // 递增 expiry_count (需要锁保护, 因为 read 路径也访问)
    // 但回调在中断上下文, 与 read 路径的锁可能冲突
    // 使用 try_lock 避免死锁: 如果锁被持有了, 说明 read 正在进行,
    // 我们在中断上下文不能等待, 直接递增 (原子性由中断禁用保证)
    //
    // 简化方案: expiry_count 使用原子操作
    // 但 TimerFdSlot 不是原子字段... 需要调整
    //
    // 实际方案: 在中断上下文中, IrqSpinLock 是安全的 (不会睡眠)
    {
        let mut table = TFD_TABLE.lock();
        // 重新定位 slot (因为锁释放后 table 可能被移动? 不, 是静态的)
        let idx = match fd_to_idx(fd) {
            Some(i) => i,
            None => return HrTimerRestart::OneShot,
        };
        table.slots[idx].expiry_count += 1;
    }

    // 唤醒 epoll
    crate::privileged::syscall::epoll::epoll_pwake(fd);

    // 周期定时器: 重新入队
    if interval_ns > 0 {
        let now = hrtimer_clock_read();
        let old_expiry = timer.expiry_ns();
        let next_expiry = old_expiry + interval_ns;
        let next = if next_expiry <= now {
            now + interval_ns
        } else {
            next_expiry
        };
        hrtimer_start(timer, next);
        // 不通过 HrTimerRestart::Periodic, 因为我们需要自定义 expiry_count 递增
        HrTimerRestart::OneShot // 我们已手动重新入队
    } else {
        // 单次定时器
        // SAFETY: slot 在 table 中, 生命周期由 table 管理
        let mut table = TFD_TABLE.lock();
        if let Some(idx) = fd_to_idx(fd) {
            table.slots[idx].armed = false;
        }
        HrTimerRestart::OneShot
    }
}

// ============================================================================
// epoll 集成
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 检查 timerfd 是否就绪 (供 epoll `check_fd_ready` 调用)
///
/// 返回 EPOLLIN (有到期事件) 或 0
pub fn timerfd_poll_events(fd: i32) -> u32 {
    use crate::privileged::syscall::{EPOLLERR, EPOLLIN};

    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return EPOLLERR,
    };

    let table = TFD_TABLE.lock();
    let slot = &table.slots[idx];

    if !slot.used {
        return EPOLLERR;
    }

    if slot.expiry_count > 0 { EPOLLIN } else { 0 }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// fd → 槽位索引
///
/// TD-15: 改走 `fd_alloc::idx_of` 集中反查, 本地不再持有 `TFD_FD_BASE` 字面量 +
/// 减法边界检查.
fn fd_to_idx(fd: i32) -> Option<usize> {
    match crate::privileged::proc::idx_of(fd) {
        Some((crate::privileged::proc::FdSubsystem::TimerFd, slot)) => Some(slot),
        _ => None,
    }
}

/// 检查 fd 是否属于 timerfd 空间
///
/// TD-15: 改走 `fd_alloc::idx_of`, 不再持有 `TFD_FD_BASE` 字面量 + 算术.
pub fn is_timerfd_fd(fd: i32) -> bool {
    matches!(
        crate::privileged::proc::idx_of(fd),
        Some((crate::privileged::proc::FdSubsystem::TimerFd, _))
    )
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_timerfd_create() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    let fd = sys_timerfd_create(CLOCK_MONOTONIC, 0);
    check!(fd >= 240, "timerfd returns fd >= 240");

    let fd2 = sys_timerfd_create(99, 0); // 无效 clockid
    check!(fd2 < 0, "timerfd invalid clockid returns error");

    sys_timerfd_close(fd as i32);
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_timerfd_settime_disarm() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    let fd = sys_timerfd_create(CLOCK_MONOTONIC, 0);
    check!(fd >= 240, "timerfd create ok");

    // disarm: it_value 全零
    let new_val = Itimerspec::zeroed();
    let ret = sys_timerfd_settime(fd as i32, 0, (&raw const new_val) as u64, 0);
    check!(ret == 0, "timerfd disarm ok");

    sys_timerfd_close(fd as i32);
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_timerfd_read_empty() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    let fd = sys_timerfd_create(CLOCK_MONOTONIC, 0);
    check!(fd >= 240, "timerfd create ok");

    // 未启动时 read → EAGAIN
    let mut val: u64 = 0;
    let ret = sys_timerfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret < 0, "timerfd read unarmed returns error");

    sys_timerfd_close(fd as i32);
    TestResult::Pass
}

/// 注册 timerfd 子系统的内核测试用例.
#[cfg(feature = "kernel_test")]
pub fn register_timerfd_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("timerfd", "create", test_timerfd_create);
    r.register("timerfd", "settime_disarm", test_timerfd_settime_disarm);
    r.register("timerfd", "read_empty", test_timerfd_read_empty);
}
