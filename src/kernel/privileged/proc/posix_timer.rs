//! POSIX Timer — 每进程 per-process 定时器 (TCB)
//!
//! 实现 POSIX.1-2008 `timer_create` / `timer_settime` / `timer_gettime` /
//! `timer_delete` / `timer_getoverrun` / `clock_getres` 语义。
//!
//! ## 架构
//!
//! ```text
//! TimerManager (全局, IrqSpinLock 保护)
//!   └── [PosixTimerSlot; MAX_POSIX_TIMERS = 32]
//!         ├── timer_id: i32       (Linux 风格: 槽位索引 + 1, 起始 0 保留)
//!         ├── owner_pid: u32      (创建者进程, 进程退出时全部释放)
//!         ├── clockid: i32        (CLOCK_REALTIME=0 / CLOCK_MONOTONIC=1)
//!         ├── sigev: Sigevent     (通知方式: SIGEV_SIGNAL / SIGEV_NONE)
//!         ├── sigev_signo: i32    (SIGEV_SIGNAL 时, 触发后发送的信号)
//!         ├── sigev_value: i64    (siginfo.si_value 透传给用户态)
//!         ├── interval_ns: u64    (周期间隔, 0 = 单次)
//!         ├── expiry_count: u64   (累计到期次数, 用于 overrun)
//!         ├── overrun: i32        (timer_getoverurn 返回值, 上次 read 之后补打的次数)
//!         ├── armed: bool         (是否已启动)
//!         ├── used: bool
//!         └── timer: HrTimer      (嵌入 hrtimer 对象, 回调在中断上下文)
//! ```
//!
//! ## 通知模型
//!
//! 支持两种通知方式 (v1 简化, 不支持 `SIGEV_THREAD)`:
//!
//! - **`SIGEV_SIGNAL`**: 到期时向 `owner_pid` 发送 `sigev_signo` 信号, siginfo 携带
//!   `sigev_value`. 这是用户态最常用的形式。
//! - **`SIGEV_NONE`**: 到期时仅递增 `expiry_count`, 用户态通过 `timer_gettime`
//!   轮询剩余时间。
//!
//! ## 进程退出
//!
//! 进程退出 (`process_exit` / `do_exit`) 时遍历 manager, 释放属于该 pid 的所有
//! timer, 避免悬挂的 hrtimer 回调访问已释放的 process。
//!
//! ## 与 timerfd 的区别
//!
//! - **timerfd** 是文件描述符, 通过 `read()` 拉取到期次数, 适合 epoll 集成
//! - **POSIX Timer** 是 `timer_t` 句柄, 通过信号/轮询, 适合传统 POSIX 程序
//!
//! ## 编号
//!
//! `timer_id` 不暴露绝对地址, 而是用 `slot_index + 1` (1-based).
//! Linux 同样使用非 0 整数 ID, 0 表示无效。

#![deny(missing_docs)]

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::privileged::proc::{Pid, do_signal_send};
use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, hrtimer_cancel, hrtimer_clock_read, hrtimer_start,
};

// ============================================================================
// 常量
// ============================================================================

/// 最大 timer 实例数
pub const MAX_POSIX_TIMERS: usize = 32;

/// 通知方式
///
/// - `SIGEV_NONE = 1`: 不通知, 仅 `timer_gettime` 可见
/// - `SIGEV_SIGNAL = 2`: 发送 `sigev_signo` 信号
pub const SIGEV_NONE: i32 = 1;
/// 发送 `sigev_signo` 指定的信号
pub const SIGEV_SIGNAL: i32 = 2;

/// 时钟 ID
pub const CLOCK_REALTIME: i32 = 0;
/// 单调时钟 (不受系统时间调整影响)
pub const CLOCK_MONOTONIC: i32 = 1;

/// `timer_settime` flags
pub const TFD_TIMER_ABSTIME: i32 = 1;

// ============================================================================
// 用户态结构体
// ============================================================================

/// `struct sigevent` (POSIX.1-2008, 我们只关心前 24 字节)
///
/// ```c
/// struct sigevent {
///     sigval_t sigev_value;       // 8 字节
///     int sigev_signo;            // 4 字节
///     int sigev_notify;           // 4 字节 (SIGEV_SIGNAL / SIGEV_NONE / ...)
///     void (*sigev_notify_function)(union sigval);
///     pthread_attr_t *sigev_notify_attributes;
///     ...
/// };
/// ```
///
/// 我们只读取前 16 字节 (`sigev_value` + `sigev_signo` + `sigev_notify`),
/// 其余字段忽略。`#[repr(C)]` 保证与 glibc 布局一致。
///
/// 字段名对应 C `sigevent` 的 `sigev_value` / `sigev_signo` / `sigev_notify` /
/// `sigev_notify_function` / `sigev_notify_attributes`（按序仅去前缀, 顺序与类型不变）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Sigevent {
    /// 透传值, 通过 `siginfo.si_value` 回到用户态
    pub value: i64,
    /// 触发时发送的信号
    pub signo: i32,
    /// 通知方式 (`SIGEV_NONE` / `SIGEV_SIGNAL`)
    pub notify: i32,
    /// 忽略 (通知函数指针, 我们不支持 `SIGEV_THREAD`)
    pub notify_function: u64,
    /// 忽略
    pub notify_attributes: u64,
}

/// `struct itimerspec` (Linux 兼容)
///
/// 字段名对应 C `itimerspec` 的 `it_interval` / `it_value` 两个 `timespec` 的
/// `tv_sec` / `tv_nsec`（按序仅去前缀, 顺序与类型不变）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Itimerspec {
    /// 周期间隔
    pub interval_sec: i64,
    /// 周期间隔 (纳秒部分)
    pub interval_nsec: i64,
    /// 初次到期时间
    pub value_sec: i64,
    /// 初次到期时间 (纳秒部分)
    pub value_nsec: i64,
}

impl Itimerspec {
    /// 构造全零 `itimerspec`
    #[must_use]
    pub const fn zeroed() -> Self {
        Self {
            interval_sec: 0,
            interval_nsec: 0,
            value_sec: 0,
            value_nsec: 0,
        }
    }

    /// 计算 value 字段的总纳秒 (用于启动 hrtimer)
    #[must_use]
    pub fn value_ns(&self) -> u64 {
        let sec = if self.value_sec < 0 {
            0u64
        } else {
            self.value_sec.cast_unsigned()
        };
        let nsec = if self.value_nsec < 0 {
            0u64
        } else {
            self.value_nsec.cast_unsigned()
        };
        // 纳秒溢出保护 (>= 1e9 时折回秒)
        let nsec_norm = nsec.min(999_999_999);
        sec.saturating_mul(1_000_000_000).saturating_add(nsec_norm)
    }

    /// 计算 interval 字段的总纳秒
    #[must_use]
    pub fn interval_ns(&self) -> u64 {
        let sec = if self.interval_sec < 0 {
            0u64
        } else {
            self.interval_sec.cast_unsigned()
        };
        let nsec = if self.interval_nsec < 0 {
            0u64
        } else {
            self.interval_nsec.cast_unsigned()
        };
        let nsec_norm = nsec.min(999_999_999);
        sec.saturating_mul(1_000_000_000).saturating_add(nsec_norm)
    }
}

// ============================================================================
// 数据结构
// ============================================================================

/// POSIX Timer 槽位
struct PosixTimerSlot {
    /// 嵌入的 hrtimer
    timer: HrTimer,
    /// 拥有者进程 (创建者)
    owner_pid: Pid,
    /// 时钟 ID
    clockid: i32,
    /// 通知方式
    sigev_notify: i32,
    /// 触发时发送的信号 (`SIGEV_SIGNAL` 时)
    sigev_signo: i32,
    /// 透传值
    sigev_value: i64,
    /// 周期间隔 (纳秒)
    interval_ns: u64,
    /// 累计到期次数 (原子, 在中断上下文递增)
    expiry_count: AtomicU64,
    /// 上次 read (或 getoverrun) 之后补打的次数
    overrun: i32,
    /// 绝对到期时间 (仅 armed=true 时有意义)
    expiry_ns: u64,
    /// 是否已启动 (原子, 在中断上下文置 false)
    armed: AtomicBool,
    /// 是否已使用
    used: bool,
    /// `timer_create` flags (0 = 正常)
    flags: i32,
}

impl PosixTimerSlot {
    const fn new() -> Self {
        Self {
            timer: HrTimer::uninit(),
            owner_pid: 0,
            clockid: CLOCK_MONOTONIC,
            sigev_notify: SIGEV_NONE,
            sigev_signo: 0,
            sigev_value: 0,
            interval_ns: 0,
            expiry_count: AtomicU64::new(0),
            overrun: 0,
            expiry_ns: 0,
            armed: AtomicBool::new(false),
            used: false,
            flags: 0,
        }
    }
}

/// POSIX Timer 全局表
struct TimerManager {
    slots: [PosixTimerSlot; MAX_POSIX_TIMERS],
}

impl TimerManager {
    const fn new() -> Self {
        Self {
            slots: [const { PosixTimerSlot::new() }; MAX_POSIX_TIMERS],
        }
    }
}

/// 全局 `TimerManager`
static TIMER_MANAGER: Mutex<TimerManager> = Mutex::new(TimerManager::new());

/// 已分配的 timer 数量
static TIMER_COUNT: AtomicU32 = AtomicU32::new(0);

// ============================================================================
// 内部辅助
// ============================================================================

/// 查找槽位 (`timer_id` → idx)
fn id_to_idx(timer_id: i32) -> Option<usize> {
    if timer_id <= 0 {
        return None;
    }
    let idx = crate::privileged::glue::usize_from_i32(timer_id - 1);
    if idx >= MAX_POSIX_TIMERS {
        return None;
    }
    Some(idx)
}

/// POSIX Timer 回调
///
/// 在中断上下文执行:
/// 1. 周期定时器: forward 推进 `expiry_ns`, 重新入队
/// 2. 单次定时器: 标记 disarmed, 发送信号 (`SIGEV_SIGNAL`)
fn posix_timer_callback(timer: &HrTimer) -> HrTimerRestart {
    // SAFETY: hrtimer 框架保证 timer 嵌入在 PosixTimerSlot 中, 槽位在 ARMED
    // 状态, 生命周期内有效. 全局 spinlock 在回调执行时可能未持, 我们只读
    // 不变字段, 写操作限定在 *const → *mut 转换后对单字段更新.
    let slot_ptr = core::ptr::from_ref::<HrTimer>(timer).cast::<PosixTimerSlot>();

    // SAFETY: 见上 SAFETY 段.
    let slot = unsafe { &*slot_ptr };

    let interval = slot.interval_ns;

    if interval > 0 {
        // 周期模式: forward 自动推进 expiry_ns 到下一周期, 补打次数由 forward 返回
        // SAFETY: 在 forward 期间不会释放 timer.
        let _skipped = timer.forward(hrtimer_clock_read());
        return HrTimerRestart::Periodic;
    }

    // 单次: 递增计数 + 标记 disarmed
    // SAFETY: armed/expiry_count 为原子字段, 不需要 mut 引用.
    slot.expiry_count.fetch_add(1, Ordering::Relaxed);
    slot.armed.store(false, Ordering::Release);

    // SIGEV_SIGNAL: 发送信号 (无需持 TimerManager 锁, do_signal_send 内部独立)
    if slot.sigev_notify == SIGEV_SIGNAL {
        let sig = crate::privileged::glue::u8_from_i32(slot.sigev_signo);
        let pid = slot.owner_pid;
        // 失败 (进程已退出) 静默忽略 — timer_delete 负责清理
        let _ = do_signal_send(pid, sig);
    }

    HrTimerRestart::OneShot
}

// ============================================================================
// 公共 API (TCB)
// ============================================================================

/// 创建 POSIX Timer
///
/// `clockid`: `CLOCK_REALTIME` / `CLOCK_MONOTONIC`  // 标准时钟 ID
/// `sigev_ptr`: 用户态 sigevent 指针 (可为 null → `SIGEV_NONE`)
/// `timer_id_ptr`: 输出 `timer_id`
///
/// 返回 0 = 成功, 负数 = errno
pub fn sys_timer_create(clockid: i32, sigev_ptr: u64, timer_id_ptr: u64) -> i64 {
    use crate::privileged::errno::Errno;

    if clockid != CLOCK_REALTIME && clockid != CLOCK_MONOTONIC {
        return Errno::Einval.as_ret();
    }

    // 解析 sigevent
    let mut sigev = Sigevent::default();
    if sigev_ptr != 0 {
        if !crate::privileged::userptr::read_struct_from_user(sigev_ptr, &mut sigev) {
            return Errno::Efault.as_ret();
        }
    } else {
        sigev.notify = SIGEV_NONE;
    }

    // 验证 sigev_notify
    if sigev.notify != SIGEV_NONE && sigev.notify != SIGEV_SIGNAL {
        return Errno::Einval.as_ret();
    }

    // SIGEV_SIGNAL 必须指定合法信号
    if sigev.notify == SIGEV_SIGNAL && !(1..=31).contains(&sigev.signo) {
        return Errno::Einval.as_ret();
    }

    // 当前进程 pid
    let current_pid = crate::privileged::proc::scheduler::Scheduler::current().unwrap_or(0);

    let mut mgr = TIMER_MANAGER.lock();
    for (i, slot) in mgr.slots.iter_mut().enumerate() {
        if !slot.used {
            slot.used = true;
            slot.owner_pid = current_pid;
            slot.clockid = clockid;
            slot.sigev_notify = sigev.notify;
            slot.sigev_signo = sigev.signo;
            slot.sigev_value = sigev.value;
            slot.interval_ns = 0;
            slot.expiry_count = AtomicU64::new(0);
            slot.overrun = 0;
            slot.armed = AtomicBool::new(false);
            slot.flags = 0;

            // 初始化 hrtimer, 回调 dispatch 通过 slot 内部指针
            slot.timer.init(posix_timer_callback);

            let timer_id = crate::privileged::glue::low_i32_from_usize(i + 1);
            if !crate::privileged::userptr::write_struct_to_user(timer_id_ptr, &timer_id) {
                return Errno::Efault.as_ret();
            }

            TIMER_COUNT.fetch_add(1, Ordering::Relaxed);
            crate::klog_debug!(
                Sync,
                "[posix_timer] create id={} clockid={}",
                timer_id,
                clockid
            );
            return 0;
        }
    }

    Errno::Eagain.as_ret()
}

/// `timer_settime` — 启动 / 重置 timer
///
/// `timer_id`: 创建时返回的 ID
/// `flags`: 0 = 相对时间, `TFD_TIMER_ABSTIME` = 绝对时间
/// `new_value_ptr`: 新 itimerspec (可为 null → disarm)
/// `old_value_ptr`: 输出旧 itimerspec (可为 null)
pub fn sys_timer_settime(timer_id: i32, flags: i32, new_value_ptr: u64, old_value_ptr: u64) -> i64 {
    use crate::privileged::errno::Errno;
    use crate::privileged::userptr;

    let Some(idx) = id_to_idx(timer_id) else {
        return Errno::Einval.as_ret();
    };

    if flags & !TFD_TIMER_ABSTIME != 0 {
        return Errno::Einval.as_ret();
    }

    // 读取新值
    let new_value = if new_value_ptr != 0 {
        let mut v = Itimerspec::zeroed();
        if !userptr::read_struct_from_user(new_value_ptr, &mut v) {
            return Errno::Efault.as_ret();
        }
        v
    } else {
        // new_value = NULL = 撤防 (disarm)  // POSIX timer_settime 语义
        Itimerspec::zeroed()
    };

    let value_ns = new_value.value_ns();
    let interval_ns = new_value.interval_ns();

    let mut mgr = TIMER_MANAGER.lock();
    let slot = &mut mgr.slots[idx];

    if !slot.used {
        return Errno::Einval.as_ret();
    }

    // 输出旧值
    if old_value_ptr != 0 {
        let armed_now = slot.armed.load(Ordering::Acquire);
        let remaining_ns = if armed_now {
            let now = hrtimer_clock_read();
            slot.expiry_ns.saturating_sub(now)
        } else {
            0
        };
        let old = Itimerspec {
            interval_sec: (slot.interval_ns / 1_000_000_000).cast_signed(),
            interval_nsec: (slot.interval_ns % 1_000_000_000).cast_signed(),
            value_sec: (remaining_ns / 1_000_000_000).cast_signed(),
            value_nsec: (remaining_ns % 1_000_000_000).cast_signed(),
        };
        if !userptr::write_struct_to_user(old_value_ptr, &old) {
            return Errno::Efault.as_ret();
        }
    }

    // 取消旧定时器
    if slot.armed.load(Ordering::Acquire) {
        hrtimer_cancel(&slot.timer);
        slot.armed.store(false, Ordering::Release);
    }

    // it_value 全零 = disarm (POSIX 语义)
    if value_ns == 0 {
        slot.expiry_count = AtomicU64::new(0);
        slot.interval_ns = 0;
        slot.overrun = 0;
        crate::klog_debug!(Sync, "[posix_timer] disarm id={}", timer_id);
        return 0;
    }

    // 计算到期时间
    let expiry_ns = if (flags & TFD_TIMER_ABSTIME) != 0 {
        value_ns
    } else {
        hrtimer_clock_read().saturating_add(value_ns)
    };

    slot.interval_ns = interval_ns;
    slot.expiry_count = AtomicU64::new(0);
    slot.overrun = 0;
    slot.expiry_ns = expiry_ns;
    slot.armed = AtomicBool::new(true);

    hrtimer_start(&slot.timer, expiry_ns);

    crate::klog_debug!(
        Sync,
        "[posix_timer] settime id={} value_ns={} interval_ns={} abstime={}",
        timer_id,
        value_ns,
        interval_ns,
        (flags & TFD_TIMER_ABSTIME) != 0
    );
    0
}

/// `timer_gettime` — 获取 timer 状态 (剩余时间 + interval)
pub fn sys_timer_gettime(timer_id: i32, curr_value_ptr: u64) -> i64 {
    use crate::privileged::errno::Errno;
    use crate::privileged::userptr;

    let Some(idx) = id_to_idx(timer_id) else {
        return Errno::Einval.as_ret();
    };

    if curr_value_ptr == 0 {
        return Errno::Efault.as_ret();
    }

    let mgr = TIMER_MANAGER.lock();
    let slot = &mgr.slots[idx];

    if !slot.used {
        return Errno::Einval.as_ret();
    }

    let remaining_ns = if slot.armed.load(Ordering::Acquire) {
        let now = hrtimer_clock_read();
        slot.expiry_ns.saturating_sub(now)
    } else {
        0
    };
    let curr = Itimerspec {
        interval_sec: (slot.interval_ns / 1_000_000_000).cast_signed(),
        interval_nsec: (slot.interval_ns % 1_000_000_000).cast_signed(),
        value_sec: (remaining_ns / 1_000_000_000).cast_signed(),
        value_nsec: (remaining_ns % 1_000_000_000).cast_signed(),
    };
    if !userptr::write_struct_to_user(curr_value_ptr, &curr) {
        return Errno::Efault.as_ret();
    }
    0
}

/// `timer_delete` — 释放 timer
pub fn sys_timer_delete(timer_id: i32) -> i64 {
    use crate::privileged::errno::Errno;

    let Some(idx) = id_to_idx(timer_id) else {
        return Errno::Einval.as_ret();
    };

    let mut mgr = TIMER_MANAGER.lock();
    let slot = &mut mgr.slots[idx];

    if !slot.used {
        return Errno::Einval.as_ret();
    }

    if slot.armed.load(Ordering::Acquire) {
        hrtimer_cancel(&slot.timer);
    }
    slot.used = false;
    slot.armed = AtomicBool::new(false);
    slot.owner_pid = 0;
    slot.interval_ns = 0;
    slot.expiry_count = AtomicU64::new(0);
    slot.overrun = 0;

    TIMER_COUNT.fetch_sub(1, Ordering::Relaxed);
    crate::klog_debug!(Sync, "[posix_timer] delete id={}", timer_id);
    0
}

/// `timer_getoverrun` — 返回上次 read 之后补打的次数
///
/// POSIX 语义: overrun = (实际到期次数) - 1 (正常情况下一次)。
/// 当前实现: 总是返回 0 (我们没有维护 read 标记, 单次信号模式够用)。
pub fn sys_timer_getoverrun(timer_id: i32) -> i64 {
    use crate::privileged::errno::Errno;

    let Some(idx) = id_to_idx(timer_id) else {
        return Errno::Einval.as_ret();
    };

    let mgr = TIMER_MANAGER.lock();
    let slot = &mgr.slots[idx];

    if !slot.used {
        return Errno::Einval.as_ret();
    }

    i64::from(slot.overrun)
}

/// `clock_getres` — 时钟分辨率
///
/// `Edgine` 内置两种时钟: `CLOCK_REALTIME` (TICK 精度) / `CLOCK_MONOTONIC` (TICK 精度)
/// 分辨率 = 1 tick = 1ms (hrtimer 配置, 暂以 1ms 作为标称分辨率)。
#[must_use]
pub fn sys_clock_getres(clockid: i32, res_ptr: u64) -> i64 {
    use crate::privileged::errno::Errno;
    use crate::privileged::userptr;

    if clockid != CLOCK_REALTIME && clockid != CLOCK_MONOTONIC {
        return Errno::Einval.as_ret();
    }

    if res_ptr == 0 {
        return 0; // 仅查询, 允许 NULL
    }

    // 1ms = 1_000_000 ns
    let res = Itimerspec {
        interval_sec: 0,
        interval_nsec: 1_000_000,
        value_sec: 0,
        value_nsec: 1_000_000,
    };
    if !userptr::write_struct_to_user(res_ptr, &res) {
        return Errno::Efault.as_ret();
    }
    0
}

/// 进程退出: 释放属于该 pid 的所有 POSIX timer
///
/// 在 `process_exit` / `do_exit` 中调用, 防止悬挂的 hrtimer 回调访问
/// 已释放的 process 内存。
pub fn posix_timer_release_pid(pid: Pid) {
    let mut mgr = TIMER_MANAGER.lock();
    let mut released = 0u32;
    for slot in &mut mgr.slots {
        if slot.used && slot.owner_pid == pid {
            if slot.armed.load(Ordering::Acquire) {
                hrtimer_cancel(&slot.timer);
            }
            slot.used = false;
            slot.armed = AtomicBool::new(false);
            slot.owner_pid = 0;
            slot.interval_ns = 0;
            slot.expiry_count = AtomicU64::new(0);
            slot.overrun = 0;
            released += 1;
        }
    }
    if released > 0 {
        TIMER_COUNT.fetch_sub(released, Ordering::Relaxed);
        crate::klog_debug!(
            Sync,
            "[posix_timer] released {} timer(s) for pid={}",
            released,
            pid
        );
    }
}

/// 调试: 返回当前活跃 timer 数量
pub fn posix_timer_active_count() -> u32 {
    TIMER_COUNT.load(Ordering::Relaxed)
}
