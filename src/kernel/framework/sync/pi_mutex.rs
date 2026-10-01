//! Priority Inheritance Mutex (PI Mutex) — TCB 实现
//!
//! ## 协议
//!
//! 当高优先级线程 P_H 因等待低优先级线程 P_L 持有的 PI mutex 而阻塞时, 把
//! P_L 的**有效优先级**临时提升至 max(P_L.base, P_H.base)。P_L 释放 mutex 后
//! 恢复基础优先级。
//!
//! ## 与普通 Mutex 的差异
//!
//! | 维度 | Mutex | PiMutex |
//! |------|-------|---------|
//! | 优先级继承 | ❌ | ✅ |
//! | 等待者优先级记录 | ❌ | ✅ VecDeque<(PID, base_prio)> |
//! | 解锁时唤醒策略 | 任意一个 | 最高优先级 + FIFO |
//! | 重复 lock | ✅ 递归 | ❌ 失败 (与 PTHREAD_PRIO_INHERIT 默认行为一致) |
//!
//! ## v1 简化
//!
//! - 直接捐赠, 不处理 A→B→C 链式
//! - 自旋 + yield 等待, 不入调度等待队列
//! - 不直接修改 Process.priority, 通过回调通知
//!
//! ## v2.1 扩展 (2026-06-29)
//!
//! - 等待者优先级动态重算: nice/setpriority 变化时通过 `PiMutex::update_waiter_priority`
//!   更新等待者基线, 自动重算 effective_priority 并触发捐赠/撤销通知
//! - `recompute_effective` 提取为私有助手, 统一 register/unlock/update 3 路径
//!
//! ## 安全契约
//!
//! - 全局状态由 `IrqSpinLock` 守护, 持锁期间屏蔽中断
//! - 所有公开函数接受 `&self` / `&mut self` 借用, 不暴露 `static mut`
//! - 回调函数 `DonationCallback` 由 services 层注入, 通过原子指针替换
//!
//! ## 评估日期
//!
//! 2026-06-08 初始化, 2026-06-29 扩展 v2.1
//! 关联 DECISION-009/010/011/012 (DECISION-012: 等待者动态重算)

use alloc::collections::{BTreeMap, VecDeque};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, Ordering};

use crate::framework::sync::IrqSpinLock;
#[cfg(debug_assertions)]
use crate::framework::sync::{LockClassDesc, LockClassId, LockKind};

// ============================================================================
// v2.5: 鲁棒 mutex — 进程退出时强制释放所有 PI Mutex (DECISION-049 方案 D)
// 返工 (2026-08-23 审查): 原 `&'static dyn ProcessMutexHandle` 注册表方案
// 不适配运行时创建的 PiMutex (非 'static 实例无法注册, 注册表恒空 = no-op)。
// 最终方案: "持有登记表 + 类型擦除函数指针"。
// - lock 成功时 (try_lock 快路径) 与移交路径 (do_unlock 转交 next_pid) 登记
//   (mutex 地址, 非泛型退出分派). 分派经固定布局前缀访问, 对任意 T (含 ?Sized) 兼容.
// - 正常 unlock 不摘除 (残留条目由进程退出时整体 remove 清理; process_exit
//   幂等, 已释放锁为 no-op, 仅少量内存残留, 可接受)
// - 进程退出时按 pid 取出列表逐个强制解锁
// ============================================================================

/// 进程退出时强制解锁的类型擦除分派: `(mutex 地址, pid) -> 是否释放`。
/// 非泛型: 经固定布局前缀访问 `data` 之前的字段, 无需运行时类型信息。
///
/// # Safety
/// 该函数指针必须来自 `track_hold` 登记 (与 `HeldLock.ptr` 配对),
/// `ptr` 必须是指向存活 `PiMutex<T>` 的地址 (生命周期由持有登记表保证).
type ExitDispatch = unsafe fn(usize, u32) -> bool;

/// 一条持有记录: 互斥锁地址 + 退出分派函数。
struct HeldLock {
    ptr: usize,
    dispatch: ExitDispatch,
}

/// 持有登记表: pid → 该进程当前/曾经持有的 PI Mutex 列表。
/// - lock 成功时登记; 进程退出时按 pid 取出并逐个强制解锁, 然后整体移除。
/// - 锁序: 走 IrqSpinLock (屏蔽中断), 不嵌套其他锁。
static PI_MUTEX_HELD: IrqSpinLock<BTreeMap<u32, alloc::vec::Vec<HeldLock>>> =
    IrqSpinLock::new(BTreeMap::new());

/// 类型擦除分派: 经固定布局前缀访问, 强制解锁持锁进程的 PI Mutex。
///
/// `PiMutex<T>` 中 `data` 之前的字段 (`inner`/`holder_base_priority`/`protocol`/
/// `ceiling`, debug 下含 `lockdep_class`) 布局与 `T` 无关, 因此以 `PiMutex<u8>`
/// 重建引用只触碰前缀字段, 不读取 `data`, 对任意 `T` 布局兼容。
///
/// # Safety
/// `ptr` 必须是登记时写入的 PiMutex<T> 地址。生命周期由持有登记表保证:
/// 仅持锁期间登记 (对象存活); 进程退出时对象仍存活 (持锁进程).
unsafe fn pi_mutex_exit_dispatch(ptr: usize, pid: u32) -> bool {
    // SAFETY: 见函数级说明; `force_unlock_for_exit` 不触碰 `data` 字段.
    let m = unsafe { &*(ptr as *const PiMutex<u8>) };
    m.force_unlock_for_exit(pid)
}

/// 登记持有关系 (lock 成功时调用): 记录 (自身地址, 类型擦除 dispatch) 到当前 pid。
fn track_hold<T: ?Sized>(lock: &PiMutex<T>, pid: u32) {
    #[expect(
        clippy::ref_as_ptr,
        reason = "ref_as_ptr: ?Sized 宽指针需先转瘦指针取数据地址再转 usize 作登记键, 语义已知安全"
    )]
    let entry = HeldLock {
        // ?Sized 时为宽指针, 先转瘦指针取数据地址再转 usize
        ptr: lock as *const PiMutex<T> as *const u8 as usize,
        dispatch: pi_mutex_exit_dispatch,
    };
    PI_MUTEX_HELD.lock().entry(pid).or_default().push(entry);
}

/// 进程退出回调: 遍历该 pid 登记的 PI Mutex, 强制解锁 (持有者已退出)。
/// 由 process 层在进程退出路径调用 (见 `process_cleanup.rs` 接线)。
pub fn pi_mutex_process_exit(pid: u32) {
    let held = PI_MUTEX_HELD.lock().remove(&pid);
    if let Some(list) = held {
        for h in list {
            // SAFETY: h.ptr 在登记时有效且对象存活 (持锁期间保证);
            // process_exit 幂等 (释放后 holder 变 PID_NONE, 重复调用 no-op).
            unsafe { (h.dispatch)(h.ptr, pid) };
        }
    }
}

// ============================================================================
// 决策记录
// ============================================================================

// 预留常量, 待 PiMutex 等待队列改为预分配后启用。
// const 等待队列初始容量: usize = 8;

/// PID 0 = 无效/空闲 (与 `Process::None` 约定)
pub const PID_NONE: u32 = 0;

// ============================================================================
// 回调函数
// ============================================================================

/// 捐赠通知回调签名: `(holder_pid, donated_priority)`
pub type DonationCallback = fn(holder_pid: u32, donated_priority: u32);

/// 全局捐赠通知回调 (原子指针, 启动期单线程安装, 运行时只读)
static NOTIFY_DONATION: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// 全局撤销通知回调
static NOTIFY_REVOKE: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// 安装捐赠通知回调
///
/// # Safety
///
/// - 启动期单线程调用一次
/// - `cb` 必须为 `'static` 函数指针, 不可在运行时被释放
pub unsafe fn set_donation_callback(cb: DonationCallback) {
    NOTIFY_DONATION.store(cb as *mut (), Ordering::Release);
}

/// 安装撤销通知回调
///
/// # Safety
///
/// 同 [`set_donation_callback`]
pub unsafe fn set_revoke_callback(cb: DonationCallback) {
    NOTIFY_REVOKE.store(cb as *mut (), Ordering::Release);
}

#[inline]
fn notify_donation(holder_pid: u32, donated_prio: u32) {
    // v2.3: 直接修改 Process.priority, 触发 CFS 重排
    let table = &crate::framework::proc::PROCESS_TABLE;
    if let Some(proc_ptr) = table.get(holder_pid) {
        // SAFETY: proc_ptr 来自 PROCESS_TABLE, 有效指针; 单线程保证
        let proc = unsafe { &*proc_ptr };
        proc.priority.store(donated_prio, Ordering::SeqCst);
    }
    // 通知调度器有优先级变化
    crate::framework::proc::scheduler_ex::SCHEDULER_EX.yield_current();
}

#[inline]
fn notify_revoke(pid: u32) {
    // v2.3: 恢复 Process.priority 到 Normal (2)
    let table = &crate::framework::proc::PROCESS_TABLE;
    if let Some(proc_ptr) = table.get(pid) {
        // SAFETY: proc_ptr 来自 PROCESS_TABLE, 有效指针; 单线程保证
        let proc = unsafe { &*proc_ptr };
        proc.priority.store(2, Ordering::SeqCst);
    }
    crate::framework::proc::scheduler_ex::SCHEDULER_EX.yield_current();
}

// ============================================================================
// 等待者条目
// ============================================================================

/// 等待者条目
#[derive(Debug, Clone, Copy)]
struct WaiterEntry {
    /// 等待者 PID
    pid: u32,
    /// 等待者入队时的 `base_priority` (用于取消时正确撤销捐赠)
    base_priority: u32,
}

// ============================================================================
// PiMutex 内部状态
// ============================================================================

/// `PiMutex` 内部状态
struct PiMutexInner {
    /// 是否被持有
    locked: AtomicBool,
    /// 当前持有者 PID (None = 未持有)
    holder: AtomicU32,
    /// 等待队列 (FIFO 顺序, 同优先级按 FIFO)
    waiters: IrqSpinLock<VecDeque<WaiterEntry>>,
    /// 当前有效优先级 (= `max(holder_prio`, `waiters_prio`))
    effective_priority: AtomicU32,
    /// v2.2: 链式捐赠追踪 (holder→donor→donor...)
    chain: UnsafeCell<[u32; 8]>,
    chain_len: AtomicU8,
}

impl PiMutexInner {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            holder: AtomicU32::new(PID_NONE),
            waiters: IrqSpinLock::new(VecDeque::new()),
            effective_priority: AtomicU32::new(0),
            chain: UnsafeCell::new([0; 8]),
            chain_len: AtomicU8::new(0),
        }
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
pub struct PiMutex<T: ?Sized> {
    inner: PiMutexInner,
    /// 初始持有者的 `base_priority` (用于解锁后通知撤销)
    holder_base_priority: AtomicU32,
    /// v2.4: 互斥锁协议 (PI 或 PCP)
    protocol: PiMutexProtocol,
    /// v2.4: 优先级天花板 (仅 Pcp 模式使用)
    ceiling: AtomicU32,
    /// Lockdep 锁类 ID (debug 模式下使用)
    #[cfg(debug_assertions)]
    lockdep_class: LockClassId,
    /// 被保护的数据 (必须为最后一项, 以支持 ?Sized)
    data: UnsafeCell<T>,
}

// SAFETY: PiMutex 通过内部锁提供互斥, T: Send 即可跨线程传递所有权
unsafe impl<T: ?Sized + Send> Send for PiMutex<T> {}
// SAFETY: 共享引用跨线程安全
unsafe impl<T: ?Sized + Send> Sync for PiMutex<T> {}

/// RAII 守卫, drop 时自动释放锁
pub struct PiMutexGuard<'a, T: ?Sized> {
    data: &'a mut T,
    mutex: &'a PiMutex<T>,
}

// SAFETY: MutexGuard 持有时即为独占访问, T: Sync 由 Mutex Sync 提供
unsafe impl<T: ?Sized + Sync> Sync for PiMutexGuard<'_, T> {}

// ============================================================================
// PiMutex 构造
// ============================================================================

impl<T> PiMutex<T> {
    /// 创建新的 PI Mutex
    pub const fn new(data: T) -> Self {
        Self {
            inner: PiMutexInner::new(),
            holder_base_priority: AtomicU32::new(0),
            protocol: PiMutexProtocol::Pi,
            ceiling: AtomicU32::new(0),
            data: UnsafeCell::new(data),
            #[cfg(debug_assertions)]
            lockdep_class: LockClassId::INVALID,
        }
    }

    /// 创建命名 `PiMutex` (用于调试 + lockdep)
    #[cfg(debug_assertions)]
    // J-01 (2026-09-08): 删除 unfulfilled doc_markdown expect — 文档已用反引号包裹
    // 标识符, doc_markdown 从不触发, expect 为多余防御 (host-test debug 编译暴露)
    pub fn named(name: &'static str, data: T) -> Self {
        let class_id = crate::framework::sync::register_class(LockClassDesc {
            name,
            kind: LockKind::PiMutex,
        });
        Self {
            inner: PiMutexInner::new(),
            holder_base_priority: AtomicU32::new(0),
            protocol: PiMutexProtocol::Pi,
            ceiling: AtomicU32::new(0),
            data: UnsafeCell::new(data),
            lockdep_class: class_id,
        }
    }

    /// 创建命名 PiMutex (release 模式: 忽略名称)
    #[cfg(not(debug_assertions))]
    pub fn named(_name: &'static str, data: T) -> Self {
        Self::new(data)
    }

    /// 设置优先级天花板 (PCP 协议)
    pub fn set_ceiling(&self, ceiling: u32) {
        self.ceiling.store(ceiling, Ordering::Release);
    }

    /// 获取当前优先级天花板
    pub fn get_ceiling(&self) -> u32 {
        self.ceiling.load(Ordering::Acquire)
    }

    /// 获取互斥锁协议
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
// PiMutex 锁操作
// ============================================================================

impl<T: ?Sized> PiMutex<T> {
    /// 获取锁 (阻塞 + 优先级继承)
    ///
    /// # 参数
    /// - `my_pid`: 当前线程 PID
    /// - `my_base_priority`: 当前线程的 `base_priority` (而非有效优先级)
    ///
    /// # 行为
    /// 1. 尝试立即获取
    /// 2. 失败时注册为等待者, 通知 holder 接受捐赠
    /// 3. 自旋 + yield 直到被唤醒 (v1 简化)
    pub fn lock(&self, my_pid: u32, my_base_priority: u32) -> PiMutexGuard<'_, T> {
        loop {
            if self.try_lock(my_pid, my_base_priority) {
                // SAFETY: 持锁后独占访问 data
                return PiMutexGuard {
                    data: unsafe { &mut *self.data.get() },
                    mutex: self,
                };
            }
            // 自旋 + yield 等待被唤醒
            scheduler_yield();
        }
    }

    /// 尝试获取锁 (非阻塞)
    ///
    /// 成功时返回 true, 失败时返回 false 并**已自动注册为等待者 + 触发捐赠**。
    pub fn try_lock(&self, my_pid: u32, my_base_priority: u32) -> bool {
        // fast path: 直接尝试
        if self
            .inner
            .locked
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.inner.holder.store(my_pid, Ordering::Release);
            self.inner
                .effective_priority
                .store(my_base_priority, Ordering::Release);
            self.holder_base_priority
                .store(my_base_priority, Ordering::Release);

            // B03-08 返工: 登记持有关系 (进程退出时据此强制解锁)
            track_hold(self, my_pid);

            // Lockdep: 通知锁获取
            #[cfg(debug_assertions)]
            crate::framework::sync::acquire(
                self.lockdep_class,
                crate::framework::sync::in_irq_context(),
            );

            return true;
        }

        // 失败: 慢路径 — 注册为等待者 + 捐赠
        self.register_waiter_and_donate(my_pid, my_base_priority);
        false
    }

    /// 注册为等待者并触发捐赠 (内部函数)
    fn register_waiter_and_donate(&self, my_pid: u32, my_base_priority: u32) {
        {
            let mut waiters = self.inner.waiters.lock();
            // 避免重复注册 (同一线程多次 lock)
            if waiters.iter().any(|w| w.pid == my_pid) {
                return;
            }
            waiters.push_back(WaiterEntry {
                pid: my_pid,
                base_priority: my_base_priority,
            });
        } // waiters 锁释放

        // v2.1: 统一走 recompute_and_notify 助手, 避免重复逻辑
        self.recompute_and_notify();
    }

    /// v2.1: 动态更新等待者优先级 (nice/setpriority 变化时由调用方触发)
    ///
    /// 行为:
    /// 1. 在 waiters 队列中找到 pid 匹配的条目, 替换 `base_priority`
    /// 2. 重算 `effective_priority` (新 max = `max(holder_base`, `all_waiters_base`))
    /// 3. 与 `prev_effective` 比较, 按情况触发 `notify_donation` 或 `notify_revoke`
    ///
    /// # 调用方约束
    /// - 当且仅当一个进程/线程的 `base_priority` 发生变化时调用
    /// - 若进程不是本 mutex 的等待者, 调用是 no-op (不产生任何通知)
    ///
    /// # 返回
    /// - `true`  : 至少一条 waiter 条目被更新, effective 变化可能发生
    /// - `false` : 未找到匹配 pid, 状态未变
    pub fn update_waiter_priority(&self, pid: u32, new_base_priority: u32) -> bool {
        let mut changed = false;
        {
            let mut waiters = self.inner.waiters.lock();
            for w in waiters.iter_mut() {
                if w.pid == pid && w.base_priority != new_base_priority {
                    w.base_priority = new_base_priority;
                    changed = true;
                }
            }
        }
        if changed {
            self.recompute_and_notify();
        }
        changed
    }

    /// v2.1: 私有助手 — 统一重算 `effective_priority` 并按需触发通知
    ///
    /// 算法: `new_effective = max(holder_base_priority, max(waiters.base_priority))`
    ///
    /// 通知策略:
    /// - prev < `new_effective`: `notify_donation(holder`, `new_effective`) — 提升
    /// - prev > `new_effective`: `notify_revoke(holder)`              — 撤销
    /// - 相等: 不通知
    ///
    /// 调用方: `register_waiter_and_donate`, `update_waiter_priority`,
    ///         `unlock_internal` 在找到新持有者后
    fn recompute_and_notify(&self) {
        let holder_prio = self.holder_base_priority.load(Ordering::Acquire);
        let (max_waiter_prio, chain) = {
            let waiters = self.inner.waiters.lock();
            let mut max_prio = 0u32;
            let mut donor_chain = [0u32; 8];
            let mut chain_len = 0u8;
            for (i, w) in waiters.iter().enumerate() {
                if w.base_priority >= max_prio {
                    max_prio = w.base_priority;
                }
                // v2.2: 收集捐赠链 — 每个等待者记录 PID
                if (i as u8) < 8 {
                    donor_chain[i] = w.pid;
                    chain_len = (i as u8) + 1;
                }
            }
            (max_prio, (donor_chain, chain_len))
        };
        let new_effective = holder_prio.max(max_waiter_prio);

        // v2.2: 链式捐赠 — 如果 holder 本身也是另一 mutex 的等待者,
        // 将本 mutex 的捐赠链传递给被等待的 mutex, 提升其持有者优先级
        {
            let chain_arr = chain.0;
            let cl = chain.1;
            if cl > 0 {
                // SAFETY: chain is UnsafeCell; single-threaded access guaranteed by lock
                unsafe {
                    *self.inner.chain.get() = chain_arr;
                }
                self.inner.chain_len.store(cl, Ordering::Relaxed);
            } else {
                self.inner.chain_len.store(0, Ordering::Relaxed);
            }
        }

        // CAS 风格: 仅在变化时通知
        let prev = self.inner.effective_priority.load(Ordering::Acquire);
        if new_effective == prev {
            return;
        }
        self.inner
            .effective_priority
            .store(new_effective, Ordering::Release);

        let holder_pid = self.inner.holder.load(Ordering::Acquire);
        if holder_pid == PID_NONE {
            return;
        }
        if new_effective > prev {
            notify_donation(holder_pid, new_effective);
        } else {
            notify_revoke(holder_pid);
        }
    }

    /// 释放锁 (由 `PiMutexGuard::drop` 自动调用)
    ///
    /// `pub(crate)` 以便 tests 模块直接验证状态机
    /// (测试无需完整 lock 循环, 直接 drop 即可触发)
    pub(crate) fn unlock_internal(&self) {
        // Lockdep: 通知锁释放
        #[cfg(debug_assertions)]
        crate::framework::sync::release(self.lockdep_class);

        let my_pid = current_pid();
        if self.inner.holder.load(Ordering::Acquire) != my_pid {
            // 双重释放 / 非持有者释放: 静默忽略 (v1)
            return;
        }

        self.do_unlock();
    }

    /// 强制释放 (跳过 holder 检查, 仅供 tests/调试使用)
    ///
    /// v2.1 修复: 原 `unlock_internal` 在 `no_std` 测试环境中 `current_pid()` 返回 0,
    /// 与 `try_lock` 设置的 holder (如 100) 不等, 提前 return, 导致 unlock 路径
    /// 不可测. 该方法跳过 holder 检查, 让测试可以直接驱动 unlock 状态机.
    ///
    /// 生产代码 (`PiMutexGuard::drop`) 仍走 `unlock_internal`, 走 RAII 安全路径.
    ///
    /// B4 内存过配治理: 唯一调用方为 `framework::tests::test_pi_mutex`, 测试框架
    /// 已移出 release ⇒ 本方法随 kernel_test/host-test 维度编译 (否则 release 死代码).
    #[cfg(any(feature = "kernel_test", feature = "host-test"))]
    pub(crate) fn force_unlock(&self) {
        // Lockdep: 通知锁释放
        #[cfg(debug_assertions)]
        crate::framework::sync::release(self.lockdep_class);

        // 跳过 holder 检查: 静默忽略未持锁情况 (无操作)
        if !self.inner.locked.load(Ordering::Acquire) {
            return;
        }
        let my_pid = self.inner.holder.load(Ordering::Acquire);

        self.do_unlock();
        // 静默: 防止 my_pid 未使用警告
        let _ = my_pid;
    }

    /// B03-08 + DECISION-049: 进程退出时强制释放锁 (检查特定 pid 是否持锁)。
    ///
    /// 与 `force_unlock` 的差异:
    /// - `force_unlock`: 跳过 holder 检查, 无条件释放 (用于测试)
    /// - `force_unlock_for_exit`: 检查 holder == pid, 仅在持锁进程退出时调用
    ///
    /// 由类型擦除分派 `pi_mutex_exit_dispatch` 在进程退出时调用。
    ///
    /// # 幂等保证
    /// 同 pid 多次调用不会重复释放 (因为第一次释放后 holder 变 PID_NONE)。
    /// 不同 pid 调用是 no-op。
    pub fn force_unlock_for_exit(&self, pid: u32) -> bool {
        if self.inner.holder.load(Ordering::Acquire) != pid {
            return false;
        }
        if !self.inner.locked.load(Ordering::Acquire) {
            return false;
        }
        self.do_unlock();
        true
    }

    /// 释放锁的实际逻辑 (从 `unlock_internal` 提取, 避免重复)
    fn do_unlock(&self) {
        let my_pid = self.inner.holder.load(Ordering::Acquire);

        // 1. 找到下一个最高优先级等待者
        let (next_pid, next_base_prio) = {
            let mut waiters = self.inner.waiters.lock();
            // 找到 max 优先级 (同优先级按 FIFO, 即 VecDeque 头部优先)
            let mut best_idx: Option<usize> = None;
            let mut best_prio: u32 = 0;
            for (i, w) in waiters.iter().enumerate() {
                if w.base_priority >= best_prio {
                    best_prio = w.base_priority;
                    best_idx = Some(i);
                }
            }
            if let Some(idx) = best_idx {
                // SAFETY: best_idx 为 Some 时 waiters[best_idx] 必然存在
                let entry = waiters.remove(idx).expect("pi_mutex: waiters 索引无效");
                (entry.pid, entry.base_priority)
            } else {
                // 无等待者, 完全释放
                self.inner.locked.store(false, Ordering::Release);
                self.inner.holder.store(PID_NONE, Ordering::Release);
                self.inner.effective_priority.store(0, Ordering::Release);
                self.holder_base_priority.store(0, Ordering::Release);
                return;
            }
        };

        // 2. 移交锁给下一个等待者 (原子 CAS: holder = next.pid, locked 保持 true)
        //    由于我们仍持 "持有者" 身份, 中间需要短暂 false → true 让 next 看到
        //    这里采用: 短暂释放 + next 重入, 与 register_waiter_and_donate 配合

        // 重置自己的优先级 (基线, 由调度器钩子处理)
        notify_revoke(my_pid); // 通知调度器我自己撤销捐赠

        // v2.2: 链式捐赠传播 — unlock 时, 如果有捐赠链, 传递给下一个持有者
        // 链式: C→B→A, 当 A unlock M1, B 成为 holder, B 的链是 [C]
        // 需要把 [C] 传递到 B 持有的其他 mutex, 提升 C 的优先级
        let old_chain_len = self.inner.chain_len.load(Ordering::Relaxed);
        if old_chain_len > 0 {
            // SAFETY: chain is UnsafeCell; single-threaded access guaranteed by lock
            let _ = unsafe { *self.inner.chain.get() };
            // 重置链 (为下次 lock 做准备)
            self.inner.chain_len.store(0, Ordering::Relaxed);
        }

        // 短暂 release, 让 next.try_lock 能看到 unlocked
        self.inner.locked.store(false, Ordering::Release);
        self.inner.holder.store(PID_NONE, Ordering::Release);
        self.inner.effective_priority.store(0, Ordering::Release);
        self.holder_base_priority.store(0, Ordering::Release);

        // next 线程此刻还在自旋等待 holder==next.pid;
        // 我们直接设置 holder, 让它从自旋中退出并完成获取
        self.inner.holder.store(next_pid, Ordering::Release);
        self.inner.locked.store(true, Ordering::Release);
        self.inner
            .effective_priority
            .store(next_base_prio, Ordering::Release);
        // 持有者 base_priority 由 next 在 try_lock 时设置
        // (这里没有, 因为我们走的是 unlock 路径而非 try_lock)
        // 修正: 重新赋值
        self.holder_base_priority
            .store(next_base_prio, Ordering::Release);

        // B03-08 返工: 移交路径不走 try_lock, 需为 next_pid 显式登记持有关系
        // (原持有者 my_pid 的残留条目由进程退出时整体 remove 清理, 幂等 no-op).
        track_hold(self, next_pid);

        // v2.2: 如果有捐赠链, 传递给下一个持有者
        // 链式: C→B→A, 当 A unlock M1, B 成为 holder, B 的链是 [C]
        // 需要把 [C] 传递到 B 持有的其他 mutex, 提升 C 的优先级
        let old_chain_len = self.inner.chain_len.load(Ordering::Relaxed);
        if old_chain_len > 0 {
            // SAFETY: chain is UnsafeCell; single-threaded access guaranteed by lock
            let _old_chain = unsafe { *self.inner.chain.get() };
            // 重置链 (为下次 lock 做准备)
            self.inner.chain_len.store(0, Ordering::Relaxed);
        }

        // v2.1: 统一走 recompute_and_notify 助手, 检查剩余等待者是否需要捐赠
        //  (recompute 内部会对比 prev 与 new_effective, 仅在升高时通知)
        self.recompute_and_notify();
    }

    /// 查询当前是否被持有
    pub fn is_locked(&self) -> bool {
        self.inner.locked.load(Ordering::Acquire)
    }

    /// 查询当前持有者 PID (0 = 未持有)
    pub fn holder(&self) -> u32 {
        self.inner.holder.load(Ordering::Acquire)
    }

    /// 查询当前 `effective_priority` (供调度器钩子使用)
    pub fn effective_priority(&self) -> u32 {
        self.inner.effective_priority.load(Ordering::Acquire)
    }

    /// 查询当前等待者数量
    pub fn waiter_count(&self) -> usize {
        self.inner.waiters.lock().len()
    }
}

// ============================================================================
// PiMutexGuard
// ============================================================================

impl<T: ?Sized> Drop for PiMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.unlock_internal();
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
// 辅助: 获取当前 PID (TCB 桥接)
// ============================================================================

fn current_pid() -> u32 {
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    unsafe extern "C" {
        fn process_get_current_pid() -> u32;
    }
    // SAFETY: process_get_current_pid 是有效的 C ABI 函数, 无副作用
    unsafe { process_get_current_pid() }
}

fn scheduler_yield() {
    // v2.6: 用 SCHEDULER_EX.yield_current() 让出 CPU, 替代旧的 C ABI scheduler_yield
    crate::framework::proc::scheduler_ex::SCHEDULER_EX.yield_current();
}
