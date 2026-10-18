//! # Framekernel 调度器
//!
//! ## 调度策略 (I-35: MLFQ 与 CFS 二选一)
//!
//! Framekernel 采用三类调度器并存的分层架构, **没有冗余**:
//!
//! | 策略 | `SchedPolicy` | 实现 | 适用进程 |
//! |------|-------------|------|----------|
//! | DL   | `Deadline`  | Earliest-Deadline-First (EDF) + CBS | `SCHED_DEADLINE` 实时 |
//! | RT   | `Fifo`/`Rr` | 固定优先级 FIFO + 时间片 RR       | `SCHED_FIFO/RR` 实时 |
//! | CFS  | `Normal`    | vruntime 红黑树 (Linux CFS 风格)  | `SCHED_NORMAL` 普通进程 |
//!
//! **MLFQ 已退役**: 历史上 MLFQ 的多级反馈队列 (level 0..3 + 时间片 `[10,20,40,80]` ms)
//! 已完全被 CFS 取代 (注释中保留 "preserved from MLFQ" 仅为历史可追溯性).
//! `add_to_run_queue` 路径已重定向到 `cfs_enqueue`; `boost_priority` 死代码已删除.
//! CFS 的反饥饿不靠周期性 boost, 而靠 `CfsRunQueue` 的**单调 vruntime 下界**
//! 随时基运行任务推进 (DECISION-099; 旧 `boost_all_vruntime` 因折叠相对次序
//! 且与任务自身 vruntime 分域, 已删除).
//!
//! ## 调度决策链
//!
//! `schedule()` 严格按 DL → RT → CFS 顺序回退, 每个层级内部找不到可运行任务时
//! 立即降级到下一层级, 与 Linux `pick_next_task` 行为一致.

use crate::privileged::sync::{IrqSpinLock as Mutex, OnceLock};
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

use super::cfs::{
    CfsRunQueue, DL_MAX_UTILIZATION_PCT, DeadlineParams, DlRunQueue, LOAD_BALANCE_THRESHOLD,
    NICE0_WEIGHT, TARGET_LATENCY_TICKS, calc_vruntime_delta, cfs_should_preempt, nice_to_weight,
};
use super::process::{MIGRATE_PENDING, NO_RQ_CPU, PROCESS_TABLE, Process};
use super::types::{BlockReason, Pid, ProcessContext, ProcessId, ProcessPriority, ProcessState};

// === E2: unsafe 集中化 — 裸子模块 ===
//
// FFI 调用与每 CPU 指针解引用无法在 safe Rust 中表达,
// 在此集中封装.
pub(crate) mod raw {
    /// 更新每 CPU 的 `current_process_ptr`, 供汇编入口路径使用.
    ///
    /// # Safety
    /// - `ptr` 必须是合法的 `*const Process` 或 0
    #[inline(always)]
    pub unsafe fn update_current_process_ptr(ptr: u64) {
        unsafe {
            // SAFETY: update_current_process_ptr 由调度器汇编路径提供, 供中断/上下文切换调用
            unsafe extern "C" {
                fn update_current_process_ptr(ptr: u64);
            }
            update_current_process_ptr(ptr);
        }
    }
}

use raw::update_current_process_ptr;

macro_rules! klog_sched_warn {
    ($($arg:tt)*) => {
        $crate::klog_ffi!(klog_ffi_warn, $($arg)*)
    };
}

const RT_PRIORITY_MAX: u8 = 99;
const RT_TIME_SLICE: u64 = 5;
const RT_FIFO_WATCHDOG: u64 = 500;

/// kswapd softirq 唤醒周期 (ticks). 100 ticks @ 1kHz timer = 100ms.
/// B3 完整实现: 周期触发 kswapd, 软中断上下文回收不活跃页面.
const KSWAPD_TICK_INTERVAL: u64 = 100;

/// hotplug softirq 唤醒周期 (ticks). 100 ticks @ 1kHz timer = 100ms.
/// 周期触发热插拔槽位轮询 (softirq 上下文读取 PCIe Slot Status).
/// 非热插拔场景下 poll 无任何 PCI 配置空间访问, 开销可忽略.
const HOTPLUG_TICK_INTERVAL: u64 = 100;

/// 周期负载均衡触发窗口 (ticks). 64 ticks @ 1kHz timer = 64ms.
///
/// 每窗口只允许一个核发起一轮 —— `load_balance` 是 pull 型 (从最忙核偷到自己核),
/// 各核同窗各自发起即双向互偷 (见 [`LAST_BALANCE_TICK`]).
const BALANCE_TICK_INTERVAL: u64 = 64;

/// 僵尸回收周期 (ticks). 1000 ticks @ 1kHz timer = 1s.
///
/// 同样每窗口只允许一个核执行: 回收路径末端是 `remove_and_free`, 两核同窗对同一
/// pid 走该路径即 double-free (见 [`LAST_REAP_TICK`]).
const REAP_TICK_INTERVAL: u64 = 1_000;

/// 单次偷取的上限 (任务数). 与旧字面量 `4` 同值, 提为常量供候选缓冲定长.
const LOAD_BALANCE_MAX_MOVE: usize = 4;

/// 队列归属认领的有限重试次数.
///
/// 认领窗口只在"持 rq 锁搬树节点"的微临界区内 (全程关中断), 正常不会重试;
/// 重试耗尽即放弃本轮搬动 (任务仍留在原队列, 不丢可运行性).
const RQ_CLAIM_RETRY: usize = 32;

/// PWM 资源的 CFS 带宽配额槽位 — 限制指定 PWM 的 CPU 带宽, 并记录本周期已消耗的运行时间。
///
/// `max_runtime` 与 `period` 构成带宽约束: 每 `period` 内任务最多运行
/// `max_runtime`; `consumed`/`next_reset` 用于跨 tick 累计与周期重置。
pub struct PwidQuota {
    pub pwm: u64,
    pub used: bool,
    pub max_runtime: u64,
    pub period: u64,
    pub consumed: u64,
    pub next_reset: u64,
}

impl PwidQuota {
    const fn new() -> Self {
        Self {
            pwm: 0,
            used: false,
            max_runtime: 0,
            period: 0,
            consumed: 0,
            next_reset: 0,
        }
    }
}

use crate::privileged::constants::limits::{MAX_LIMITS, MAX_QUOTAS};

/// PWM 资源的进程数上限槽位 — 限制指定 PWM 可同时存在的进程数。
///
/// `max_procs` 为上限, `current` 为当前已归属该 PWM 的进程数。
pub struct PwidLimit {
    pub pwm: u64,
    pub used: bool,
    pub max_procs: u32,
    pub current: u32,
}

/// 周期任务的上次触发时刻 (tick 单位).
///
/// 与 `timer::get_ticks()` 比较而非 `is_multiple_of` 取模: 时基改由硬件计数器
/// 派生后, 读数可能在一次 tick 之间跳过整点 (重新建锚 / E-III 停 tick 后的
/// 追赶补偿), 取模判断会永久漏触发 (P-1 收敛时一并修).
///
/// **每窗口单名额** (CAS 赢家才执行) 是 E-II-b 后的新要求: 每个核都收 tick, 而
/// `get_ticks()` 是全局派生读数, 若只比时间窗不夺名额, 各核会在同一个 tick 上
/// 各自发起同一轮全局副作用 (互相偷取运行队列 / 重复回收同一 pid).
static LAST_KSWAPD_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_HOTPLUG_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_BALANCE_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_REAP_TICK: AtomicU64 = AtomicU64::new(0);

/// 调度策略 — 决定任务进入哪条运行队列及其抢占语义。
///
/// `Normal` 为 CFS 公平调度; `Fifo`/`Rr` 为实时策略 (先进先出 / 时间片轮转);
/// `Idle` 仅在本核无其它可运行任务时执行; `Deadline` 为截止时间调度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedPolicy {
    Normal = 0,
    Fifo = 1,
    Rr = 2,
    Idle = 3,
    Deadline = 4,
}

impl SchedPolicy {
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Normal,
            1 => Self::Fifo,
            2 => Self::Rr,
            3 => Self::Idle,
            4 => Self::Deadline,
            _ => Self::Normal,
        }
    }
}

/// 实时任务信息 — 记录实时运行队列中任务的 pid、静态实时优先级、调度策略与剩余时间片。
pub struct RtTaskInfo {
    pub pid: Pid,
    pub rt_priority: u8,
    pub policy: SchedPolicy,
    pub time_slice_remaining: u64,
}

struct PerCpuSched {
    rt_queue: Mutex<VecDeque<RtTaskInfo>>,
    cfs_rq: Mutex<CfsRunQueue>,
    dl_rq: Mutex<DlRunQueue>,
    current: AtomicU32,
    /// 本 CPU 的 idle 任务 pid (0 = 尚未创建).
    ///
    /// idle 任务不进入任何运行队列 (CFS/RT/DL), 仅作为 `schedule()` 在
    /// 本地无候选任务时的最后兜底, 使调度器在运行期永不为 `None`.
    idle: AtomicU32,
    /// 本 CPU 最近一次被切走的任务 pid (0 = 无).
    ///
    /// 由 `schedule()` 在上下文切换**前**写入; 由下一次 `schedule()` 入口的
    /// `reap_off_cpu` 消费 —— 彼时本 CPU 已在新任务上下文运行, 被切走者确已
    /// 离开 CPU, 可安全完成其退出回收 (方案 B: exit/回收竞态根治).
    prev: AtomicU32,
    /// 单向切换用的临时上下文落点 (方案 B 兜底).
    ///
    /// 若 `current` 在进程表中不可解析 (不变式违反), 该上下文永不可能再被恢复,
    /// 其内核栈亦可能已释放 —— 绝不能写回其 PCB 的 `context`, 故改用本 scratch
    /// 承接被丢弃的 prev 寄存器, 单向切到 next (见 `schedule()` 末段).
    scratch_ctx: crate::privileged::racy_cell::RacyCell<ProcessContext>,
    need_reschedule: AtomicBool,
    rt_running: AtomicBool,
    dl_running: AtomicBool,
    fifo_watchdog: AtomicU64,
}

// 所有字段 (Mutex<VecDeque<Pid>>, Mutex<CfsRunQueue>, Mutex<DlRunQueue>, Atomic*) 自动实现 Send + Sync.

/// BSP 的 per-CPU 调度状态 (静态实例).
///
/// BSP 的调度状态在启动期即需可用; 用 `OnceLock` 惰性构造 —— 因 `PerCpuSched`
/// 含 CFS/DL 运行队列 (其 `new()` 非 `const`), 无法直接静态零初始化.
static SCHED_BSP: OnceLock<PerCpuSched> = OnceLock::new();

/// AP 的 per-CPU 调度状态表 (按 `cpu_index` 索引, 槽位 0 保留给 BSP).
///
/// AP 的状态由 [`init_per_cpu_sched`] 在 AP 上线前按需从页池分配, 以**内核
/// 高半区直接映射别名**存放 —— aarch64 运行期 TTBR0 的每进程 EL1 视图刻意移除
/// DRAM 块, 低半区物理地址在运行期并非有效可解引用地址, 故必须走高半区别名.
/// 槽位为 `null` 表示尚未分配.
static PER_CPU_SCHED: [AtomicPtr<PerCpuSched>; crate::privileged::config::MAX_CPUS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; crate::privileged::config::MAX_CPUS];

/// 构造一个新的 per-CPU 调度状态 (全字段初值).
fn new_per_cpu_sched() -> PerCpuSched {
    PerCpuSched {
        rt_queue: Mutex::new(VecDeque::new()),
        cfs_rq: Mutex::named("cfs_rq", CfsRunQueue::new()),
        dl_rq: Mutex::new(DlRunQueue::new()),
        current: AtomicU32::new(0),
        idle: AtomicU32::new(0),
        prev: AtomicU32::new(0),
        scratch_ctx: crate::privileged::racy_cell::RacyCell::new(ProcessContext::new()),
        need_reschedule: AtomicBool::new(false),
        rt_running: AtomicBool::new(false),
        dl_running: AtomicBool::new(false),
        fifo_watchdog: AtomicU64::new(0),
    }
}

/// 将 CPU 编号映射到 per-CPU 调度槽位下标.
///
/// 槽位容量与 `MAX_CPUS` 一致; 取模仅为防御越界 (越界时退化为槽位别名而非 UB).
#[inline]
fn sched_slot(cpu_id: u32) -> usize {
    (cpu_id as usize) % crate::privileged::config::MAX_CPUS
}

/// 获取 BSP 调度状态 (惰性构造, 幂等).
#[inline]
fn bsp_sched() -> &'static PerCpuSched {
    SCHED_BSP.get_or_init(|slot| {
        slot.write(new_per_cpu_sched());
    })
}

/// 为指定 `cpu_index` 分配 per-CPU 调度状态 (幂等).
///
/// 槽位 0 走 BSP 静态实例. 返回 `false` 表示页池分配失败.
fn sched_alloc_cpu(cpu_index: u32) -> bool {
    let idx = sched_slot(cpu_index);
    if idx == 0 {
        bsp_sched();
        return true;
    }
    if !PER_CPU_SCHED[idx].load(Ordering::Acquire).is_null() {
        return true;
    }
    let Some(ptr) = crate::privileged::mm::alloc_zeroed_page_as::<PerCpuSched>() else {
        return false;
    };
    // 页已清零, 但 PerCpuSched 含非全零构造 (VecDeque/BTreeMap 元数据), 须就地构造.
    // SAFETY: ptr 来自页池新分配页 (排他所有权), 高半区直接映射可写; 此路径在
    // AP 启动期单线程执行, 无并发写入该槽位.
    unsafe { ptr.write(new_per_cpu_sched()) };
    PER_CPU_SCHED[idx].store(ptr, Ordering::Release);
    true
}

/// 获取指定 CPU 的 per-CPU 调度状态引用.
///
/// # 不变量
///
/// 调用方保证目标槽位已分配 (启动时序: `init_per_cpu_sched` 早于该 CPU 使用
/// 调度器). 若槽位为 `null` (目标 `cpu_index` 尚未分配调度状态), 回退 BSP 状态
/// 以保证内存安全.
#[inline]
fn sched_for(cpu_id: u32) -> &'static PerCpuSched {
    let idx = sched_slot(cpu_id);
    if idx == 0 {
        return bsp_sched();
    }
    let ptr = PER_CPU_SCHED[idx].load(Ordering::Acquire);
    if ptr.is_null() {
        // SAFETY: 回退 BSP 仅用于避免 null 解引用 (见 doc 的槽位未分配情形).
        return bsp_sched();
    }
    // SAFETY: 指针由 sched_alloc_cpu 以 Release 发布, 指向页池分配并已构造的
    // PerCpuSched; 高半区直接映射使其运行期可访问; 只读/原子访问.
    unsafe { &*ptr }
}

/// 初始化指定 `cpu_id` 的 per-CPU 调度状态 (上线前一次性, 幂等).
///
/// 返回 `false` 表示页池分配失败, 由调用方放弃启动该 AP.
pub fn init_per_cpu_sched(cpu_id: u32) -> bool {
    sched_alloc_cpu(cpu_id)
}

#[inline]
fn per_cpu() -> &'static PerCpuSched {
    sched_for(crate::privileged::smp::get_current_cpu())
}

#[inline]
fn per_cpu_for(cpu_id: u32) -> &'static PerCpuSched {
    sched_for(cpu_id)
}

/// 每 CPU idle 任务的入口: 无条件停机等待中断, 永不返回.
///
/// 由 [`Scheduler::init_per_cpu_idle`] 作为 `context.rip` 写入 idle 任务的
/// 执行上下文, 首次被调度时由上下文切换直接跳入本函数: x86_64 走
/// `process_switch_asm` 的内核线程 (cs=0x08) 分支 (`mov rsp + jmp`, 不经
/// iretq —— 同特权级 iretq 不加载 RSP/SS); aarch64 走 `context_switch_asm`
/// 的 `eret`.
///
/// `sti` 与 `hlt` 必须融合在同一条 `asm!` 内: 分两次执行时, `sti` 之后到
/// `hlt` 之前存在中断窗口, 该窗口内到达的中断会在 `hlt` 之前返回, 随后的
/// `hlt` 将错过唤醒 —— 待下一个中断源才可能退出停机.
#[cfg(target_arch = "x86_64")]
pub extern "C" fn idle_entry() -> ! {
    loop {
        // SAFETY: `sti`/`hlt` 在 ring 0 合法; 不读写内存也不修改栈,
        // `options(nomem, nostack)` 与之一致; 循环保证本函数永不返回.
        unsafe {
            core::arch::asm!("sti; hlt", options(nomem, nostack));
        }
    }
}

/// 每 CPU idle 任务的入口: 无条件等待中断, 永不返回 (aarch64 版).
///
/// 语义与 x86_64 版相同, 由 [`Scheduler::init_per_cpu_idle`] 作为
/// `context.ELR_EL1` 写入 idle 任务的执行上下文.
#[cfg(target_arch = "aarch64")]
pub extern "C" fn idle_entry() -> ! {
    loop {
        // SAFETY: `wfi` 在 EL1 合法; 不读写内存也不修改栈,
        // `options(nomem, nostack)` 与之一致; 循环保证本函数永不返回.
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

/// 队列归属认领凭证 (RAII) —— 跨核迁移协议的**唯一**认领形态.
///
/// 它存在不是为优雅, 而是为必需: 手工配对 `claim` / `release` 时, 任何一条早退
/// 分支忘记回滚都会把 `Process::cfs_rq_cpu` 永久留在 `MIGRATE_PENDING` —— 该 pid
/// 之后所有认领必然失败, 既入不了队也撤不了账, 变成"永远唤不醒"的隐形态.
/// E-II-b 回归实测: `cfs_deactivate` 的 `NO_RQ_CPU` 早退分支漏一次回滚, 就把全内核
/// 搬定成功率打到 4/144373. 未 `commit` 即在 Drop 时把归属写回认领前的值,
/// 使这类泄漏在结构上不可表达.
struct RqClaim<'a> {
    /// 归属写回的执行者 (进程表访问).
    sched: &'a Scheduler,
    /// 被认领的任务.
    pid: Pid,
    /// 认领前的归属核 (`NO_RQ_CPU` = 原本不在任何队列); 未提交即 Drop 时原样回滚.
    prev_cpu: u32,
    /// 是否已把归属提交为新值; `false` 时 Drop 回滚.
    committed: bool,
}

impl RqClaim<'_> {
    /// 提交归属: 任务的队列归属改为 `cpu_id`, Drop 不再回滚.
    ///
    /// 归属字段的**唯一**写入路径 (除认领本身的 CAS), 保证"谁持有凭证谁能改归属".
    fn commit(&mut self, cpu_id: u32) {
        self.sched.set_rq_cpu(self.pid, cpu_id);
        self.committed = true;
    }
}

impl Drop for RqClaim<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.sched.set_rq_cpu(self.pid, self.prev_cpu);
        }
    }
}

/// 全局调度器 — 持有 PWM 级 CPU 带宽配额表与进程数上限表, 并跟踪初始化状态。
pub struct Scheduler {
    quotas: Mutex<[PwidQuota; MAX_QUOTAS]>,
    limits: Mutex<[PwidLimit; MAX_LIMITS]>,
    initialized: AtomicBool,
}

// All fields (Mutex<[PwidQuota; N]>, Mutex<[PwidLimit; N]>, AtomicBool) auto-implement Send + Sync.

impl Scheduler {
    pub const fn new() -> Self {
        const QUOTA_ZERO: PwidQuota = PwidQuota::new();
        const LIMIT_ZERO: PwidLimit = PwidLimit {
            pwm: 0,
            used: false,
            max_procs: 0,
            current: 0,
        };
        Self {
            quotas: Mutex::new([QUOTA_ZERO; MAX_QUOTAS]),
            limits: Mutex::new([LIMIT_ZERO; MAX_LIMITS]),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn init(&self) {
        let _ = init_per_cpu_sched(0);

        self.initialized.store(true, Ordering::SeqCst);

        // 先建立本核 idle 任务, 再建立 init: idle 是 `schedule()` 在本地无候选
        // 任务时的最后兜底, 必须在任何其它任务可能被调度之前就绪.
        // (创建失败时本核 idle 缺席, `schedule()` 在无候选任务时仍会返回 None.)
        let _ = self.init_per_cpu_idle(0);

        let init_pid = self.create_process("init", None, 0);
        if let Some(pid) = init_pid {
            PROCESS_TABLE.with_process(pid, |proc| {
                // 必须经 `Created → Ready → Running` 两步: 状态机不允许
                // `Created → Running` 直跳 (见 `Process::set_state_safe`), 直跳返回
                // Err 被忽略后 init 会停在 `Created` —— 此后任一 tick 触发
                // `should_yield` 时, `schedule()` 因本地无候选切到本核 idle, 而
                // `prev_requeue` 判据要求 `Running || Ready` 对 `Created` 判假, init
                // 不再入队 ⇒ boot 流程被永久丢弃 (双核同闲挂死).
                let _ = proc.set_state_safe(ProcessState::Ready);
                let _ = proc.set_state_safe(ProcessState::Running);
                proc.set_priority(ProcessPriority::Normal);
            });
            self.set_current(pid);

            if let Some(process_ptr) = PROCESS_TABLE.get(pid) {
                // SAFETY: process_ptr is valid from PROCESS_TABLE.get()
                unsafe {
                    update_current_process_ptr(process_ptr as u64);
                }
            }
        }

        if per_cpu().need_reschedule.swap(false, Ordering::SeqCst) {
            self.schedule();
        }
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    pub fn create_process(&self, name: &str, parent: Option<Pid>, pwm: u64) -> Option<Pid> {
        let pid = PROCESS_TABLE.allocate_pid()?;

        // L4: per-PWM proc count limit
        if pwm != 0 {
            let mut limits = self.limits.lock();
            for l in limits.iter_mut() {
                if l.used && l.pwm == pwm {
                    if l.max_procs > 0 && l.current >= l.max_procs {
                        return None;
                    }
                    l.current += 1;
                    break;
                }
            }
        }

        let parent_id = parent.map(ProcessId);
        let process = alloc::boxed::Box::new(Process::new(pid, name, parent_id));
        process.set_pwm(pwm);

        let process_ptr = alloc::boxed::Box::into_raw(process);

        if !PROCESS_TABLE.insert(process_ptr) {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                alloc::alloc::dealloc(
                    process_ptr as *mut u8,
                    alloc::alloc::Layout::new::<Process>(),
                );
            };
            return None;
        }

        // 初始化进程组 ID (POSIX: 新进程默认自成一组, pgid = pid)
        crate::privileged::proc::proc_init_pgid(pid);

        Some(pid)
    }

    /// 为指定 CPU 创建 (或复用) 其 idle 任务, 返回 idle 的 pid.
    ///
    /// 幂等: 该 CPU 已有 idle (per-CPU `idle` 字段非 0) 时直接返回既有 pid,
    /// 不重复创建. `Scheduler::init()` 可能被多次调用, 幂等性是必需的.
    ///
    /// idle 任务特征:
    /// - 无父进程 (`parent = None`), 内核态 (`set_kernel(true)`);
    /// - 调度策略 `SchedPolicy::Idle` + 最空闲优先级 `ProcessPriority::Idle`,
    ///   因此**不会**被 `pick_cfs_task` / `pick_deadline_task` / RT 队列选中;
    /// - **不进入任何运行队列** (CFS/RT/DL), 只作为 `schedule()` 的兜底;
    /// - 拥有独立内核栈 + 预置 context, 首次被调度时直接从 `idle_entry` 开始执行.
    ///
    /// 返回 `None` 表示创建失败 (进程表或内核栈分配失败); 此时该 CPU 的 `idle`
    /// 保持 0, `schedule()` 仍可能返回 `None`.
    pub fn init_per_cpu_idle(&self, cpu_id: u32) -> Option<Pid> {
        let per_cpu = per_cpu_for(cpu_id);

        let existing = per_cpu.idle.load(Ordering::SeqCst);
        if existing != 0 {
            return Some(existing);
        }

        let pid = self.create_process("idle", None, 0)?;

        let mut stack_top = 0u64;
        PROCESS_TABLE.with_process(pid, |proc| {
            proc.set_sched_policy(SchedPolicy::Idle);
            proc.set_priority(ProcessPriority::Idle);
            proc.set_kernel(true);
            // Created → Ready 是状态机允许的转换; idle 由 schedule() 兜底选中,
            // 无需 (也不应) 进入 CFS/RT/DL 运行队列.
            let _ = proc.set_state_safe(ProcessState::Ready);
            let _ = proc.allocate_kernel_stack();
            stack_top = proc.kernel_stack.load(Ordering::SeqCst);
        });

        if stack_top == 0 {
            // SIMPLIFIED: 内核栈分配失败时放弃本轮 idle 创建 (进程表槽位不再回收);
            // 影响面: 仅当 PMM 未就绪 (内核已不可用) 时出现, 该 CPU 的 idle 保持 0;
            // 何时需扩展: 若引入可恢复的 PMM 失败重试, 需先 remove_and_free 该进程.
            return None;
        }

        let cr3 = crate::privileged::mm::get_kernel_pml4();
        PROCESS_TABLE.with_process(pid, |proc| {
            proc.init_kernel_idle_context(idle_entry as *const () as u64, cr3);
        });

        per_cpu.idle.store(pid, Ordering::SeqCst);

        Some(pid)
    }

    /// 让指定 CPU 接管本核 idle 任务, 建立该核的调度身份.
    ///
    /// 次核上线时**必须**经此建立调度身份: 仅调用 [`Self::init_per_cpu_idle`]
    /// 只创建 idle 进程并记录 `idle`, 而 `PerCpuSched.current` 仍为 0 ⇒
    /// [`Self::schedule`] 中 `prev_ctx_ptr` 为 null, 永不执行上下文切换 (只更新
    /// `current` 字段), 该核便无法真正切到其它任务.
    ///
    /// 返回被接管的 idle pid; `None` 表示 idle 创建失败, 调用方须放弃本核上线.
    pub fn adopt_cpu_idle(&self, cpu_id: u32) -> Option<Pid> {
        let idle_pid = self.init_per_cpu_idle(cpu_id)?;
        per_cpu_for(cpu_id)
            .current
            .store(idle_pid, Ordering::SeqCst);
        Some(idle_pid)
    }

    pub fn add(&self, pid: Pid) {
        self.cfs_enqueue(pid);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 设置 nice 值并更新进程的 CFS 权重.
    // SIMPLIFIED: 已入队任务改权重时, 其所在核 `total_weight` 里仍是旧权重,
    // 后续撤账按新权重扣减 ⇒ 权重和出现 (新-旧) 偏差; 影响面 = 仅负载均衡判定精度
    // (可运行性判据取自树, IC2 不受影响); 何时需扩展 = `set_nice` 接入 syscall 且
    // 允许在队改 nice 时, 按 Linux 语义改为 dequeue + enqueue 重投.
    pub fn set_nice(&self, pid: Pid, nice: i8) {
        PROCESS_TABLE.with_process(pid, |proc| {
            let clamped = nice.clamp(-20, 19);
            let w = nice_to_weight(clamped);
            proc.nice.store(clamped as u32, Ordering::Release);
            proc.cfs_weight.store(w, Ordering::Release);
        });
    }

    /// 将进程入队**当前核**的 CFS 运行队列.
    ///
    /// 返回 `false` = 本次搬定未发生 (任务不存在 / 已终 / 认领耗尽).
    /// 返回 `true` 有两种含义: 已投进本核树, 或该任务正被投送屏障 (P-21) 挡住 —— 后者
    /// 只做了延迟投送 (搬状态不入树), 落地由占用者核接手.
    /// 调用方**必须**据此决定后续动作 —— 尤其不得先把任务置 `Ready` 再入队,
    /// 否则失败即造成"Ready 但不在任何树上"的永久隐形 (见 [`Self::cfs_place_on`]).
    fn cfs_enqueue(&self, pid: Pid) -> bool {
        self.cfs_enqueue_to(pid, crate::privileged::smp::get_current_cpu())
    }

    /// 将进程入队**指定核**的 CFS 运行队列.
    ///
    /// 与 [`Self::cfs_enqueue`] 的区别: 本函数可投送到目标核, 供跨核投送
    /// (fork / 唤醒) 使用。归属搬定一律经 [`Self::cfs_place_on`] —— 它保证任务
    /// **只在一棵树上** (旧写法只持目的锁, 遇到任务仍挂在别的核时既不去重也不搬账).
    fn cfs_enqueue_to(&self, pid: Pid, cpu_id: u32) -> bool {
        self.cfs_place_on(pid, cpu_id)
    }

    // ========================================================================
    // 队列归属认领 —— 跨核迁移协议的地基
    // ========================================================================

    /// 写回 `pid` 的队列归属.
    ///
    /// 只由 [`RqClaim`] 调用 (认领的 CAS 除外), 调度路径不得直接写该字段 ——
    /// 否则又制造一处会泄漏的 `MIGRATE_PENDING`.
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名: 归属写回是凭证的方法, 凭证持 &Scheduler"
    )]
    fn set_rq_cpu(&self, pid: Pid, cpu_id: u32) {
        let _ = PROCESS_TABLE.with_process(pid, |p| {
            p.cfs_rq_cpu.store(cpu_id, Ordering::Release);
        });
    }

    /// 非阻塞认领 `pid` 的队列归属.
    ///
    /// 返回 `None` 表示任务不存在或归属正被他人认领 —— 两者都只能走人, **不得等待**:
    /// 等待者手里可能正攥着对方需要的 rq 锁 (见 `process::MIGRATE_PENDING`).
    /// 成功返回认领凭证, 未提交即 Drop 自动回滚.
    fn try_claim_rq(&self, pid: Pid) -> Option<RqClaim<'_>> {
        PROCESS_TABLE
            .with_process(pid, |p| {
                let cur = p.cfs_rq_cpu.load(Ordering::Acquire);
                if cur == MIGRATE_PENDING {
                    return None;
                }
                p.cfs_rq_cpu
                    .compare_exchange(cur, MIGRATE_PENDING, Ordering::AcqRel, Ordering::Acquire)
                    .ok()
                    .map(|_| cur)
            })
            .flatten()
            .map(|prev_cpu| RqClaim {
                sched: self,
                pid,
                prev_cpu,
                committed: false,
            })
    }

    /// 认领 + 有限重试 ([`Self::try_claim_rq`] 的等待者语义在锁外完成, 不持 PCB 锁自旋).
    fn claim_rq(&self, pid: Pid) -> Option<RqClaim<'_>> {
        for _ in 0..RQ_CLAIM_RETRY {
            if let Some(claim) = self.try_claim_rq(pid) {
                return Some(claim);
            }
            if PROCESS_TABLE.get(pid).is_none() {
                return None;
            }
            core::hint::spin_loop();
        }
        None
    }

    /// 队列归属搬定的**唯一入口**: 把 `pid` 放进 `dst_cpu` 的可运行记账.
    ///
    /// 旧形态是"摘源 → **放锁** → 投目的"两段式: 中间窗口里任务既不在源树也不在
    /// 目的树, 而 `cfs_on_rq` 已被清掉, 于是任何并发唤醒都看得到"Ready 且不在队"
    /// 并把它投进第三个队列 —— 同一任务同时挂在两棵树上, 两核同时上核运行它,
    /// 进程上下文与 socket 状态随即被踩坏 (E-II-b 后每核收 tick, 该窗口第一次被
    /// 实测打中: x86_64 e2e 由 6/6 退化为 1/6, 特征为纯 RX 侧全灭).
    ///
    /// P-21 投送屏障使本入口对"正占用某个核"的任务只做**延迟投送** (搬状态不入树,
    /// 仍返回 `true`), 落地由占用者核的 prev 收尾 / `reap_off_cpu` 补投两条腿接手
    /// —— 故 `true` 不等于"已在树上", 调用方不得据此再动该任务的记账.
    fn cfs_place_on(&self, pid: Pid, dst_cpu: u32) -> bool {
        self.cfs_place_on_inner(pid, dst_cpu, false)
    }

    /// [`Self::cfs_place_on`] 的内部形态, 多一个**调用者身份**参数.
    ///
    /// `owner_landing`: 调用者是否就是投送屏障的**占用者本核** —— 只有
    /// `schedule()` 的 prev 收尾传 `true` (语义同 Linux `put_prev_task`: 被切走者
    /// 必须当场回到本核可运行集). 它不是逃生口: 凭证、锁序、去重、记账与落点
    /// 回写全部走同一条路, 唯一差别是屏障不再拦它 —— 而屏障本来就是为拦**别人**
    /// 而设的 (本核把 prev 投回**自己**的树, 不构成两核交叠).
    /// 若这里也拒收, prev 的可运行性会被藏到下一次切换 (`reap_off_cpu`) 为止,
    /// 本核的抢占判据在这段区间里看不见它.
    fn cfs_place_on_inner(&self, pid: Pid, dst_cpu: u32, owner_landing: bool) -> bool {
        let Some(mut claim) = self.claim_rq(pid) else {
            return false;
        };
        let src_cpu = claim.prev_cpu;
        // 源与目的同一把锁 (或本就无归属) 时只锁目的 —— 嵌套同一把 IrqSpinLock 即自死锁.
        if src_cpu == NO_RQ_CPU || src_cpu == dst_cpu {
            let mut dst_rq = per_cpu_for(dst_cpu).cfs_rq.lock();
            self.place_locked(&mut claim, dst_cpu, &mut dst_rq, None, owner_landing)
        } else if src_cpu < dst_cpu {
            // 双持锁序: cpu index **升序** (同层级内定序, 见 docs/explain/ref-lock-order.md);
            // 反向取锁的两个搬动者会 AB-BA.
            let mut src_rq = per_cpu_for(src_cpu).cfs_rq.lock();
            let mut dst_rq = per_cpu_for(dst_cpu).cfs_rq.lock();
            self.place_locked(
                &mut claim,
                dst_cpu,
                &mut dst_rq,
                Some(&mut src_rq),
                owner_landing,
            )
        } else {
            let mut dst_rq = per_cpu_for(dst_cpu).cfs_rq.lock();
            let mut src_rq = per_cpu_for(src_cpu).cfs_rq.lock();
            self.place_locked(
                &mut claim,
                dst_cpu,
                &mut dst_rq,
                Some(&mut src_rq),
                owner_landing,
            )
        }
    }

    /// [`Self::cfs_place_on_inner`] 的持锁主体.
    ///
    /// 前提: `claim` 已握在 caller 手上 (归属已认领), 且 `src`/`dst` 两把 rq 锁在手
    /// (同核时 `src` 为 `None`). 成功路径 `commit` 新归属; 任何失败路径直接 return —
    /// 凭证 Drop 把归属回滚成认领前的值, 不会留下 `MIGRATE_PENDING`.
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便与其余调度路径统一用法 (归属写入已集中到认领凭证, 本函数不再直击 self)"
    )]
    fn place_locked(
        &self,
        claim: &mut RqClaim<'_>,
        dst_cpu: u32,
        dst: &mut CfsRunQueue,
        src: Option<&mut CfsRunQueue>,
        owner_landing: bool,
    ) -> bool {
        let pid = claim.pid;
        let src_cpu = claim.prev_cpu;
        // 锁序 `cfs_rq -> PROCESS_TABLE` (与本文件其余调度路径一致).
        //
        // 快照里没有 `policy`: 可调度性 (policy == Normal) 的准入判据属于
        // `pick_cfs_task` / `steal_locked`, 搬定层只管"任务在一棵树上 + 账随任务走".
        // 在两层各卡一次 policy 会令 RT 降级入队 (见 `schedule()` 的 `_ =>` 分支)
        // 与唤醒投送静默失效.
        let snap = PROCESS_TABLE.with_process(pid, |p| {
            (
                p.get_state(),
                p.cfs_vruntime.load(Ordering::Acquire),
                p.cfs_weight.load(Ordering::Acquire),
                p.cfs_on_rq.load(Ordering::Acquire),
                p.cfs_on_cpu.load(Ordering::Acquire),
            )
        });
        let Some((state, vr, weight, on_rq, on_cpu)) = snap else {
            return false;
        };
        let gone = state == ProcessState::Zombie || state == ProcessState::Terminated;
        if gone {
            return false;
        }
        // 投送屏障 (P-21): `Process::cfs_on_cpu` 在位 ⇒ 该任务正占用某个核 (其上下文
        // 可能尚未保存完), 不得进树 —— 进树即"树上与核上各一份", 被别核拣走就是两个
        // 核交叠使用同一块进程内核栈.
        //
        // 屏障期到达的唤醒**不丢**: 本支只搬状态 Blocked/Frozen → Ready, 不动任何一棵树,
        // 落地由占用者核接手。但「两条腿覆盖全部到达次序」的成立依赖屏障判定与状态翻转
        // 的原子性 —— 见下.
        // 例外: `owner_landing` —— 占用者本核的 prev 收尾正是屏障的解除者, 它入自己
        // 的树不构成两核交叠 (见 [`Self::cfs_place_on_inner`]).
        if on_cpu && !owner_landing {
            // 屏障 TOCTOU 修复 (P-26): 上方快照的 `on_cpu` 读取与本支的状态翻转分属两次
            // `PROCESS_TABLE` 加锁, 占用者核的 `reap_off_cpu` 可夹在两者之间 —— 它清掉
            // `on_cpu`、读到仍是 `Blocked` 的状态而放弃补投腿; 本支随后才把状态翻成
            // `Ready`, 于是 prev 收尾腿与 reap 补投腿同时落空, 任务永久停在
            // 「Ready 且不在任何树上」(`rq_cpu=NO_RQ_CPU`). 实测: x86_64 双核 e2e 中 init
            // 网络探针 recv 唤醒约 6% 轮命中该窗口, 就绪超时挂死 (init 即 pid=5).
            //
            // 修法是在同一临界区内复查屏障再决定投送路径:
            // - 屏障仍在位 => 就地翻 `Ready` (Blocked/Frozen→Ready), 落地交占用者核 `reap`
            //   接手 —— 它此刻尚未取锁清屏障, 必然在本临界区之后重读到 `Ready` 即补投;
            // - 屏障已在本快照之后被清除 => 补投腿已放弃, 本调用点必须亲自入树, 落到
            //   下方完整搬定路径, 不再依赖任何补投腿.
            let still_on_cpu = PROCESS_TABLE.with_process(pid, |p| {
                if p.cfs_on_cpu.load(Ordering::Acquire) {
                    let s = p.get_state();
                    if s == ProcessState::Blocked || s == ProcessState::Frozen {
                        let _ = p.set_state_safe(ProcessState::Ready);
                    }
                    true
                } else {
                    false
                }
            });
            if let Some(still_barred) = still_on_cpu {
                // 屏障仍在位: 已在本临界区把状态翻成 Ready, 落地交占用者核 reap 补投.
                if still_barred {
                    return true;
                }
                // 屏障已在本快照之后被清除: 往下走完整搬定路径, 由本入口把任务投进目的核的树.
            } else {
                // pid 已从进程表消失: 无权投送, 认领凭证 Drop 自动回滚归属.
                return false;
            }
        }
        // 兜底: 非 `schedule()` 路径写入的 `per_cpu.current` (boot / AP 首发经
        // `set_current`) 不带屏障, 故仍按归属核查在核判据 (语义同 Linux 不迁移
        // `->on_cpu` 任务); 屏障在位时上面的分支已拦住, 这里只补那个缺口.
        let on_current = per_cpu_for(dst_cpu).current.load(Ordering::SeqCst) == pid
            || (src_cpu != NO_RQ_CPU && per_cpu_for(src_cpu).current.load(Ordering::SeqCst) == pid);
        if on_current {
            return false;
        }
        let placed = match src {
            // 跨核: 源撤账摘节点, 目的重新记账投节点 —— 两核权重和守恒.
            Some(src_rq) => {
                src_rq.dequeue(pid, weight, on_rq);
                dst.enqueue(pid, vr, weight, false)
            }
            // 同核 (或原本不在队): 幂等移动, 记账凭证原样带走.
            None => dst.enqueue(pid, vr, weight, on_rq),
        };
        PROCESS_TABLE.with_process(pid, |p| {
            let _ = p.set_state_safe(ProcessState::Ready);
            // IC1: 落点必须回写任务自身 vruntime —— 只钳树键而不回写正是旧模型
            // "运行任务 vr 与队列 floor 分属两域" 的起点 (S-1 饥饿根).
            p.cfs_vruntime.store(placed, Ordering::Release);
            p.cfs_on_rq.store(true, Ordering::Release);
        });
        claim.commit(dst_cpu);
        true
    }

    /// 让 `pid` 离开 CFS 可运行记账 (阻塞 / 被切走且不可重排 / 被拣出但不可调度).
    ///
    /// 幂等: 以 `Process::cfs_on_rq` 为记账凭证, 重复调用不二次扣减权重 ——
    /// [`Self::block`] 与 `schedule()` 的 "prev 不可重排" 分支覆盖同一任务时
    /// 正是这种重复调用. 旧模型缺这一步, 使 `total_weight` 随阻塞/退出单调
    /// 虚增, 连均衡判定也随之失真.
    ///
    /// 归属核取自 `cfs_rq_cpu` 而非"当前核": 任务被跨核投送后, 它的账在目的核队列上,
    /// 而 `block()` 运行在它正在跑的核上 —— 两者今天恒等 (不搬在跑的任务), 但只有
    /// 按归属核撤账才能把"账随任务走"钉成不变式.
    fn cfs_deactivate(&self, pid: Pid) {
        let Some(mut claim) = self.claim_rq(pid) else {
            return;
        };
        if claim.prev_cpu == NO_RQ_CPU {
            // 本就不在任何队列: 只清记账凭证, 不取锁;
            // 归属由凭证 Drop 原样回滚成 `NO_RQ_CPU` —— 这里正是旧写法漏回滚的位置.
            let _ = PROCESS_TABLE.with_process(pid, |p| {
                p.cfs_on_rq.store(false, Ordering::Release);
            });
            return;
        }
        let src_cpu = claim.prev_cpu;
        let mut rq = per_cpu_for(src_cpu).cfs_rq.lock();
        let (weight, on_rq) = PROCESS_TABLE
            .with_process(pid, |p| {
                (
                    p.cfs_weight.load(Ordering::Acquire),
                    p.cfs_on_rq.load(Ordering::Acquire),
                )
            })
            .unwrap_or((0, false));
        rq.dequeue(pid, weight, on_rq);
        PROCESS_TABLE.with_process(pid, |p| {
            p.cfs_on_rq.store(false, Ordering::Release);
        });
        claim.commit(NO_RQ_CPU);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 为进程设置 `SCHED_DEADLINE` 参数.
    pub fn set_deadline_params(&self, pid: Pid, params: DeadlineParams) -> bool {
        if !params.is_valid() {
            return false;
        }
        let util = params.utilization_pct();
        if util > DL_MAX_UTILIZATION_PCT {
            return false;
        }
        PROCESS_TABLE.with_process(pid, |proc| {
            proc.set_sched_policy(SchedPolicy::Deadline);
            proc.dl_runtime.store(params.runtime, Ordering::Release);
            proc.dl_deadline.store(params.deadline, Ordering::Release);
            proc.dl_period.store(params.period, Ordering::Release);
            let now = crate::privileged::timer::get_ticks();
            proc.dl_abs.store(now + params.deadline, Ordering::Release);
            proc.dl_remaining.store(params.runtime, Ordering::Release);
        });
        true
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn add_rt_task(&self, pid: Pid, rt_priority: u8, policy: SchedPolicy) {
        let priority = rt_priority.min(RT_PRIORITY_MAX);

        let mut rt_queue = per_cpu().rt_queue.lock();
        let mut inserted = false;

        for i in 0..rt_queue.len() {
            if rt_queue[i].rt_priority < priority {
                rt_queue.insert(
                    i,
                    RtTaskInfo {
                        pid,
                        rt_priority: priority,
                        policy,
                        time_slice_remaining: RT_TIME_SLICE,
                    },
                );
                inserted = true;
                break;
            }
        }

        if !inserted {
            rt_queue.push_back(RtTaskInfo {
                pid,
                rt_priority: priority,
                policy,
                time_slice_remaining: RT_TIME_SLICE,
            });
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 选取一个 deadline 任务 (EDF —— 绝对 deadline 最早者优先).
    fn pick_deadline_task(&self) -> Option<Pid> {
        let per_cpu = per_cpu();
        let mut dl_rq = per_cpu.dl_rq.lock();
        if dl_rq.is_empty() {
            per_cpu.dl_running.store(false, Ordering::SeqCst);
            return None;
        }
        if let Some((pid, dl_abs)) = dl_rq.pick_next() {
            let alive = PROCESS_TABLE
                .with_process(pid, |p| {
                    p.get_state() != ProcessState::Zombie
                        && p.get_sched_policy() == SchedPolicy::Deadline
                })
                .unwrap_or(false);
            if alive {
                per_cpu.dl_running.store(true, Ordering::SeqCst);
                Some(pid)
            } else {
                // 不可调度 (僵尸 / 策略已改): 拣出时已离树, 放回仅供调度其它
                // 候选; DlRunQueue 无可运行性判据依赖计数器 (IC2: 树即真相).
                dl_rq.reinsert(pid, dl_abs);
                per_cpu.dl_running.store(false, Ordering::SeqCst);
                None
            }
        } else {
            per_cpu.dl_running.store(false, Ordering::SeqCst);
            None
        }
    }

    /// 选取一个 CFS 任务 (vruntime 最小者).
    fn pick_cfs_task(&self) -> Option<Pid> {
        let per_cpu = per_cpu();
        let mut cfs_rq = per_cpu.cfs_rq.lock();
        // 循环取下一个节点, 直到取到可调度者 (或树空).
        //
        // 不能"取一个不可调度节点就 return None" —— 若最小 vruntime 位置恰好
        // 是不可调度节点, 每次调度都会在此早返回, 树上其余可调度任务永远选不
        // 出来 (本核 CFS 永久饥饿).
        //
        // 不可调度节点一律**撤账摘除** (旧实现是暂存后原样放回): 树是 IC2 的
        // 唯一真相, 留在树上的不可调度节点会长期占据最小键位, 既挡住其余任务,
        // 又让 `is_empty()` / 抢占判据与真实可运行集背离. 被摘除者后续由
        // `unblock` 重新投送 (唤醒语义), 故不丢任务.
        let mut picked: Option<Pid> = None;
        while let Some((pid, _vr)) = cfs_rq.pick_next() {
            // 锁序 cfs_rq -> PROCESS_TABLE (本函数既有语义): 一次加锁取回
            // 可调度性与撤账所需的权重/记账凭证.
            let (schedulable, weight, on_rq) = PROCESS_TABLE
                .with_process(pid, |p| {
                    let state = p.get_state();
                    (
                        state != ProcessState::Blocked
                            && state != ProcessState::Zombie
                            && p.get_sched_policy() == SchedPolicy::Normal,
                        p.cfs_weight.load(Ordering::Acquire),
                        p.cfs_on_rq.load(Ordering::Acquire),
                    )
                })
                .unwrap_or((false, 0, false));
            if schedulable {
                // IC1: 被拣者随即上核运行, 仍在可运行记账中 (`cfs_on_rq` 保持
                // true, 语义同 Linux on_rq) —— 其 vruntime 由 tick 推进并同步
                // 队列 floor, 切走时经幂等 `enqueue` 原位放回.
                picked = Some(pid);
                break;
            }
            cfs_rq.dequeue(pid, weight, on_rq);
            if on_rq {
                // 不可调度节点: 已离树 (`pick_next` 摘除), 此处只需撤账 + 归属归零.
                // 归属必须经认领凭证写: 直接写会与并发搬动者抢同一字段 (它可能正持
                // `MIGRATE_PENDING`), 且让"归属只有一个写入者"重新退化成约定.
                if let Some(mut claim) = self.try_claim_rq(pid) {
                    let _ = PROCESS_TABLE.with_process(pid, |p| {
                        p.cfs_on_rq.store(false, Ordering::Release);
                    });
                    claim.commit(NO_RQ_CPU);
                }
                // 认领失败 = 有人正搬它: 账目与归属都归那个搬动者收尾, 本轮直接跳过
                // (不等待不持锁重试 —— 它可能正需要本函数手里的 rq 锁).
            }
        }
        picked
    }

    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::manual_let_else,
        reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
    )]
    pub fn schedule(&self) -> Option<Pid> {
        let saved_flags = crate::arch!(interrupt_disable()) as u64;

        let per_cpu = per_cpu();
        let current_pid = per_cpu.current.load(Ordering::SeqCst);

        // 方案 B: 消费上一轮被切走的任务. 本函数此刻正运行在**新任务**上下文中,
        // 被切走者 (prev) 的上下文切换已完成, 确已离开 CPU 且不会再被调度
        // (退出的进程为 Zombie, 不入任何运行队列), 故可安全完成其退出回收.
        // 事件顺序保证: 每次切换前写入 `prev`, 而本 CPU 不可能在未再次进入本函数
        // 的情况下发生第二次切换 ⇒ 无遗漏.
        self.reap_off_cpu(per_cpu);

        let mut next_pid = self.pick_deadline_task();

        // 2. RT (FIFO/RR) —— 从 MLFQ 保留
        if next_pid.is_none() {
            per_cpu.dl_running.store(false, Ordering::SeqCst);

            let mut rt_queue = per_cpu.rt_queue.lock();

            while !rt_queue.is_empty() {
                let rt_task = if let Some(task) = rt_queue.pop_front() {
                    task
                } else {
                    klog_sched_warn!("[SCHEDULER] RT queue race condition detected");
                    break;
                };
                let rt_pid = rt_task.pid;

                let alive = PROCESS_TABLE
                    .with_process(rt_pid, |p| p.get_state() != ProcessState::Zombie)
                    .unwrap_or(false);

                if !alive {
                    continue;
                }

                match rt_task.policy {
                    SchedPolicy::Fifo => {
                        next_pid = Some(rt_pid);
                        per_cpu.rt_running.store(true, Ordering::SeqCst);
                        per_cpu
                            .fifo_watchdog
                            .store(RT_FIFO_WATCHDOG, Ordering::SeqCst);
                        break;
                    }
                    SchedPolicy::Rr => {
                        next_pid = Some(rt_pid);
                        per_cpu.rt_running.store(true, Ordering::SeqCst);
                        let mut updated_rt = rt_task;
                        updated_rt.time_slice_remaining = RT_TIME_SLICE;
                        rt_queue.push_back(updated_rt);
                        break;
                    }
                    _ => {
                        self.cfs_enqueue(rt_pid);
                        per_cpu.rt_running.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            }

            if next_pid.is_none() {
                per_cpu.rt_running.store(false, Ordering::SeqCst);
            }
        }

        // 3. CFS (vruntime 最小) —— 取代 MLFQ 用于 SCHED_NORMAL
        if next_pid.is_none() {
            next_pid = self.pick_cfs_task();
        }

        // 4. 本地无候选时进行负载均衡
        if next_pid.is_none() && crate::privileged::smp::is_enabled() {
            self.load_balance();
            next_pid = self.pick_cfs_task();
        }

        // 5. 每 CPU idle 任务兜底 —— 保证 schedule() 在运行期永不为 None
        //
        // 真实内核不会因"无任务可运行"而结束运行: 本地没有可调度任务时运行
        // 本 CPU 的 idle 任务 (等待中断/事件唤醒其它任务). 整机退出只由测试
        // 框架 (privileged::debug 的 qemu_exit) 承担, 不属调度器职责.
        if next_pid.is_none() {
            let idle_pid = per_cpu.idle.load(Ordering::SeqCst);
            // 优先保持 current: 运行队列中无其它候选, 但本核 current 仍是可运行的
            // 非 idle 任务 (Running/Ready) 时, 切到 idle 会白白让出 CPU 并令 current
            // 经 `prev_requeue` 重新入队 —— 对"从未入过 CFS 队列"的任务 (如 boot 的
            // init) 而言不入树 / 不计 `total_weight`, 之后 `pick_cfs_task` / `has_runnable`
            // 均判空, 该任务永久饥饿 (boot 流程被丢弃, 双核同闲挂死).
            // 仅当 current 已不可运行 (Created/Blocked/Zombie) 或本核就是 idle 时,
            // 才回退到 idle 等待中断.
            let keep_current = current_pid != 0
                && idle_pid != 0
                && current_pid != idle_pid
                && PROCESS_TABLE
                    .with_process(current_pid, |p| {
                        let s = p.get_state();
                        s == ProcessState::Running || s == ProcessState::Ready
                    })
                    .unwrap_or(false);
            if keep_current {
                next_pid = Some(current_pid);
            } else if idle_pid != 0 {
                next_pid = Some(idle_pid);
            }
        }

        let next = if let Some(pid) = next_pid {
            pid
        } else {
            if saved_flags & 0x200 != 0 {
                crate::arch!(interrupt_enable());
            }
            return None;
        };

        if next == current_pid {
            if saved_flags & 0x200 != 0 {
                crate::arch!(interrupt_enable());
            }
            return Some(next);
        }

        let prev_ptr = if current_pid != 0 {
            PROCESS_TABLE.get(current_pid)
        } else {
            None
        };

        let next_ptr = PROCESS_TABLE.get(next);

        if next_ptr.is_none() {
            if saved_flags & 0x200 != 0 {
                crate::arch!(interrupt_enable());
            }
            return None;
        }

        // 内核栈顶同步必须 hoist 到闭包外: 该值随后要交给
        // `kpti::map_rsp0_page` 映射进切换目标的用户页表 (KPTI-08).
        //
        // P-21 投送屏障**唯一置位点**: 必须早于下面 `per_cpu.current.store(next)` ——
        // 屏障的语义是"该任务正在占用本核", 而从写入 `current` 到真正切走 prev 之间
        // 已可被别核观察到 (它还是本核 `current`), 故在拣中的同一刻落下. 清位只在
        // `reap_off_cpu` (见其文档).
        let next_kernel_stack = PROCESS_TABLE
            .with_process(next, |proc| {
                let _ = proc.set_state_safe(ProcessState::Running);
                proc.cfs_on_cpu.store(true, Ordering::Release);
                proc.kernel_stack.load(Ordering::SeqCst)
            })
            .unwrap_or(0);
        if next_kernel_stack != 0 {
            crate::privileged::cpu::arch::set_kernel_stack(next_kernel_stack);
        }

        // 方案 B: 记录本次被切走者, 供下一次 `schedule()` 入口的 `reap_off_cpu`
        // 在确认其已离开 CPU 后完成退出回收. 必须在切换前写入.
        per_cpu.prev.store(current_pid, Ordering::SeqCst);
        per_cpu.current.store(next, Ordering::SeqCst);

        // 线程级 SchedulerEx 并行实现已移除 (P-29 route C): 本 per-CPU 进程级
        // 调度器是唯一调度主体, `per_cpu.current` 持有进程号 `Pid`, 不再存在与
        // 全局 `SCHEDULER_EX.current` (*mut Thread) 的类型混用与跨层同步问题.
        // 进程级 user/sys 时间记账由下方 `Scheduler::tick` 内的 `proc_account_tick` 承担.

        if let Some(next_ptr_raw) = next_ptr {
            // SAFETY: next_ptr_raw is valid from PROCESS_TABLE.get()
            unsafe {
                update_current_process_ptr(next_ptr_raw as u64);
            }
        }

        if let Some(user_proc) = super::user_proc::USER_PROC_MANAGER.get(next) {
            super::user_proc::USER_PROC_MANAGER.set_current(Some(user_proc));
            // 更新 per-CPU 用户页表 CR3, 使 syscall/中断返回用户态时
            // 从 [gs:USER_PML4_OFF] 读取到正确的进程用户页表.
            // SAFETY: user_proc 来自 PROCESS_TABLE, 通过 process() 访问权威 Process;
            // 当前在调度器持锁上下文, 独占访问 per-CPU 数据.
            unsafe {
                let user_cr3 = (*user_proc).process().cr3.load(Ordering::SeqCst);
                #[cfg(target_arch = "x86_64")]
                {
                    crate::privileged::arch::gdt::gdt_set_user_cr3(user_cr3);
                    // KPTI-08: 切换目标的用户页表必须含其内核栈顶页 ——
                    // 该页随任务变化, 不在 `assemble_kernel_half` 的统一装配面内,
                    // 故在此 (上下文切换) 按目标任务追加. COW fork 出的子进程页表
                    // 只经 `assemble_kernel_half` 装配, 必须靠此处补齐.
                    // SAFETY: user_cr3 是目标进程有效用户页表 PML4 物理地址;
                    // next_kernel_stack 是该进程内核栈顶高半区 VA (页对齐);
                    // 该 VA 在目标页表内 PTE 值恒定 (见 map_rsp0_page 文档).
                    crate::privileged::mm::map_rsp0_page(user_cr3, next_kernel_stack);
                }
                let _ = (user_cr3, next_kernel_stack);
            }
        }

        // 重新入队上一个任务 —— 仅当 prev 确实可被重新调度时才入队.
        //
        // 两个排除条件 (缺一即导致该 CPU 的 CFS 永久饥饿):
        // - prev 是本 CPU 的 idle 任务: idle 不属任何运行队列, 入队后
        //   `pick_cfs_task` 会选中它, 但因其策略为 `SchedPolicy::Idle` (!= Normal)
        //   而返回 None (见 pick_cfs_task 中"不可调度节点"分支), CFS 树上的普通
        //   任务再也无法被选中;
        // - prev 已不在 Running/Ready (如 `exit()` 之后的 Zombie, 或阻塞中的
        //   Blocked): 同样会被 `pick_cfs_task` 判为不可调度而返回 None; 且把
        //   Blocked/Zombie 任务塞回 CFS 树/置回 Ready 与阻塞、退出语义冲突.
        let prev_state_code = PROCESS_TABLE
            .with_process(current_pid, |p| {
                let s = p.get_state();
                if s == ProcessState::Running {
                    1u32
                } else if s == ProcessState::Ready {
                    2u32
                } else if s == ProcessState::Blocked {
                    3u32
                } else if s == ProcessState::Zombie {
                    4u32
                } else {
                    5u32
                }
            })
            .unwrap_or(9);
        let prev_requeue = if prev_ptr.is_some() {
            per_cpu.idle.load(Ordering::SeqCst) != current_pid
                && (prev_state_code == 1 || prev_state_code == 2)
        } else {
            false
        };

        if prev_requeue {
            let was_dl = per_cpu.dl_running.load(Ordering::SeqCst);
            let was_rt = per_cpu.rt_running.load(Ordering::SeqCst);

            if was_dl {
                let dl_info =
                    PROCESS_TABLE.with_process(current_pid, |p| p.dl_abs.load(Ordering::Acquire));
                if let Some(dl_abs) = dl_info {
                    // 拣出仅为选候选, 任务仍属可运行集: reinsert 只放回树上,
                    // DlRunQueue 无平行计数器 (IC2: 树即真相).
                    per_cpu.dl_rq.lock().reinsert(current_pid, dl_abs);
                }
            } else if was_rt {
                let rt_info = PROCESS_TABLE
                    .with_process(current_pid, |p| (p.get_rt_priority(), p.get_sched_policy()));
                if let Some((rt_priority, policy)) = rt_info {
                    if policy != SchedPolicy::Fifo {
                        let mut rt_queue = per_cpu.rt_queue.lock();
                        let mut inserted = false;
                        for i in 0..rt_queue.len() {
                            if rt_queue[i].rt_priority < rt_priority {
                                rt_queue.insert(
                                    i,
                                    RtTaskInfo {
                                        pid: current_pid,
                                        rt_priority,
                                        policy,
                                        time_slice_remaining: RT_TIME_SLICE,
                                    },
                                );
                                inserted = true;
                                break;
                            }
                        }
                        if !inserted {
                            rt_queue.push_back(RtTaskInfo {
                                pid: current_pid,
                                rt_priority,
                                policy,
                                time_slice_remaining: RT_TIME_SLICE,
                            });
                        }
                    }
                }
            } else {
                // prev 落地 (P-21): 走搬定唯一入口的 `owner_landing` 形态 —— 认领凭证 +
                // 去重 + 记账 + 落点回写 (IC1) 在同一段持锁期内一次闭合. 旧的裸
                // `requeue` 两头都缺: 不动账 (屏障窗口内被 `block()` 撤过账者会把权重
                // 漏在 `total_weight` 外), 也不去重 (并发插入即同一棵树两个节点).
                //
                // 返回 false = 归属没抢到 (有人正搬它): 不重试也不强行落账 —— 保持
                // `Ready 且不在队`, 由本核下一次切换入口清屏障时 (`reap_off_cpu`) 补投.
                let _ = self.cfs_place_on_inner(
                    current_pid,
                    crate::privileged::smp::get_current_cpu(),
                    true,
                );
            }
            PROCESS_TABLE.with_process(current_pid, |p| {
                let _ = p.set_state_safe(ProcessState::Ready);
            });
        } else if prev_ptr.is_some() && (prev_state_code == 3 || prev_state_code == 4) {
            // prev 切走且不可重排 (Blocked / Zombie): 离开 CFS 可运行集 —— 撤账.
            // 幂等: `block()` 已撤账者其 on_rq 凭证已为 false, 此处不二次扣减;
            // 旧模型缺这一步, 使 total_weight 随退出单调虚增, 均衡判定随之失真.
            self.cfs_deactivate(current_pid);
        }

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        let prev_ctx_ptr = prev_ptr.map_or(core::ptr::null_mut(), |p| unsafe {
            &raw mut (*p).context as *mut Mutex<ProcessContext>
        });

        let next_ctx_ptr = next_ptr.map_or(core::ptr::null(), |p| {
            // SAFETY: next_ptr 来自 PROCESS_TABLE.get(), 它返回指向活动
            // Process 的合法指针. 这里只读取 context 字段地址.
            unsafe { &raw const (*p).context as *const Mutex<ProcessContext> }
        });

        // `context_switch` 会在**本任务稍后被重新调度上核时返回** —— 恢复点即
        // 其后代码, 且执行时用的是本任务自身的栈上局部变量 (`next`/`current_pid`
        // 均为本次切换的陈旧值). 故**不能**以"执行到切换段之后"推断未发生切换;
        // 必须用 `switched` 标志区分"恢复路径"与"真·未切换".
        let switched = !prev_ctx_ptr.is_null();
        if switched {
            // 关键修复 (B05-55): 不能持 MutexGuard 调 context_switch.
            // process_switch_asm 切到 next 后本处暂不执行 (待本任务被重新调度上核
            // 才从其后继续), 若持 Guard 则其 Drop 要等到恢复之后才可能执行, 期间
            // prev 的 context 锁泄漏 → 本任务被重新调度上核后 p.context.lock()
            // (如 proc_save_user_regs) 自旋死锁.
            // 改用 get_mut_unchecked 裸访问: 单核 + process_switch_asm 开头 cli
            // 保证切换期间无并发访问.
            // SAFETY: prev/next_ptr 均派生自 PROCESS_TABLE 中活动的 Process 条目;
            // 单核 + cli 排他, 详见 get_mut_unchecked 文档.
            unsafe {
                let prev_ctx = core::ptr::addr_of_mut!(*((*prev_ctx_ptr).get_mut_unchecked()));
                let next_ctx = core::ptr::addr_of!(*((*next_ctx_ptr).get_mut_unchecked()));
                crate::arch!(context_switch(prev_ctx as *mut u8, next_ctx as *const u8));
            }
            // 恢复路径: 本任务已被重新调度上核, 本次切换早已由"切走本任务的那次
            // schedule()"完成. 仅做 RCU 收尾并按原语义返回 (返回值调用方不依赖).
            crate::privileged::sync::rcu::rcu_note_quiescent_state();
            return Some(next);
        }

        // 能执行到此处 ⇔ `prev_ctx_ptr` 为 null —— 本次**未发生**上下文切换.
        // - `current_pid == 0`: 引导路径 (本 CPU 尚无 current), 无 prev 可保存,
        //   把 next 返回给调用方即可.
        // - `current_pid != 0`: 不变式违反 —— 当前进程的 PCB 在表中不可解析, 其
        //   内核栈可能已释放, **绝不能返回** (返回即回到已失效上下文 = UAF, 正是
        //   "exit 后返回用户态" 的直接机理). 此处丢弃该上下文 (寄存器写入本 CPU
        //   scratch), 单向切到 next 保活; 绝不静默改写 current 后返回.
        // 方案 B 的 `exiting` 自引用已使本分支在 `current_pid != 0` 时不可达,
        // 保留兜底仅为"故障不静默".
        crate::privileged::sync::rcu::rcu_note_quiescent_state();
        if current_pid == 0 {
            return Some(next);
        }
        crate::klog_error!(
            "[SCHED] invariant violated: current pid={} not in table; one-way switch to {}",
            current_pid,
            next
        );
        // SAFETY: next_ptr 来自 PROCESS_TABLE.get(next) 且上面已判非 None; 单核 +
        // 本函数开头 cli 排他, 切换期间无并发访问. scratch 为本 CPU 私有槽, 仅
        // 承接被丢弃的 prev 寄存器 (其内容不会被读回).
        unsafe {
            let next_ctx = core::ptr::addr_of!(*((*next_ctx_ptr).get_mut_unchecked()));
            let scratch_ctx = per_cpu.scratch_ctx.map_mut(|c| core::ptr::addr_of_mut!(*c));
            crate::arch!(context_switch(
                scratch_ctx as *mut u8,
                next_ctx as *const u8
            ));
        }
        // context_switch 永不返回; 以下仅为类型收尾.
        Some(next)
    }

    /// 方案 B: 在 `schedule()` 入口完成上一轮被切走任务的退出回收.
    ///
    /// 调用时机保证: 本函数运行在**新任务**上下文中, 即被切走者 (prev) 的上下文
    /// 切换已经完成 —— 它确已离开 CPU, 且退出进程为 Zombie (不入任何运行队列)
    /// 不会再次上核. 此时释放 `exit()` 持有的自引用: 若父进程已 `wait4` 收割
    /// (置位 `pending_free`), 引用归零即完成 PCB / 内核栈 / 用户页表的最终释放;
    /// 若尚未收割, 仅自引用归零, PCB 保留至父进程收割 (由 `remove_and_free`
    /// 看到引用归零而释放).
    ///
    /// 仅对"退出中"(`exiting`) 的 prev 释放自引用; 普通被抢占任务仍可运行 (可能
    /// 迁移到其它核), 绝不能在此误减其引用计数.
    ///
    /// 同时这里是 [`Process::cfs_on_cpu`] 投送屏障的**唯一清位点** (语义同 Linux
    /// `finish_task_switch` 的 `prev->on_cpu = 0`): 只有在本函数被执行时, "prev 已
    /// 离开 CPU" 才是**被确证**的事实 —— 提前清位等于允许别核在上下文未保存完时
    /// 把它投进树, 两个核即交叠使用同一块进程内核栈.
    ///
    /// 清位后紧接一次补投: 屏障期只搬了状态而未入树的 `Ready` 任务 (投送屏障的
    /// 延迟投送支, 或 prev 收尾认领失败) 在此落地; 若 prev 收尾已就地入树, 其
    /// `cfs_on_rq` 为 true, 本支自然跳过.
    fn reap_off_cpu(&self, per_cpu: &PerCpuSched) {
        let prev = per_cpu.prev.swap(0, Ordering::SeqCst);
        if prev == 0 {
            return;
        }
        // 锁序 PROCESS_TABLE 单独加锁 (不持 rq 锁): 补投走 `cfs_place_on`, 它自己取
        // `cfs_rq` 锁, 在本闭包里取会构成 `with_process` 重入 (同核自死锁).
        let snap = PROCESS_TABLE.with_process(prev, |p| {
            p.cfs_on_cpu.store(false, Ordering::Release);
            (
                p.get_state(),
                p.cfs_on_rq.load(Ordering::Acquire),
                p.get_sched_policy(),
                p.exiting.swap(false, Ordering::AcqRel),
            )
        });
        let Some((state, on_rq, policy, was_exiting)) = snap else {
            return;
        };
        if was_exiting {
            PROCESS_TABLE.dec_ref_and_maybe_free(prev);
        }
        // 补投: 只针对 CFS 可运行集 (RT/DL 的 `cfs_on_rq` 恒为 false, 不据此判据);
        // idle 任务不属任何运行队列 (见 `schedule()` 的 prev 排除条件), 同样跳过.
        if state == ProcessState::Ready
            && !on_rq
            && policy == SchedPolicy::Normal
            && per_cpu.idle.load(Ordering::SeqCst) != prev
        {
            self.cfs_place_on(prev, crate::privileged::smp::get_current_cpu());
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn current(&self) -> Option<Pid> {
        let pid = per_cpu().current.load(Ordering::SeqCst);
        if pid == 0 { None } else { Some(pid) }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn get_current_process(&self) -> Option<*mut Process> {
        let pid = per_cpu().current.load(Ordering::SeqCst);
        if pid == 0 {
            None
        } else {
            PROCESS_TABLE.get(pid)
        }
    }

    pub fn block(&self, reason: BlockReason) {
        let per_cpu = per_cpu();
        if let Some(pid) = self.current() {
            PROCESS_TABLE.with_process(pid, |proc| {
                let _ = proc.set_state_safe(ProcessState::Blocked);
                proc.block_reason.store(reason as u32, Ordering::SeqCst);
            });
            // IC2/IC4: 阻塞即离开可运行集 —— 摘节点 + 撤账. 树上只留可运行
            // 任务, `is_empty()` / `leftmost_vruntime()` 才与真实可运行集一致.
            self.cfs_deactivate(pid);
            per_cpu.need_reschedule.store(true, Ordering::SeqCst);
        }
    }

    pub fn unblock(&self, pid: Pid) {
        let sched_policy = PROCESS_TABLE
            .with_process(pid, |proc| {
                let state = proc.get_state();
                // 唤醒判据取"是否已在可运行记账中", 而非只看状态: 旧实现
                // "状态非 Blocked/Frozen 即返回 None" 会吞掉"Ready 但未入队"的
                // 唤醒, 使该任务在队列外永久隐形 (实测 boot 期 unb-skip).
                // Running (已在核上) / Created (尚未启动) / Zombie / Terminated
                // 一律不投送.
                //
                // 本闭包**不改状态** —— 置 Ready 是搬定成功后的结果 (CFS 路由由
                // `place_locked` 写, RT/DL 路由在各自分支里写). 提前置 Ready 会直接
                // 打断唤醒面的电平重扫自愈: `poll_network` / 睡眠扫描以
                // `state == Blocked` 为选集条件, 一旦搬定失败, 该任务既不在树上
                // 也不再会被重扫拾起 —— 一次可恢复的丢唤醒就变成永久隐形.
                let need_enqueue = match state {
                    ProcessState::Blocked | ProcessState::Frozen => true,
                    ProcessState::Ready => !proc.cfs_on_rq.load(Ordering::Acquire),
                    _ => false,
                };
                if !need_enqueue {
                    return None;
                }
                let policy = proc.get_sched_policy();
                if policy == SchedPolicy::Deadline {
                    let dl_abs = proc.dl_abs.load(Ordering::Acquire);
                    let runtime = proc.dl_runtime.load(Ordering::Acquire);
                    let period = proc.dl_period.load(Ordering::Acquire);
                    Some((
                        policy,
                        dl_abs,
                        if period > 0 {
                            (runtime * 100) / period
                        } else {
                            0
                        },
                    ))
                } else {
                    Some((policy, 0, 0))
                }
            })
            .flatten();

        match sched_policy {
            Some((SchedPolicy::Normal, _, _)) => {
                // 统一走搬定入口: 落点钳制 (IC1) + 回写 vruntime + 记账凭证 + 置 Ready.
                //
                // 失败 ⇒ 保持原状态直接返回: 不动唤醒不变式 (本核无新可运行任务,
                // 不需要重调度), 且下一轮电平重扫/超时重投会再次尝试.
                if !self.cfs_enqueue_to(pid, crate::privileged::smp::get_current_cpu()) {
                    return;
                }
            }
            Some((SchedPolicy::Fifo | SchedPolicy::Rr, _, _)) => {
                PROCESS_TABLE.with_process(pid, |proc| {
                    let _ = proc.set_state_safe(ProcessState::Ready);
                });
                let (prio, pol) = PROCESS_TABLE
                    .with_process(pid, |p| (p.get_rt_priority(), p.get_sched_policy()))
                    .unwrap_or((0, SchedPolicy::Normal));

                let mut rt_q = per_cpu().rt_queue.lock();
                let mut inserted = false;
                for i in 0..rt_q.len() {
                    if rt_q[i].rt_priority < prio {
                        rt_q.insert(
                            i,
                            RtTaskInfo {
                                pid,
                                rt_priority: prio,
                                policy: pol,
                                time_slice_remaining: RT_TIME_SLICE,
                            },
                        );
                        inserted = true;
                        break;
                    }
                }
                if !inserted {
                    rt_q.push_back(RtTaskInfo {
                        pid,
                        rt_priority: prio,
                        policy: pol,
                        time_slice_remaining: RT_TIME_SLICE,
                    });
                }
            }
            Some((SchedPolicy::Deadline, dl_abs, util)) => {
                PROCESS_TABLE.with_process(pid, |proc| {
                    let _ = proc.set_state_safe(ProcessState::Ready);
                });
                per_cpu().dl_rq.lock().enqueue(pid, dl_abs, util);
            }
            _ => {}
        }

        // 唤醒不变式: 置任务可运行后, 必须叫醒持有其运行队列的 CPU — 本函数把任务
        // 入到**本核** cfs/rq, 故置本核 pending-resched 与 `block` 对称. 否则若本核
        // 正处于 idle (BSP `idle_entry` 为 `sti; hlt`, 不像 AP idle 每轮调
        // `schedule()`), 新入队任务将永不被拾取 → 丢失唤醒 (P6a 实测: hrtimer 唤醒
        // 被投到 idle 核后, 睡眠进程永不恢复). 中断上下文下只置标志, 由 IRQ 退出
        // 路径 `run_pending_resched` 完成切换 (见 `cpu_queue::mark_resched_pending_local`).
        crate::privileged::proc::cpu_queue::mark_resched_pending_local();
    }

    pub fn exit(&self, exit_code: u32) {
        let per_cpu = per_cpu();
        if let Some(pid) = self.current() {
            // 方案 B (exit/回收竞态根治): 在置为 Zombie **之前**, 于同一锁域内
            // 持有一次性自引用并置 `exiting`. 该自引用使父进程 `wait4` 的
            // `remove_and_free` 只能置 `pending_free` 而不能真正释放本 PCB ——
            // 否则退出进程会在已被释放的内核栈上完成最后一次 `schedule()`, 因
            // 拿不到 prev 上下文而不切换、直接返回用户态 (UAF). 自引用由本 CPU
            // 下一次 `schedule()` 入口的 `reap_off_cpu` 释放.
            PROCESS_TABLE.with_process(pid, |proc| {
                if proc.try_inc_ref() {
                    proc.exiting.store(true, Ordering::Release);
                }
            });
            // 埋点 (见 docs/plan/tlb-shootdown-epoch.md S-10): 退出事件是延迟释放覆盖的
            // 上游分位 —— 与 `vmm_x86_64.rs` 的 `destroy_page_table` 埋点配对, 可区分
            // "子进程根本没跑到 exit" 与 "已退出成 zombie 但无人回收 (无 wait4 ⇒ 无人调
            // `remove_and_free` ⇒ `Process::drop` 不运行 ⇒ 页表不销毁)" 两种情形.
            crate::klog_info!(Process, "exit: pid={} code={}", pid, exit_code);
            // 会话 leader 退出时释放控制终端
            crate::privileged::proc::session_leader_exit(pid);
            // P-7 根治: 终止即撤防 per-pid 截止期类资源 — `ITIMER_REAL`/`alarm` 与
            // POSIX timer. 两者的到期登记在 hrtimer 框架里, 不会随 `Process` 销毁
            // 而消失; 不撤防则 pid 复用后新进程会收到前一进程遗留的 SIGALRM/信号.
            // (槽位表是静态数组, 不存在 UAF, 但归属信息已失效)
            crate::privileged::proc::itimer_real_release_pid(pid);
            crate::privileged::proc::posix_timer_release_pid(pid);

            let parent_pid_opt = PROCESS_TABLE.with_process(pid, |proc| {
                let pwm = proc.get_pwm();
                proc.exit_code.store(exit_code, Ordering::SeqCst);
                let _ = proc.set_state_safe(ProcessState::Zombie);
                self.dec_limit(pwm);

                // D2: 进程退出时从 cgroup 移除
                let cg_id = proc.cgroup_id.load(core::sync::atomic::Ordering::Acquire);
                if crate::privileged::proc::cgroup_is_initialized() {
                    let sub = crate::privileged::proc::cgroup_subsystem();
                    if let Some(cg) = sub.find(cg_id) {
                        cg.detach_proc(pid);
                    }
                }

                proc.parent.map(|p| p.0)
            });

            if let Some(parent_pid) = parent_pid_opt.flatten() {
                self.unblock(parent_pid);
            }

            // 单次加锁取出子进程列表, 随后逐个独立加锁处理.
            // `processes` 为不可重入锁, 在持锁闭包内再次 `with_process*` /
            // `remove_and_free` 会同核自死锁.
            let children: alloc::vec::Vec<Pid> = PROCESS_TABLE
                .with_process(pid, |proc| {
                    proc.children.lock().iter().map(|c| c.0).collect()
                })
                .unwrap_or_default();
            for child_pid in children {
                PROCESS_TABLE.with_process_mut(child_pid, |child| {
                    let state = child.get_state();
                    if state == ProcessState::Zombie {
                        let _ = child.set_state_safe(ProcessState::Terminated);
                    } else {
                        child.parent = Some(ProcessId(1));
                    }
                });
                let child_terminated = PROCESS_TABLE
                    .with_process(child_pid, |c| c.get_state() == ProcessState::Terminated)
                    .unwrap_or(false);
                if child_terminated {
                    PROCESS_TABLE.remove_and_free(child_pid);
                }
            }
            let _ = PROCESS_TABLE.with_process(pid, |proc| proc.children.lock().clear());

            // 本函数**不**释放本进程的用户地址空间: 唯一的销毁点是
            // `Process::drop` -> `vmm_destroy_page_table`, 而 `Process` 只在
            // **收割 (reap)** 时释放 —— 即 `wait4` 的 `remove_and_free`, 或
            // `tick_accounting` 的周期僵尸回收 (条件: 父已死或父为 pid 1).
            // 地址空间因此晚于退出被释放; 这与上游 (Asterinas `set_vmar(None)` /
            // Linux `exit_mm`) 的"退出即释放"不同, 属已登记缺陷 (见
            // docs/plan/tlb-shootdown-epoch.md §6 D4 与 docs/plan/cr3-lifetime-ownership.md G4).
        }

        per_cpu.need_reschedule.store(true, Ordering::SeqCst);

        // 调度下一个任务. 生产路径不因"无任务可运行"而结束运行: 本 CPU 的
        // idle 任务保证 `schedule()` 在运行期永不为 None. 整机退出 (QEMU exit)
        // 由测试框架承担 (`privileged::debug` 的 `qemu_exit`), 不属调度器职责.
        self.schedule();
    }

    pub fn yield_current(&self) {
        per_cpu().need_reschedule.store(true, Ordering::SeqCst);
        self.schedule();
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn set_need_reschedule(&self) {
        per_cpu().need_reschedule.store(true, Ordering::SeqCst);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn should_reschedule(&self) -> bool {
        per_cpu().need_reschedule.swap(false, Ordering::SeqCst)
    }

    pub fn add_to_run_queue(&self, pid: Pid) {
        // I-35: 重定向到 cfs_enqueue. 历史 MLFQ queues[0] 路径曾导致新进程
        // 永远不会被 pick_cfs_task 选中 (调度器只读 cfs_rq, 不读 queues[]).
        // 现在 add / add_to_run_queue 等价, 都走 vruntime 红黑树.
        //
        // APS-03: 多核下把任务投送到**空闲核** (而非恒留当前核), 并在跨核时向
        // 目标核发重调度 IPI 唤醒其 idle 调度循环; 单核 / 无空闲核时退化为本核
        // 入队, 与既有一致 (E11: 目标核 CFS 成为入队对象 + IPI + AP 有调度身份 +
        // AP 被唤醒 四者齐备, 任务才会真正落到 AP 上).
        let this_cpu = crate::privileged::smp::get_current_cpu();
        let target = self.find_idle_cpu(pid, this_cpu);
        // 搬定结果不在这里分支: 新子进程尚未入过队, 失败只可能是"它已被别的路径
        // 放上了某个队列" (幂等) 或任务已终 —— 两种情形下目标核仍需一次重调度提名,
        // 多发一个 IPI 无代价, 少了 IPI 则会拖到下一轮 tick.
        self.cfs_enqueue_to(pid, target);
        if target != this_cpu {
            // 有界诊断 (仅前若干次): 供 `-smp 2` 验证观测跨核投送.
            static MIGRATE_LOG: AtomicU64 = AtomicU64::new(0);
            if MIGRATE_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
                crate::klog_info!(Kernel, "[SMP] migrate pid={} -> cpu={}", pid, target);
            }
            // 锁外发送 IPI: resched_cpu 内部锁 cpu_queue, 与 cfs_rq 锁无嵌套.
            crate::privileged::proc::cpu_queue::resched_cpu(target);
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn set_current(&self, pid: Pid) {
        per_cpu().current.store(pid, Ordering::SeqCst);

        if let Some(process_ptr) = PROCESS_TABLE.get(pid) {
            // SAFETY: process_ptr is valid from PROCESS_TABLE.get()
            unsafe {
                update_current_process_ptr(process_ptr as u64);
            }
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::SeqCst)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn has_runnable(&self) -> bool {
        let per_cpu = per_cpu();
        if !per_cpu.dl_rq.lock().is_empty() {
            return true;
        }
        if !per_cpu.rt_queue.lock().is_empty() {
            return true;
        }
        if !per_cpu.cfs_rq.lock().is_empty() {
            return true;
        }
        false
    }

    /// 便捷: 检查是否存在任意可运行任务.
    pub fn has_any_runnable(&self) -> bool {
        self.has_runnable()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    pub fn tick(&self, cpu_id: usize) {
        // SMP: 禁用中断保护整个 tick 临界区
        // 防止非中断上下文的 schedule() 调用与 timer ISR 的 tick() 并发修改 per-CPU 状态
        let flags = crate::privileged::sync::disable_interrupts();

        // 单一时基: tick 读数取 `timer::get_ticks()` (本调度器不再自存一份计数,
        // P-1 双份计数收敛: 旧形态由本行 `fetch_add` 维护全局计数, 与
        // `timer::tick` 的中断计数并行演化, 两个读数会背离).
        let new_tick = crate::privileged::timer::get_ticks();
        let per_cpu = per_cpu_for(cpu_id as u32);

        // P-30 根治: 调度器 tick 与 FREG 恢复解耦 —— 旧形态每 tick 都在本关中断
        // 热路径里 `lock` 全局 RECOVERY_MANAGER 跑 `manager.tick()` 周期 FREG
        // capture 与 BSR 升级序列. FREG 是阶段性版本 (恢复域回调 inert, 生产不可达
        // DomainRecovery), 该周期维护本无收益, 却把每核热路径耦合到一把全局锁
        // (lockdep 下 LockDepMap 元锁把各 IrqSpinLock 簿记串行化, 放大成可见 convoy).
        // 摘除后 tick 不再触碰 RECOVERY_MANAGER; FREG 周期 capture / BSR 升级随之不
        // 再由调度器驱动 (登记为 FREG 冻结版本的已知解耦项).

        crate::privileged::proc::oomd::OOMD.tick();
        // 进程级 user/sys 时间记账: 原经全局线程级 SchedulerEx.tick_accounting 驱动,
        // 现收敛到本 per-CPU tick 直接承担 (P-29 route C, 消除每核并发驱动全局无锁结构的 livelock).
        let in_kern = crate::privileged::proc::proc_get_in_kern();
        crate::privileged::proc::proc_account_tick(in_kern);

        // Periodic kswapd wakeup — 每 KSWAPD_TICK_INTERVAL tick 唤醒一次内存回收
        // (B3 完整实现: kswapd 走 softirq 路径, 由 scheduler tick 周期驱动).
        // carry + CAS: 多核看到同一个硬件派生 tick ⇒ 只允许一个核触发本轮.
        let last_kswapd = LAST_KSWAPD_TICK.load(Ordering::Acquire);
        if new_tick.saturating_sub(last_kswapd) >= KSWAPD_TICK_INTERVAL
            && LAST_KSWAPD_TICK
                .compare_exchange(last_kswapd, new_tick, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            crate::privileged::mm::kswapd_wakeup();
        }

        // Periodic hotplug poll — 每 HOTPLUG_TICK_INTERVAL tick 唤醒一次 PCIe/USB
        // 热插拔槽位检查 (softirq 路径, 由 scheduler tick 周期驱动).
        let last_hotplug = LAST_HOTPLUG_TICK.load(Ordering::Acquire);
        if new_tick.saturating_sub(last_hotplug) >= HOTPLUG_TICK_INTERVAL
            && LAST_HOTPLUG_TICK
                .compare_exchange(last_hotplug, new_tick, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            crate::privileged::driver::hotplug::hotplug_wakeup();
        }

        let current_pid = per_cpu.current.load(Ordering::SeqCst);
        if current_pid != 0 {
            // RT FIFO watchdog
            let is_rt = per_cpu.rt_running.load(Ordering::SeqCst);
            if is_rt && per_cpu.fifo_watchdog.load(Ordering::SeqCst) > 0 {
                let remaining = per_cpu.fifo_watchdog.fetch_sub(1, Ordering::SeqCst);
                if remaining <= 1 {
                    per_cpu.need_reschedule.store(true, Ordering::SeqCst);
                    per_cpu.rt_running.store(false, Ordering::SeqCst);
                }
            }

            // Tick 计数: 按策略的时间跟踪
            let is_dl = per_cpu.dl_running.load(Ordering::SeqCst);

            if is_dl {
                let (expired, should_replenish) = PROCESS_TABLE
                    .with_process(current_pid, |p| {
                        let old_rem = p.dl_remaining.fetch_sub(1, Ordering::SeqCst);
                        let rem = old_rem - 1;
                        let expired = rem == 0;
                        let deadline = p.dl_deadline.load(Ordering::Acquire);
                        let dl_abs = p.dl_abs.load(Ordering::Acquire);
                        let now = crate::privileged::timer::get_ticks();
                        let should_replenish =
                            now >= dl_abs || (expired && now + deadline > dl_abs);
                        (expired, should_replenish)
                    })
                    .unwrap_or((true, false));

                if should_replenish {
                    PROCESS_TABLE.with_process(current_pid, |p| {
                        let runtime = p.dl_runtime.load(Ordering::Acquire);
                        let deadline = p.dl_deadline.load(Ordering::Acquire);
                        let now = crate::privileged::timer::get_ticks();
                        p.dl_remaining.store(runtime, Ordering::Release);
                        p.dl_abs.store(now + deadline, Ordering::Release);
                    });
                }

                if expired || should_replenish {
                    per_cpu.need_reschedule.store(true, Ordering::SeqCst);
                }
            } else if is_rt {
                // RT FIFO watchdog
                let policy = PROCESS_TABLE
                    .with_process(current_pid, super::process::Process::get_sched_policy)
                    .unwrap_or(SchedPolicy::Normal);

                if policy == SchedPolicy::Fifo {
                    let old_watchdog = per_cpu.fifo_watchdog.fetch_sub(1, Ordering::SeqCst);
                    if old_watchdog - 1 == 0 {
                        per_cpu.need_reschedule.store(true, Ordering::SeqCst);
                        crate::klog_crit!(
                            Kernel,
                            "[SCHEDULER] RT-FIFO watchdog triggered for pid={}",
                            current_pid
                        );
                    }
                }
            } else if current_pid != 0 {
                // CFS —— 基于 vruntime 的抢占.

                // 重要: 不要在此把运行中任务重新插入树.
                // 运行中任务保持在树外, 仅在停止运行时
                // (schedule 的重新入队路径) 才会被重新入队.
                let (should_preempt, should_yield) = {
                    let cfs_rq = per_cpu.cfs_rq.lock();
                    let (vr, weight) = PROCESS_TABLE
                        .with_process(current_pid, |p| {
                            let weight = p.cfs_weight.load(Ordering::Acquire);
                            // IC1 兜底: 经 `keep_current` 回退上核的任务 (如 boot
                            // 期 init) 可能从未在队列落过点, 其 vr 与 floor 不同域
                            // —— 先对齐再累加, 使两者恒在同一时基上比较.
                            let base = cfs_rq.placement(p.cfs_vruntime.load(Ordering::Acquire));
                            let delta = calc_vruntime_delta(weight);
                            // L2 修复: 使用 saturating_add 防止 vruntime 溢出
                            let new_vr = base.saturating_add(delta);
                            p.cfs_vruntime.store(new_vr, Ordering::Release);
                            let sum = p.cfs_sum_exec_runtime.load(Ordering::Acquire);
                            // L2 修复: 使用 saturating_add 防止 sum_exec_runtime 溢出
                            p.cfs_sum_exec_runtime
                                .store(sum.saturating_add(1), Ordering::Release);
                            (new_vr, weight)
                        })
                        .unwrap_or((0, NICE0_WEIGHT));

                    // IC3: 运行任务的时基推进队列下界 —— 这是本调度器唯一的
                    // 反饥饿机制 (旧的周期性整树折叠 boost 会抹掉相对次序, 且与
                    // 任务自身 vruntime 分域, 已删除).
                    cfs_rq.advance_floor(vr);

                    // IC1: `vr` 与树左端同域, 差值才真正表示"领先了多少 tick".
                    // IC2: 候选数判据取自树 (旧 `nr_running` 与树背离时, 抢占
                    // 判据会在 `nr>0 / tree=0` 的假象下永久失效).
                    let should_preempt = cfs_rq
                        .leftmost_vruntime()
                        .is_some_and(|next_vr| cfs_should_preempt(vr, next_vr, weight));

                    let should_yield = vr > cfs_rq.floor() + TARGET_LATENCY_TICKS;

                    (should_preempt, should_yield)
                };

                if should_preempt || should_yield {
                    per_cpu.need_reschedule.store(true, Ordering::SeqCst);
                }
            }
        }

        // 睡眠唤醒扫描
        {
            let current_ticks = new_tick;
            let mut to_wake: [Pid; 8] = [0; 8];
            let mut wake_count = 0;
            for pid in 1..=255 {
                if wake_count >= 8 {
                    break;
                }
                if pid == self.current().unwrap_or(0) {
                    continue;
                }
                PROCESS_TABLE.with_process(pid, |proc| {
                    let state = proc.get_state();
                    if state == ProcessState::Blocked {
                        let reason = proc.block_reason.load(Ordering::Relaxed);
                        if reason == BlockReason::Sleeping as u32 {
                            let until = proc.sleep_until.load(Ordering::SeqCst);
                            if until > 0 && current_ticks >= until {
                                to_wake[wake_count] = pid;
                                wake_count += 1;
                            }
                        }
                    }
                });
            }
            for i in 0..wake_count {
                self.unblock(to_wake[i]);
            }
        }

        // Zombie cleanup — 每 REAP_TICK_INTERVAL tick 一轮, 多核下只 CAS 赢家执行
        // (回收路径末端是 `remove_and_free`, 两核同窗对同一 pid 重复执行即 double-free).
        let last_reap = LAST_REAP_TICK.load(Ordering::Acquire);
        if new_tick.saturating_sub(last_reap) >= REAP_TICK_INTERVAL
            && LAST_REAP_TICK
                .compare_exchange(last_reap, new_tick, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            let mut to_reap: [Pid; 16] = [0; 16];
            let mut reap_count = 0;
            for pid in 1..=255 {
                if reap_count >= 16 {
                    break;
                }
                if let Some(_proc) = PROCESS_TABLE.get(pid) {
                    // 单次加锁取本进程状态与父 pid, 父存活另起一次加锁查询.
                    // `processes` 为不可重入锁, 在持锁闭包内再次 `with_process`
                    // 会同核自死锁 (本轮 hang 根因).
                    let zombie_parent = PROCESS_TABLE
                        .with_process(pid, |p| {
                            if p.get_state() == ProcessState::Zombie {
                                Some(p.parent)
                            } else {
                                None
                            }
                        })
                        .flatten();
                    let is_zombie = zombie_parent.is_some_and(|parent| {
                        let parent_alive = parent.map_or(true, |ppid| {
                            PROCESS_TABLE
                                .with_process(ppid.0, |pp| {
                                    let s = pp.get_state();
                                    s != ProcessState::Zombie && s != ProcessState::Terminated
                                })
                                .unwrap_or(false)
                        });
                        !parent_alive || parent == Some(ProcessId(1))
                    });
                    if is_zombie {
                        to_reap[reap_count] = pid;
                        reap_count += 1;
                    }
                }
            }
            for i in 0..reap_count {
                PROCESS_TABLE.with_process(to_reap[i], |p| {
                    let _ = p.set_state_safe(ProcessState::Terminated);
                });
                PROCESS_TABLE.remove_and_free(to_reap[i]);
            }
        }

        // 周期性负载均衡 — 每 BALANCE_TICK_INTERVAL tick 一轮, 且每轮只有一个核.
        //
        // `load_balance` 是 pull 型 (从最忙核偷到**本核**): E-II-b 后每个核都收 tick,
        // 若各核在同一全局 tick 上各自发起, 就会出现 CPU0 偷 CPU1 同时 CPU1 偷 CPU0
        // 的双向互偷 (ping-pong), 每次迁移再叠一发 TLB shootdown IPI. 实测后果是网络
        // poll 窗口被打穿 (x86_64 e2e 由 6/6 退化为 0/6), 并在迁移密集处偶发挂死.
        let last_balance = LAST_BALANCE_TICK.load(Ordering::Acquire);
        if new_tick.saturating_sub(last_balance) >= BALANCE_TICK_INTERVAL
            && LAST_BALANCE_TICK
                .compare_exchange(last_balance, new_tick, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            let local_load = self.total_runnable_for(crate::privileged::smp::get_current_cpu());
            if local_load < 2 {
                self.load_balance();
            }
        }

        // 本核 current 为 idle 且存在可运行任务 ⇒ 必须重调度.
        //
        // idle 不在任何运行队列中, 上面基于 vruntime 的 CFS 抢占判据对它无意义:
        // 当任务在 `schedule()` 末尾被重新入队 (`prev_requeue`) 而本核已切到 idle 时,
        // 只有本判据能把它重新拉回运行 (否则该核永久停在 idle, 见 ISSUE-RT-004).
        if current_pid != 0
            && per_cpu.idle.load(Ordering::SeqCst) == current_pid
            && self.has_runnable()
        {
            per_cpu.need_reschedule.store(true, Ordering::SeqCst);
        }

        // 本函数运行于定时器中断内且中断尚未 EOI, 此刻直接 `schedule()` 会悬置
        // 本核 GIC/LAPIC 运行优先级, 阻断后续中断; 故只登记本核挂起重调度
        // (见 `mark_resched_pending_local`), 实际切换交由中断退出路径在
        // `do_softirq()` 返回之后经 `run_pending_resched` 执行.
        if per_cpu.need_reschedule.swap(false, Ordering::SeqCst) {
            crate::privileged::proc::cpu_queue::mark_resched_pending_local();
        }

        crate::privileged::sync::restore_interrupts(&flags);
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn set_sched_policy(&self, pid: Pid, policy: SchedPolicy, rt_priority: u8) -> bool {
        PROCESS_TABLE
            .with_process(pid, |proc| {
                proc.set_sched_policy(policy);
                proc.set_rt_priority(rt_priority.min(RT_PRIORITY_MAX));
            })
            .is_some()
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn get_rt_count(&self) -> usize {
        per_cpu().rt_queue.lock().len()
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// C2: 检查目标 CPU 是否在进程的 allowed cpuset 中
    ///
    /// 调度器选 CPU / 负载均衡迁移时调用, 约束进程 CPU 亲和性.
    /// 单核系统: 始终返回 true.
    pub fn is_cpu_allowed(&self, pid: Pid, cpu_id: u32) -> bool {
        if pid == 0 {
            return true;
        }
        let cpu_count = crate::privileged::smp::get_cpu_count();
        if cpu_count <= 1 || cpu_id >= cpu_count {
            return true;
        }
        if (cpu_id as usize) >= 64 {
            return true; // Edgine 当前 cpuset 是 64-bit
        }
        let allowed = PROCESS_TABLE
            .with_process(pid, |p| p.cpuset_allowed.load(Ordering::Acquire))
            .unwrap_or(u64::MAX);
        (allowed >> cpu_id) & 1 == 1
    }

    /// C2: 为进程选择最合适的 CPU
    ///
    /// 策略: 在 allowed cpuset 中选 load 最低的 CPU.
    /// 单核: 直接返回当前 CPU.
    /// 找不到 allowed CPU: 返回 `hint_cpu` (退化路径, 调度器仍可工作).
    pub fn select_cpu_for(&self, pid: Pid, hint_cpu: u32) -> u32 {
        let cpu_count = crate::privileged::smp::get_cpu_count();
        if cpu_count <= 1 {
            return hint_cpu.min(cpu_count.saturating_sub(1));
        }

        // 优先尝试 hint_cpu (通常为当前 CPU, 缓存亲和)
        if self.is_cpu_allowed(pid, hint_cpu) {
            return hint_cpu;
        }

        // 在 allowed cpuset 中选 load 最低的 CPU
        let mut best_cpu = hint_cpu;
        let mut best_load: u64 = u64::MAX;
        for cpu in 0..cpu_count {
            if !self.is_cpu_allowed(pid, cpu) {
                continue;
            }
            let sched = per_cpu_for(cpu);
            let load = sched.cfs_rq.lock().total_weight.load(Ordering::Acquire);
            if load < best_load {
                best_load = load;
                best_cpu = cpu;
            }
        }
        best_cpu
    }

    /// 在 allowed cpuset 内为 `pid` 挑选一个**空闲核**作为投送目标.
    ///
    /// 判据: 该核已建立调度身份 (`current == idle != 0`) 且 CFS 无待运行任务;
    /// 命中即返回该核. 单核或找不到空闲核时返回 `hint` (通常为当前核), 保证
    /// 退化路径仍可在当前核调度.
    ///
    /// **不复用 [`Self::select_cpu_for`]**: 后者优先返回 `hint`(当前核), 语义是
    /// "缓存亲和", 与本函数"投送到空闲核"相反 (E6).
    fn find_idle_cpu(&self, pid: Pid, hint: u32) -> u32 {
        let cpu_count = crate::privileged::smp::get_cpu_count();
        if cpu_count <= 1 {
            return hint;
        }
        for cpu in 0..cpu_count {
            if cpu == hint || !self.is_cpu_allowed(pid, cpu) {
                continue;
            }
            let sched = per_cpu_for(cpu);
            let cur = sched.current.load(Ordering::SeqCst);
            let idle = sched.idle.load(Ordering::SeqCst);
            if cur != 0 && cur == idle && sched.cfs_rq.lock().is_empty() {
                return cpu;
            }
        }
        hint
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    fn total_runnable_for(&self, cpu_id: u32) -> usize {
        let sched = per_cpu_for(cpu_id);
        // IC2: 可运行数一律取自结构 (树长度), 不读并行计数器.
        let mut count = sched.cfs_rq.lock().len();
        count += sched.dl_rq.lock().len();
        count += sched.rt_queue.lock().len();
        // 统计运行中的任务
        if sched.current.load(Ordering::SeqCst) != 0 {
            count += 1;
        }
        count
    }

    pub fn load_balance(&self) {
        let cpu_count = crate::privileged::smp::get_cpu_count();
        if cpu_count <= 1 {
            return;
        }

        let this_cpu = crate::privileged::smp::get_current_cpu();
        let local_weight = {
            let sched = per_cpu_for(this_cpu);
            sched.cfs_rq.lock().total_weight.load(Ordering::Acquire)
        };

        let mut max_weight: u64 = 0;
        let mut busiest_cpu: u32 = this_cpu;

        for cpu in 0..cpu_count {
            if cpu == this_cpu {
                continue;
            }
            let w = {
                let sched = per_cpu_for(cpu);
                sched.cfs_rq.lock().total_weight.load(Ordering::Acquire)
            };
            if w > max_weight {
                max_weight = w;
                busiest_cpu = cpu;
            }
        }

        // 检查最忙 CPU 是否显著更忙 (按权重)
        if max_weight < local_weight.saturating_add(LOAD_BALANCE_THRESHOLD) {
            return;
        }

        // 偷取全程**同时持有源/目的两把 rq 锁** —— 分两段 (摘源后放锁再投目的) 会留下
        // "既不在源树也不在目的树" 的归属真空窗口, 并发唤醒正是在那里把同一任务投进
        // 第三棵树 (见 [`Self::cfs_place_on`] 的同源说明).
        // 双持锁序: cpu index **升序** (同层级内定序, 见 docs/explain/ref-lock-order.md);
        // 两个方向相反的搬动者各持一把再抢对方那把, 即 AB-BA 死锁.
        let moved = if busiest_cpu < this_cpu {
            let mut src_rq = per_cpu_for(busiest_cpu).cfs_rq.lock();
            let mut dst_rq = per_cpu_for(this_cpu).cfs_rq.lock();
            self.steal_locked(busiest_cpu, this_cpu, &mut src_rq, &mut dst_rq)
        } else {
            let mut dst_rq = per_cpu_for(this_cpu).cfs_rq.lock();
            let mut src_rq = per_cpu_for(busiest_cpu).cfs_rq.lock();
            self.steal_locked(busiest_cpu, this_cpu, &mut src_rq, &mut dst_rq)
        };

        // 偷到的任务进了本核可运行集: 置重调度标记, 出 ISR 后由既有 `should_reschedule`
        // 路径决定是否立即上核 (本核此刻可能正跑 idle 任务).
        if moved > 0 {
            per_cpu_for(this_cpu)
                .need_reschedule
                .store(true, Ordering::SeqCst);
        }
    }

    /// 在同时持有 `src_cpu`/`dst_cpu` 两把 rq 锁的临界区内, 从源队列偷至多
    /// [`LOAD_BALANCE_MAX_MOVE`] 个任务到目的队列; 返回实际搬动数.
    ///
    /// 只由 [`Self::load_balance`] 按升序双持锁后调用 (锁序前提不在此处重建).
    /// 锁序 `cfs_rq -> PROCESS_TABLE` 与本文件其余调度路径一致.
    fn steal_locked(
        &self,
        src_cpu: u32,
        dst_cpu: u32,
        src: &mut CfsRunQueue,
        dst: &mut CfsRunQueue,
    ) -> usize {
        let mut cands: [Pid; LOAD_BALANCE_MAX_MOVE] = [0; LOAD_BALANCE_MAX_MOVE];
        let n = src.leftmost_pids(&mut cands);
        let mut moved = 0usize;
        for i in 0..n {
            let pid = cands[i];
            // 非阻塞认领: 已被他人认领者直接放过. 等待即死锁 —— 那个认领者正在等的
            // rq 锁就在我们手里.
            let Some(mut claim) = self.try_claim_rq(pid) else {
                continue;
            };
            // 归属已不在源核 (候选取回后被别的路径搬走) 或任务正在源核上跑
            // (离树但记账): 两种都不能搬, 直接 continue —— 凭证 Drop 原样交回认领.
            if claim.prev_cpu != src_cpu
                || per_cpu_for(src_cpu).current.load(Ordering::SeqCst) == pid
                || !self.is_cpu_allowed(pid, dst_cpu)
            {
                continue;
            }
            let (state, policy, vr, weight, on_rq, on_cpu) = PROCESS_TABLE
                .with_process(pid, |p| {
                    (
                        p.get_state(),
                        p.get_sched_policy(),
                        p.cfs_vruntime.load(Ordering::Acquire),
                        p.cfs_weight.load(Ordering::Acquire),
                        p.cfs_on_rq.load(Ordering::Acquire),
                        p.cfs_on_cpu.load(Ordering::Acquire),
                    )
                })
                .unwrap_or((
                    ProcessState::Zombie,
                    SchedPolicy::Normal,
                    0,
                    0,
                    false,
                    false,
                ));
            // 投送屏障 (P-21): 被偷者若仍占用某个核 (上下文未保存完), 搬进本核队列
            // 就等于两核交叠同一块进程内核栈. 与 `place_locked` 同一判据, 不走
            // 下面的"不可调度即撤账摘除"分支 —— 它只是**暂时**不可搬, 账目属于源核.
            if on_cpu {
                continue;
            }
            let schedulable = state != ProcessState::Blocked
                && state != ProcessState::Zombie
                && state != ProcessState::Terminated
                && policy == SchedPolicy::Normal;
            if !schedulable {
                // 与 `pick_cfs_task` 同口径: 不可调度者撤账摘除, 不留树上占位 (IC2).
                src.dequeue(pid, weight, on_rq);
                let _ = PROCESS_TABLE.with_process(pid, |p| {
                    p.cfs_on_rq.store(false, Ordering::Release);
                });
                claim.commit(NO_RQ_CPU);
                continue;
            }
            // 跨核迁移 = 记账随任务一起搬走: 源核撤账摘节点, 目的核重新记账投节点,
            // 两核权重和守恒 (旧实现两边都不动账, 使 `total_weight` 与真实可运行集
            // 发散, 均衡判定越跑越偏).
            src.dequeue(pid, weight, on_rq);
            let placed = dst.enqueue(pid, vr, weight, false);
            PROCESS_TABLE.with_process(pid, |p| {
                // IC1: 落点回写任务自身 vruntime.
                p.cfs_vruntime.store(placed, Ordering::Release);
                p.cfs_on_rq.store(true, Ordering::Release);
            });
            claim.commit(dst_cpu);
            moved += 1;
        }
        moved
    }

    /// 为 PWM 设置 CPU 配额. 调用方必须持有 `SYSTEM_CAP_QUOTA_ADMIN`.
    pub fn set_quota(&self, pwm: u64, max_runtime: u64, period: u64) {
        let mut quotas = self.quotas.lock();
        let now = crate::privileged::timer::get_ticks();
        for q in quotas.iter_mut() {
            if q.used && q.pwm == pwm {
                q.max_runtime = max_runtime;
                q.period = period;
                q.consumed = 0;
                q.next_reset = now + period;
                return;
            }
        }
        for q in quotas.iter_mut() {
            if !q.used {
                q.used = true;
                q.pwm = pwm;
                q.max_runtime = max_runtime;
                q.period = period;
                q.consumed = 0;
                q.next_reset = now + period;
                return;
            }
        }
    }

    /// Remove CPU quota for a PWM
    pub fn remove_quota(&self, pwm: u64) {
        let mut quotas = self.quotas.lock();
        for q in quotas.iter_mut() {
            if q.used && q.pwm == pwm {
                q.used = false;
                q.pwm = 0;
                return;
            }
        }
    }

    /// 设置 PWM 的进程数上限.
    pub fn set_limit(&self, pwm: u64, max_procs: u32) {
        let mut limits = self.limits.lock();
        for l in limits.iter_mut() {
            if l.used && l.pwm == pwm {
                l.max_procs = max_procs;
                return;
            }
        }
        for l in limits.iter_mut() {
            if !l.used {
                l.used = true;
                l.pwm = pwm;
                l.max_procs = max_procs;
                l.current = 0;
                return;
            }
        }
    }

    /// 进程退出时递减进程计数 (由 `exit()` 调用)
    fn dec_limit(&self, pwm: u64) {
        if pwm == 0 {
            return;
        }
        let mut limits = self.limits.lock();
        for l in limits.iter_mut() {
            if l.used && l.pwm == pwm && l.current > 0 {
                l.current -= 1;
                return;
            }
        }
    }
}

pub static SCHEDULER: Scheduler = Scheduler::new();

pub static SCHEDULER_READY: AtomicBool = AtomicBool::new(false);

/// 调度器子系统初始化入口 — 初始化全局调度器并置位 [`SCHEDULER_READY`]。
///
/// 具体工作 (建立 BSP per-CPU 状态、idle 任务与 init 进程) 由
/// [`Scheduler::init`] 完成; 本函数仅负责对外暴露入口并发布就绪标志。
pub fn init() {
    SCHEDULER.init();
    // D6: 进程级 tick 与跨核 resched IPI 只登记重调度请求 (见
    // `cpu_queue::mark_resched_pending_local`), 实际切换由中断退出路径在
    // `do_softirq()` 返回后经 `cpu_queue::run_pending_resched` 执行.
    SCHEDULER_READY.store(true, Ordering::Release);
}
