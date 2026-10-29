//! `ITIMER_REAL` / `alarm(2)` — 每进程实时间隔定时器 (hrtimer 驱动)
//!
//! ## 设计立场
//!
//! 本模块把 POSIX `alarm(2)` 与 `setitimer(ITIMER_REAL)` 统一到 **一个 per-pid
//! 槽位**: 与 Linux 一致, `alarm(n)` 等价于 `setitimer(ITIMER_REAL, {value=n,
//! interval=0})`, 二者互相覆盖 (先前的 `alarm` 会被 `setitimer` 撤防, 反之亦然).
//!
//! 到期时刻不再记账在 `Process` 的 jiffies 字段上、由调度器 tick 全表轮询, 而是
//! 直接向 hrtimer 框架登记**绝对纳秒截止期**; 到期即在 hardirq 上下文经
//! `do_signal_send(pid, SIGALRM)` 投递信号. 这是「内核绝对截止期统一由 hrtimer
//! 驱动」的机制主张落地 (见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §S-3/P-7),
//! 与 `sleep.rs` / `posix_timer.rs` / `timerfd` 共用 `hrtimer_run_queues()` 单一
//! 驱动源, 从而消除 jiffies 时基假设 (100Hz/1000Hz 混淆) 与每 tick 的 O(N) 扫描.
//!
//! ## 数据结构
//!
//! 槽表按 pid 直接索引 (pid 空间上限 = `MAX_PROCESSES`), 每进程恰好一个槽,
//! 无需分配扫描; `used` 标记该槽是否有归属进程.
//!
//! ## 继承语义
//!
//! `fork` 不复制 (子进程 pid 的槽位天然为空), `execve` 保留 (pid 不变 ⇒ 槽位不变),
//! 与 Linux 一致.
//!
//! ## 生命周期
//!
//! 进程终止 (`Scheduler::exit`) 时经 [`itimer_real_release_pid`] 撤防, 防止 pid
//! 复用后收到前一进程遗留的 `SIGALRM`.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::privileged::proc::{MAX_PROCESSES, Pid, SIGALRM, do_signal_send};
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::timer::{
    HrTimer, HrTimerRestart, hrtimer_cancel, hrtimer_clock_read, hrtimer_start,
    hrtimer_start_periodic,
};

/// 每秒纳秒数 (秒 ↔ hrtimer 纳秒时基的换算因子)
const NS_PER_SEC: u64 = 1_000_000_000;

/// 单进程的 `ITIMER_REAL` 槽位
struct ItimerSlot {
    /// 嵌入的 hrtimer (回调经 `container_of` 反推本槽)
    timer: HrTimer,
    /// 槽位归属进程 (`used` 为真时有效)
    pid: Pid,
    /// 周期间隔 (纳秒, 0 = 单次触发)
    interval_ns: u64,
    /// 是否处于设防状态 (中断上下文到期时置 `false`)
    armed: AtomicBool,
    /// 槽位是否被占用
    used: bool,
}

impl ItimerSlot {
    /// 常量构造 (供静态表初始化)
    const fn new() -> Self {
        Self {
            timer: HrTimer::uninit(),
            pid: 0,
            interval_ns: 0,
            armed: AtomicBool::new(false),
            used: false,
        }
    }
}

/// 全局 `ITIMER_REAL` 槽表 (按 pid 索引)
static ITIMER_SLOTS: IrqSpinLock<[ItimerSlot; MAX_PROCESSES]> =
    IrqSpinLock::new([const { ItimerSlot::new() }; MAX_PROCESSES]);

/// `ITIMER_REAL` 到期回调 (hardirq 上下文, 由 `hrtimer_run_queues` 在无锁状态调用)
fn itimer_real_callback(timer: &HrTimer) -> HrTimerRestart {
    // 从 timer 指针反推容器槽: `HrTimer` 是 `ItimerSlot` 的第一个字段 (偏移 0),
    // 与 `posix_timer.rs` / `timerfd.rs` 同一模式.
    let slot_ptr = core::ptr::from_ref(timer).cast::<ItimerSlot>();
    // SAFETY: hrtimer 框架保证 `timer` 指向 `ITIMER_SLOTS` 中某槽的首字段
    // (`timer`), 故由 `&HrTimer` 反推容器地址有效; 槽位位于静态表, 生命周期
    // 覆盖整个内核运行期. 本回调只读 `pid`/`interval_ns` 不变字段并写原子
    // `armed`, 与设防路径的并发由 `ITIMER_SLOTS` 互斥 + Release/Acquire 语义
    // 保证可见性.
    let slot = unsafe { &*slot_ptr };

    // 投递 SIGALRM; 目标进程已退出/已是 Zombie 时 `do_signal_send` 返回 Err,
    // 静默忽略 — 撤防由 `Scheduler::exit` → `itimer_real_release_pid` 负责.
    let _ = do_signal_send(slot.pid, SIGALRM);

    if slot.interval_ns > 0 {
        // 周期模式: 下一截止期由 `hrtimer_run_queues` 按 `HrTimer::interval_ns`
        // 推进并重新入队, 本回调不再二次推进 (否则跳过一个周期).
        return HrTimerRestart::Periodic;
    }

    // 单次: 标记 disarmed, 剩余时间查询自然返回 0.
    slot.armed.store(false, Ordering::Release);
    HrTimerRestart::OneShot
}

/// 读取槽位剩余纳秒 (未设防 = 0)
fn slot_remaining(slot: &ItimerSlot) -> u64 {
    if !slot.armed.load(Ordering::Acquire) {
        return 0;
    }
    slot.timer.expiry_ns().saturating_sub(hrtimer_clock_read())
}

/// 设置指定进程的 `ITIMER_REAL`.
///
/// `value_ns` 为首次到期延时 (纳秒), 0 = 撤防并清空槽位 (POSIX `it_value == 0`
/// 语义); `interval_ns` 为周期间隔, 0 = 单次触发.
///
/// 返回**设防前**的 `(剩余纳秒, 周期间隔纳秒)` (未设防时为 `(0, 0)`), 供
/// `alarm(2)` 与 `setitimer` 的 `old` 出参换算秒/微秒.
pub fn itimer_real_set(pid: Pid, value_ns: u64, interval_ns: u64) -> (u64, u64) {
    if pid as usize >= MAX_PROCESSES {
        return (0, 0);
    }
    let mut slots = ITIMER_SLOTS.lock();
    let slot = &mut slots[pid as usize];

    let prev_remaining = slot_remaining(slot);
    let prev_interval = if slot.used { slot.interval_ns } else { 0 };

    if slot.armed.load(Ordering::Acquire) {
        hrtimer_cancel(&slot.timer);
        slot.armed.store(false, Ordering::Relaxed);
    }

    if value_ns == 0 {
        slot.used = false;
        slot.pid = 0;
        slot.interval_ns = 0;
        return (prev_remaining, prev_interval);
    }

    // `init` 复位 expiry/interval/state, 避免继承上一轮的周期配置.
    slot.timer.init(itimer_real_callback);
    slot.used = true;
    slot.pid = pid;
    slot.interval_ns = interval_ns;
    slot.armed.store(true, Ordering::Release);

    if interval_ns > 0 {
        hrtimer_start_periodic(&slot.timer, value_ns, interval_ns);
    } else {
        hrtimer_start(&slot.timer, hrtimer_clock_read().saturating_add(value_ns));
    }

    (prev_remaining, prev_interval)
}

/// 读取指定进程的 `ITIMER_REAL` 状态.
///
/// 返回 `Some((剩余纳秒, 周期间隔纳秒))`; 进程未设防或 pid 越界时返回 `None`.
pub fn itimer_real_get(pid: Pid) -> Option<(u64, u64)> {
    if pid as usize >= MAX_PROCESSES {
        return None;
    }
    let slots = ITIMER_SLOTS.lock();
    let slot = &slots[pid as usize];
    if !slot.used {
        return None;
    }
    Some((slot_remaining(slot), slot.interval_ns))
}

/// `alarm(2)` — 设置单次 `SIGALRM` 延时 (秒), 返回旧的剩余秒数 (向上取整).
///
/// 语义与 Linux 一致: 等价于 `setitimer(ITIMER_REAL, {value=seconds,
/// interval=0})`, 与 `setitimer` 互相覆盖; `seconds == 0` 仅撤防.
#[must_use]
pub fn itimer_alarm(pid: Pid, seconds: u32) -> u32 {
    let value_ns = u64::from(seconds).saturating_mul(NS_PER_SEC);
    let (prev_ns, _) = itimer_real_set(pid, value_ns, 0);
    // POSIX: alarm 返回剩余秒数, 非零剩余不足 1 秒时向上取整为 1 而非 0.
    crate::privileged::glue::low_u32_from_u64(prev_ns.div_ceil(NS_PER_SEC))
}

/// 进程退出钩子: 撤防并释放该 pid 的 `ITIMER_REAL` 槽位.
///
/// 由 `Scheduler::exit` 调用. 不释放则 pid 复用后新进程会收到遗留 `SIGALRM`.
pub fn itimer_real_release_pid(pid: Pid) {
    if pid as usize >= MAX_PROCESSES {
        return;
    }
    let mut slots = ITIMER_SLOTS.lock();
    let slot = &mut slots[pid as usize];
    if !slot.used {
        return;
    }
    if slot.armed.load(Ordering::Acquire) {
        hrtimer_cancel(&slot.timer);
    }
    slot.armed.store(false, Ordering::Relaxed);
    slot.used = false;
    slot.pid = 0;
    slot.interval_ns = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例一个专用 pid (槽表按 pid 直接索引, 并行跑测时互不干扰;
    /// MAX_PROCESSES = 256, 与其他模块单元用例的 pid 空间不重叠).
    const PID_ONESHOT: Pid = 201;
    const PID_PERIODIC: Pid = 202;
    const PID_ALARM: Pid = 203;
    const PID_RELEASE: Pid = 204;
    const PID_OVERWRITE: Pid = 205;
    const PID_DISARM: Pid = 206;
    const PID_OUT_OF_RANGE: Pid = 9999;

    /// 用例前置: hrtimer 框架就绪 + 本用例槽位干净.
    ///
    /// `hrtimer_start` 在框架未初始化时不登记截止期, 故必须先 `hrtimer_init()`;
    /// 只清本用例的 pid, 不触碰并行用例的槽位.
    fn prepare(pid: Pid) {
        if !crate::privileged::timer::is_hrtimer_ready() {
            crate::privileged::timer::hrtimer_init();
        }
        itimer_real_release_pid(pid);
    }

    /// 用例时延量级取分钟级: host 侧 `hrtimer_clock_read()` 回退到 tick 计数,
    /// 并行跑测时其他用例会推高全局 tick, 秒级阈值容易被吃掉造成假失败.
    const T_SHORT: u64 = 60 * NS_PER_SEC;
    const T_LONG: u64 = 120 * NS_PER_SEC;

    #[test]
    fn set_oneshot_reports_remaining() {
        prepare(PID_ONESHOT);
        let (prev, prev_interval) = itimer_real_set(PID_ONESHOT, T_LONG, 0);
        assert_eq!(prev, 0, "首次设防无旧值");
        assert_eq!(prev_interval, 0);

        let (remaining, interval) = itimer_real_get(PID_ONESHOT).expect("槽位已占用");
        assert!(
            remaining > 0 && remaining <= T_LONG,
            "剩余应落在 (0, 120s], 实际 {remaining}"
        );
        assert_eq!(interval, 0, "单次触发无周期");
    }

    #[test]
    fn set_periodic_reports_interval() {
        prepare(PID_PERIODIC);
        itimer_real_set(PID_PERIODIC, T_SHORT, T_LONG);
        let (remaining, interval) = itimer_real_get(PID_PERIODIC).expect("槽位已占用");
        assert!(remaining > 0);
        assert_eq!(interval, T_LONG, "getitimer 需回报 it_interval");
    }

    #[test]
    fn value_zero_disarms() {
        prepare(PID_DISARM);
        itimer_real_set(PID_DISARM, T_SHORT, 0);
        let (prev, _) = itimer_real_set(PID_DISARM, 0, 0);
        assert!(prev > 0, "撤防需回报撤防前的剩余");
        assert!(
            itimer_real_get(PID_DISARM).is_none(),
            "it_value=0 = 完全撤防"
        );
    }

    #[test]
    fn alarm_shares_itimer_slot_and_rounds_up() {
        prepare(PID_ALARM);
        // alarm(10) 与 setitimer(ITIMER_REAL) 共用同一 per-pid 槽 (Linux 语义)
        assert_eq!(itimer_alarm(PID_ALARM, 10), 0, "首次 alarm 无旧值");
        let (remaining, interval) = itimer_real_get(PID_ALARM).expect("alarm 应占用槽位");
        assert!(remaining > 0 && remaining <= 10 * NS_PER_SEC);
        assert_eq!(interval, 0, "alarm 不引入周期");

        // 旧剩余 1.9s → 向上取整报 2s, 不能截断为 1 (POSIX alarm(2))
        itimer_real_set(PID_ALARM, 2 * NS_PER_SEC - NS_PER_SEC / 10, 0);
        assert_eq!(itimer_alarm(PID_ALARM, 0), 2);
        assert!(itimer_real_get(PID_ALARM).is_none(), "alarm(0) 仅撤防");

        // alarm 必须覆盖已有的 ITIMER_REAL 设防 (双向覆盖)
        itimer_real_set(PID_ALARM, T_SHORT, 5 * T_SHORT);
        assert_eq!(
            itimer_alarm(PID_ALARM, 3),
            60,
            "alarm 覆盖 setitimer 并回报旧剩余"
        );
        let (_, interval) = itimer_real_get(PID_ALARM).expect("alarm 应占用槽位");
        assert_eq!(interval, 0, "alarm 覆盖后不得遗留旧周期");
        itimer_real_release_pid(PID_ALARM);
    }

    #[test]
    fn release_pid_clears_slot_and_unknown_pid_is_safe() {
        prepare(PID_RELEASE);
        itimer_real_set(PID_RELEASE, T_SHORT, T_SHORT);
        assert!(itimer_real_get(PID_RELEASE).is_some());
        itimer_real_release_pid(PID_RELEASE);
        assert!(itimer_real_get(PID_RELEASE).is_none());
        // 重复释放幂等
        itimer_real_release_pid(PID_RELEASE);
        // pid 越界: 不设防, 不 panic
        assert_eq!(itimer_real_set(PID_OUT_OF_RANGE, T_SHORT, 0), (0, 0));
        assert!(itimer_real_get(PID_OUT_OF_RANGE).is_none());
        itimer_real_release_pid(PID_OUT_OF_RANGE);
    }

    #[test]
    fn set_overwrites_previous_periodic_config() {
        prepare(PID_OVERWRITE);
        itimer_real_set(PID_OVERWRITE, T_SHORT, 3 * T_SHORT);
        let (prev_remaining, prev_interval) = itimer_real_set(PID_OVERWRITE, T_LONG, 0);
        assert!(prev_remaining > 0);
        assert_eq!(prev_interval, 3 * T_SHORT, "需回报设防前的周期配置");
        let (_, interval) = itimer_real_get(PID_OVERWRITE).expect("槽位仍占用");
        assert_eq!(interval, 0, "新的单次设防必须清除旧的 interval");
    }
}
