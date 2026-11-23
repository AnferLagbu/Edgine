//! 高精度定时器框架 (High-Resolution Timer)
//!
//! 提供纳秒级精度的内核定时器机制：
//! - **单次触发** (one-shot): 到期执行回调后自动移除
//! - **周期触发** (periodic): 到期执行回调后自动重新入队
//! - **取消**: 随时取消未触发的定时器
//!
//! ## 架构
//!
//! ```text
//! HrTimer Privileged
//! ├── hrtimer.rs       核心数据结构与 API
//! │   ├── HrTimer      定时器对象 (嵌入用户结构体)
//! │   ├── HrTimerQueue 全局优先队列 (按到期时间排序)
//! │   └── hrtimer_*    公共 API
//! │
//! ├── [x86_64] LAPIC Timer one-shot 模式 (Phase 2)
//! └── [aarch64] Generic Timer CNTP_CVAL one-shot (Phase 2)
//! ```
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use kernel::privileged::timer::hrtimer::{HrTimer, HrTimerRestart, hrtimer_start};
//!
//! struct MyDevice {
//!     timer: HrTimer,
//! }
//!
//! fn my_timer_callback(_timer: &HrTimer, _seq: u64) -> HrTimerRestart {
//!     // 处理超时逻辑; `_seq` 为本次激活的入队序号, 需要代际校验时使用
//!     HrTimerRestart::Periodic  // 或 OneShot
//! }
//!
//! // 初始化并启动 (10ms 后触发)
//! device.timer.init(my_timer_callback);
//! hrtimer_start(&device.timer, 10_000_000);  // 10ms = 10_000_000 ns
//! ```
//!
//! # Safety
//!
//! - HrTimer 必须在有效内存中 (通常嵌入其他结构体), 生命周期覆盖 `Pending` /
//!   `Running` 两个状态. `hrtimer_try_cancel` 在队列锁内真出队后即可返回; 若回调
//!   已被收集/正在执行, 它返回 `Busy`, 此时调用方仍须保活. `hrtimer_cancel`
//!   会等到在途回调退出后返回, 返回即可释放 timer 内存.
//! - **阻塞取消的锁域约束**: 等待期间 `hrtimer_cancel` 不持队列锁, 但调用方不得
//!   持有回调所需的任何锁 (否则回调无法前进 ⇒ 死锁). 不得在自身回调内、中断
//!   上下文或关中断上下文调用 `hrtimer_cancel`; 这些场景用 `hrtimer_try_cancel`.
//! - 回调在中断上下文执行, 不可睡眠
//! - 回调内不得再取消自身并等待其退出

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::privileged::error::KernelError;
use crate::privileged::sync::IrqSpinLock as Mutex;
use alloc::vec::Vec;
// ============================================================================
// 公共类型
// ============================================================================

/// 定时器回调返回值 — 控制定时器是否重新入队
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HrTimerRestart {
    /// 单次触发, 回调后移除
    OneShot,
    /// 周期触发, 按 `interval_ns` 重新入队
    Periodic,
}

/// 定时器回调函数类型
///
/// 第 2 个参数是本次激活的入队序号 (`run_queues` 收集条目时携带), 供需要
/// 代际校验的调用方 (如 timerfd 槽位复用) 丢弃迟到回调.
///
/// # 约束
/// - 在中断上下文执行, 不可睡眠
/// - 不可长时间阻塞 (建议 < 1ms)
/// - 回调在队列锁释放后执行, 允许调用 `hrtimer_start` 自行重挂 (timerfd
///   周期路径依赖此语义); 自行重挂后应返回 [`HrTimerRestart::OneShot`],
///   框架不会覆盖回调写入的 Pending 状态
pub type HrTimerCallback = fn(&HrTimer, u64) -> HrTimerRestart;

/// 定时器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HrTimerState {
    /// 未初始化或已取消
    Inactive,
    /// 已入队等待触发
    Pending,
    /// 回调正在执行
    Running,
}

/// 非阻塞取消结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 本次调用真正摘除了一个 Pending 激活 (返回即可释放)
    Cancelled,
    /// 没有待取消的激活 (已 Inactive, 或定时器从未入队)
    NotPending,
    /// 回调已被收集/正在执行; 不能安全释放, 须等待 `in_flight` 清零后重试
    Busy,
}

// ============================================================================
// HrTimer — 定时器对象 (嵌入用户结构体)
// ============================================================================

/// 高精度定时器
///
/// 设计为嵌入用户结构体使用 (类似 Linux `struct hrtimer`)。
/// 用户负责 `HrTimer` 的内存生命周期: 从 `init()` 到最后一次回调返回 `OneShot` 之间,
/// `HrTimer` 必须在有效内存中。
///
/// # 线程安全
///
/// - `expiry_ns` / `interval_ns` / `callback` / `state` / `queue_seq` 均为原子量,
///   支持跨 CPU 读写; `init` 只接收 `&self`, 不再要求调用方独占引用
/// - 入队/出队/收集由全局 spinlock 保护; 回调在锁释放后执行 (允许回调内重挂)
/// - 取消在队列锁内真出队; 若回调已被收集/正在执行 (`in_flight`), `try_cancel`
///   返回 `Busy`, `cancel` 等待其在途回调退出后返回
pub struct HrTimer {
    /// 绝对到期时间 (纳秒)
    expiry_ns: AtomicU64,
    /// 周期间隔 (纳秒), 0 = 单次
    interval_ns: AtomicU64,
    /// 回调函数 (fn 指针按 usize 存; 0 = 未设置 ⇒ `noop_callback`)
    callback: AtomicUsize,
    /// 当前状态
    state: AtomicU64, // 编码为 HrTimerState 的 u64
    /// 队列内序号 (用于快速定位, 0 = 未入队)
    queue_seq: AtomicU64,
    /// 是否已被收集/回调在途 (dispatch 退出前为 true; 取消方据此判定 Busy)
    in_flight: AtomicBool,
    /// shutdown 关闭位: 置位后 `start` 拒绝入队, 回调自重挂/Periodic 重挂失效
    closed: AtomicBool,
}

impl HrTimer {
    /// 创建未初始化的 `HrTimer`
    ///
    /// 必须调用 `init()` 后才能使用。
    #[must_use]
    pub const fn uninit() -> Self {
        Self {
            expiry_ns: AtomicU64::new(0),
            interval_ns: AtomicU64::new(0),
            callback: AtomicUsize::new(0),
            state: AtomicU64::new(HrTimerState::Inactive as u64),
            queue_seq: AtomicU64::new(0),
            in_flight: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    /// 初始化定时器
    ///
    /// 设置回调函数, 状态置为 Inactive。
    /// 可重复调用以更换回调。
    ///
    /// 只接收 `&self`: 回调指针经原子量写入, 与队列侧跨核读取构成 Release/Acquire
    /// 同步, 不再要求调用方持有独占引用 (`&mut`)。
    pub fn init(&self, callback: HrTimerCallback) {
        self.callback.store(callback as usize, Ordering::Release);
        self.expiry_ns.store(0, Ordering::Relaxed);
        self.interval_ns.store(0, Ordering::Relaxed);
        self.state
            .store(HrTimerState::Inactive as u64, Ordering::Relaxed);
        self.queue_seq.store(0, Ordering::Relaxed);
        self.closed.store(false, Ordering::Release);
    }

    /// 读取当前回调 (0 = 未设置 ⇒ `noop_callback`)
    fn callback_fn(&self) -> HrTimerCallback {
        let raw = self.callback.load(Ordering::Acquire);
        if raw == 0 {
            return noop_callback;
        }
        // SAFETY: `callback` 只由 `init` 以 `HrTimerCallback as usize` 写入非零值,
        // 该整数值就是同类型的函数指针; 本处按原类型还原位模式, 不构造新地址.
        unsafe { core::mem::transmute::<usize, HrTimerCallback>(raw) }
    }

    /// 获取到期时间 (纳秒)
    pub fn expiry_ns(&self) -> u64 {
        self.expiry_ns.load(Ordering::Acquire)
    }

    /// 当前激活序号 (最近一次 `hrtimer_start` 分配的入队序号; 0 = 未入队/已取消)
    ///
    /// 与回调第 2 参数的比较用于代际校验: 定时器被重新入队或取消后, 旧激活
    /// 的迟到回调携带的序号与当前值不一致, 应被丢弃.
    pub fn activation_seq(&self) -> u64 {
        self.queue_seq.load(Ordering::Acquire)
    }

    /// 获取周期间隔 (纳秒)
    pub fn interval_ns(&self) -> u64 {
        self.interval_ns.load(Ordering::Acquire)
    }

    /// 获取当前状态
    pub fn state(&self) -> HrTimerState {
        match self.state.load(Ordering::Acquire) {
            1 => HrTimerState::Pending,
            2 => HrTimerState::Running,
            _ => HrTimerState::Inactive,
        }
    }

    /// 是否处于 Pending 状态
    pub fn is_pending(&self) -> bool {
        self.state() == HrTimerState::Pending
    }

    /// 推进周期定时器到下一个周期
    ///
    /// 如果当前时间已超过到期时间, 按 `interval_ns` 向前推进到期时间,
    /// 直到到期时间 > 当前时间。
    ///
    /// 返回跳过的周期数。
    pub fn forward(&self, now_ns: u64) -> u64 {
        let interval = self.interval_ns.load(Ordering::Acquire);
        if interval == 0 {
            return 0;
        }

        let mut expiry = self.expiry_ns.load(Ordering::Acquire);
        let mut skipped = 0u64;

        while expiry <= now_ns {
            expiry += interval;
            skipped += 1;
        }

        if skipped > 0 {
            self.expiry_ns.store(expiry, Ordering::Release);
        }

        skipped
    }

    fn set_state(&self, state: HrTimerState) {
        self.state.store(state as u64, Ordering::Release);
    }
}

/// 空回调 (未初始化定时器的默认值)
fn noop_callback(_timer: &HrTimer, _seq: u64) -> HrTimerRestart {
    HrTimerRestart::OneShot
}

// ============================================================================
// 全局定时器队列
// ============================================================================

/// 队列内条目
struct TimerEntry {
    /// 定时器指针 (`NonNull` 确保非空)
    timer: core::ptr::NonNull<HrTimer>,
    /// 入队时的序号 (用于取消时快速判断是否过期)
    seq: u64,
}

// SAFETY: TimerEntry 通过全局 spinlock 同步访问, 指针在回调前有效。
// NonNull<HrTimer> 本身不是 Send, 但内核中定时器可在任意 CPU 上操作,
// 由 spinlock 保证互斥。
unsafe impl Send for TimerEntry {}
// SAFETY: `TimerEntry` 的两个字段 (`NonNull<HrTimer>` / seq: u64) 都是 Copy 值, 无
// 内部可变性; 条目只存在于 `HRTIMER_QUEUE: Mutex<Vec<TimerEntry>>` 内, 全部读写
// (入队 / 遍历 / retain / sort_by_key) 都在该锁临界区里 ⇒ &TimerEntry 跨核共享安全。
// 指针所指 HrTimer 的有效性不属本 impl 的依据, 由 `hrtimer_start` 的 `# Safety`
// 契约 (回调触发前保持有效内存) 单独承担。
unsafe impl Sync for TimerEntry {}

/// 全局序号计数器
static QUEUE_SEQ: AtomicU64 = AtomicU64::new(1);

/// 全局定时器队列
///
/// 使用 Vec + spinlock 实现, 按到期时间升序排列。
/// 后续优化为 per-CPU 无锁队列。
static HRTIMER_QUEUE: Mutex<Vec<TimerEntry>> = Mutex::new(Vec::new());

/// 框架初始化标志
static HRTIMER_READY: AtomicBool = AtomicBool::new(false);

// ============================================================================
// 公共 API
// ============================================================================

/// 初始化 hrtimer 框架
///
/// 在内核启动早期调用一次。
pub fn hrtimer_init() {
    let mut queue = HRTIMER_QUEUE.lock();
    queue.clear();
    HRTIMER_READY.store(true, Ordering::Release);
}

/// 检查 hrtimer 框架是否已初始化
pub fn is_hrtimer_ready() -> bool {
    HRTIMER_READY.load(Ordering::Acquire)
}

/// 启动定时器 (绝对时间)
///
/// 将定时器以绝对到期时间 `expiry_ns` 入队。若定时器已在队列中, 先移除旧条目
/// 再重新入队。返回本次激活的入队序号; `0` 表示框架未就绪或定时器已
/// `hrtimer_shutdown` (未入队)。
///
/// # Arguments
/// * `timer` - 定时器对象 (必须已 init)
/// * `expiry_ns` - 绝对到期时间 (纳秒, 从启动起)
///
/// # Safety
///
/// 调用方必须保证:
/// - `timer` 在回调触发前保持有效内存
/// - 回调在队列锁释放后执行, 允许在回调内调用本函数 (timerfd 周期路径)
pub fn hrtimer_start(timer: &HrTimer, expiry_ns: u64) -> u64 {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return 0;
    }

    let seq = QUEUE_SEQ.fetch_add(1, Ordering::Relaxed);

    let mut queue = HRTIMER_QUEUE.lock();

    // shutdown 与 start 的关闭位判定同在队列锁内: 要么 start 先入队、shutdown
    // 随后摘除; 要么 shutdown 先置位、start 看到关闭位后不再入队.
    if timer.closed.load(Ordering::Acquire) {
        return 0;
    }

    timer.expiry_ns.store(expiry_ns, Ordering::Release);
    timer.queue_seq.store(seq, Ordering::Release);
    timer.set_state(HrTimerState::Pending);

    // 移除同一 timer 的旧条目 (该 timer 的所有旧激活都失效)
    queue.retain(|entry| !core::ptr::eq(entry.timer.as_ptr(), timer));

    // 按到期时间插入 (保持升序)
    let insert_pos = queue
        .iter()
        .position(|entry| {
            // SAFETY: 同 `queue.retain`: `entry.timer` 在回调前保持有效, 本处只读
            // `expiry_ns` 原子字段.
            let other_expiry = unsafe { (*entry.timer.as_ptr()).expiry_ns.load(Ordering::Acquire) };
            other_expiry > expiry_ns
        })
        .unwrap_or(queue.len());

    // SAFETY: `timer` 由 `&HrTimer` 引用经 `from_ref` 取得, 必然非空;
    // `NonNull::new_unchecked` 只承诺非空, 该引用由调用点在回调前保持有效.
    let ptr = unsafe {
        core::ptr::NonNull::new_unchecked(core::ptr::from_ref::<HrTimer>(timer).cast_mut())
    };
    queue.insert(insert_pos, TimerEntry { timer: ptr, seq });

    seq
}

/// 启动定时器 (相对时间)
///
/// 便捷接口: 从当前时间起延迟 `delay_ns` 纳秒后触发。返回入队序号, `0` = 未入队。
///
/// # Arguments
/// * `timer` - 定时器对象
/// * `delay_ns` - 延迟时间 (纳秒)
pub fn hrtimer_start_rel(timer: &HrTimer, delay_ns: u64) -> u64 {
    let now = hrtimer_clock_read();
    hrtimer_start(timer, now.saturating_add(delay_ns))
}

/// 启动周期定时器
///
/// 首次在 `delay_ns` 纳秒后触发, 之后每 `interval_ns` 纳秒触发一次。
/// 返回入队序号, `0` = 未入队。
///
/// # Arguments
/// * `timer` - 定时器对象
/// * `delay_ns` - 首次延迟 (纳秒)
/// * `interval_ns` - 周期间隔 (纳秒)
pub fn hrtimer_start_periodic(timer: &HrTimer, delay_ns: u64, interval_ns: u64) -> u64 {
    timer.interval_ns.store(interval_ns, Ordering::Release);
    hrtimer_start_rel(timer, delay_ns)
}

/// 非阻塞取消定时器
///
/// 在队列锁内摘除该定时器的全部队列条目:
/// - `Cancelled`: 确实摘除了 Pending 激活, 返回即可释放 timer 内存
/// - `NotPending`: 没有待取消的激活
/// - `Busy`: 回调已被收集/正在执行, 不能安全释放; 等 `in_flight` 清零后重试,
///   或改用阻塞版 [`hrtimer_cancel`]
///
/// 与 `hrtimer_run_queues` 的收集互斥: 取消成功后队列不再持有该 timer 的
/// `NonNull`, 调用方不会在下次 `run_queues` 时被踩到悬垂指针.
pub fn hrtimer_try_cancel(timer: &HrTimer) -> CancelOutcome {
    let mut queue = HRTIMER_QUEUE.lock();

    if timer.in_flight.load(Ordering::Acquire) {
        return CancelOutcome::Busy;
    }

    let was_pending = timer.state() == HrTimerState::Pending;
    queue.retain(|entry| !core::ptr::eq(entry.timer.as_ptr(), timer));

    if was_pending {
        timer.set_state(HrTimerState::Inactive);
        timer.queue_seq.store(0, Ordering::Release);
        CancelOutcome::Cancelled
    } else {
        CancelOutcome::NotPending
    }
}

/// 等待 `timer` 的在途回调退出.
///
/// 调用方必须保证当前上下文允许该回调推进 (不持有回调所需锁、中断已开启)。
fn wait_in_flight(timer: &HrTimer) {
    while timer.in_flight.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
}

/// 阻塞取消定时器
///
/// 摘除 Pending 激活; 若回调已被收集/正在执行, 先等待其在途回调退出, 再重试。
/// 返回 `true` 表示最终摘除了一个 Pending 激活 (返回即可释放 timer 内存);
/// `false` 表示取消时本来就没有待取消的激活。
///
/// # 锁域约束
///
/// 等待期间不持队列锁, 但调用方不得持有回调所需的锁 —— 否则回调无法前进,
/// 形成死锁。不得在自身回调内、中断上下文或关中断上下文调用; 这些场景用
/// [`hrtimer_try_cancel`]。
pub fn hrtimer_cancel(timer: &HrTimer) -> bool {
    loop {
        match hrtimer_try_cancel(timer) {
            CancelOutcome::Cancelled => return true,
            CancelOutcome::NotPending => return false,
            CancelOutcome::Busy => wait_in_flight(timer),
        }
    }
}

/// 关闭定时器: 置关闭位、摘除队列条目并等待在途回调退出.
///
/// 关闭位使回调内的自重挂与框架 `Periodic` 重挂都不再入队; 返回后 timer 可安全
/// 释放或复用 (`init` 会清除关闭位)。与 `hrtimer_cancel` 相同的锁域/上下文约束.
pub fn hrtimer_shutdown(timer: &HrTimer) {
    {
        let mut queue = HRTIMER_QUEUE.lock();
        timer.closed.store(true, Ordering::Release);
        queue.retain(|entry| !core::ptr::eq(entry.timer.as_ptr(), timer));
        timer.set_state(HrTimerState::Inactive);
        timer.queue_seq.store(0, Ordering::Release);
    }
    wait_in_flight(timer);
}

/// 处理已到期的定时器
///
/// 在定时器中断处理程序中调用。遍历队列, 执行所有已到期定时器的回调。
///
/// # Safety
///
/// - 必须在中断上下文调用 (或中断禁用状态)
/// - 不可递归调用
pub fn hrtimer_run_queues() {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return;
    }

    let now_ns = hrtimer_clock_read();

    // 收集到期定时器 (释放锁后执行回调, 避免死锁)
    let expired: Vec<(core::ptr::NonNull<HrTimer>, HrTimerCallback, u64)> = {
        let mut queue = HRTIMER_QUEUE.lock();

        let mut expired = Vec::new();

        // 队列按到期时间升序, 找到第一个未到期的位置
        for entry in queue.iter() {
            let timer_ptr = entry.timer.as_ptr();
            // SAFETY: 入队时保证指针有效, 出队前不会释放
            let expiry = unsafe { (*timer_ptr).expiry_ns.load(Ordering::Acquire) };
            // SAFETY: `timer_ptr` 取自队列条目, 有效性由 `hrtimer_start` / 队列契约
            // 承担: 取消已在队列锁内真出队, 故本次收集期间队列条目指向的 timer 仍有效
            // (调用方只在 Pending/Running 窗口外才可释放). `state()` 只读原子字段.
            let state = unsafe { (*timer_ptr).state() };
            // 上一次回调仍持有该 timer (自重挂后 in_flight 仍为 true): 本轮不得
            // 再次收集, 否则同一 timer 的两个回调会并发执行.
            // SAFETY: `timer_ptr` 取自队列条目, 有效期由 `hrtimer_start` / 取消契约
            // 承担; 本处只读 `in_flight` 原子字段.
            let in_flight = unsafe { (*timer_ptr).in_flight.load(Ordering::Acquire) };

            if expiry <= now_ns && state == HrTimerState::Pending && !in_flight {
                // SAFETY: 指针有效性同上; 回调指针经原子量以 Acquire 读出, 与 `init`
                // 的 Release 写入构成同步 —— 跨核并发 `init` 不再产生数据竞争或
                // &/&mut 别名 (init 只接收 &self).
                let cb = unsafe { (*timer_ptr).callback_fn() };
                let seq = entry.seq;
                // SAFETY: `set_state` 只对 HrTimer 的原子 state 字段做 store, 指针有效性同由
                // `hrtimer_start` 契约承担。本处在队列锁内把状态置 Running、置 in_flight,
                // 随后的 `queue.retain` 只保留 Pending 条目 ⇒ 已收集条目同轮出队.
                unsafe {
                    (*timer_ptr).in_flight.store(true, Ordering::Release);
                    (*timer_ptr).set_state(HrTimerState::Running);
                }
                expired.push((entry.timer, cb, seq));
            } else if state == HrTimerState::Inactive {
                // 已取消, 跳过
            } else if expiry > now_ns {
                break; // 后续都未到期
            }
        }

        // 移除已到期和已取消的条目
        queue.retain(|entry| {
            // SAFETY: `entry.timer` 在回调前保持有效 (队列持有期), 本处只读 `state`.
            let state = unsafe { (*entry.timer.as_ptr()).state() };
            state == HrTimerState::Pending
        });

        // 重新排序 (retain 可能打乱顺序)
        // SAFETY: `entry.timer` 在回调前保持有效, 本处只读 `expiry_ns` 原子字段.
        queue.sort_by_key(|entry| unsafe {
            (*entry.timer.as_ptr()).expiry_ns.load(Ordering::Acquire)
        });

        expired
    };

    // 执行回调 (无锁, 允许回调中操作定时器)
    for (timer_ptr, callback, seq) in expired {
        // SAFETY: 入队时保证指针有效
        let timer = unsafe { timer_ptr.as_ref() };
        dispatch_expired(timer, callback, seq, now_ns);
    }
}

/// 执行单个已到期条目的回调, 并收敛其状态.
///
/// 收集与执行之间定时器可能已被取消 (`queue_seq = 0`) 或重新入队 (新序号),
/// 此时条目的激活序号已失效 — 不得执行回调, 否则迟到回调会触碰已被复用/
/// 重新编排的状态 (timerfd 槽位复用场景下的真实缺陷). 回调内自行 `hrtimer_start`
/// 重挂时, 保留回调写入的 Pending 状态, 不被 `OneShot` 收敛覆盖.
fn dispatch_expired(timer: &HrTimer, callback: HrTimerCallback, seq: u64, now_ns: u64) {
    if timer.activation_seq() != seq {
        // 迟到激活. 若因取消而失效 (序号为 0), 把收集阶段置上的 Running 收敛回
        // Inactive; 若已重新入队 (序号非 0), 新激活的状态归其所有, 本处不得覆盖.
        if timer.activation_seq() == 0 && timer.state() == HrTimerState::Running {
            timer.set_state(HrTimerState::Inactive);
        }
        timer.in_flight.store(false, Ordering::Release);
        return;
    }

    // shutdown 已置关闭位: 不再执行回调, 直接收敛并放行等待方.
    if timer.closed.load(Ordering::Acquire) {
        timer.set_state(HrTimerState::Inactive);
        timer.queue_seq.store(0, Ordering::Release);
        timer.in_flight.store(false, Ordering::Release);
        return;
    }

    let restart = callback(timer, seq);

    match restart {
        HrTimerRestart::OneShot => {
            // 回调内可能已自行重挂: 仅收敛仍处于 Running 的定时器.
            if timer.state() == HrTimerState::Running {
                timer.set_state(HrTimerState::Inactive);
                timer.queue_seq.store(0, Ordering::Release);
            }
        }
        HrTimerRestart::Periodic => {
            // 取消可能恰在回调执行期间发生 (置 Inactive): 此时不得重挂, 否则已取消
            // 的定时器会被回调复活. 回调自行重挂后状态为 Pending, 同样不得覆盖.
            if timer.state() == HrTimerState::Running {
                let interval = timer.interval_ns.load(Ordering::Acquire);
                if interval > 0 {
                    let old_expiry = timer.expiry_ns.load(Ordering::Acquire);
                    let new_expiry = old_expiry.saturating_add(interval);
                    // 如果已严重过期, 从当前时间开始
                    let next = if new_expiry <= now_ns {
                        now_ns.saturating_add(interval)
                    } else {
                        new_expiry
                    };
                    if hrtimer_start(timer, next) == 0 {
                        timer.set_state(HrTimerState::Inactive);
                        timer.queue_seq.store(0, Ordering::Release);
                    }
                } else {
                    timer.set_state(HrTimerState::Inactive);
                    timer.queue_seq.store(0, Ordering::Release);
                }
            }
        }
    }

    // 所有路径统一放行等待方; 必须在状态收敛/重挂写入之后 (Release).
    timer.in_flight.store(false, Ordering::Release);
}

/// 获取下一个到期时间 (纳秒)
///
/// 返回 None 表示队列为空。
/// 用于编程硬件定时器的下次触发时间。
pub fn hrtimer_next_expiry() -> Option<u64> {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return None;
    }

    let queue = HRTIMER_QUEUE.lock();
    queue.first().map(|entry| {
        // SAFETY: 入队时保证指针有效
        unsafe { (*entry.timer.as_ptr()).expiry_ns.load(Ordering::Acquire) }
    })
}

/// 获取队列中待触发定时器数量
pub fn hrtimer_pending_count() -> usize {
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return 0;
    }
    let queue = HRTIMER_QUEUE.lock();
    queue.len()
}

// ============================================================================
// 时钟源
// ============================================================================

/// 读取当前时间 (纳秒, 单调递增)
///
/// 直接取 `tick` 的纳秒视图 —— 全内核只有一个时基 (同一个锚点 + 同一个自由
/// 计数器), hrtimer 不另开一条换算路径 (旧形态: 已校准走 `cycles × 1e9 / freq`,
/// 未校准走 `ticks_to_ns(get_ticks())`, 两条路径的历元与粒度并不一致).
///
/// # 精度
///
/// - 硬件时基已建立 (`x86_64`: TSC, aarch64: `CNTPCT_EL0`): 计数器粒度 (亚 tick)
/// - 未建立: tick 粒度 (毫秒级)
#[must_use]
pub fn hrtimer_clock_read() -> u64 {
    crate::privileged::timer::tick::get_uptime_ns()
}

// ============================================================================
// I-50: hrtimer_sleep — 高精度 sleep 公共 API
// ============================================================================

/// 高精度 sleep (纳秒)
///
/// 用 hrtimer 机制 sleep 指定时长, 由 `hrtimer_run_queues()` 在 tick handler
/// 中统一触发 (见 `tick::on_timer_interrupt`). 精度高于 `timer_sleep(ms)` (毫秒级).
///
/// # 阻塞模型
///
/// 当前实现是 **busy-wait** (spin on atomic flag), 不让出 CPU. 适用于:
/// - 短延时 (< 1ms, 让出 CPU 调度开销更大)
/// - 中断上下文外
/// - 精度敏感场景 (网络超时, 设备探测重试)
///
/// # 后续优化 (后续阶段, 本 fix 不阻塞)
/// - 阻塞式 sleep: 注册定时器后调 `scheduler_yield`, 定时器 ISR 唤醒
/// - per-CPU timer 队列 (Phase 2 目标)
///
/// # Arguments
/// * `delay_ns` - 延迟时长 (纳秒, 0 = 立即返回)
///
/// # Returns
/// * `Ok(())`  - sleep 成功
/// * `Err(KernelError::NotInitialized)` - hrtimer 框架未初始化 (`hrtimer_init()` 未调用)
///
/// # Errors
/// 当 hrtimer 框架尚未初始化 (`HRTIMER_READY == false`) 时返回 `Err(KernelError::NotInitialized)`.
pub fn hrtimer_sleep(delay_ns: u64) -> Result<(), KernelError> {
    static SLEEP_FLAG: AtomicBool = AtomicBool::new(false);
    const SLEEP_WAIT_BOUND: u32 = 1_000_000_000; // ~1s @ 1GHz spin_loop
    if delay_ns == 0 {
        return Ok(());
    }
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return Err(KernelError::NotInitialized);
    }

    // 闭包用 static 状态在 ISR (hrtimer 回调) 与调用方 (本函数) 之间传递完成信号.
    // SAFETY: SLEEP_FLAG 仅为本函数独占, 不会跨调用并发 (单线程执行模型).

    let timer = HrTimer::uninit();
    timer.init(|_t, _seq| {
        // SLEEP_FLAG 为原子量, 回调仅 store true.
        SLEEP_FLAG.store(true, Ordering::Release);
        HrTimerRestart::OneShot
    });

    hrtimer_start_rel(&timer, delay_ns);

    // 自旋等待. hrtimer_run_queues 会在下一个 tick (或下次硬件定时器中断)
    // 处理到期定时器, 触发回调 set SLEEP_FLAG=true.
    let mut spins: u32 = 0;
    while !SLEEP_FLAG.load(Ordering::Acquire) {
        core::hint::spin_loop();
        spins = spins.saturating_add(1);
        if spins >= SLEEP_WAIT_BOUND {
            // 兜底: 超时仍按已完成返回, 不让进程卡死
            break;
        }
    }
    SLEEP_FLAG.store(false, Ordering::Release);
    // 契约 (见模块 Safety): timer 内存必须在 Pending/Running 期间有效. 本函数的
    // timer 在栈上, 返回前必须确保它不再留在队列中 —— 超时退路下它仍是 Pending.
    let _ = hrtimer_cancel(&timer);
    Ok(())
}

// ============================================================================
// 单元测试 (host-side, 不依赖硬件)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use core::sync::atomic::AtomicU32;

    /// 全局定时器队列是进程级单例: 队列相关用例串行执行, 避免并行测试互相清空/入队.
    ///
    /// 用 AtomicBool 自旋而非 `std::sync::Mutex`: 本文件属生产模块, 引入一把仅测试
    /// 使用的锁会污染 `audit_deadlock_matrix` 的锁类表 (该腿不区分 `cfg(test)`).
    static QUEUE_TEST_BUSY: AtomicBool = AtomicBool::new(false);

    struct QueueTestGuard;

    impl QueueTestGuard {
        fn acquire() -> Self {
            while QUEUE_TEST_BUSY.swap(true, Ordering::Acquire) {
                std::thread::yield_now();
            }
            Self
        }
    }

    impl Drop for QueueTestGuard {
        fn drop(&mut self) {
            QUEUE_TEST_BUSY.store(false, Ordering::Release);
        }
    }

    fn serialize_queue_tests() -> QueueTestGuard {
        QueueTestGuard::acquire()
    }

    /// 队列是否仍持有该 timer (只比较指针, 不解引用).
    fn queue_contains(timer: &HrTimer) -> bool {
        let queue = HRTIMER_QUEUE.lock();
        queue
            .iter()
            .any(|entry| core::ptr::eq(entry.timer.as_ptr(), timer))
    }

    #[test]
    fn test_hrtimer_state_encoding() {
        assert_eq!(HrTimerState::Inactive as u64, 0);
        assert_eq!(HrTimerState::Pending as u64, 1);
        assert_eq!(HrTimerState::Running as u64, 2);
    }

    #[test]
    fn test_hrtimer_uninit() {
        let timer = HrTimer::uninit();
        assert_eq!(timer.state(), HrTimerState::Inactive);
        assert_eq!(timer.expiry_ns(), 0);
        assert_eq!(timer.interval_ns(), 0);
        assert!(!timer.is_pending());
    }

    #[test]
    fn test_hrtimer_init() {
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        assert_eq!(timer.state(), HrTimerState::Inactive);
    }

    #[test]
    fn test_hrtimer_forward() {
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        timer.interval_ns.store(1_000_000, Ordering::Release); // 1ms
        timer.expiry_ns.store(5_000_000, Ordering::Release); // 5ms

        // 当前时间 8ms: 自 5ms 起按 1ms 推进, 到期时间须 > 当前时间,
        // 故 5→6→7→8→9 共跳过 4 个周期 (2026-09-24 UT-06 实测修正原断言 3/8ms).
        let skipped = timer.forward(8_000_000);
        assert_eq!(skipped, 4);
        assert_eq!(timer.expiry_ns(), 9_000_000);

        // 当前时间 7ms, 未超过到期时间, 不跳过
        let skipped = timer.forward(7_000_000);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_hrtimer_forward_zero_interval() {
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        timer.expiry_ns.store(5_000_000, Ordering::Release);
        let skipped = timer.forward(10_000_000);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_hrtimer_clock_read_monotonic() {
        // 单一时基: 时钟读数取自 `tick` 纳秒视图, 两次读取不得回退.
        let t1 = hrtimer_clock_read();
        let t2 = hrtimer_clock_read();
        assert!(t2 >= t1, "clock must not rewind");
    }

    #[test]
    fn test_cancel_while_inactive() {
        let _serial = serialize_queue_tests();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        let cancelled = hrtimer_cancel(&timer);
        assert!(!cancelled); // 未入队, 取消返回 false
    }

    #[test]
    fn test_next_expiry_empty() {
        // UT-06 (2026-09-24): 先 hrtimer_init() 清空队列使断言与环境无关;
        // 队列是进程级单例, 本用例与其它队列用例经 QUEUE_TEST_LOCK 串行.
        let _serial = serialize_queue_tests();
        hrtimer_init();
        assert!(hrtimer_next_expiry().is_none());
    }

    #[test]
    fn test_pending_count_empty() {
        // UT-06 (2026-09-24): 同上, 清空队列后待触发数必为 0.
        let _serial = serialize_queue_tests();
        hrtimer_init();
        assert_eq!(hrtimer_pending_count(), 0);
    }

    #[test]
    fn test_cancel_removes_timer_from_queue() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000));
        assert!(queue_contains(&timer), "启动后队列必须持有该 timer");

        assert!(hrtimer_cancel(&timer));
        assert!(
            !queue_contains(&timer),
            "取消必须在队列锁内真出队 —— 否则调用方释放 timer 后队列持悬垂 NonNull"
        );
    }

    #[test]
    fn test_try_cancel_pending_and_busy() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000));

        assert_eq!(
            hrtimer_try_cancel(&timer),
            CancelOutcome::Cancelled,
            "Pending 激活必须被 try_cancel 摘除"
        );
        assert!(!queue_contains(&timer));
        assert_eq!(timer.state(), HrTimerState::Inactive);

        // 模拟 run_queues 已收集、回调在途: try_cancel 必须报 Busy 且不摘队列.
        timer.init(noop_callback);
        hrtimer_start(&timer, 0);
        timer.set_state(HrTimerState::Running);
        timer.in_flight.store(true, Ordering::Release);
        assert_eq!(
            hrtimer_try_cancel(&timer),
            CancelOutcome::Busy,
            "在途回调未退出前不得承诺可释放"
        );
        timer.in_flight.store(false, Ordering::Release);
        timer.set_state(HrTimerState::Inactive);
        timer.queue_seq.store(0, Ordering::Release);
        let _ = hrtimer_try_cancel(&timer);
    }

    #[test]
    fn test_shutdown_rejects_restart_until_init() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000));
        assert!(queue_contains(&timer));

        hrtimer_shutdown(&timer);
        assert!(!queue_contains(&timer), "shutdown 后队列不得再持有该 timer");
        assert_eq!(timer.state(), HrTimerState::Inactive);
        assert_eq!(hrtimer_pending_count(), 0);

        // 关闭位生效: 直接 start 不得复活.
        assert_eq!(
            hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000)),
            0
        );
        assert_eq!(hrtimer_pending_count(), 0, "shutdown 后 start 必须被拒绝");

        // init 清除关闭位后可重新使用.
        timer.init(noop_callback);
        assert_ne!(
            hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000)),
            0
        );
        assert!(hrtimer_cancel(&timer));
        assert_eq!(hrtimer_pending_count(), 0);
    }

    #[test]
    fn test_blocking_cancel_waits_for_in_flight() {
        use std::time::{Duration, Instant};

        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000));
        // 模拟 run_queues 已收集、回调在途.
        timer.set_state(HrTimerState::Running);
        timer.in_flight.store(true, Ordering::Release);

        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                timer.in_flight.store(false, Ordering::Release);
                timer.set_state(HrTimerState::Inactive);
                timer.queue_seq.store(0, Ordering::Release);
            });
            // Busy → 等 in_flight 清零 → 重试; 回调已自行收敛为 Inactive.
            assert!(!hrtimer_cancel(&timer));
        });
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "cancel 必须等到在途回调退出, 不得提前返回"
        );
        assert!(!queue_contains(&timer));
    }

    #[test]
    fn test_cancel_then_drop_leaves_nothing_to_dispatch() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = Box::new(HrTimer::uninit());
        timer.init(cancelled_timer_callback);
        // 已到期: 若取消未出队, 下一次 run_queues 会踩释放后的内存.
        hrtimer_start(&timer, 0);
        assert!(hrtimer_cancel(&timer));
        assert!(!queue_contains(&timer), "取消后队列不得再持有该 timer");
        drop(timer);

        hrtimer_run_queues();
        assert_eq!(
            CANCELLED_CALLS.load(Ordering::Relaxed),
            0,
            "已取消并释放的 timer 不得再被调度"
        );
    }

    #[test]
    fn test_init_through_shared_ref_replaces_callback() {
        // init 只接收 &self: 回调指针经原子量替换, 跨核读取不会撕裂/无需 &mut 别名.
        let timer = HrTimer::uninit();
        let shared: &HrTimer = &timer;
        assert!(core::ptr::fn_addr_eq(
            shared.callback_fn(),
            noop_callback as HrTimerCallback
        ));
        shared.init(stale_guard_callback);
        assert!(
            core::ptr::fn_addr_eq(
                shared.callback_fn(),
                stale_guard_callback as HrTimerCallback
            ),
            "共享引用上 init 必须替换回调"
        );
    }

    // I-50: hrtimer_sleep 公共 API host-test
    #[test]
    fn test_hrtimer_sleep_zero() {
        // 0 纳秒应立即返回 Ok
        assert!(hrtimer_sleep(0).is_ok());
    }

    #[test]
    fn test_hrtimer_sleep_init() {
        let _serial = serialize_queue_tests();
        // 初始化后, 短延时 sleep 应成功 (实际精度由硬件 tick 决定, 此处只验 API 通路)
        hrtimer_init();
        // 1ms = 1_000_000 ns, 在 host 环境下无 tick 中断, 走 SLEEP_WAIT_BOUND 退路,
        // 仍返回 Ok (不卡死, 不 panic); 返回前 cancel 保证栈上 timer 不留在队列.
        assert!(hrtimer_sleep(1_000_000).is_ok());
        assert_eq!(hrtimer_pending_count(), 0, "sleep 返回后队列必须为空");
    }

    static STALE_CALLS: AtomicU32 = AtomicU32::new(0);
    static REARM_CALLS: AtomicU32 = AtomicU32::new(0);
    static CANCELLED_CALLS: AtomicU32 = AtomicU32::new(0);

    fn cancelled_timer_callback(_timer: &HrTimer, _seq: u64) -> HrTimerRestart {
        CANCELLED_CALLS.fetch_add(1, Ordering::Relaxed);
        HrTimerRestart::OneShot
    }

    fn stale_guard_callback(_timer: &HrTimer, _seq: u64) -> HrTimerRestart {
        STALE_CALLS.fetch_add(1, Ordering::Relaxed);
        HrTimerRestart::OneShot
    }

    fn self_rearm_callback(timer: &HrTimer, _seq: u64) -> HrTimerRestart {
        REARM_CALLS.fetch_add(1, Ordering::Relaxed);
        // 模拟 timerfd 周期路径: 回调内自行重挂 (新激活序号 + Pending).
        timer.queue_seq.store(99, Ordering::Release);
        timer.set_state(HrTimerState::Pending);
        HrTimerRestart::OneShot
    }

    #[test]
    fn test_dispatch_skips_stale_activation() {
        let timer = HrTimer::uninit();
        timer.init(stale_guard_callback);
        timer.set_state(HrTimerState::Running);
        timer.queue_seq.store(5, Ordering::Release);
        STALE_CALLS.store(0, Ordering::Relaxed);

        // 收集后定时器已被重新入队 (序号 5), 旧条目 (序号 4) 不得执行回调.
        dispatch_expired(&timer, stale_guard_callback, 4, 0);
        assert_eq!(
            STALE_CALLS.load(Ordering::Relaxed),
            0,
            "迟到激活不得执行回调"
        );
        // 当前激活正常执行并收敛到 Inactive.
        dispatch_expired(&timer, stale_guard_callback, 5, 0);
        assert_eq!(STALE_CALLS.load(Ordering::Relaxed), 1, "当前激活应执行回调");
        assert_eq!(timer.state(), HrTimerState::Inactive);
    }

    #[test]
    fn test_dispatch_preserves_callback_rearm() {
        let timer = HrTimer::uninit();
        timer.init(self_rearm_callback);
        timer.set_state(HrTimerState::Running);
        timer.queue_seq.store(7, Ordering::Release);
        REARM_CALLS.store(0, Ordering::Relaxed);

        dispatch_expired(&timer, self_rearm_callback, 7, 0);
        assert_eq!(REARM_CALLS.load(Ordering::Relaxed), 1);
        // 回调内自行重挂后返回 OneShot: 框架不得覆盖回调写入的 Pending.
        assert_eq!(
            timer.state(),
            HrTimerState::Pending,
            "回调内重挂的 Pending 不得被覆盖"
        );
        assert_eq!(timer.activation_seq(), 99);
    }

    #[test]
    fn test_dispatch_stale_after_cancel_converges_running() {
        let timer = HrTimer::uninit();
        timer.init(stale_guard_callback);
        timer.set_state(HrTimerState::Running);
        // 取消已把激活序号清零, 但回调在取消前已被收集.
        timer.queue_seq.store(0, Ordering::Release);
        STALE_CALLS.store(0, Ordering::Relaxed);

        dispatch_expired(&timer, stale_guard_callback, 9, 0);

        assert_eq!(
            STALE_CALLS.load(Ordering::Relaxed),
            0,
            "已取消的激活不得执行回调"
        );
        assert_eq!(
            timer.state(),
            HrTimerState::Inactive,
            "取消后迟到的收集结果必须把 Running 收敛回 Inactive"
        );
    }

    /// 模拟取消恰在回调执行期间发生 (由取消方置 Inactive + 序号清零).
    fn cancel_during_callback(timer: &HrTimer, _seq: u64) -> HrTimerRestart {
        timer.set_state(HrTimerState::Inactive);
        timer.queue_seq.store(0, Ordering::Release);
        HrTimerRestart::Periodic
    }

    #[test]
    fn test_periodic_dispatch_does_not_rearm_after_cancel() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(cancel_during_callback);
        timer.interval_ns.store(1_000_000, Ordering::Release);
        timer.set_state(HrTimerState::Running);
        timer.queue_seq.store(7, Ordering::Release);

        dispatch_expired(&timer, cancel_during_callback, 7, 0);

        assert_eq!(
            timer.state(),
            HrTimerState::Inactive,
            "取消后 Periodic 回调不得把定时器复活"
        );
        assert!(!queue_contains(&timer), "已取消的定时器不得被重新入队");
    }
}

// ============================================================================
// 内核测试 (QEMU feature gate)
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_state_encoding() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    assert_eq_test!(HrTimerState::Inactive as u64, 0, "Inactive=0");
    assert_eq_test!(HrTimerState::Pending as u64, 1, "Pending=1");
    assert_eq_test!(HrTimerState::Running as u64, 2, "Running=2");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_uninit_state() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    let timer = HrTimer::uninit();
    assert_eq_test!(timer.state(), HrTimerState::Inactive, "state");
    assert_eq_test!(timer.expiry_ns(), 0, "expiry");
    check!(!timer.is_pending(), "not pending");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_init_and_cancel() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    static mut TEST_TIMER: HrTimer = HrTimer::uninit();
    // SAFETY: 测试单线程, 无竞争; 经 &raw mut 取裸指针避免对 static mut 取引用
    unsafe {
        let timer = &mut *core::ptr::addr_of_mut!(TEST_TIMER);
        timer.init(noop_callback);
        assert_eq_test!(timer.state(), HrTimerState::Inactive, "after init");
        let cancelled = hrtimer_cancel(timer);
        check!(!cancelled, "cancel inactive returns false");
    }
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_forward_periodic() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    let timer = HrTimer::uninit();
    timer.init(noop_callback);
    timer.interval_ns.store(1_000_000, Ordering::Release);
    timer.expiry_ns.store(5_000_000, Ordering::Release);
    // forward 用 `expiry <= now` 语义: expiry 恰好等于 now 时视为已到期再推进一位.
    // 5ms → 6 → 7 → 8(==now, 再推) → 9ms, 跳过 4 个周期, 新 expiry 9ms.
    let skipped = timer.forward(8_000_000);
    assert_eq_test!(skipped, 4, "skipped 4 periods");
    assert_eq_test!(timer.expiry_ns(), 9_000_000, "new expiry");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_clock_read() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    // u64 类型即非负契约; 此测试只验证调用不 panic + 返回合理量级.
    let ns = hrtimer_clock_read();
    check!(ns < u64::MAX, "clock read bounded");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_queue_operations() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    // J-01 (2026-09-08): static mut 移至函数顶部 (items_after_statements 清理) —
    // 测试用持久 HrTimer, 生命周期跨多次调用
    static mut T1: HrTimer = HrTimer::uninit();
    hrtimer_init();
    check!(is_hrtimer_ready(), "initialized");
    assert_eq_test!(hrtimer_pending_count(), 0, "empty queue");

    // SAFETY: `T1` 是测试专用的 `static mut HrTimer`; 经 `addr_of_mut!` 取裸指针后
    // 转 `&mut`, 避免对 `static mut` 直接建引用; 测试单线程串行执行, 无并发别名.
    unsafe {
        let timer = &mut *core::ptr::addr_of_mut!(T1);
        timer.init(noop_callback);
        hrtimer_start(timer, 1_000_000_000);
    }
    assert_eq_test!(hrtimer_pending_count(), 1, "1 timer");

    let next = hrtimer_next_expiry();
    check!(next.is_some(), "has next expiry");
    assert_eq_test!(next.unwrap(), 1_000_000_000, "expiry matches");

    // SAFETY: 经 &raw mut 取裸指针避免对 static mut 取引用
    hrtimer_cancel(unsafe { &*core::ptr::addr_of!(T1) });
    // 取消在队列锁内立即出队, 无需等下一次 run_queues 清理
    hrtimer_run_queues();
    assert_eq_test!(hrtimer_pending_count(), 0, "empty after cancel");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_periodic_restart() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    static mut T2: HrTimer = HrTimer::uninit();
    // SAFETY: 经 &raw mut 取裸指针避免对 static mut 取引用
    unsafe {
        let timer = &mut *core::ptr::addr_of_mut!(T2);
        timer.init(periodic_test_callback);
        let now = hrtimer_clock_read();
        hrtimer_start(timer, now + 1_000_000); // 1ms 后
    }
    check!(
        // SAFETY: T2 是本测试独占的 static mut, 上方已经 `addr_of_mut!` 完成 init 与
        // `hrtimer_start`; 本处经 `addr_of!` 取址再解引用, 不对 static mut 构造共享引用,
        // `is_pending()` 经 `state()` 读 AtomicU64, 是原子读而非裸字段读。
        unsafe { (*core::ptr::addr_of!(T2)).is_pending() },
        "pending after start"
    );

    // 运行队列 (可能未到期, 但测试回调逻辑)
    hrtimer_run_queues();
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn periodic_test_callback(_timer: &HrTimer, _seq: u64) -> HrTimerRestart {
    HrTimerRestart::Periodic
}

/// 注册高精度定时器 (hrtimer) 测试用例 (`kernel_test`)
#[cfg(feature = "kernel_test")]
pub fn register_hrtimer_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("hrtimer", "state_encoding", test_state_encoding);
    r.register("hrtimer", "uninit_state", test_uninit_state);
    r.register("hrtimer", "init_and_cancel", test_init_and_cancel);
    r.register("hrtimer", "forward_periodic", test_forward_periodic);
    r.register("hrtimer", "clock_read", test_clock_read);
    r.register("hrtimer", "queue_ops", test_queue_operations);
    r.register("hrtimer", "periodic_restart", test_periodic_restart);
}
