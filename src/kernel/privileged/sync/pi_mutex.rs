//! Priority Inheritance Mutex (PI Mutex) — TCB 实现
//!
//! ## 协议
//!
//! 当高优先级线程 P_H 因等待低优先级线程 P_L 持有的 PI mutex 而阻塞时, 把
//! P_L 的**有效权重**临时提升至 max(P_L 基础权重, P_H 基础权重), P_L 释放
//! mutex 后恢复基础权重.
//!
//! ## v3 重设计 (A-7, 2026-11-07)
//!
//! 相对完整的五项边界闭合:
//!
//! 1. **单状态锁收敛**: `locked` / `holder` / 等待队列 / 有效权重 / 移交标记
//!    全部收进 `IrqSpinLock<PiMutexState>` 一个临界区, 不再散落多个原子量.
//! 2. **退出释放前移到线程 exit**: 等待登记表与持有登记表按 (锁地址, pid)
//!    记入固定容量数组; `process_exit` 入口先撤销等待、再强制释放持有, 收割期
//!    的 `process_cleanup` 调用保留为兜底.
//! 3. **`try_lock` 失败零副作用**: 非阻塞失败不改等待队列、不捐赠、不登记;
//!    需要"阻塞到拿到"的调用方显式走 `lock()`.
//! 4. **自旋改调度阻塞等待**: `lock()` 在状态锁内入队并置 Blocked
//!    (`BlockReason::PiMutexWait`), 释放状态锁后 `process_block_finish` 切换;
//!    移交由释放方 `Scheduler::unblock` 投送, 不再依赖等待者轮询.
//! 5. **捐赠接入 EEVDF 权重链**: 等待者按 `eevdf_weight` 参与 max 计算, 持有者
//!    权重经 `Scheduler::set_nice` 落地, 释放/降级时恢复基础权重.
//!
//! ## 安全契约
//!
//! - 锁序: mutex 状态锁 → 退出登记表; mutex 状态锁 → `PROCESS_TABLE`
//!   (`process_block_prepare` 在状态锁内调用, 与 futex A-11 同型).
//! - 状态锁释放后才调用 `Scheduler::set_nice` / `Scheduler::unblock` —— 它们会取
//!   运行队列锁与进程表锁, 不得压在关中断的自旋锁里.
//! - PI Mutex 可在系统调用上下文睡眠, 不可在硬中断上下文使用 (lockdep 以
//!   `LockKind::PiMutex` 的 `may_sleep` 标记执法).
//! - 锁对象一旦被持有即不得移动: 退出登记按实例地址记账, 加锁后按值移动会让
//!   登记指向旧地址 (生产用法为 static 或堆上固定地址).
//! - 内核上下文 (`PID_NONE`) 的获取允许接管尚未被目标接收的定向移交
//!   (`holder == handoff`): 此时临界区事实结束, 且 pid 0 既不能睡眠也永远不会
//!   成为移交目标; 拒绝接管会让退出路径的 `futex_wake` 永久自旋 (A-7 冻结归因,
//!   见 `try_acquire`).
//!
//! ## SIMPLIFIED
//!
//! - 不做 A→B→C 跨锁链式捐赠 (v2.2 的 chain 字段已删除): 单层捐赠足以覆盖
//!   futex 桶锁这类短临界区; 出现跨锁等待链时再引入链式传播.
//! - 等待者权重取入队时的 `eevdf_weight` 快照, `nice` 变化需调用方显式
//!   `update_waiter_priority` 刷新 (与 v2.1 调用方约束一致).
//! - 权重经 `weight_to_nice` 取最近 nice 桶落地, 非桶值会被取整.
//! - 重调度请求只投本核 (`set_need_reschedule`), 不跨核 IPI.
//! - 等待登记 / 持有登记为固定容量 (16 等待者 / 128 登记), 满时 `lock()` 退化为
//!   yield 轮询 (`EnqueueOutcome::Fallback`), 不分配堆内存.
//! - 同一线程重复 `lock()` 不受支持 (与 PTHREAD_PRIO_INHERIT 默认行为一致),
//!   自持重入退化为 yield 轮询.
//!
//! ## 评估日期
//!
//! 2026-06-08 初始化; 2026-06-29 v2.1 等待者动态重算; 2026-08-23 v2.5 鲁棒退出;
//! 2026-11-07 v3 原地重设计 (A-7). 关联 DECISION-009/010/011/012/049.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::privileged::sync::IrqSpinLock;
#[cfg(any(debug_assertions, feature = "lockdep"))]
use crate::privileged::sync::{LockClassDesc, LockClassId, LockKind};

/// PID 0 = 无效/空闲 (与 `Process::None` 约定), 同时是"无定向移交"哨兵.
pub const PID_NONE: u32 = 0;

/// 单把锁的等待队列容量 (固定数组, 无堆分配).
const PI_MAX_WAITERS: usize = 16;

/// 退出登记表单表容量: (锁地址, pid, 分派) 条目数.
///
/// 只有阻塞在状态锁上或持有 PI Mutex 的 (线程, 锁) 对才占条目; futex 桶锁的
/// 临界区极短, 并发占用远小于该容量. 满时 `lock()` 退化为 yield 轮询.
const PI_TRACK_CAPACITY: usize = 128;

// ============================================================================
// 退出登记表 — 进程退出时撤销等待 / 强制释放持有
// ============================================================================

/// 退出清理的类型擦除分派: `(mutex 地址, pid) -> 是否处理`.
///
/// 非泛型: 经 `PiMutex` 的固定布局前缀访问状态锁, 无需运行时类型信息.
///
/// # Safety
/// `mutex 地址` 必须是 [`track_record`] 登记时写入的 `PiMutex<T>` 活对象地址;
/// `T` 不影响 `data` 之前的前缀字段, 分派实现只经该前缀访问状态锁, 不读 `data`.
type ExitDispatch = unsafe fn(usize, u32) -> bool;

/// 一条登记: 锁地址 + 属主 pid + 退出分派.
#[derive(Clone, Copy)]
struct LockRecord {
    ptr: usize,
    pid: u32,
    dispatch: ExitDispatch,
}

/// 空槽 (ptr == 0 表示未占用).
const EMPTY_RECORD: LockRecord = LockRecord {
    ptr: 0,
    pid: PID_NONE,
    dispatch: pi_mutex_exit_dispatch_hold,
};

/// 持有登记表: 进程退出时据此强制释放它仍持有的 PI Mutex.
static PI_MUTEX_HELD: IrqSpinLock<[LockRecord; PI_TRACK_CAPACITY]> =
    IrqSpinLock::new([EMPTY_RECORD; PI_TRACK_CAPACITY]);

/// 等待登记表: 进程退出时据此把它从各锁的等待队列里撤销.
///
/// 撤销必须在强制释放之前完成: 残留的等待者在持有者退出后既等不到移交, 又
/// 已经不再运行, 会把锁永久留在一个已退出 PID 名下.
static PI_MUTEX_WAITING: IrqSpinLock<[LockRecord; PI_TRACK_CAPACITY]> =
    IrqSpinLock::new([EMPTY_RECORD; PI_TRACK_CAPACITY]);

/// 类型擦除分派: 强制释放 `pid` 持有的锁.
///
/// `PiMutex<T>` 的 `state` / `protocol` / `ceiling` (debug 或 feature="lockdep"
/// 下含 `lockdep_class`) 全在 `data` 之前, 布局与 `T` 无关; 以 `PiMutex<u8>`
/// 重建引用只触碰前缀字段, 不读取 `data`, 对任意 `T` (含 ?Sized) 兼容.
///
/// # Safety
/// `ptr` 必须是 [`track_hold`] 登记时写入的 `PiMutex<T>` 地址. 生命周期由登记
/// 表保证: 仅持锁期间登记 (对象存活), 析构时 [`untrack_all`] 已摘除条目.
unsafe fn pi_mutex_exit_dispatch_hold(ptr: usize, pid: u32) -> bool {
    // SAFETY: 见函数级 # Safety; `force_unlock_for_exit` 不触碰 `data`.
    let mutex = unsafe { &*(ptr as *const PiMutex<u8>) };
    mutex.force_unlock_for_exit(pid)
}

/// 类型擦除分派: 把 `pid` 从锁的等待队列里撤销.
///
/// # Safety
/// 同 [`pi_mutex_exit_dispatch_hold`].
unsafe fn pi_mutex_exit_dispatch_wait(ptr: usize, pid: u32) -> bool {
    // SAFETY: 见 [`pi_mutex_exit_dispatch_hold`]; `cancel_wait_for_exit` 不触碰 `data`.
    let mutex = unsafe { &*(ptr as *const PiMutex<u8>) };
    mutex.cancel_wait_for_exit(pid)
}

/// 锁实例地址 (与登记键同源; ?Sized 先转瘦指针再转 usize).
fn lock_addr<T: ?Sized>(lock: &PiMutex<T>) -> usize {
    core::ptr::from_ref::<PiMutex<T>>(lock).cast::<u8>() as usize
}

/// 登记 (ptr, pid) 对; 已存在或成功占用空槽返回 true, 表满返回 false.
fn track_record(
    table: &IrqSpinLock<[LockRecord; PI_TRACK_CAPACITY]>,
    ptr: usize,
    pid: u32,
    dispatch: ExitDispatch,
) -> bool {
    let mut records = table.lock();
    if records.iter().any(|r| r.ptr == ptr && r.pid == pid) {
        return true;
    }
    if let Some(slot) = records.iter_mut().find(|r| r.ptr == 0) {
        *slot = LockRecord { ptr, pid, dispatch };
        return true;
    }
    false
}

/// 摘除 (ptr, pid) 登记; 无对应条目返回 false.
fn untrack_record(
    table: &IrqSpinLock<[LockRecord; PI_TRACK_CAPACITY]>,
    ptr: usize,
    pid: u32,
) -> bool {
    let mut records = table.lock();
    for record in records.iter_mut() {
        if record.ptr == ptr && record.pid == pid {
            *record = EMPTY_RECORD;
            return true;
        }
    }
    false
}

/// 取出该 pid 的一条登记 (不调用分派), 供退出清理逐条消费.
fn take_record_for(
    table: &IrqSpinLock<[LockRecord; PI_TRACK_CAPACITY]>,
    pid: u32,
) -> Option<LockRecord> {
    if pid == PID_NONE {
        return None;
    }
    let mut records = table.lock();
    let slot = records.iter_mut().find(|r| r.ptr != 0 && r.pid == pid)?;
    let record = *slot;
    *slot = EMPTY_RECORD;
    Some(record)
}

/// 登记持有关系 (加锁成功 / 移交时调用); pid 0 (内核上下文) 不登记.
fn track_hold<T: ?Sized>(lock: &PiMutex<T>, pid: u32) -> bool {
    if pid == PID_NONE {
        return true;
    }
    track_record(
        &PI_MUTEX_HELD,
        lock_addr(lock),
        pid,
        pi_mutex_exit_dispatch_hold,
    )
}

/// 摘除持有登记; 无对应条目返回 false.
fn untrack_hold<T: ?Sized>(lock: &PiMutex<T>, pid: u32) -> bool {
    untrack_record(&PI_MUTEX_HELD, lock_addr(lock), pid)
}

/// 登记等待关系 (入队成功时调用); pid 0 (内核上下文) 不登记.
fn track_wait<T: ?Sized>(lock: &PiMutex<T>, pid: u32) -> bool {
    if pid == PID_NONE {
        return true;
    }
    track_record(
        &PI_MUTEX_WAITING,
        lock_addr(lock),
        pid,
        pi_mutex_exit_dispatch_wait,
    )
}

/// 摘除等待登记; 无对应条目返回 false.
fn untrack_wait<T: ?Sized>(lock: &PiMutex<T>, pid: u32) -> bool {
    untrack_record(&PI_MUTEX_WAITING, lock_addr(lock), pid)
}

/// 摘除所有指向本实例的登记 (对象析构兜底, 防止登记表留下悬垂地址).
fn untrack_all<T: ?Sized>(lock: &PiMutex<T>) {
    let ptr = lock_addr(lock);
    for table in [&PI_MUTEX_HELD, &PI_MUTEX_WAITING] {
        let mut records = table.lock();
        for record in records.iter_mut() {
            if record.ptr == ptr {
                *record = EMPTY_RECORD;
            }
        }
    }
}

/// 进程/线程退出清理: 先撤销等待, 再强制释放仍持有的锁.
///
/// 由 `process_exit` 在线程退出时调用 (A-7 前移); 收割期的
/// `process_cleanup` 调用保留为兜底 —— 两边都幂等 (登记取出即空,
/// 释放后 holder 不再匹配).
pub fn pi_mutex_process_exit(pid: u32) {
    if pid == PID_NONE {
        return;
    }
    while let Some(record) = take_record_for(&PI_MUTEX_WAITING, pid) {
        // SAFETY: record.ptr 来自本表登记, 对象存活由登记生命周期保证
        // (析构时 `untrack_all` 摘除条目); 分派幂等.
        let _ = unsafe { (record.dispatch)(record.ptr, pid) };
    }
    while let Some(record) = take_record_for(&PI_MUTEX_HELD, pid) {
        // SAFETY: 同上; `force_unlock_for_exit` 只释放 holder == pid 的锁.
        let _ = unsafe { (record.dispatch)(record.ptr, pid) };
    }
}

/// 查询某 PID 当前的 PI 持有登记条数 (仅测试构建, K-05 登记生命周期回归锁).
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
#[must_use]
pub fn held_lock_count(pid: u32) -> usize {
    if pid == PID_NONE {
        return 0;
    }
    PI_MUTEX_HELD
        .lock()
        .iter()
        .filter(|r| r.ptr != 0 && r.pid == pid)
        .count()
}

/// 查询某 PID 当前的 PI 等待登记条数 (仅测试构建, 退出撤销等待的回归锁).
#[cfg(any(feature = "kernel_test", feature = "host-test"))]
#[must_use]
pub fn waiting_lock_count(pid: u32) -> usize {
    if pid == PID_NONE {
        return 0;
    }
    PI_MUTEX_WAITING
        .lock()
        .iter()
        .filter(|r| r.ptr != 0 && r.pid == pid)
        .count()
}

// ============================================================================
// 状态
// ============================================================================

/// 等待者条目: 入队时的 pid 与 EEVDF 权重快照.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PiWaiter {
    pid: u32,
    weight: u32,
}

impl PiWaiter {
    const EMPTY: Self = Self {
        pid: PID_NONE,
        weight: 0,
    };
}

/// 已落地的权重捐赠记录 (用于释放/改目标时恢复基础权重).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppliedBoost {
    pid: u32,
    base: u32,
    weight: u32,
}

/// `PiMutex` 的全部可变状态, 由一把 `IrqSpinLock` 守护.
struct PiMutexState {
    locked: bool,
    holder: u32,
    /// 持有者基础权重 (调用方传入的 `eevdf_weight`).
    holder_base_weight: u32,
    /// 当前有效权重 = `max(holder_base, 全部等待者权重)`.
    effective_weight: u32,
    /// 定向移交目标 (`PID_NONE` = 无). 释放方先在状态锁内落好新持有者, 再置位
    /// 该标记; 目标线程在 `try_lock` 里确认接收.
    handoff: u32,
    waiters: [PiWaiter; PI_MAX_WAITERS],
    waiter_count: u8,
    /// 已实际写入调度器的捐赠 (None = 无).
    boost: Option<AppliedBoost>,
    /// 状态版本: 权重落地后据此复核对账, 防止并发 `sync_weights` 的旧写覆盖新值.
    version: u64,
}

impl PiMutexState {
    const fn new() -> Self {
        Self {
            locked: false,
            holder: PID_NONE,
            holder_base_weight: 0,
            effective_weight: 0,
            handoff: PID_NONE,
            waiters: [PiWaiter::EMPTY; PI_MAX_WAITERS],
            waiter_count: 0,
            boost: None,
            version: 0,
        }
    }

    fn waiters(&self) -> &[PiWaiter] {
        &self.waiters[..usize::from(self.waiter_count)]
    }

    fn max_waiter_weight(&self) -> u32 {
        self.waiters().iter().map(|w| w.weight).max().unwrap_or(0)
    }

    fn recompute_effective(&mut self) {
        let base = if self.locked {
            self.holder_base_weight
        } else {
            0
        };
        self.effective_weight = base.max(self.max_waiter_weight());
    }

    fn find_waiter(&self, pid: u32) -> Option<usize> {
        self.waiters().iter().position(|w| w.pid == pid)
    }

    /// 入队或刷新等待者权重; 队列满 (且是新 pid) 返回 false.
    fn upsert_waiter(&mut self, pid: u32, weight: u32) -> bool {
        if let Some(idx) = self.find_waiter(pid) {
            self.waiters[idx].weight = weight;
            return true;
        }
        let idx = usize::from(self.waiter_count);
        if idx >= PI_MAX_WAITERS {
            return false;
        }
        self.waiters[idx] = PiWaiter { pid, weight };
        self.waiter_count += 1;
        true
    }

    fn remove_at(&mut self, idx: usize) {
        let count = usize::from(self.waiter_count);
        if idx + 1 < count {
            self.waiters.copy_within(idx + 1..count, idx);
        }
        self.waiter_count -= 1;
        self.waiters[usize::from(self.waiter_count)] = PiWaiter::EMPTY;
    }

    fn remove_waiter(&mut self, pid: u32) -> bool {
        let Some(idx) = self.find_waiter(pid) else {
            return false;
        };
        self.remove_at(idx);
        true
    }

    /// 取走权重最大的等待者 (同权重取先入队者).
    fn take_next(&mut self) -> Option<PiWaiter> {
        let mut best: Option<(usize, u32)> = None;
        for (idx, waiter) in self.waiters().iter().enumerate() {
            if best.is_none_or(|(_, weight)| waiter.weight > weight) {
                best = Some((idx, waiter.weight));
            }
        }
        let (idx, _) = best?;
        let next = self.waiters[idx];
        self.remove_at(idx);
        Some(next)
    }

    /// 当前应落地的捐赠: 仅在持锁、且有效权重高于持有者基础权重时产生.
    fn desired_boost(&self) -> Option<AppliedBoost> {
        if !self.locked
            || self.holder == PID_NONE
            || self.effective_weight <= self.holder_base_weight
        {
            return None;
        }
        Some(AppliedBoost {
            pid: self.holder,
            base: self.holder_base_weight,
            weight: self.effective_weight,
        })
    }

    fn bump_version(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
}

// ============================================================================
// PiMutex 公开类型
// ============================================================================

/// v2.4: 互斥锁协议 — PI (继承) 或 PCP (天花板)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PiMutexProtocol {
    /// 标准优先级继承 (默认)
    Pi = 0,
    /// 优先级天花板协议 (PCP): 所有者升到 ceiling 优先级
    Pcp = 1,
}

/// 优先级继承互斥锁
///
/// 权重口径: `lock` / `try_lock` / `update_waiter_priority` 接收的是 EEVDF
/// `eevdf_weight` (nice 0 = 1024), 不是 legacy priority 等级.
#[repr(C)]
pub struct PiMutex<T: ?Sized> {
    /// 全部可变状态 (单临界区)
    state: IrqSpinLock<PiMutexState>,
    /// v2.4: 互斥锁协议 (PI 或 PCP)
    protocol: PiMutexProtocol,
    /// v2.4: 优先级天花板 (仅 Pcp 模式使用)
    ceiling: AtomicU32,
    /// Lockdep 锁类 ID (debug 或 feature="lockdep" 下使用)
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    lockdep_class: LockClassId,
    /// 被保护的数据 (必须为最后一项, 以支持 ?Sized)
    data: UnsafeCell<T>,
}

// SAFETY: PiMutex 通过内部状态锁提供互斥, T: Send 即可跨线程传递所有权
unsafe impl<T: ?Sized + Send> Send for PiMutex<T> {}
// SAFETY: 共享引用跨线程安全, 临界区由内部 IrqSpinLock 串行化
unsafe impl<T: ?Sized + Send> Sync for PiMutex<T> {}

/// RAII 守卫, drop 时自动释放锁
pub struct PiMutexGuard<'a, T: ?Sized> {
    data: &'a mut T,
    mutex: &'a PiMutex<T>,
    /// 取得锁的 pid (释放按取得者身份核对; 不再依赖"当前线程"桥接)
    pid: u32,
}

// SAFETY: MutexGuard 持有时即为独占访问, T: Sync 由 Mutex Sync 提供
unsafe impl<T: ?Sized + Sync> Sync for PiMutexGuard<'_, T> {}

// ============================================================================
// 构造
// ============================================================================

impl<T> PiMutex<T> {
    /// 创建新的 PI Mutex
    pub const fn new(data: T) -> Self {
        Self {
            state: IrqSpinLock::new(PiMutexState::new()),
            protocol: PiMutexProtocol::Pi,
            ceiling: AtomicU32::new(0),
            data: UnsafeCell::new(data),
            #[cfg(any(debug_assertions, feature = "lockdep"))]
            lockdep_class: LockClassId::INVALID,
        }
    }

    /// 创建命名 `PiMutex` (用于调试 + lockdep)
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    pub fn named(name: &'static str, data: T) -> Self {
        let class_id = crate::privileged::sync::register_class(LockClassDesc {
            name,
            kind: LockKind::PiMutex,
        });
        Self {
            state: IrqSpinLock::new(PiMutexState::new()),
            protocol: PiMutexProtocol::Pi,
            ceiling: AtomicU32::new(0),
            data: UnsafeCell::new(data),
            lockdep_class: class_id,
        }
    }

    /// 创建命名 `PiMutex` (其余构建: 忽略名称)
    #[cfg(not(any(debug_assertions, feature = "lockdep")))]
    pub fn named(_name: &'static str, data: T) -> Self {
        Self::new(data)
    }

    /// 设置优先级天花板 (PCP 协议)
    pub fn set_ceiling(&self, ceiling: u32) {
        self.ceiling.store(ceiling, Ordering::Release);
    }

    /// 获取当前优先级天花板
    #[must_use]
    pub fn get_ceiling(&self) -> u32 {
        self.ceiling.load(Ordering::Acquire)
    }

    /// 获取互斥锁协议
    #[must_use]
    pub fn get_protocol(&self) -> PiMutexProtocol {
        self.protocol
    }
}

impl<T: Default> Default for PiMutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// ============================================================================
// 锁操作
// ============================================================================

/// `lock()` 单轮入队结果
enum EnqueueOutcome {
    /// 已置 Blocked 并登记为等待者, 调用方可以切换
    Blocked,
    /// 状态已变化 (锁已空闲), 直接重试获取
    Retry,
    /// 无法入队 (pid 0 / 自持重入 / 队列或登记表满 / 非当前线程), 退化为 yield
    Fallback,
}

impl<T: ?Sized> PiMutex<T> {
    /// lockdep 实例键: 本锁基址.
    ///
    /// `PiMutex<T: ?Sized>` 是宽指针, 须先转瘦指针 (`*const u8`) 再转 `usize`,
    /// 与 [`lock_addr`] 的登记键同源.
    #[cfg(any(debug_assertions, feature = "lockdep"))]
    fn lockdep_instance(&self) -> usize {
        core::ptr::from_ref::<Self>(self).cast::<u8>() as usize
    }

    #[cfg(any(debug_assertions, feature = "lockdep"))]
    fn lockdep_acquire(&self) {
        crate::privileged::sync::acquire(
            self.lockdep_class,
            self.lockdep_instance(),
            crate::privileged::sync::in_irq_context(),
        );
    }

    #[cfg(any(debug_assertions, feature = "lockdep"))]
    fn lockdep_release(&self) {
        crate::privileged::sync::release(self.lockdep_class, self.lockdep_instance());
    }

    /// 获取锁 (阻塞 + 优先级继承)
    ///
    /// # 参数
    /// - `my_pid`: 当前线程 PID
    /// - `my_base_weight`: 当前线程的基础 EEVDF 权重 (`eevdf_weight`)
    ///
    /// # 行为
    /// 1. 尝试立即获取 (含接收定向移交)
    /// 2. 失败时在状态锁内入队 + 置 Blocked, 释放状态锁后捐赠并切换
    /// 3. 被唤醒后重试; 释放方已把所有权定向移交时直接接收
    pub fn lock(&self, my_pid: u32, my_base_weight: u32) -> PiMutexGuard<'_, T> {
        loop {
            if self.try_acquire(my_pid, my_base_weight) {
                // SAFETY: 已取得所有权, 持有期间独占访问 data
                return PiMutexGuard {
                    data: unsafe { &mut *self.data.get() },
                    mutex: self,
                    pid: my_pid,
                };
            }
            match self.enqueue_and_block(my_pid, my_base_weight) {
                EnqueueOutcome::Blocked => {
                    // 阻塞态已置位: 切换后醒来重试 (移交由释放方投送)
                    crate::privileged::proc::process_block_finish(my_pid);
                }
                EnqueueOutcome::Retry | EnqueueOutcome::Fallback => scheduler_yield(),
            }
        }
    }

    /// 尝试获取锁 (非阻塞, **失败零副作用**)
    ///
    /// 成功返回 true 并置持有者; 失败返回 false, 不改等待队列、不捐赠、不登记.
    #[must_use]
    pub fn try_lock(&self, my_pid: u32, my_base_weight: u32) -> bool {
        self.try_acquire(my_pid, my_base_weight)
    }

    /// 尝试获取锁并返回 RAII 守卫 (非阻塞, 失败零副作用)
    #[must_use]
    pub fn try_lock_guard(&self, my_pid: u32, my_base_weight: u32) -> Option<PiMutexGuard<'_, T>> {
        if self.try_acquire(my_pid, my_base_weight) {
            // SAFETY: 已取得所有权, 持有期间独占访问 data
            Some(PiMutexGuard {
                data: unsafe { &mut *self.data.get() },
                mutex: self,
                pid: my_pid,
            })
        } else {
            None
        }
    }

    /// 单次获取: 接收定向移交或抢占空闲锁. 失败不改任何等待/捐赠状态.
    ///
    /// 内核上下文 (`PID_NONE`) 额外允许**接管未接收的定向移交**: 移交目标还没
    /// 上核确认 (`holder == handoff`) 时临界区事实上已经结束, 而 pid 0 既不能
    /// 睡眠等待、也永远无法成为移交目标 —— 不给接管口, 退出路径的 `futex_wake`
    /// 就会在"锁已交出、目标还排在唤醒者自己的运行队列里"的窗口永久自旋
    /// (A-7 冻结现场: 目标 Ready 在等 CPU, 占着 CPU 的正是唤醒者).
    fn try_acquire(&self, my_pid: u32, my_base_weight: u32) -> bool {
        let mut took_over = false;
        {
            let mut state = self.state.lock();
            if state.locked {
                if my_pid == PID_NONE {
                    if state.handoff == PID_NONE || state.holder != state.handoff {
                        return false;
                    }
                    let previous = state.holder;
                    state.handoff = PID_NONE;
                    state.holder = PID_NONE;
                    state.holder_base_weight = 0;
                    state.effective_weight = state.max_waiter_weight();
                    state.bump_version();
                    // 移交目标不再持有: 摘除其持有登记, 否则退出清理会向一把它
                    // 从未进入的锁分派强制释放 (分派幂等, 但白占一个登记槽).
                    let _ = untrack_hold(self, previous);
                    took_over = true;
                } else if state.handoff != my_pid || state.holder != my_pid {
                    // 只有被释放方选中的移交目标可以"无锁空窗"地接手.
                    return false;
                } else {
                    state.handoff = PID_NONE;
                    state.bump_version();
                }
            } else {
                state.locked = true;
                state.holder = my_pid;
                state.handoff = PID_NONE;
                state.holder_base_weight = my_base_weight;
                state.effective_weight = my_base_weight.max(state.max_waiter_weight());
                state.bump_version();
                // 登记在状态锁内完成: 与退出清理的"先撤销等待、再释放持有"配对,
                // 避免移交目标在登记落账前退出而留下无人释放的持有.
                let _ = track_hold(self, my_pid);
            }
        }
        if took_over {
            // 接管后持有者基础权重归零, 撤销可能已落在原移交目标上的捐赠.
            self.sync_weights();
        }
        #[cfg(any(debug_assertions, feature = "lockdep"))]
        self.lockdep_acquire();
        true
    }

    /// 入队 + 置 Blocked. 状态锁内完成, 关闭"入队与阻塞之间被唤醒"的丢唤醒窗口.
    fn enqueue_and_block(&self, my_pid: u32, my_base_weight: u32) -> EnqueueOutcome {
        if my_pid == PID_NONE {
            // 内核上下文无法睡眠, 退化为让出 CPU 轮询.
            return EnqueueOutcome::Fallback;
        }
        let mut mutated = false;
        let outcome = {
            let mut state = self.state.lock();
            if !state.locked {
                EnqueueOutcome::Retry
            } else if state.holder == my_pid || !state.upsert_waiter(my_pid, my_base_weight) {
                // 自持重入或队列满: 不入队 (不制造自己的等待条目).
                EnqueueOutcome::Fallback
            } else {
                mutated = true;
                state.recompute_effective();
                state.bump_version();
                if !track_wait(self, my_pid)
                    || !crate::privileged::proc::process_block_prepare(
                        my_pid,
                        crate::privileged::proc::BlockReason::PiMutexWait,
                        0,
                    )
                {
                    // 登记表满 / 非当前线程: 回滚等待条目, 本线程继续轮询.
                    state.remove_waiter(my_pid);
                    state.recompute_effective();
                    state.bump_version();
                    let _ = untrack_wait(self, my_pid);
                    EnqueueOutcome::Fallback
                } else {
                    EnqueueOutcome::Blocked
                }
            }
        };
        if mutated {
            // 状态锁外落地捐赠: set_nice 会取运行队列锁与进程表锁.
            self.sync_weights();
        }
        outcome
    }

    /// 动态更新等待者权重 (nice/setpriority 变化时由调用方触发)
    ///
    /// 若 `pid` 不是本锁等待者 (或权重未变) 返回 false, 不产生任何副作用.
    pub fn update_waiter_priority(&self, pid: u32, new_base_weight: u32) -> bool {
        let changed = {
            let mut state = self.state.lock();
            let Some(idx) = state.find_waiter(pid) else {
                return false;
            };
            if state.waiters[idx].weight == new_base_weight {
                return false;
            }
            state.waiters[idx].weight = new_base_weight;
            state.recompute_effective();
            state.bump_version();
            true
        };
        self.sync_weights();
        changed
    }

    /// 把状态里的捐赠目标与实参落地到调度器, 直到物理权重与最新状态一致.
    ///
    /// 版本复核: 落地期间状态若再变 (其它线程入队/释放), 重新读一次当前期望
    /// 并强制重落, 保证最后一次写不是过期值. 状态锁不跨 `set_nice` 持有.
    fn sync_weights(&self) {
        let mut forced = false;
        loop {
            let (version, restore, boost) = {
                let mut state = self.state.lock();
                let desired = state.desired_boost();
                if !forced && state.boost == desired {
                    return;
                }
                let previous = state.boost;
                state.boost = desired;
                state.bump_version();
                let restore = previous.filter(|prev| desired.is_none_or(|d| d.pid != prev.pid));
                (state.version, restore, desired)
            };
            if let Some(prev) = restore {
                set_task_weight(prev.pid, prev.base);
            }
            if let Some(boost) = boost {
                set_task_weight(boost.pid, boost.weight);
            }
            if self.state.lock().version == version {
                return;
            }
            forced = true;
        }
    }

    /// 释放锁 (由 `PiMutexGuard::drop` 自动调用)
    pub(crate) fn unlock_internal(&self, pid: u32) {
        let owned = {
            let state = self.state.lock();
            state.locked && state.holder == pid
        };
        if owned {
            #[cfg(any(debug_assertions, feature = "lockdep"))]
            self.lockdep_release();
        }
        let _ = self.release_holder(pid);
    }

    /// 强制释放 (跳过调用方身份核对, 仅供测试驱动状态机使用)
    ///
    /// 生产路径 (`PiMutexGuard::drop` / 退出清理) 不走这里: guard 携带取得者
    /// pid, 退出清理经类型擦除分派核对 holder.
    #[cfg(any(feature = "kernel_test", feature = "host-test"))]
    pub(crate) fn force_unlock(&self) {
        let holder = {
            let state = self.state.lock();
            if !state.locked {
                return;
            }
            state.holder
        };
        #[cfg(any(debug_assertions, feature = "lockdep"))]
        self.lockdep_release();
        let _ = self.release_holder(holder);
    }

    /// 进程退出时强制释放 (仅 holder == pid 时生效, 幂等)
    fn force_unlock_for_exit(&self, pid: u32) -> bool {
        self.release_holder(pid)
    }

    /// 进程退出时撤销等待: 出队 + 重算有效权重 + 摘除等待登记.
    fn cancel_wait_for_exit(&self, pid: u32) -> bool {
        let removed = {
            let mut state = self.state.lock();
            if state.remove_waiter(pid) {
                state.recompute_effective();
                state.bump_version();
                let _ = untrack_wait(self, pid);
                true
            } else {
                false
            }
        };
        if removed {
            self.sync_weights();
        }
        removed
    }

    /// 释放所有权的唯一实现: 定向移交给最重等待者, 无等待者则完全释放.
    fn release_holder(&self, pid: u32) -> bool {
        let next_pid = {
            let mut state = self.state.lock();
            if !state.locked || state.holder != pid {
                return false;
            }
            state.handoff = PID_NONE;
            state.bump_version();
            if let Some(next) = state.take_next() {
                // 定向移交: locked 始终为 true, 新持有者与权重在同一临界区落账.
                state.holder = next.pid;
                state.holder_base_weight = next.weight;
                state.recompute_effective();
                state.handoff = next.pid;
                let _ = untrack_hold(self, pid);
                let _ = track_hold(self, next.pid);
                let _ = untrack_wait(self, next.pid);
                next.pid
            } else {
                state.locked = false;
                state.holder = PID_NONE;
                state.holder_base_weight = 0;
                state.effective_weight = 0;
                let _ = untrack_hold(self, pid);
                PID_NONE
            }
        };
        // 状态锁外撤销自己/前持有者的捐赠, 再投送唤醒 (顺序不可倒: 新持有者必须
        // 带着最终权重入队, 否则第一轮 CPU 预算按旧权重记账).
        self.sync_weights();
        if next_pid != PID_NONE {
            crate::privileged::proc::scheduler::Scheduler::unblock(next_pid);
        }
        true
    }

    /// 查询当前是否被持有
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.state.lock().locked
    }

    /// 查询当前持有者 PID (0 = 未持有)
    #[must_use]
    pub fn holder(&self) -> u32 {
        self.state.lock().holder
    }

    /// 查询当前有效权重 (EEVDF `eevdf_weight` 口径)
    #[must_use]
    pub fn effective_priority(&self) -> u32 {
        self.state.lock().effective_weight
    }

    /// 查询当前等待者数量
    #[must_use]
    pub fn waiter_count(&self) -> usize {
        usize::from(self.state.lock().waiter_count)
    }

    /// 测试专用: 直接把 `pid` 注册为等待者 (不阻塞), 用于覆盖等待队列状态机.
    ///
    /// 生产路径由 `lock()` 的 `enqueue_and_block` 完成同样的入队 + 登记.
    #[cfg(any(feature = "kernel_test", feature = "host-test"))]
    pub fn register_waiter_for_test(&self, pid: u32, weight: u32) -> bool {
        if pid == PID_NONE {
            return false;
        }
        let registered = {
            let mut state = self.state.lock();
            if !state.locked || state.holder == pid || !state.upsert_waiter(pid, weight) {
                false
            } else {
                state.recompute_effective();
                state.bump_version();
                track_wait(self, pid)
            }
        };
        if registered {
            self.sync_weights();
        }
        registered
    }
}

/// 单 pid 权重落地: 权重 → 最近 nice 桶 → `Scheduler::set_nice` → 请求重调度.
fn set_task_weight(pid: u32, weight: u32) {
    if pid == PID_NONE || weight == 0 {
        return;
    }
    let nice = crate::privileged::proc::eevdf::weight_to_nice(u64::from(weight));
    if crate::privileged::proc::scheduler::Scheduler::set_nice(pid, nice) {
        crate::privileged::proc::scheduler::Scheduler::set_need_reschedule();
    }
}

impl<T: ?Sized> Drop for PiMutex<T> {
    /// 对象析构时摘除所有指向本实例的退出登记.
    ///
    /// 登记键是实例地址; 若不摘除, 地址被复用后新锁会继承陈旧条目, 进程退出
    /// 分派可能按陈旧地址强制解锁 (悬垂). 正常释放/移交已摘除条目, 此处只兜底
    /// 异常路径 (如持锁实例未解锁即被释放).
    fn drop(&mut self) {
        untrack_all(self);
    }
}

// ============================================================================
// PiMutexGuard
// ============================================================================

impl<T: ?Sized> Drop for PiMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.unlock_internal(self.pid);
    }
}

impl<T: ?Sized> core::ops::Deref for PiMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data
    }
}

impl<T: ?Sized> core::ops::DerefMut for PiMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

// ============================================================================
// 辅助
// ============================================================================

/// 让出 CPU (v2.6: 线程级 `SchedulerEx` 已移除, 统一走进程级调度器).
fn scheduler_yield() {
    crate::privileged::proc::scheduler::Scheduler::yield_current();
}
