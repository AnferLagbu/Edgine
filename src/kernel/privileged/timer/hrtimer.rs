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
//! │   ├── HrTimer      定时器对象 (内嵌侵入式红黑树节点)
//! │   ├── TimerBase    每 CPU 一棵树 + 一把锁 (无全局队列)
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
//! let _ = hrtimer_start(&device.timer, 10_000_000);  // 10ms = 10_000_000 ns
//! ```
//!
//! # Safety
//!
//! - HrTimer 必须在有效内存中 (通常嵌入其他结构体), 生命周期覆盖 `Pending` /
//!   `Running` / `Shutdown` 状态. `hrtimer_try_cancel` 在所属 base 锁内真出队后即可返回; 若回调
//!   已被收集/正在执行, 它返回 `Busy`, 此时调用方仍须保活. `hrtimer_cancel`
//!   会等到在途回调退出后返回, 返回即可释放 timer 内存.
//! - **阻塞取消的锁域约束**: 等待期间 `hrtimer_cancel` 不持 base 锁, 但调用方不得
//!   持有回调所需的任何锁 (否则回调无法前进 ⇒ 死锁). 不得在自身回调内、中断
//!   上下文或关中断上下文调用 `hrtimer_cancel`; 这些场景用 `hrtimer_try_cancel`.
//! - 回调在中断上下文执行, 不可睡眠
//! - 回调内不得再取消自身并等待其退出

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::privileged::config::MAX_CPUS;
use crate::privileged::error::KernelError;
use crate::privileged::smp;
use crate::privileged::sync::IrqSpinLock as Mutex;

use super::rb_tree::{RbNode, RbTree};
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
/// - 回调在 base 锁释放后执行, 允许调用 `hrtimer_start` 自行重挂 (timerfd
///   周期路径依赖此语义); 自行重挂后应返回 [`HrTimerRestart::OneShot`],
///   框架不会覆盖回调写入的 Pending 状态
pub type HrTimerCallback = fn(&HrTimer, u64) -> HrTimerRestart;

/// 定时器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HrTimerState {
    /// 刚构造, 尚未 `init`
    Uninit,
    /// 已初始化或已取消, 未入队, 无在途回调
    Inactive,
    /// 已入队等待触发
    Pending,
    /// 回调正在执行
    Running,
    /// 已 shutdown: 拒绝入队, 回调自重挂/Periodic 重挂失效
    Shutdown,
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

/// 启动定时器的结果
///
/// 取代旧 API 的 `0` 哨兵: `0` 曾同时表示框架未就绪、未 init、已 shutdown,
/// 调用方无法区分也无法据此诊断。`Started` 内保留入队序号 (恒非 0)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "启动结果必须被消费; 非 Started 表示定时器未入队"]
pub enum StartOutcome {
    /// 已入队; 携带本次激活的入队序号 (恒非 0)
    Started(u64),
    /// hrtimer 框架尚未 `hrtimer_init()`
    NotReady,
    /// 定时器尚未 `init` (`HrTimerState::Uninit`)
    NotInitialized,
    /// 定时器已 `hrtimer_shutdown`
    Shutdown,
}

impl StartOutcome {
    /// 是否成功入队
    #[must_use]
    pub fn is_started(self) -> bool {
        matches!(self, Self::Started(_))
    }
}

/// `init` 在活动定时器上的运行时拒绝原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    /// 定时器仍在队列中 (`Pending`)
    Pending,
    /// 回调正在执行 (`Running`)
    Running,
    /// 状态已收敛但 `in_flight` 尚未清零 (取消/重挂窗口)
    InFlight,
    /// CPU offline 强制迁移在途 (节点已离源 base、尚未插入目标 base)
    Migrating,
}

// ============================================================================
// per-CPU 定时器基
// ============================================================================

/// 红黑树排序键: `(到期时间, 激活序号)`。
///
/// 到期时间相同时按激活序号稳定排序；序号单调递增且非零，因此同一个
/// `HrTimer` 不会在同一键上被重复插入。
type TimerKey = (u64, u64);

/// 一个 base 的生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseState {
    /// base 可接收新定时器、参与本 CPU 分发
    Online,
    /// CPU 正在下线：拒绝新分发，已有 Pending 迁移到 `quiesce_target`
    Quiescing,
    /// CPU 尚未上线或已下线，拒绝新激活
    Offline,
}

/// base 锁内数据。
struct BaseInner {
    /// 侵入式红黑树，节点内嵌在 `HrTimer` 里
    tree: RbTree<TimerKey>,
    /// base 生命周期状态
    state: BaseState,
    /// 已摘树、回调尚未返回的条目数（quiesce 判据）。
    ///
    /// 全部用 `u16`: `TIMER_BASES` 是 `MAX_CPUS = 1024` 个常驻 base，字段收窄
    /// 后 `BaseInner` 仍为 24 字节，避免 1024 核容量包络把内核镜像顶过
    /// `USER_CODE_BASE`（K-08）。`running` 超过 65535 时饱和。
    running: u16,
    /// 从本 base 摘下、尚未插入目标的迁移数（quiesce 判据）
    migrations: u16,
    /// `Quiescing` 期间新激活的落点；仅 `Quiescing` 时有效 (`NO_CPU` = 无)
    quiesce_target: u16,
}

impl BaseInner {
    const fn new() -> Self {
        Self {
            tree: RbTree::new(),
            state: BaseState::Offline,
            running: 0,
            migrations: 0,
            quiesce_target: NO_CPU,
        }
    }
}

// SAFETY: `BaseInner` 只在对应 `TimerBase` 的自旋锁内被访问；树中节点的地址来自
// Pending/Running 期间保持存活的 `HrTimer`，锁外只通过 `HrTimer` 自带的原子状态读取。
unsafe impl Send for BaseInner {}

/// 一个 CPU 的定时器基：一把锁 + 一棵树 + 运行计数。
struct TimerBase {
    base_lock: Mutex<BaseInner>,
}

impl TimerBase {
    const fn new() -> Self {
        Self {
            base_lock: Mutex::new(BaseInner::new()),
        }
    }
}

/// 每个 CPU 一个 base；无全局后备队列。
static TIMER_BASES: [TimerBase; MAX_CPUS] = [const { TimerBase::new() }; MAX_CPUS];

/// 当前处于 `Online` 的 base 数量；用于拒绝最后一个在线 CPU 下线。
static ONLINE_BASE_COUNT: AtomicU32 = AtomicU32::new(0);

/// 串行化 `try_init` 与 `Inactive -> Pending` 首次绑定，消除回调/到期字段
/// 写入与首次入队之间的 TOCTOU。锁序固定为 `TIMER_INIT_LOCK -> TIMER_BASE`。
static TIMER_INIT_LOCK: Mutex<()> = Mutex::new(());

fn base_for_cpu(cpu: u32) -> Option<&'static TimerBase> {
    TIMER_BASES.get(cpu as usize)
}

fn current_cpu_for_timer() -> Option<u32> {
    let cpu = smp::current_cpu_index();
    if (cpu as usize) < MAX_CPUS {
        Some(cpu)
    } else {
        None
    }
}

fn next_activation_seq() -> u64 {
    QUEUE_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// 从树节点反查 `HrTimer`。
///
/// `HrTimer::node` 是 `#[repr(C)]` 的首字段，节点地址等于定时器地址。
///
/// # Safety
///
/// `node` 必须来自一个仍存活、且 `node` 为首字段的 `HrTimer`。
unsafe fn timer_from_node(
    node: core::ptr::NonNull<RbNode<TimerKey>>,
) -> core::ptr::NonNull<HrTimer> {
    node.cast()
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
/// - `expiry_ns` / `interval_ns` / `callback` / `state` / `queue_seq` / `base_id`
///   均为原子量, 支持跨 CPU 读写; `init` 只接收 `&self`, 不再要求调用方独占引用
/// - 入队/出队/收集由所属 CPU 的 `TimerBase` 锁保护; 回调在锁释放后执行
///   (允许回调内重挂)
/// - 取消在 base 锁内真出队; 若回调已被收集/正在执行 (`in_flight`), `try_cancel`
///   返回 `Busy`, `cancel` 等待其在途回调退出后返回
///
/// `node` 必须是首字段: `timer_from_node` 依赖节点地址回推定时器地址.
#[repr(C)]
pub struct HrTimer {
    /// 侵入式红黑树节点 (首字段, 地址即 `HrTimer` 地址)
    node: UnsafeCell<RbNode<TimerKey>>,
    /// 当前绑定的 CPU base (`NO_BASE` = 未绑定)
    base_id: AtomicU32,
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
    /// CPU offline 强制迁移的瞬态: 节点已离源 base、尚未插入目标 base.
    /// `try_cancel` 见到 true 返回 `Busy`; `start` 返回 `NotReady`; `shutdown`
    /// 先等待其清零, 避免锁到错误的 base.
    migrating: AtomicBool,
}

// SAFETY: `HrTimer` 的标量字段全部是原子量或只在 base 锁内读写的节点; 节点地址
// 只在所属 base 树中、由该 base 锁保护。跨线程/跨核共享引用本身不会产生对节点
// 字段的数据竞争; timer 内存生命周期由 `hrtimer_start` 的 `# Safety` 契约承担。
unsafe impl Send for HrTimer {}
// SAFETY: 同上; `&HrTimer` 可跨核共享, 树结构修改必须经所属 base 锁。
unsafe impl Sync for HrTimer {}

impl HrTimer {
    /// 创建未初始化的 `HrTimer`
    ///
    /// 必须调用 `init()` 后才能使用。
    #[must_use]
    pub const fn uninit() -> Self {
        Self {
            node: UnsafeCell::new(RbNode::new((0, 0))),
            base_id: AtomicU32::new(NO_BASE),
            expiry_ns: AtomicU64::new(0),
            interval_ns: AtomicU64::new(0),
            callback: AtomicUsize::new(0),
            state: AtomicU64::new(HrTimerState::Uninit as u64),
            queue_seq: AtomicU64::new(0),
            in_flight: AtomicBool::new(false),
            migrating: AtomicBool::new(false),
        }
    }

    /// 初始化定时器
    ///
    /// 设置回调函数, 状态置为 `Inactive`。可从 `Uninit`、`Inactive` 或
    /// `Shutdown` 调用; `Pending`/`Running`/仍有在途回调时调用是编程错误。
    ///
    /// 运行时检查在 `TIMER_INIT_LOCK` 内完成 (`try_init`)，不再依赖 debug 构建的断言；
    /// [`init`](Self::init) 对活动定时器直接 panic，需要可恢复判定的调用方
    /// 应使用 [`try_init`](Self::try_init)。
    ///
    /// 只接收 `&self`: 回调指针经原子量写入, 与 base 侧跨核读取构成 Release/Acquire
    /// 同步, 不再要求调用方持有独占引用 (`&mut`)。
    ///
    /// # Panics
    ///
    /// 定时器仍 `Pending` / `Running`，或 `in_flight` 未清零时 panic。调用方若需
    /// 可恢复判定应使用 [`try_init`](Self::try_init)。
    pub fn init(&self, callback: HrTimerCallback) {
        if let Err(err) = self.try_init(callback) {
            panic!("HrTimer::init called on active timer: {err:?}");
        }
    }

    /// 可诊断地初始化定时器
    ///
    /// 与 [`init`](Self::init) 相同，但活动定时器返回错误而不是 panic。
    /// `TIMER_INIT_LOCK` 同时串行化 `try_init` 与 `Inactive` 首次 `start`，
    /// 避免“检查后被并发入队”的 TOCTOU；
    /// 字段写入全部在状态发布为 `Inactive` 之前完成。
    ///
    /// # Errors
    ///
    /// 定时器 `Pending` / `Running` 或 `in_flight` 非零时返回对应 [`InitError`]。
    pub fn try_init(&self, callback: HrTimerCallback) -> Result<(), InitError> {
        let _init = TIMER_INIT_LOCK.lock();

        match self.state() {
            HrTimerState::Pending => return Err(InitError::Pending),
            HrTimerState::Running => return Err(InitError::Running),
            HrTimerState::Uninit | HrTimerState::Inactive | HrTimerState::Shutdown => {}
        }
        if self.in_flight.load(Ordering::Acquire) {
            return Err(InitError::InFlight);
        }
        if self.migrating.load(Ordering::Acquire) {
            return Err(InitError::Migrating);
        }

        self.callback.store(callback as usize, Ordering::Release);
        self.expiry_ns.store(0, Ordering::Relaxed);
        self.interval_ns.store(0, Ordering::Relaxed);
        self.queue_seq.store(0, Ordering::Relaxed);
        self.base_id.store(NO_BASE, Ordering::Relaxed);
        self.migrating.store(false, Ordering::Relaxed);
        // `Uninit` 与 `Shutdown` 不可能在树中; 重置节点指针, 避免复用时带入旧链接.
        // SAFETY: 持 `TIMER_INIT_LOCK` 且定时器非 Pending/Running, 节点不在此刻的树中.
        unsafe { (*self.node.get()).reset() };
        // 字段写完之后才发布 Inactive: 并发的 start 看到 Inactive 时，回调与
        // expiry/interval 已就绪。
        self.state
            .store(HrTimerState::Inactive as u64, Ordering::Release);
        Ok(())
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

    /// 当前绑定的 CPU base 索引 (`None` = 未绑定)
    pub fn base_id(&self) -> Option<u32> {
        let id = self.base_id.load(Ordering::Acquire);
        if id == NO_BASE { None } else { Some(id) }
    }

    fn node_ptr(&self) -> core::ptr::NonNull<RbNode<TimerKey>> {
        // SAFETY: `UnsafeCell` 的地址恒非空, `node` 是结构体内存的一部分.
        unsafe { core::ptr::NonNull::new_unchecked(self.node.get()) }
    }

    /// 写树键。调用方必须持有该 timer 所属 base 的锁。
    fn set_node_key(&self, expiry: u64, seq: u64) {
        // SAFETY: 节点当前不在树中, 且调用方持有所属 base 锁; 不会有并发树遍历.
        unsafe { (*self.node.get()).key = (expiry, seq) };
    }

    /// 获取周期间隔 (纳秒)
    pub fn interval_ns(&self) -> u64 {
        self.interval_ns.load(Ordering::Acquire)
    }

    /// 获取当前状态
    pub fn state(&self) -> HrTimerState {
        match self.state.load(Ordering::Acquire) {
            1 => HrTimerState::Inactive,
            2 => HrTimerState::Pending,
            3 => HrTimerState::Running,
            4 => HrTimerState::Shutdown,
            _ => HrTimerState::Uninit,
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

/// base 索引哨兵: 未绑定任何 CPU.
const NO_BASE: u32 = u32::MAX;

/// `BaseInner::quiesce_target` 的"无目标"哨兵 (`MAX_CPUS` 远小于 u16 上限).
const NO_CPU: u16 = u16::MAX;

/// 全局激活序号计数器
static QUEUE_SEQ: AtomicU64 = AtomicU64::new(1);

/// 框架初始化标志
static HRTIMER_READY: AtomicBool = AtomicBool::new(false);

// ============================================================================
// 公共 API
// ============================================================================

/// 初始化 hrtimer 框架
///
/// 在内核启动早期调用一次。按 `smp::is_cpu_online` (BSP 自身始终视为在线)
/// 复位各 base：在线 CPU 置 `Online`，未上线 CPU 保持 `Offline`。AP 在本核
/// 开始调度前调用 [`hrtimer_cpu_online`] 上线自己的 base。
///
/// 本函数只能在无活动定时器时调用。
pub fn hrtimer_init() {
    let _init = TIMER_INIT_LOCK.lock();
    let current = current_cpu_for_timer();
    let mut online_count = 0u32;
    for (cpu, base) in TIMER_BASES.iter().enumerate() {
        let mut inner = base.base_lock.lock();
        inner.tree.clear();
        inner.running = 0;
        inner.migrations = 0;
        inner.quiesce_target = NO_CPU;
        let cpu_id = u32::try_from(cpu).unwrap_or(NO_BASE);
        let online = current == Some(cpu_id) || smp::is_cpu_online(cpu_id);
        inner.state = if online {
            online_count = online_count.saturating_add(1);
            BaseState::Online
        } else {
            BaseState::Offline
        };
    }
    ONLINE_BASE_COUNT.store(online_count, Ordering::Release);
    HRTIMER_READY.store(true, Ordering::Release);
}

/// 检查 hrtimer 框架是否已初始化
pub fn is_hrtimer_ready() -> bool {
    HRTIMER_READY.load(Ordering::Acquire)
}

/// timer 侧 CPU 上下线错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuTimerError {
    /// 目标 CPU 不存在、等于源 CPU，或目标 base 不处于 `Online`
    NoOnlineTarget,
    /// 源 CPU 是最后一个在线 CPU，不允许下线
    LastOnlineCpu,
    /// 源 base 已在 quiesce，或迁移状态冲突
    Busy,
}

/// CPU 上线时复位本核 base
///
/// 平台在本核开始调度前调用。若 base 已 `Online` 则幂等；`Offline` 则清空树、
/// 复位 `running` 并置 `Online`。**不做反向迁移**：原先迁走的 timer 不自动搬回，
/// 新 `start` 才按当前 CPU 绑定。
pub fn hrtimer_cpu_online(cpu: u32) {
    let Some(base) = base_for_cpu(cpu) else {
        return;
    };
    // 与 `hrtimer_init` 的全 base 复位串行: AP 上线不能和 boot 的"按实际在线
    // CPU 重置"交错, 否则在线计数会被两边各加一次. 锁序同 init: INIT -> BASE.
    let _init = TIMER_INIT_LOCK.lock();
    let mut inner = base.base_lock.lock();
    if inner.state == BaseState::Online {
        return;
    }
    debug_assert!(
        inner.tree.is_empty(),
        "CPU online 时 base 树非空: quiesce 未完成或平台协议违约"
    );
    inner.tree.clear();
    inner.running = 0;
    inner.migrations = 0;
    inner.quiesce_target = NO_CPU;
    inner.state = BaseState::Online;
    ONLINE_BASE_COUNT.fetch_add(1, Ordering::AcqRel);
}

/// CPU 下线前 quiesce 本核 timer base，并把 Pending timer 迁到 `target`
///
/// 调用约束：必须由平台 offline 线程在进程上下文调用，且不得在源 CPU 上等待；
/// `target` 在 quiesce 期间保持在线；同一时刻只有一个 CPU 执行 quiesce。
///
/// 流程：源 base 置 `Quiescing` 并记录 `quiesce_target` → 逐个把 Pending 节点
/// 离源、插目标（迁移保留 `expiry_ns`/`seq`，置 `migrating` 瞬态）→ 等待源
/// `running` 归零 → 置 `Offline`。迁移不双持源/目标 base 锁；`start` 在迁移中
/// 返回 `NotReady`，`try_cancel` 返回 `Busy`，`cancel` 等待瞬态清零后重试。
///
/// # Errors
///
/// - [`CpuTimerError::NoOnlineTarget`]：目标非法/离线
/// - [`CpuTimerError::LastOnlineCpu`]：源是最后一个在线 CPU
/// - [`CpuTimerError::Busy`]：源 base 已在 quiesce 或不在 `Online`
pub fn hrtimer_cpu_quiesce(cpu: u32, target: u32) -> Result<(), CpuTimerError> {
    if cpu == target {
        return Err(CpuTimerError::NoOnlineTarget);
    }
    let Some(source) = base_for_cpu(cpu) else {
        return Err(CpuTimerError::NoOnlineTarget);
    };
    let Some(target_base) = base_for_cpu(target) else {
        return Err(CpuTimerError::NoOnlineTarget);
    };
    if ONLINE_BASE_COUNT.load(Ordering::Acquire) <= 1 {
        // 只剩当前 CPU: 没有其它在线 CPU 可承接, 先报"最后一个在线 CPU"
        // (此时 target 即便索引有效也必然离线, 不把协议错误报成 NoOnlineTarget).
        return Err(CpuTimerError::LastOnlineCpu);
    }
    if target_base.base_lock.lock().state != BaseState::Online {
        return Err(CpuTimerError::NoOnlineTarget);
    }
    {
        let mut inner = source.base_lock.lock();
        if inner.state != BaseState::Online {
            return Err(CpuTimerError::Busy);
        }
        inner.state = BaseState::Quiescing;
        inner.quiesce_target = u16::try_from(target).unwrap_or(NO_CPU);
    }

    loop {
        if migrate_one_pending(source, target_base, target) {
            continue;
        }
        let done = {
            let inner = source.base_lock.lock();
            debug_assert!(
                inner.tree.is_empty(),
                "Quiescing base 树非空但没有可迁移的 Pending 节点"
            );
            inner.tree.is_empty() && inner.running == 0 && inner.migrations == 0
        };
        if done {
            break;
        }
        core::hint::spin_loop();
    }

    {
        let mut inner = source.base_lock.lock();
        debug_assert!(
            inner.tree.is_empty() && inner.running == 0 && inner.migrations == 0,
            "quiesce 收敛时源 base 仍有活动条目"
        );
        inner.state = BaseState::Offline;
        inner.quiesce_target = NO_CPU;
    }
    ONLINE_BASE_COUNT.fetch_sub(1, Ordering::AcqRel);
    Ok(())
}

/// 从源 base 摘一个 Pending timer，以原 `expiry_ns`/`seq` 插入目标 base.
///
/// 返回 `false` 表示当前没有可迁移节点（可能正有回调在途，由调用方继续等）。
/// 源锁与目标锁不重叠：摘除并置 `migrating` 后释放源锁，再锁目标插入。
fn migrate_one_pending(
    source: &'static TimerBase,
    target_base: &'static TimerBase,
    target: u32,
) -> bool {
    let Some((timer_ptr, expiry_ns, seq)) = ({
        let mut inner = source.base_lock.lock();
        // SAFETY: 本处持源 base 锁, 树结构不会被并发修改.
        let mut cursor = unsafe { inner.tree.first() };
        let mut found = None;
        while let Some(node) = cursor {
            // SAFETY: 树中节点由存活 `HrTimer` 提供; node 是首字段.
            let timer = unsafe { timer_from_node(node) };
            // SAFETY: 节点在树中 ⇒ timer 存活; 只读原子字段.
            let timer = unsafe { timer.as_ref() };
            if timer.state() == HrTimerState::Pending && !timer.migrating.load(Ordering::Acquire) {
                let expiry_ns = timer.expiry_ns();
                let seq = timer.activation_seq();
                found = Some((node, timer, expiry_ns, seq));
                break;
            }
            // SAFETY: 同上.
            cursor = unsafe { RbTree::<TimerKey>::next(node) };
        }
        if let Some((node, timer, expiry_ns, seq)) = found {
            // SAFETY: node 刚由源树返回, 仍在树中; 摘除后由目标树重插.
            unsafe { inner.tree.remove(node) };
            inner.migrations = inner.migrations.saturating_add(1);
            timer.migrating.store(true, Ordering::Release);
            let timer_ptr = core::ptr::NonNull::from(timer);
            Some((timer_ptr, expiry_ns, seq))
        } else {
            None
        }
    }) else {
        return false;
    };

    // SAFETY: 摘除源树后 timer 尚未被释放 (hrtimer_cancel 返回/超时前保持其存活);
    // migrating=true 阻止取消/重挂.
    let timer = unsafe { timer_ptr.as_ref() };
    let mut inner = target_base.base_lock.lock();
    debug_assert_eq!(
        inner.state,
        BaseState::Online,
        "quiesce_target 必须在迁移期间保持在线"
    );
    timer.base_id.store(target, Ordering::Release);
    timer.expiry_ns.store(expiry_ns, Ordering::Release);
    timer.queue_seq.store(seq, Ordering::Release);
    timer.set_node_key(expiry_ns, seq);
    timer.set_state(HrTimerState::Pending);
    // SAFETY: 已离源树, timer 存活; 目标锁内插入, 不与源锁双持.
    unsafe { inner.tree.insert(timer.node_ptr()) };
    timer.migrating.store(false, Ordering::Release);
    drop(inner);
    // 目标插入完成后才回落源迁移计数; 期间 quiesce 不会宣告源 base Offline.
    let mut source_inner = source.base_lock.lock();
    source_inner.migrations = source_inner.migrations.saturating_sub(1);
    true
}

/// 启动定时器 (绝对时间)
///
/// - `Inactive`: 绑定**当前 CPU** 的 base 并入树
/// - `Pending` / `Running`: 使用已有 `base_id`, 不跨 CPU 搬迁
/// - `Uninit` / `Shutdown`: 分别返回 `NotInitialized` / `Shutdown`
///
/// 成功返回 [`StartOutcome::Started`] 及本次激活的序号；失败返回明确原因。
///
/// # Arguments
/// * `timer` - 定时器对象 (必须已 init)
/// * `expiry_ns` - 绝对到期时间 (纳秒)
///
/// # Safety
///
/// - `timer` 在回调触发前保持有效内存
/// - 回调在 base 锁释放后执行, 允许在回调内调用本函数 (timerfd 周期路径)
pub fn hrtimer_start(timer: &HrTimer, expiry_ns: u64) -> StartOutcome {
    start_with_ready(timer, expiry_ns, HRTIMER_READY.load(Ordering::Acquire))
}

/// `hrtimer_start` 的框架就绪维单点实现
///
/// `ready` 显式传入，便于 host 测试覆盖 `NotReady` 而不改写全局就绪位
/// (全局位被并行用例共享，翻转会误伤其它测试).
fn start_with_ready(timer: &HrTimer, expiry_ns: u64, ready: bool) -> StartOutcome {
    if !ready {
        return StartOutcome::NotReady;
    }
    match timer.state() {
        HrTimerState::Uninit => StartOutcome::NotInitialized,
        HrTimerState::Shutdown => StartOutcome::Shutdown,
        HrTimerState::Inactive => start_inactive(timer, expiry_ns),
        HrTimerState::Pending | HrTimerState::Running => start_bound(timer, expiry_ns),
    }
}

/// `Inactive -> Pending`: 取 `TIMER_INIT_LOCK` 串行化首次绑定。
fn start_inactive(timer: &HrTimer, expiry_ns: u64) -> StartOutcome {
    let Some(cpu) = current_cpu_for_timer() else {
        return StartOutcome::NotReady;
    };
    start_inactive_on(timer, expiry_ns, cpu)
}

/// 解析 `Inactive` 定时器的实际落点：源 base `Quiescing`/`Offline` 时改绑
/// `quiesce_target`；没有可用目标返回 `None`。只在源/目标 base 锁内各看一眼，
/// 不双持两把 base 锁。
fn resolve_start_cpu(cpu: u32) -> Option<u32> {
    let base = base_for_cpu(cpu)?;
    let (state, target) = {
        let inner = base.base_lock.lock();
        (inner.state, inner.quiesce_target)
    };
    match state {
        BaseState::Online => Some(cpu),
        BaseState::Quiescing => {
            let target = u32::from(target);
            if target == u32::from(NO_CPU) || target == cpu {
                return None;
            }
            let target_base = base_for_cpu(target)?;
            if target_base.base_lock.lock().state == BaseState::Online {
                Some(target)
            } else {
                None
            }
        }
        BaseState::Offline => None,
    }
}

/// `start_inactive` 的显式 CPU 版本 (host 测试用它覆盖多 base 隔离).
fn start_inactive_on(timer: &HrTimer, expiry_ns: u64, cpu: u32) -> StartOutcome {
    let Some(cpu) = resolve_start_cpu(cpu) else {
        return StartOutcome::NotReady;
    };
    let _init = TIMER_INIT_LOCK.lock();
    match timer.state() {
        HrTimerState::Inactive => {}
        HrTimerState::Uninit => return StartOutcome::NotInitialized,
        HrTimerState::Shutdown => return StartOutcome::Shutdown,
        HrTimerState::Pending | HrTimerState::Running => return StartOutcome::NotReady,
    }

    let Some(base) = base_for_cpu(cpu) else {
        return StartOutcome::NotReady;
    };
    let mut inner = base.base_lock.lock();
    if inner.state != BaseState::Online || timer.state() != HrTimerState::Inactive {
        return StartOutcome::NotReady;
    }

    let seq = next_activation_seq();
    timer.base_id.store(cpu, Ordering::Release);
    timer.expiry_ns.store(expiry_ns, Ordering::Release);
    timer.queue_seq.store(seq, Ordering::Release);
    timer.set_node_key(expiry_ns, seq);
    timer.set_state(HrTimerState::Pending);
    // SAFETY: `timer` 是 `&HrTimer`, 地址非空; 节点存活由 `hrtimer_start` 的 `# Safety` 契约承担.
    unsafe { inner.tree.insert(timer.node_ptr()) };
    StartOutcome::Started(seq)
}

/// `Pending` / `Running` 的重挂: 只取已有 base 锁。
///
/// 源 base 处于 `Quiescing` 时（CPU offline 或回调内 Periodic 续期），把 timer
/// 从源树摘下、置 `migrating`，释放源锁后再插 `quiesce_target`；全程不双持
/// 两把 base 锁。`Pending` 的旧节点按原键摘除，新激活分配新 `seq`。
fn start_bound(timer: &HrTimer, expiry_ns: u64) -> StartOutcome {
    let Some(base_id) = timer.base_id() else {
        return StartOutcome::NotReady;
    };
    let Some(base) = base_for_cpu(base_id) else {
        return StartOutcome::NotReady;
    };
    let mut inner = base.base_lock.lock();

    match timer.state() {
        HrTimerState::Uninit => return StartOutcome::NotInitialized,
        HrTimerState::Shutdown => return StartOutcome::Shutdown,
        HrTimerState::Inactive => return StartOutcome::NotReady,
        HrTimerState::Pending | HrTimerState::Running => {}
    }
    if timer.migrating.load(Ordering::Acquire) {
        return StartOutcome::NotReady;
    }
    if timer.base_id() != Some(base_id) {
        return StartOutcome::NotReady;
    }

    let old_state = timer.state();
    match inner.state {
        BaseState::Online => {
            if old_state == HrTimerState::Pending {
                // SAFETY: Pending 的节点必在该 base 树中; 移除后节点即将以新键重新插入.
                unsafe { inner.tree.remove(timer.node_ptr()) };
            }

            let seq = next_activation_seq();
            timer.expiry_ns.store(expiry_ns, Ordering::Release);
            timer.queue_seq.store(seq, Ordering::Release);
            timer.set_node_key(expiry_ns, seq);
            timer.set_state(HrTimerState::Pending);
            // SAFETY: 同上; 两种入口都保证 timer 内存在回调前有效.
            unsafe { inner.tree.insert(timer.node_ptr()) };
            StartOutcome::Started(seq)
        }
        BaseState::Quiescing => {
            let target = u32::from(inner.quiesce_target);
            if target == u32::from(NO_CPU) || target == base_id {
                return StartOutcome::NotReady;
            }
            if old_state == HrTimerState::Pending {
                // SAFETY: Pending 的节点必在该 base 树中; 本处摘下后由目标 base 重插.
                unsafe { inner.tree.remove(timer.node_ptr()) };
            }
            inner.migrations = inner.migrations.saturating_add(1);
            timer.migrating.store(true, Ordering::Release);
            drop(inner);
            start_bound_on_target(timer, expiry_ns, target, base)
        }
        BaseState::Offline => StartOutcome::NotReady,
    }
}

/// 把 `start_bound` 从 `Quiescing` 源 base 摘下的 timer 插入目标 base。
///
/// 目标 base 已由 quiesce 协议保证在线；本处只锁目标 base，不同时持源锁。
fn start_bound_on_target(
    timer: &HrTimer,
    expiry_ns: u64,
    target: u32,
    source: &'static TimerBase,
) -> StartOutcome {
    let Some(base) = base_for_cpu(target) else {
        // 协议上 quiesce_target 必为有效在线索引; 防御性回滚, 不把半迁移的
        // timer 留在源 base 之外 (否则 quiesce 的 migrations 永远不归零).
        timer.migrating.store(false, Ordering::Release);
        timer.base_id.store(NO_BASE, Ordering::Release);
        timer.set_state(HrTimerState::Inactive);
        let mut source_inner = source.base_lock.lock();
        source_inner.migrations = source_inner.migrations.saturating_sub(1);
        return StartOutcome::NotReady;
    };
    let mut inner = base.base_lock.lock();
    debug_assert_eq!(
        inner.state,
        BaseState::Online,
        "quiesce_target 必须在迁移期间保持在线"
    );

    let seq = next_activation_seq();
    timer.base_id.store(target, Ordering::Release);
    timer.expiry_ns.store(expiry_ns, Ordering::Release);
    timer.queue_seq.store(seq, Ordering::Release);
    timer.set_node_key(expiry_ns, seq);
    timer.set_state(HrTimerState::Pending);
    // SAFETY: 节点已从源树摘下且 timer 存活; 目标树锁内插入, 不与源锁双持.
    unsafe { inner.tree.insert(timer.node_ptr()) };
    timer.migrating.store(false, Ordering::Release);
    drop(inner);
    // 迁移计数在目标插入后回落: quiesce 只有看到它归零才会把源 base 置 Offline.
    let mut source_inner = source.base_lock.lock();
    source_inner.migrations = source_inner.migrations.saturating_sub(1);
    StartOutcome::Started(seq)
}

/// 启动定时器 (相对时间)
///
/// 便捷接口: 从当前时间起延迟 `delay_ns` 纳秒后触发。
/// 未入队时返回 [`StartOutcome`] 的失败原因。
pub fn hrtimer_start_rel(timer: &HrTimer, delay_ns: u64) -> StartOutcome {
    let now = hrtimer_clock_read();
    hrtimer_start(timer, now.saturating_add(delay_ns))
}

/// 启动周期定时器
///
/// 首次在 `delay_ns` 纳秒后触发, 之后每 `interval_ns` 纳秒触发一次。
/// 未入队时返回 [`StartOutcome`] 的失败原因。
pub fn hrtimer_start_periodic(timer: &HrTimer, delay_ns: u64, interval_ns: u64) -> StartOutcome {
    timer.interval_ns.store(interval_ns, Ordering::Release);
    hrtimer_start_rel(timer, delay_ns)
}

/// 非阻塞取消定时器
///
/// 在所属 base 锁内摘除该定时器的 Pending 节点:
/// - `Cancelled`: 确实摘除了 Pending 激活, 返回即可释放 timer 内存
/// - `NotPending`: 没有待取消的激活
/// - `Busy`: 回调已被收集/正在执行, 不能安全释放; 等 `in_flight` 清零后重试,
///   或改用阻塞版 [`hrtimer_cancel`]
///
/// 与 `hrtimer_run_queues` 的收集互斥: 取消成功后 base 树不再持有该 timer 的
/// 节点地址, 调用方不会在下次 `run_queues` 时被踩到悬垂指针.
pub fn hrtimer_try_cancel(timer: &HrTimer) -> CancelOutcome {
    if timer.in_flight.load(Ordering::Acquire) || timer.migrating.load(Ordering::Acquire) {
        return CancelOutcome::Busy;
    }
    match timer.state() {
        HrTimerState::Pending => {}
        HrTimerState::Running => return CancelOutcome::Busy,
        HrTimerState::Uninit | HrTimerState::Inactive | HrTimerState::Shutdown => {
            return CancelOutcome::NotPending;
        }
    }

    let Some(base_id) = timer.base_id() else {
        return CancelOutcome::NotPending;
    };
    let Some(base) = base_for_cpu(base_id) else {
        return CancelOutcome::NotPending;
    };
    let mut inner = base.base_lock.lock();

    if timer.in_flight.load(Ordering::Acquire) || timer.migrating.load(Ordering::Acquire) {
        return CancelOutcome::Busy;
    }
    if timer.state() != HrTimerState::Pending {
        return CancelOutcome::NotPending;
    }
    if timer.base_id() != Some(base_id) {
        // 迁移刚把 base_id 改到目标, 调用方必须重试目标 base; 不能误报 NotPending
        // 让调用方释放仍挂在目标树中的 timer.
        return CancelOutcome::Busy;
    }
    // SAFETY: Pending 节点的节点地址在所属 base 树中, Pending 状态隐含 timer 存活.
    unsafe { inner.tree.remove(timer.node_ptr()) };
    timer.set_state(HrTimerState::Inactive);
    timer.queue_seq.store(0, Ordering::Release);
    timer.base_id.store(NO_BASE, Ordering::Release);
    CancelOutcome::Cancelled
}

/// 等待 `timer` 的在途回调退出且强制迁移完成.
///
/// 调用方必须保证当前上下文允许该回调推进 (不持有回调所需锁、中断已开启)。
fn wait_in_flight(timer: &HrTimer) {
    while timer.in_flight.load(Ordering::Acquire) || timer.migrating.load(Ordering::Acquire) {
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
/// 等待期间不持 base 锁, 但调用方不得持有回调所需的锁 —— 否则回调无法前进,
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

/// 关闭定时器: 置 `Shutdown` 状态、摘除 Pending 节点并等待在途回调退出.
///
/// `Shutdown` 状态使回调内的自重挂与框架 `Periodic` 重挂都不再入队; 返回后 timer
/// 可安全释放或复用 (`init` 会把它转回 `Inactive`)。与 `hrtimer_cancel` 相同的
/// 锁域/上下文约束.
pub fn hrtimer_shutdown(timer: &HrTimer) {
    loop {
        // 迁移先把节点离源、再插目标; 此时 base_id 不可信, 必须等瞬态结束.
        wait_in_flight(timer);
        let _init = TIMER_INIT_LOCK.lock();
        if timer.migrating.load(Ordering::Acquire) {
            // 等待期间迁移被重新拉起: 放掉 init 锁后重来, 不在错误 base 上摘节点.
            continue;
        }
        let state = timer.state();
        match state {
            HrTimerState::Pending | HrTimerState::Running => {
                if let Some(base_id) = timer.base_id() {
                    if let Some(base) = base_for_cpu(base_id) {
                        let mut inner = base.base_lock.lock();
                        if timer.migrating.load(Ordering::Acquire) {
                            drop(inner);
                            continue;
                        }
                        timer.set_state(HrTimerState::Shutdown);
                        timer.queue_seq.store(0, Ordering::Release);
                        if state == HrTimerState::Pending {
                            // SAFETY: Pending 节点地址属于该 base 树, Pending 状态隐含 timer 存活.
                            unsafe { inner.tree.remove(timer.node_ptr()) };
                            timer.base_id.store(NO_BASE, Ordering::Release);
                        }
                    } else {
                        timer.set_state(HrTimerState::Shutdown);
                        timer.queue_seq.store(0, Ordering::Release);
                    }
                } else {
                    timer.set_state(HrTimerState::Shutdown);
                    timer.queue_seq.store(0, Ordering::Release);
                }
            }
            HrTimerState::Uninit | HrTimerState::Inactive | HrTimerState::Shutdown => {
                timer.set_state(HrTimerState::Shutdown);
                timer.queue_seq.store(0, Ordering::Release);
                timer.base_id.store(NO_BASE, Ordering::Release);
                // SAFETY: 非 Pending/Running 不在树中; 清链接避免复用时带入旧指针.
                unsafe { (*timer.node.get()).reset() };
            }
        }
        break;
    }
    wait_in_flight(timer);
    if timer.state() == HrTimerState::Shutdown {
        timer.base_id.store(NO_BASE, Ordering::Release);
    }
}

/// 处理指定 CPU 上已到期的定时器
///
/// 在定时器中断处理程序中调用。只处理 `cpu` 自己的 base；回调在释放 base 锁后
/// 执行，允许回调内重挂。
///
/// # Safety
///
/// - 必须在中断上下文调用 (或中断禁用状态)
/// - 不可递归调用
pub fn hrtimer_run_queues(cpu: u32) {
    const MAX_EXPIRED_PER_RUN: usize = 64;
    if !HRTIMER_READY.load(Ordering::Acquire) {
        return;
    }
    let Some(base) = base_for_cpu(cpu) else {
        return;
    };

    for _ in 0..MAX_EXPIRED_PER_RUN {
        let now_ns = hrtimer_clock_read();

        // 在 base 锁内摘掉一个到期节点并置 Running/in_flight; 锁外执行回调.
        let collected = {
            let mut inner = base.base_lock.lock();
            if inner.state != BaseState::Online {
                break;
            }

            let mut found = None;
            // SAFETY: 本处持有 base 锁, 树结构不会被并发修改.
            let mut cursor = unsafe { inner.tree.first() };
            while let Some(node) = cursor {
                // SAFETY: 树中节点都由存活的 `HrTimer` 提供, 且 `node` 是首字段.
                let timer = unsafe { timer_from_node(node) };
                // SAFETY: 节点在树中即 timer 存活; 只读原子字段.
                let timer = unsafe { timer.as_ref() };
                let state = timer.state();
                if state == HrTimerState::Pending
                    && !timer.in_flight.load(Ordering::Acquire)
                    && !timer.migrating.load(Ordering::Acquire)
                    && timer.expiry_ns() <= now_ns
                {
                    let seq = timer.activation_seq();
                    let cb = timer.callback_fn();
                    found = Some((node, timer, cb, seq));
                    break;
                }
                // SAFETY: 同上.
                cursor = unsafe { RbTree::<TimerKey>::next(node) };
            }

            if let Some((node, timer, cb, seq)) = found {
                // SAFETY: `node` 刚由本 base 树返回, 仍在树中; timer 由其首字段反推.
                unsafe { inner.tree.remove(node) };
                timer.in_flight.store(true, Ordering::Release);
                timer.set_state(HrTimerState::Running);
                inner.running = inner.running.saturating_add(1);
                // SAFETY: 节点在树中 ⇒ timer 内存有效; 回调在锁外执行, 调用方契约
                // 要求回调前 timer 不释放.
                let timer_ptr = core::ptr::NonNull::from(timer);
                Some((timer_ptr, cb, seq))
            } else {
                None
            }
        };

        let Some((timer_ptr, callback, seq)) = collected else {
            break;
        };

        // SAFETY: 收集时节点仍在树中, 证明 timer 内存有效; 调用方契约保证回调
        // 期间不释放. 回调在 base 锁外执行, 允许回调内重挂/取消.
        let timer = unsafe { timer_ptr.as_ref() };
        dispatch_expired(timer, callback, seq, now_ns);

        let mut inner = base.base_lock.lock();
        inner.running = inner.running.saturating_sub(1);
    }
}

/// 查询指定 CPU 上最近的到期时间
///
/// 只扫描该 CPU 的 base；tickless 必须按 CPU 调用，不能返回全局最小值。
#[must_use]
pub fn hrtimer_next_expiry(cpu: u32) -> Option<u64> {
    let base = base_for_cpu(cpu)?;
    let inner = base.base_lock.lock();
    if inner.state != BaseState::Online {
        return None;
    }
    // SAFETY: 本处持有 base 锁.
    let mut cursor = unsafe { inner.tree.first() };
    while let Some(node) = cursor {
        // SAFETY: 节点在树中 ⇒ timer 存活; `node` 是首字段.
        let timer = unsafe { timer_from_node(node) };
        // SAFETY: `node` 由 base 树返回 ⇒ timer 存活; 只读原子状态.
        let timer = unsafe { timer.as_ref() };
        if timer.state() == HrTimerState::Pending
            && !timer.in_flight.load(Ordering::Acquire)
            && !timer.migrating.load(Ordering::Acquire)
        {
            return Some(timer.expiry_ns());
        }
        // SAFETY: 同上.
        cursor = unsafe { RbTree::<TimerKey>::next(node) };
    }
    None
}

/// 当前在所有 per-CPU base 中处于 Pending 的定时器数量.
///
/// 仅用于诊断/测试；热路径不应调用 (最坏 O(`MAX_CPUS`) 次加锁)。
#[must_use]
pub fn hrtimer_pending_count() -> usize {
    let mut total = 0;
    for base in &TIMER_BASES {
        total += base.base_lock.lock().tree.len();
    }
    total
}

/// 执行单个已到期条目的回调, 并收敛其状态.
///
/// 收集与执行之间定时器可能已被取消 (`queue_seq = 0`) 或重新入队 (新序号),
/// 此时条目的激活序号已失效 — 不得执行回调, 否则迟到回调会触碰已被复用/
/// 重新编排的状态 (timerfd 槽位复用场景下的真实缺陷). 回调内自行 `hrtimer_start`
/// 重挂时, 保留回调写入的 Pending 状态, 不被 `OneShot` 收敛覆盖.
/// 把仍属本次激活的 `Running` 收敛回 `Inactive`; 必须与 `hrtimer_start` 的
/// 「Pending 插入」在同一把 base 锁下互斥.
///
/// 锁外裸写的问题见 [`dispatch_expired`] 的 `OneShot` 分支: 另一 CPU 可能已把同一
/// timer 重挂为 Pending 并插入树, 此时再把状态覆盖为 Inactive 会留下"树中有节点、
/// 状态说未入队"的分裂态, 下一次 start 就会把同一节点插第二次。
fn collapse_running_to_inactive(timer: &HrTimer, seq: u64) {
    let Some(base_id) = timer.base_id() else {
        // 无 base 时 start 恒 NotReady, 不存在并发插入; 直接按 seq 复核后收敛.
        if timer.state() == HrTimerState::Running && timer.activation_seq() == seq {
            timer.set_state(HrTimerState::Inactive);
            timer.queue_seq.store(0, Ordering::Release);
        }
        return;
    };
    let Some(base) = base_for_cpu(base_id) else {
        return;
    };
    let _inner = base.base_lock.lock();
    if timer.base_id() != Some(base_id) {
        return;
    }
    if timer.state() == HrTimerState::Running && timer.activation_seq() == seq {
        timer.set_state(HrTimerState::Inactive);
        timer.queue_seq.store(0, Ordering::Release);
    }
}

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

    // shutdown 已置关闭状态: 不再执行回调, 保持 Shutdown 并放行等待方.
    if timer.state() == HrTimerState::Shutdown {
        timer.queue_seq.store(0, Ordering::Release);
        timer.in_flight.store(false, Ordering::Release);
        return;
    }

    let restart = callback(timer, seq);

    match restart {
        HrTimerRestart::OneShot => {
            // 回调内可能已自行重挂: 仅收敛仍处于 Running 的定时器.
            //
            // 收敛必须在 base 锁内按 `(Running, seq)` 复核: 回调返回后到本处之间,
            // 另一 CPU 可经 `hrtimer_start` 把同一 timer 重新置为 Pending 并**已插入**
            // 对应 base 树。锁外无条件写 Inactive 会把新激活覆盖成"未入队", 但节点
            // 仍在树中; 下一次 start 走 Inactive 路径再插同一节点 ⇒ 树内自环
            // (`rb insert fixup cycle`, UEFI 线程压力下实测)。
            collapse_running_to_inactive(timer, seq);
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
                    match hrtimer_start(timer, next) {
                        // Started: 已重挂. Shutdown: 保持 Shutdown，不得覆盖回 Inactive.
                        StartOutcome::Started(_) | StartOutcome::Shutdown => {}
                        // 框架未就绪/定时器未就绪: 本次重挂失败, 收敛回 Inactive;
                        // 迟到的 queue_seq 清零, 避免等待方误判仍 Pending.
                        StartOutcome::NotReady | StartOutcome::NotInitialized => {
                            collapse_running_to_inactive(timer, seq);
                        }
                    }
                } else {
                    collapse_running_to_inactive(timer, seq);
                }
            }
        }
    }

    // 所有路径统一放行等待方; 必须在状态收敛/重挂写入之后 (Release).
    timer.in_flight.store(false, Ordering::Release);
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
/// - per-CPU timer 队列已由条目 3 落地 (`TimerBase`)
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

    match hrtimer_start_rel(&timer, delay_ns) {
        StartOutcome::Started(_) => {}
        StartOutcome::NotReady => return Err(KernelError::NotReady),
        StartOutcome::NotInitialized => return Err(KernelError::NotInitialized),
        StartOutcome::Shutdown => return Err(KernelError::InvalidArgument),
    }

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

    /// `TIMER_BASES` 是进程级单例: 队列相关用例串行执行, 避免并行测试互相清空/入队.
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

    /// 该 timer 所属 base 的树是否仍持有其节点 (只比较指针, 不解引用).
    fn queue_contains(timer: &HrTimer) -> bool {
        let Some(base_id) = timer.base_id() else {
            return false;
        };
        let Some(base) = base_for_cpu(base_id) else {
            return false;
        };
        let inner = base.base_lock.lock();
        // SAFETY: 本处持有 base 锁, 测试串行访问树.
        let mut cursor = unsafe { inner.tree.first() };
        while let Some(node) = cursor {
            // SAFETY: 节点在树中 ⇒ timer 存活.
            let candidate = unsafe { timer_from_node(node) };
            if core::ptr::eq(candidate.as_ptr(), timer) {
                return true;
            }
            // SAFETY: 同上.
            cursor = unsafe { RbTree::<TimerKey>::next(node) };
        }
        false
    }

    fn current_cpu() -> u32 {
        smp::current_cpu_index()
    }

    /// 模拟 `run_queues` 的收集动作: 从所属 base 树摘节点、置 Running/in_flight.
    fn simulate_collected(timer: &HrTimer) {
        let base_id = timer.base_id().expect("Pending 定时器必须已绑定 base");
        let base = base_for_cpu(base_id).expect("base 必须存在");
        let mut inner = base.base_lock.lock();
        // SAFETY: Pending 节点在树中; 测试串行且 timer 存活.
        unsafe { inner.tree.remove(timer.node_ptr()) };
        inner.running = inner.running.saturating_add(1);
        timer.in_flight.store(true, Ordering::Release);
        timer.set_state(HrTimerState::Running);
    }

    /// 断言启动成功并返回非零入队序号.
    fn expect_started(outcome: StartOutcome) -> u64 {
        match outcome {
            StartOutcome::Started(seq) => {
                assert_ne!(seq, 0, "Started 必须携带非零序号");
                seq
            }
            StartOutcome::NotReady | StartOutcome::NotInitialized | StartOutcome::Shutdown => {
                panic!("定时器未入队: {outcome:?}")
            }
        }
    }

    #[test]
    fn test_base_inner_stays_within_image_budget() {
        // K-08: `TIMER_BASES` 是 MAX_CPUS=1024 个常驻 base; `BaseInner` 超过
        // 24 字节会把 x86 内核镜像重新顶过 `USER_CODE_BASE`. 本断言锁住预算.
        assert!(
            core::mem::size_of::<BaseInner>() <= 24,
            "BaseInner 超预算: {} bytes",
            core::mem::size_of::<BaseInner>()
        );
    }

    #[test]
    fn test_hrtimer_state_encoding() {
        assert_eq!(HrTimerState::Uninit as u64, 0);
        assert_eq!(HrTimerState::Inactive as u64, 1);
        assert_eq!(HrTimerState::Pending as u64, 2);
        assert_eq!(HrTimerState::Running as u64, 3);
        assert_eq!(HrTimerState::Shutdown as u64, 4);
    }

    #[test]
    fn test_hrtimer_uninit() {
        let timer = HrTimer::uninit();
        assert_eq!(timer.state(), HrTimerState::Uninit);
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
    fn test_start_uninit_is_rejected() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        assert_eq!(
            hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000)),
            StartOutcome::NotInitialized
        );
        assert!(!queue_contains(&timer));
        assert_eq!(timer.state(), HrTimerState::Uninit);
    }

    #[test]
    fn test_start_outcome_reports_not_ready() {
        let _serial = serialize_queue_tests();
        let timer = HrTimer::uninit();
        assert_eq!(
            start_with_ready(
                &timer,
                hrtimer_clock_read().saturating_add(1_000_000_000),
                false
            ),
            StartOutcome::NotReady
        );
    }

    #[test]
    fn test_init_rejects_active_timer() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        expect_started(hrtimer_start(
            &timer,
            hrtimer_clock_read().saturating_add(1_000_000_000),
        ));
        assert_eq!(timer.try_init(noop_callback), Err(InitError::Pending));

        // Running 也必须拒绝 (按真实收集语义先摘树).
        simulate_collected(&timer);
        assert_eq!(timer.try_init(noop_callback), Err(InitError::Running));

        // 模拟回调在途但状态已收敛: in_flight 非零也必须拒绝初始化.
        timer.set_state(HrTimerState::Inactive);
        timer.in_flight.store(true, Ordering::Release);
        assert_eq!(timer.try_init(noop_callback), Err(InitError::InFlight));
        timer.in_flight.store(false, Ordering::Release);
        let _ = hrtimer_try_cancel(&timer);
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
    fn test_per_cpu_base_isolation() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        hrtimer_cpu_online(1);

        let timer0 = HrTimer::uninit();
        timer0.init(noop_callback);
        let timer1 = HrTimer::uninit();
        timer1.init(noop_callback);

        expect_started(start_inactive_on(&timer0, 0, 0));
        expect_started(start_inactive_on(&timer1, 0, 1));
        assert_eq!(timer0.base_id(), Some(0));
        assert_eq!(timer1.base_id(), Some(1));
        assert_eq!(hrtimer_pending_count(), 2);
        assert_eq!(hrtimer_next_expiry(0), Some(0));
        assert_eq!(hrtimer_next_expiry(1), Some(0));

        // CPU0 分发只摘自己的树, CPU1 的定时器保持 Pending.
        hrtimer_run_queues(0);
        assert_eq!(timer0.state(), HrTimerState::Inactive);
        assert_eq!(timer1.state(), HrTimerState::Pending);
        assert_eq!(hrtimer_pending_count(), 1);

        hrtimer_run_queues(1);
        assert_eq!(timer1.state(), HrTimerState::Inactive);
        assert_eq!(hrtimer_pending_count(), 0);
        hrtimer_init();
    }

    #[test]
    fn test_offline_or_invalid_cpu_has_no_fallback() {
        let _serial = serialize_queue_tests();
        hrtimer_init();

        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        {
            let mut inner = base_for_cpu(0).expect("base0").base_lock.lock();
            inner.state = BaseState::Offline;
        }
        assert_eq!(
            start_inactive_on(&timer, 0, 0),
            StartOutcome::NotReady,
            "离线 base 必须拒绝, 不得落到全局后备队列"
        );
        assert_eq!(timer.state(), HrTimerState::Inactive);
        assert_eq!(timer.base_id(), None);
        assert_eq!(hrtimer_pending_count(), 0);

        assert_eq!(
            start_inactive_on(&timer, 0, MAX_CPUS as u32),
            StartOutcome::NotReady,
            "超出容量的 CPU 索引必须拒绝"
        );
        hrtimer_init(); // 恢复 base0 状态
    }

    #[test]
    fn test_next_expiry_empty() {
        // UT-06 (2026-09-24): 先 hrtimer_init() 清空所有 base 使断言与环境无关;
        // TIMER_BASES 是进程级单例, 本用例与其它队列用例经 QUEUE_TEST_LOCK 串行.
        let _serial = serialize_queue_tests();
        hrtimer_init();
        assert!(hrtimer_next_expiry(current_cpu()).is_none());
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
        expect_started(hrtimer_start(
            &timer,
            hrtimer_clock_read().saturating_add(1_000_000_000),
        ));
        assert!(queue_contains(&timer), "启动后队列必须持有该 timer");

        assert!(hrtimer_cancel(&timer));
        assert!(
            !queue_contains(&timer),
            "取消必须在 base 锁内真出队 —— 否则调用方释放 timer 后树持悬垂 NonNull"
        );
    }

    #[test]
    fn test_try_cancel_pending_and_busy() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        expect_started(hrtimer_start(
            &timer,
            hrtimer_clock_read().saturating_add(1_000_000_000),
        ));

        assert_eq!(
            hrtimer_try_cancel(&timer),
            CancelOutcome::Cancelled,
            "Pending 激活必须被 try_cancel 摘除"
        );
        assert!(!queue_contains(&timer));
        assert_eq!(timer.state(), HrTimerState::Inactive);

        // 模拟 run_queues 已收集、回调在途: try_cancel 必须报 Busy 且不摘队列.
        timer.init(noop_callback);
        expect_started(hrtimer_start(&timer, 0));
        simulate_collected(&timer);
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
        expect_started(hrtimer_start(
            &timer,
            hrtimer_clock_read().saturating_add(1_000_000_000),
        ));
        assert!(queue_contains(&timer));

        hrtimer_shutdown(&timer);
        assert!(!queue_contains(&timer), "shutdown 后队列不得再持有该 timer");
        assert_eq!(timer.state(), HrTimerState::Shutdown);
        assert_eq!(hrtimer_pending_count(), 0);

        // Shutdown 状态: 直接 start 不得复活.
        assert_eq!(
            hrtimer_start(&timer, hrtimer_clock_read().saturating_add(1_000_000_000)),
            StartOutcome::Shutdown
        );
        assert_eq!(hrtimer_pending_count(), 0, "shutdown 后 start 必须被拒绝");

        // init 清除 Shutdown 后可重新使用.
        timer.init(noop_callback);
        assert_eq!(timer.state(), HrTimerState::Inactive);
        expect_started(hrtimer_start(
            &timer,
            hrtimer_clock_read().saturating_add(1_000_000_000),
        ));
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
        expect_started(hrtimer_start(
            &timer,
            hrtimer_clock_read().saturating_add(1_000_000_000),
        ));
        // 模拟 run_queues 已收集、回调在途.
        simulate_collected(&timer);

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
    fn test_shutdown_waits_for_migration_before_unlink() {
        use std::time::{Duration, Instant};

        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        expect_started(hrtimer_start(
            &timer,
            hrtimer_clock_read().saturating_add(1_000_000_000),
        ));
        timer.migrating.store(true, Ordering::Release);

        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                timer.migrating.store(false, Ordering::Release);
            });
            hrtimer_shutdown(&timer);
        });
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "shutdown 必须等迁移瞬态结束, 不得在错误 base 上摘节点"
        );
        assert_eq!(timer.state(), HrTimerState::Shutdown);
        assert!(!queue_contains(&timer));
    }

    #[test]
    fn test_cancel_then_drop_leaves_nothing_to_dispatch() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = Box::new(HrTimer::uninit());
        timer.init(cancelled_timer_callback);
        // 已到期: 若取消未出队, 下一次 run_queues 会踩释放后的内存.
        expect_started(hrtimer_start(&timer, 0));
        assert!(hrtimer_cancel(&timer));
        assert!(!queue_contains(&timer), "取消后队列不得再持有该 timer");
        drop(timer);

        hrtimer_run_queues(current_cpu());
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

    /// OneShot 收尾不得覆盖"回调返回后由另一 CPU 重挂"的新激活.
    ///
    /// 旧实现在 base 锁外直接 `if state == Running { set Inactive; queue_seq=0 }`:
    /// 外部 start 已把状态置 Pending 并插入树后, 这次覆盖会留下"树中有节点、状态
    /// 说未入队"的分裂态; 下一次 start 走 Inactive 路径二次插入同一节点 ⇒
    /// `rb insert fixup cycle` (UEFI 线程压力下实测). 收敛必须持 base 锁并按
    /// `(Running, seq)` 复核.
    #[test]
    fn test_one_shot_collapse_preserves_external_rearm() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(stale_guard_callback);
        let seq = expect_started(start_inactive_on(&timer, hrtimer_clock_read(), 0));
        simulate_collected(&timer);

        // 回调返回后、框架收尾前, 外部 start 已重挂: 状态 Pending + 新序号.
        timer.queue_seq.store(99, Ordering::Release);
        timer.set_state(HrTimerState::Pending);
        collapse_running_to_inactive(&timer, seq);

        assert_eq!(
            timer.state(),
            HrTimerState::Pending,
            "外部重挂的新激活不得被 OneShot 收尾覆盖成 Inactive"
        );
        assert_eq!(timer.activation_seq(), 99);
        // 该用例中的新激活未真插入树 (只模拟状态互斥), 清干净后重建框架.
        timer.set_state(HrTimerState::Inactive);
        timer.queue_seq.store(0, Ordering::Release);
        timer.base_id.store(NO_BASE, Ordering::Release);
        hrtimer_init();
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

    #[test]
    fn test_init_rejects_migrating_timer() {
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        timer.migrating.store(true, Ordering::Release);
        assert_eq!(timer.try_init(noop_callback), Err(InitError::Migrating));
        timer.migrating.store(false, Ordering::Release);
    }

    #[test]
    fn test_cpu_quiesce_migrates_pending_preserving_seq() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        hrtimer_cpu_online(1);

        let timer0 = HrTimer::uninit();
        timer0.init(noop_callback);
        let timer1 = HrTimer::uninit();
        timer1.init(noop_callback);
        let expiry_ns = 42_000_000;
        let seq0 = expect_started(start_inactive_on(&timer0, expiry_ns, 0));
        let seq1 = expect_started(start_inactive_on(&timer1, expiry_ns, 0));
        assert_eq!(timer0.base_id(), Some(0));
        assert_eq!(timer1.base_id(), Some(0));

        assert_eq!(hrtimer_cpu_quiesce(0, 1), Ok(()));

        assert_eq!(timer0.base_id(), Some(1), "Pending 必须迁到目标 base");
        assert_eq!(timer1.base_id(), Some(1));
        assert_eq!(timer0.state(), HrTimerState::Pending);
        assert_eq!(timer1.state(), HrTimerState::Pending);
        assert_eq!(timer0.activation_seq(), seq0, "迁移保留激活序号");
        assert_eq!(timer1.activation_seq(), seq1);
        assert_eq!(hrtimer_next_expiry(0), None, "源 base 已 Offline");
        assert_eq!(hrtimer_next_expiry(1), Some(expiry_ns));
        assert_eq!(hrtimer_pending_count(), 2);

        assert!(hrtimer_cancel(&timer0));
        assert!(hrtimer_cancel(&timer1));
        hrtimer_init();
    }

    #[test]
    fn test_cpu_quiesce_rejects_last_online_cpu() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        assert_eq!(hrtimer_cpu_quiesce(0, 1), Err(CpuTimerError::LastOnlineCpu));
    }

    #[test]
    fn test_cpu_quiesce_rejects_offline_target() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        hrtimer_cpu_online(1);
        hrtimer_cpu_online(2);
        assert_eq!(hrtimer_cpu_quiesce(2, 1), Ok(()));
        // CPU2 已 Offline; CPU0 仍 Online, 但目标不在线.
        assert_eq!(
            hrtimer_cpu_quiesce(0, 2),
            Err(CpuTimerError::NoOnlineTarget)
        );
        hrtimer_init();
    }

    #[test]
    fn test_cpu_quiesce_rejects_busy_source() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        hrtimer_cpu_online(1);
        {
            let mut inner = base_for_cpu(0).expect("base0").base_lock.lock();
            inner.state = BaseState::Quiescing;
            inner.quiesce_target = 1;
        }
        assert_eq!(hrtimer_cpu_quiesce(0, 1), Err(CpuTimerError::Busy));
        hrtimer_init();
    }

    #[test]
    fn test_inactive_start_during_quiesce_uses_target() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        hrtimer_cpu_online(1);
        {
            let mut inner = base_for_cpu(0).expect("base0").base_lock.lock();
            inner.state = BaseState::Quiescing;
            inner.quiesce_target = 1;
        }

        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        expect_started(start_inactive_on(&timer, 7, 0));
        assert_eq!(
            timer.base_id(),
            Some(1),
            "Quiescing 源上的新激活改绑 target"
        );
        assert_eq!(timer.state(), HrTimerState::Pending);
        assert_eq!(hrtimer_next_expiry(0), None);
        assert_eq!(hrtimer_next_expiry(1), Some(7));
        assert!(hrtimer_cancel(&timer));
        hrtimer_init();
    }

    #[test]
    fn test_migrating_timer_rejects_cancel_and_start() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        let timer = HrTimer::uninit();
        timer.init(noop_callback);
        expect_started(start_inactive_on(&timer, 9, 0));

        timer.migrating.store(true, Ordering::Release);
        assert_eq!(
            hrtimer_try_cancel(&timer),
            CancelOutcome::Busy,
            "迁移瞬态中的取消必须 Busy"
        );
        assert_eq!(
            hrtimer_start(&timer, 11),
            StartOutcome::NotReady,
            "迁移瞬态中的 start 必须 NotReady"
        );
        assert_eq!(timer.base_id(), Some(0), "迁移未完成不得改 base_id");
        timer.migrating.store(false, Ordering::Release);
        assert!(hrtimer_cancel(&timer));
        hrtimer_init();
    }

    fn periodic_rehang_callback(_timer: &HrTimer, _seq: u64) -> HrTimerRestart {
        HrTimerRestart::Periodic
    }

    #[test]
    fn test_periodic_callback_rehangs_to_quiesce_target() {
        let _serial = serialize_queue_tests();
        hrtimer_init();
        hrtimer_cpu_online(1);

        let timer = HrTimer::uninit();
        timer.init(periodic_rehang_callback);
        timer.interval_ns.store(1_000_000, Ordering::Release);
        let seq = expect_started(start_inactive_on(&timer, hrtimer_clock_read(), 0));
        simulate_collected(&timer);
        {
            let mut inner = base_for_cpu(0).expect("base0").base_lock.lock();
            inner.state = BaseState::Quiescing;
            inner.quiesce_target = 1;
        }

        dispatch_expired(&timer, periodic_rehang_callback, seq, hrtimer_clock_read());

        assert_eq!(
            timer.base_id(),
            Some(1),
            "Periodic 续期必须落到 quiesce_target, 不回源 base"
        );
        assert_eq!(timer.state(), HrTimerState::Pending);
        assert!(!timer.in_flight.load(Ordering::Acquire));
        assert_eq!(hrtimer_next_expiry(0), None);
        assert!(hrtimer_next_expiry(1).is_some());
        {
            let inner = base_for_cpu(0).expect("base0").base_lock.lock();
            assert_eq!(inner.migrations, 0, "target 插入后迁移计数必须归零");
        }
        assert!(hrtimer_cancel(&timer));
        hrtimer_init();
    }
}

// ============================================================================
// 内核测试 (QEMU feature gate)
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_state_encoding() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    assert_eq_test!(HrTimerState::Uninit as u64, 0, "Uninit=0");
    assert_eq_test!(HrTimerState::Inactive as u64, 1, "Inactive=1");
    assert_eq_test!(HrTimerState::Pending as u64, 2, "Pending=2");
    assert_eq_test!(HrTimerState::Running as u64, 3, "Running=3");
    assert_eq_test!(HrTimerState::Shutdown as u64, 4, "Shutdown=4");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_uninit_state() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test, check};
    let timer = HrTimer::uninit();
    assert_eq_test!(timer.state(), HrTimerState::Uninit, "state");
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
        check!(
            hrtimer_start(timer, 1_000_000_000).is_started(),
            "start accepted"
        );
    }
    assert_eq_test!(hrtimer_pending_count(), 1, "1 timer");

    let next = hrtimer_next_expiry(crate::privileged::smp::current_cpu_index());
    check!(next.is_some(), "has next expiry");
    assert_eq_test!(next.unwrap(), 1_000_000_000, "expiry matches");

    // SAFETY: 经 &raw mut 取裸指针避免对 static mut 取引用
    hrtimer_cancel(unsafe { &*core::ptr::addr_of!(T1) });
    // 取消在 base 锁内立即出队, 无需等下一次 run_queues 清理
    hrtimer_run_queues(crate::privileged::smp::current_cpu_index());
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
        check!(
            hrtimer_start(timer, now + 1_000_000).is_started(), // 1ms 后
            "periodic start accepted"
        );
    }
    check!(
        // SAFETY: T2 是本测试独占的 static mut, 上方已经 `addr_of_mut!` 完成 init 与
        // `hrtimer_start`; 本处经 `addr_of!` 取址再解引用, 不对 static mut 构造共享引用,
        // `is_pending()` 经 `state()` 读 AtomicU64, 是原子读而非裸字段读。
        unsafe { (*core::ptr::addr_of!(T2)).is_pending() },
        "pending after start"
    );

    // 运行队列 (可能未到期, 但测试回调逻辑)
    hrtimer_run_queues(crate::privileged::smp::current_cpu_index());
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
