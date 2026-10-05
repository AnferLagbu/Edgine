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
//! `add_to_run_queue` 路径已重定向到 `cfs_enqueue`; `boost_priority` 死代码已删除
//! (与 `boost_all_vruntime` 逻辑 100% 等价), 周期性 boost 统一走 `boost_all_vruntime`.
//!
//! ## 调度决策链
//!
//! `schedule()` 严格按 DL → RT → CFS 顺序回退, 每个层级内部找不到可运行任务时
//! 立即降级到下一层级, 与 Linux `pick_next_task` 行为一致.

use crate::privileged::sync::{IrqSpinLock as Mutex, OnceLock};
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

use super::cfs::{
    CFS_BOOST_INTERVAL_TICKS, CfsRunQueue, DL_MAX_UTILIZATION_PCT, DeadlineParams, DlRunQueue,
    LOAD_BALANCE_THRESHOLD, NICE0_WEIGHT, TARGET_LATENCY_TICKS, calc_vruntime_delta,
    cfs_should_preempt, nice_to_weight,
};
use super::process::{PROCESS_TABLE, Process};
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

pub struct PwidLimit {
    pub pwm: u64,
    pub used: bool,
    pub max_procs: u32,
    pub current: u32,
}

pub static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// 获取当前全局 tick 计数 (供 `tick_query` 注册回调使用).
#[inline]
pub fn get_tick() -> u64 {
    TICK_COUNT.load(Ordering::SeqCst)
}

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
        cfs_rq: Mutex::new(CfsRunQueue::new()),
        dl_rq: Mutex::new(DlRunQueue::new()),
        current: AtomicU32::new(0),
        idle: AtomicU32::new(0),
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
    pub fn set_nice(&self, pid: Pid, nice: i8) {
        PROCESS_TABLE.with_process(pid, |proc| {
            let clamped = nice.clamp(-20, 19);
            let w = nice_to_weight(clamped);
            proc.nice.store(clamped as u32, Ordering::Release);
            proc.cfs_weight.store(w, Ordering::Release);
        });
    }

    /// 将进程入队**当前核**的 CFS 运行队列.
    fn cfs_enqueue(&self, pid: Pid) {
        self.cfs_enqueue_to(pid, crate::privileged::smp::get_current_cpu());
    }

    /// 将进程入队**指定核**的 CFS 运行队列.
    ///
    /// 与 [`Self::cfs_enqueue`] 的区别: 本函数可投送到目标核, 供跨核投送
    /// (fork / 唤醒) 使用; 只持目标核的 `cfs_rq`, 不与其它核的队列锁嵌套.
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    fn cfs_enqueue_to(&self, pid: Pid, cpu_id: u32) {
        let vr = PROCESS_TABLE.with_process(pid, |p| {
            let _ = p.set_state_safe(ProcessState::Ready);
            let v = p.cfs_vruntime.load(Ordering::Acquire);
            let w = p.cfs_weight.load(Ordering::Acquire);
            p.cfs_on_rq.store(true, Ordering::Release);
            (v, w)
        });

        if let Some((vruntime, weight)) = vr {
            per_cpu_for(cpu_id)
                .cfs_rq
                .lock()
                .enqueue(pid, vruntime, weight);
        }
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
            let now = TICK_COUNT.load(Ordering::Acquire);
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
                // pick_next() 已从树中移除任务, 但
                // 保留 nr_running (与 CfsRunQueue 相同).
                // reinsert() 把它放回去, 不修改计数器.
                dl_rq.reinsert(pid, dl_abs);
                per_cpu.dl_running.store(false, Ordering::SeqCst);
                None
            }
        } else {
            per_cpu.dl_running.store(false, Ordering::SeqCst);
            None
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 选取一个 CFS 任务 (vruntime 最小者).
    fn pick_cfs_task(&self) -> Option<Pid> {
        let per_cpu = per_cpu();
        let mut cfs_rq = per_cpu.cfs_rq.lock();
        if cfs_rq.is_empty() {
            return None;
        }
        // 不可调度节点的暂存区: 全部取回后统一放回 (见下).
        let mut skipped: alloc::vec::Vec<(Pid, u64)> = alloc::vec::Vec::new();
        // 循环取下一个节点, 直到取到可调度者 (或树空).
        //
        // 不能"取一个不可调度节点就 return None" —— 若最小 vruntime 位置恰好
        // 是不可调度节点, 每次调度都会在此早返回, 树上其余可调度任务永远选不
        // 出来 (本核 CFS 永久饥饿). 这里保持"不静默丢失"语义: 不可调度节点
        // 暂存, 循环继续; 结束后统一放回.
        let mut picked: Option<Pid> = None;
        while let Some((pid, vr)) = cfs_rq.pick_next() {
            let schedulable = PROCESS_TABLE
                .with_process(pid, |p| {
                    let state = p.get_state();
                    let policy = p.get_sched_policy();
                    state != ProcessState::Blocked
                        && state != ProcessState::Zombie
                        && policy == SchedPolicy::Normal
                })
                .unwrap_or(false);
            if schedulable {
                PROCESS_TABLE.with_process(pid, |p| {
                    p.cfs_on_rq.store(false, Ordering::Release);
                });
                picked = Some(pid);
                break;
            }
            // 该节点已被 pick_next() 从树中移除, 但不可调度 (阻塞/僵尸/策略错):
            // 暂存, 继续取下一个节点.
            skipped.push((pid, vr));
        }
        // 放回暂存的不可调度节点: update_curr 只插树, 不动 nr_running
        // (与 pick_next 只移除、不改计数器对称), 保证它们不被丢弃.
        // 全程持有同一把 cfs_rq 锁, 中途未解锁.
        for (pid, vr) in skipped {
            cfs_rq.update_curr(pid, vr);
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
            if idle_pid != 0 {
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
        let next_kernel_stack = PROCESS_TABLE
            .with_process(next, |proc| {
                let _ = proc.set_state_safe(ProcessState::Running);
                proc.kernel_stack.load(Ordering::SeqCst)
            })
            .unwrap_or(0);
        if next_kernel_stack != 0 {
            crate::privileged::cpu::arch::set_kernel_stack(next_kernel_stack);
        }

        per_cpu.current.store(next, Ordering::SeqCst);

        // 注意: 此处**不得**同步写 `SCHEDULER_EX.current`.
        //
        // 该字段语义为 `*mut Thread` (由 `SchedulerEx` 独占写: `init`/`schedule`),
        // 而本函数持有的是进程号 `Pid` ⇒ 写入即造成类型混用: 下一 tick 的
        // `SCHEDULER_EX.tick_accounting` (aarch64 定时器 IRQ 路径) 会把 pid 当
        // 指针解引用, 触发非法访问 (aarch64 对齐异常 / x86 低地址静默写坏).
        // 另: `ThreadManager::create_thread` 当前无调用者, 进程无对应 `Thread`
        // 可供解析, 故不存在"查询后同步"的合法实现.
        // SIMPLIFIED: 直接省略跨层同步; 影响面 = `SCHEDULER_EX` 的线程级记账只
        // 作用于其自身 idle 线程 (进程级记账由 `proc_account_tick` 独立承担, 不受
        // 影响); 何时需扩展 = 线程层接线后由"双调度器合并"工程统一承载.

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
        let prev_requeue = if prev_ptr.is_some() {
            per_cpu.idle.load(Ordering::SeqCst) != current_pid
                && PROCESS_TABLE
                    .with_process(current_pid, |p| {
                        let state = p.get_state();
                        state == ProcessState::Running || state == ProcessState::Ready
                    })
                    .unwrap_or(false)
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
                    // pick_next() 保留了 nr_running (与 CFS 相同).
                    // reinsert() 把任务放回树, 不修改计数器
                    // —— 任务在最初入队时已计入.
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
                let (vr, _wt, _nice) = PROCESS_TABLE
                    .with_process(current_pid, |p| {
                        (
                            p.cfs_vruntime.load(Ordering::Acquire),
                            p.cfs_weight.load(Ordering::Acquire),
                            p.nice.load(Ordering::Acquire) as i8,
                        )
                    })
                    .unwrap_or((0, NICE0_WEIGHT, 0i8));
                per_cpu.cfs_rq.lock().update_curr(current_pid, vr);
                PROCESS_TABLE.with_process(current_pid, |p| {
                    p.cfs_on_rq.store(true, Ordering::Release);
                });
            }
            PROCESS_TABLE.with_process(current_pid, |p| {
                let _ = p.set_state_safe(ProcessState::Ready);
            });
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

        if !prev_ctx_ptr.is_null() {
            // 关键修复 (B05-55): 不能持 MutexGuard 调 context_switch.
            // process_switch_asm 切换后永不返回 (iretq 到 next 用户态), Guard 的
            // Drop 不执行 → prev/next 的 context 锁永久泄漏 → next 进程运行后
            // p.context.lock() (如 proc_save_user_regs) 自旋死锁.
            // 改用 get_mut_unchecked 裸访问: 单核 + process_switch_asm 开头 cli
            // 保证切换期间无并发访问.
            // SAFETY: prev/next_ptr 均派生自 PROCESS_TABLE 中活动的 Process 条目;
            // 单核 + cli 排他, 详见 get_mut_unchecked 文档.
            unsafe {
                let prev_ctx = core::ptr::addr_of_mut!(*((*prev_ctx_ptr).get_mut_unchecked()));
                let next_ctx = core::ptr::addr_of!(*((*next_ctx_ptr).get_mut_unchecked()));
                crate::arch!(context_switch(prev_ctx as *mut u8, next_ctx as *const u8));
            }
        }

        crate::privileged::sync::rcu::rcu_note_quiescent_state();

        Some(next)
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
            per_cpu.need_reschedule.store(true, Ordering::SeqCst);
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub fn unblock(&self, pid: Pid) {
        let sched_policy = PROCESS_TABLE
            .with_process(pid, |proc| {
                let state = proc.get_state();
                if state != ProcessState::Blocked && state != ProcessState::Frozen {
                    return None;
                }
                let _ = proc.set_state_safe(ProcessState::Ready);
                let policy = proc.get_sched_policy();
                if policy == SchedPolicy::Normal {
                    proc.cfs_on_rq.store(true, Ordering::Release);
                    let vr = proc.cfs_vruntime.load(Ordering::Acquire);
                    let w = proc.cfs_weight.load(Ordering::Acquire);
                    Some((policy, vr, w))
                } else if policy == SchedPolicy::Deadline {
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
            Some((SchedPolicy::Normal, vr, weight)) => {
                per_cpu().cfs_rq.lock().enqueue(pid, vr, weight);
            }
            Some((SchedPolicy::Fifo | SchedPolicy::Rr, _, _)) => {
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
                per_cpu().dl_rq.lock().enqueue(pid, dl_abs, util);
            }
            _ => {}
        }
    }

    pub fn exit(&self, exit_code: u32) {
        let per_cpu = per_cpu();
        if let Some(pid) = self.current() {
            // 埋点 (见 docs/plan/tlb-shootdown-epoch.md S-10): 退出事件是延迟释放覆盖的
            // 上游分位 —— 与 `vmm_x86_64.rs` 的 `destroy_page_table` 埋点配对, 可区分
            // "子进程根本没跑到 exit" 与 "已退出成 zombie 但无人回收 (无 wait4 ⇒ 无人调
            // `remove_and_free` ⇒ `Process::drop` 不运行 ⇒ 页表不销毁)" 两种情形.
            crate::klog_info!(Process, "exit: pid={} code={}", pid, exit_code);
            // 会话 leader 退出时释放控制终端
            crate::privileged::proc::session_leader_exit(pid);

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

            PROCESS_TABLE.with_process(pid, |proc| {
                let children: alloc::vec::Vec<Pid> =
                    proc.children.lock().iter().map(|c| c.0).collect();
                for child_pid in children {
                    PROCESS_TABLE.with_process_mut(child_pid, |child| {
                        let state = child.get_state();
                        if state == ProcessState::Zombie {
                            let _ = child.set_state_safe(ProcessState::Terminated);
                        } else {
                            child.parent = Some(ProcessId(1));
                        }
                    });
                    if PROCESS_TABLE
                        .with_process(child_pid, |c| c.get_state() == ProcessState::Terminated)
                        .unwrap_or(false)
                    {
                        PROCESS_TABLE.remove_and_free(child_pid);
                    }
                }
                proc.children.lock().clear();
            });

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
        let _ = self.schedule();
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

        let new_tick = TICK_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        let per_cpu = per_cpu_for(cpu_id as u32);

        crate::privileged::freg::RECOVERY_MANAGER
            .lock()
            .tick(new_tick);

        if crate::privileged::freg::check_and_clear_bsr_escalation() {
            crate::privileged::freg::reset::config::set_reset_in_progress(true);
            crate::privileged::freg::reset::config::set_current_layer(
                crate::privileged::freg::reset::config::RecoveryLayer::Layer2,
            );
            crate::privileged::freg::reset::bsr::freeze_all_domains();
            crate::privileged::freg::reset::bsr::rollback_to_init();
            crate::privileged::freg::reset::bsr::reset_devices();
            crate::privileged::freg::reset::bsr::unfreeze_all_domains();
            crate::privileged::freg::reset::bsr::clear_panic_state();
        }

        crate::privileged::proc::oomd::OOMD.tick();
        crate::privileged::proc::SCHEDULER_EX.tick_accounting();

        // Periodic kswapd wakeup — 每 100 ticks 唤醒一次内存回收
        // (B3 完整实现: kswapd 走 softirq 路径, 由 scheduler tick 周期驱动)
        if new_tick.is_multiple_of(KSWAPD_TICK_INTERVAL) {
            crate::privileged::mm::kswapd_wakeup();
        }

        // Periodic hotplug poll — 每 100 ticks 唤醒一次 PCIe/USB 热插拔槽位检查
        // (softirq 路径, 由 scheduler tick 周期驱动)
        if new_tick.is_multiple_of(HOTPLUG_TICK_INTERVAL) {
            crate::privileged::driver::hotplug::hotplug_wakeup();
        }

        // 周期性 CFS 提升 —— 防止 vruntime 饥饿
        if new_tick.is_multiple_of(CFS_BOOST_INTERVAL_TICKS) {
            let mut cfs_rq = per_cpu.cfs_rq.lock();
            cfs_rq.boost_all_vruntime();
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
                        let now = TICK_COUNT.load(Ordering::Acquire);
                        let should_replenish =
                            now >= dl_abs || (expired && now + deadline > dl_abs);
                        (expired, should_replenish)
                    })
                    .unwrap_or((true, false));

                if should_replenish {
                    PROCESS_TABLE.with_process(current_pid, |p| {
                        let runtime = p.dl_runtime.load(Ordering::Acquire);
                        let deadline = p.dl_deadline.load(Ordering::Acquire);
                        let now = TICK_COUNT.load(Ordering::Acquire);
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
                    let vr = PROCESS_TABLE
                        .with_process(current_pid, |p| {
                            let old_vr = p.cfs_vruntime.load(Ordering::Acquire);
                            let weight = p.cfs_weight.load(Ordering::Acquire);
                            let delta = calc_vruntime_delta(weight);
                            // L2 修复: 使用 saturating_add 防止 vruntime 溢出
                            let new_vr = old_vr.saturating_add(delta);
                            p.cfs_vruntime.store(new_vr, Ordering::Release);
                            let sum = p.cfs_sum_exec_runtime.load(Ordering::Acquire);
                            // L2 修复: 使用 saturating_add 防止 sum_exec_runtime 溢出
                            p.cfs_sum_exec_runtime
                                .store(sum.saturating_add(1), Ordering::Release);
                            new_vr
                        })
                        .unwrap_or(0);

                    let should_preempt = cfs_rq.nr_running > 0
                        && cfs_should_preempt(
                            vr,
                            cfs_rq.min_vruntime.load(Ordering::Acquire),
                            PROCESS_TABLE
                                .with_process(current_pid, |p| p.cfs_weight.load(Ordering::Acquire))
                                .unwrap_or(NICE0_WEIGHT),
                        );

                    let should_yield =
                        vr > cfs_rq.min_vruntime.load(Ordering::Acquire) + TARGET_LATENCY_TICKS;

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

        // Zombie cleanup
        let socks_clean_interval: u64 = 1000;
        if new_tick.is_multiple_of(socks_clean_interval) {
            let mut to_reap: [Pid; 16] = [0; 16];
            let mut reap_count = 0;
            for pid in 1..=255 {
                if reap_count >= 16 {
                    break;
                }
                if let Some(_proc) = PROCESS_TABLE.get(pid) {
                    let is_zombie = PROCESS_TABLE
                        .with_process(pid, |p| {
                            if p.get_state() == ProcessState::Zombie {
                                let parent_alive = p.parent.map_or(true, |ppid| {
                                    PROCESS_TABLE
                                        .with_process(ppid.0, |pp| {
                                            let s = pp.get_state();
                                            s != ProcessState::Zombie
                                                && s != ProcessState::Terminated
                                        })
                                        .unwrap_or(false)
                                });
                                !parent_alive || p.parent == Some(ProcessId(1))
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
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

        // 周期性负载均衡
        if new_tick.is_multiple_of(64) {
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
        let mut count = sched.cfs_rq.lock().nr_running as usize;
        count += sched.dl_rq.lock().nr_running as usize;
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

        // 从最忙 CPU 偷取任务
        let mut tasks_to_migrate: [Pid; 4] = [0; 4];
        let mut count = 0;
        {
            let mut src_rq = per_cpu_for(busiest_cpu).cfs_rq.lock();
            for _ in 0..4 {
                match src_rq.pick_next() {
                    Some((pid, vr)) => {
                        let weight = PROCESS_TABLE
                            .with_process(pid, |p| p.cfs_weight.load(Ordering::Acquire))
                            .unwrap_or(NICE0_WEIGHT);
                        src_rq.dequeue(pid, vr, weight);
                        tasks_to_migrate[count] = pid;
                        count += 1;
                    }
                    None => break,
                }
            }
        }

        let mut dst_rq = per_cpu_for(this_cpu).cfs_rq.lock();
        for i in 0..count {
            let pid = tasks_to_migrate[i];
            // C2: 亲和性检查 — 目标 CPU (this_cpu) 必须在进程 allowed 集合中,
            // 否则跳过该进程, 留给后续在 allowed CPU 上调度
            if !self.is_cpu_allowed(pid, this_cpu) {
                // 放回源队列 (避免丢失)
                let vr = PROCESS_TABLE
                    .with_process(pid, |p| p.cfs_vruntime.load(Ordering::Acquire))
                    .unwrap_or(0);
                let weight = PROCESS_TABLE
                    .with_process(pid, |p| p.cfs_weight.load(Ordering::Acquire))
                    .unwrap_or(NICE0_WEIGHT);
                drop(dst_rq);
                per_cpu_for(busiest_cpu)
                    .cfs_rq
                    .lock()
                    .enqueue(pid, vr, weight);
                dst_rq = per_cpu_for(this_cpu).cfs_rq.lock();
                continue;
            }
            let (vr, weight) = PROCESS_TABLE
                .with_process(pid, |p| {
                    (
                        p.cfs_vruntime.load(Ordering::Acquire),
                        p.cfs_weight.load(Ordering::Acquire),
                    )
                })
                .unwrap_or((0, NICE0_WEIGHT));
            dst_rq.enqueue(pid, vr, weight);
        }
    }

    /// 为 PWM 设置 CPU 配额. 调用方必须持有 `SYSTEM_CAP_QUOTA_ADMIN`.
    pub fn set_quota(&self, pwm: u64, max_runtime: u64, period: u64) {
        let mut quotas = self.quotas.lock();
        let now = TICK_COUNT.load(Ordering::SeqCst);
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

pub fn init() {
    SCHEDULER.init();
    // D6: 进程级 tick 与跨核 resched IPI 只登记重调度请求 (见
    // `cpu_queue::mark_resched_pending_local`), 实际切换由中断退出路径在
    // `do_softirq()` 返回后经 `cpu_queue::run_pending_resched` 执行.
    SCHEDULER_READY.store(true, Ordering::Release);
}
