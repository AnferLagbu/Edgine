//! # RCU (Read-Copy-Update) 同步原语
//!
//! 读多写少场景的零开销读者锁。读者无原子操作开销，
//! 写者等待所有已有读者退出临界区后释放旧数据。
//!
//! ## 核心 API
//!
//! | 操作 | 说明 |
//! |------|------|
//! | `rcu_read_lock()` | 进入 RCU 读临界区 |
//! | `rcu_read_unlock()` | 退出 RCU 读临界区 |
//! | `rcu_dereference(p)` | 安全读取 RCU 保护指针 |
//! | `rcu_assign_pointer(p, v)` | 安全更新 RCU 保护指针 |
//! | `synchronize_rcu()` | 阻塞直到宽限期结束 |
//! | `call_rcu(head, func)` | 注册宽限期回调 |
//!
//! ## 多核宽限期
//!
//! 每 CPU 维护独立的嵌套计数和取值单调的宽限期序号 (epoch)。
//! `synchronize_rcu()` 为每轮宽限期取全局唯一序号, 等待所有在线 CPU
//! 给出「序号不早于本轮且已通过静止点」的证明后才认为宽限期结束。
//! 序号单调使并发宽限期互不覆盖: 更晚轮次的静止证明同样满足更早轮次,
//! 复位只清本轮, 不会抹掉其它写者的等待请求。

#![deny(missing_docs)]

use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering, fence};

use crate::privileged::racy_cell::RacyCell;

/// RCU 回调链表节点: 记录下一个节点与宽限期结束后执行的回调.
pub struct RcuHead {
    /// 链表后继节点指针 (`null` 表示链尾)
    pub next: *mut Self,
    // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
    /// 宽限期结束后的回调函数 (`None` 表示未注册)
    pub func: Option<unsafe fn(*mut Self)>,
}

impl Default for RcuHead {
    fn default() -> Self {
        Self::new()
    }
}

impl RcuHead {
    /// 构造未注册回调的 RCU 链表节点 (全零, 常量上下文可用).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next: ptr::null_mut(),
            func: None,
        }
    }
}

struct PerCpuRcu {
    nesting: AtomicU32,
    /// 打包的宽限期槽位字: 高 32 位为序号 (epoch), 低 32 位为三态.
    gp_epoch_state: AtomicU64,
    callbacks: UnsafeCell<*mut RcuHead>,
    callback_tail: UnsafeCell<*mut RcuHead>,
    callback_count: AtomicU32,
    need_callback_process: AtomicBool,
}

impl PerCpuRcu {
    const fn new() -> Self {
        Self {
            nesting: AtomicU32::new(0),
            // 全零 = `gp_pack(0, GP_IDLE)`: 初始槽位尚无任何宽限期轮次.
            gp_epoch_state: AtomicU64::new(0),
            callbacks: UnsafeCell::new(ptr::null_mut()),
            callback_tail: UnsafeCell::new(ptr::null_mut()),
            callback_count: AtomicU32::new(0),
            need_callback_process: AtomicBool::new(false),
        }
    }
}

// SAFETY: PerCpuRcu 仅在关中断路径下访问;
// SAFETY: PerCpuRcu 含 UnsafeCell, 但仅由对应 CPU 访问, UnsafeCell 为回调链表提供内部可变性.
unsafe impl Sync for PerCpuRcu {}

// SAFETY: PerCpuRcu 含裸指针 (callbacks/callback_tail), 裸指针本身非 Send; 但该
// 结构仅在对应 CPU (或关中断的单线程上下文) 内访问, 回调链表指针的所有权始终
// 在同一个 CPU 内部传递, 不跨线程共享裸指针. 故显式实现 Send, 以满足
// `RacyCell<T: Send>` 对静态承载 `T` 的要求.
unsafe impl Send for PerCpuRcu {}

const GP_IDLE: u32 = 0;
const GP_WAIT: u32 = 1;
const GP_DONE: u32 = 2;

/// 宽限期序号在槽位字中的位偏移 (低 32 位为三态).
const GP_EPOCH_SHIFT: u32 = 32;

/// 把 `(epoch, state)` 打包为单个 64 位原子字.
///
/// 序号与三态必须同一原子字读写: 拆成两个原子字会出现「新序号 + 旧状态」的
/// 撕裂窗口, 等待方可能据此误判或覆盖并发请求.
#[inline]
fn gp_pack(epoch: u32, state: u32) -> u64 {
    (u64::from(epoch) << GP_EPOCH_SHIFT) | u64::from(state)
}

/// 从槽位字取出宽限期序号.
#[inline]
fn gp_epoch(word: u64) -> u32 {
    // 右移后恰在 u32 范围内, `unwrap_or` 分支不可达, 仅为保持无 panic 形态.
    u32::try_from(word >> GP_EPOCH_SHIFT).unwrap_or(0)
}

/// 从槽位字取出三态.
#[inline]
fn gp_state(word: u64) -> u32 {
    u32::try_from(word & u64::from(u32::MAX)).unwrap_or(GP_IDLE)
}

/// 回绕安全的序号先后比较: `candidate` 是否不早于 `reference`.
///
/// 以「差值小于半个序号空间」判先后, 允许序号跨 `u32::MAX` 回绕; 只有两个
/// 序号相差达到 2^31 时才会给出保守的「不晚于」否定, 而单个宽限期窗口内
/// 不可能积累如此多的并发轮次.
#[inline]
fn epoch_at_or_after(candidate: u32, reference: u32) -> bool {
    candidate.wrapping_sub(reference) < (1u32 << 31)
}

/// 为远端 CPU 登记本轮 (`ticket`) 宽限期请求.
///
/// 分两种情形:
/// - 槽位持有**更新轮次的未完成请求** (`GP_WAIT`): 保留之 — 它的静止证明
///   必然产生于本轮登记之后, 同样满足本轮;
/// - 其余情形 (含更新轮次**已完成/已复位**的记录): 一律覆盖为本轮 `GP_WAIT`.
///   已完成记录产生于本轮登记**之前**, 不能作为本轮所需的静止证明; 覆盖后
///   强迫目标在本轮登记之后重新通过静止点.
#[inline]
fn request_gp(data: &PerCpuRcu, ticket: u32) {
    let mut word = data.gp_epoch_state.load(Ordering::Acquire);
    loop {
        let epoch = gp_epoch(word);
        if gp_state(word) == GP_WAIT && epoch != ticket && epoch_at_or_after(epoch, ticket) {
            return;
        }
        match data.gp_epoch_state.compare_exchange_weak(
            word,
            gp_pack(ticket, GP_WAIT),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(actual) => word = actual,
        }
    }
}

/// 为指定 CPU 上报静止状态: 把其当前等待轮次由 `GP_WAIT` 推为 `GP_DONE`.
///
/// 只推进槽位当前登记的轮次, 不引入新序号; 用 CAS 而非 store, 避免把并发
/// 请求方刚写入的更新轮次覆盖回旧序号 (那会吞掉对方的等待请求).
#[inline]
fn report_qs(data: &PerCpuRcu) {
    if data.nesting.load(Ordering::Acquire) > 0 {
        return;
    }
    let mut word = data.gp_epoch_state.load(Ordering::Acquire);
    loop {
        if gp_state(word) != GP_WAIT {
            return;
        }
        let done = gp_pack(gp_epoch(word), GP_DONE);
        match data.gp_epoch_state.compare_exchange_weak(
            word,
            done,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(actual) => word = actual,
        }
    }
}

/// 本轮是否已在该 CPU 上获得静止证明.
///
/// 判据 = 槽位序号不早于本轮, 且状态不是 `GP_WAIT`:
/// - `GP_DONE`: 该 CPU 已在本轮或更晚轮次上报静止;
/// - `GP_IDLE`: 只会在某轮 `GP_DONE` 之后被该轮持有者复位, 同样证明更晚
///   轮次已完成;
/// - `GP_WAIT`: 即使序号更新也只是别人刚提出的等待, 不构成静止证明.
#[inline]
fn gp_satisfied(data: &PerCpuRcu, ticket: u32) -> bool {
    let word = data.gp_epoch_state.load(Ordering::Acquire);
    gp_state(word) != GP_WAIT && epoch_at_or_after(gp_epoch(word), ticket)
}

/// 结束本轮: 只复位「序号属本轮且已获静止证明」的槽位.
///
/// 序号已被更新轮次抬高时保持原状 —— 既不能清掉别人的 `GP_WAIT`, 也不能把
/// 更晚轮次的完成记录降级; 超时收尾时仍是 `GP_WAIT`, 也不得复位成看似
/// 完成的 `GP_IDLE` (那会让其它等待方把无证明的轮次误判为已通过静止点).
#[inline]
fn finish_gp(data: &PerCpuRcu, ticket: u32) {
    let word = data.gp_epoch_state.load(Ordering::Acquire);
    if gp_epoch(word) != ticket || gp_state(word) != GP_DONE {
        return;
    }
    let _ = data.gp_epoch_state.compare_exchange(
        word,
        gp_pack(ticket, GP_IDLE),
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

/// BSP 的静态 per-CPU RCU 状态.
///
/// RCU 在启动早期 (BSP 单线程) 即可能被使用, 故 BSP 槽位固定为静态实例.
/// `PerCpuRcu::new()` 全零, 该实例落入 `.bss` (不占镜像 `.data`).
static RCU_BSP: RacyCell<PerCpuRcu> = RacyCell::new(PerCpuRcu::new());

/// AP 的 per-CPU RCU 状态表 (按 `cpu_index` 索引, 槽位 0 保留给 BSP).
///
/// AP 的状态由 [`rcu_alloc_cpu`] 在 AP 上线前按需从页池分配, 以**内核高半区
/// 直接映射别名**存放 —— aarch64 运行期 TTBR0 的每进程 EL1 视图刻意移除 DRAM
/// 块, 低半区物理地址在运行期并非有效可解引用地址, 故必须走高半区别名.
/// 槽位为 `null` 表示尚未分配.
static RCU_PER_CPU: [AtomicPtr<PerCpuRcu>; crate::privileged::config::MAX_CPUS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; crate::privileged::config::MAX_CPUS];

static RCU_GP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// 将 CPU 编号映射到 per-CPU RCU 槽位下标.
///
/// 槽位容量与 `MAX_CPUS` 一致; 取模仅为防御越界 (越界时退化为槽位别名而非 UB).
#[inline]
fn rcu_slot(cpu: u32) -> usize {
    (cpu as usize) % crate::privileged::config::MAX_CPUS
}

/// 获取指定 CPU 已分配的 per-CPU RCU 状态引用; 槽位未分配时返回 `None`.
///
/// 跨 CPU 路径 (宽限期状态机 / 远程回调通知) 必须用本函数而非 [`rcu_data`]:
/// 后者在槽位为 `null` 时回退 BSP, 会让调用方把**远程 CPU 的状态写到 BSP 槽位**,
/// 与 BSP 自身状态互相覆盖. 返回 `None` 表示该 CPU 尚无独立 RCU 状态
/// (启动时序: `rcu_alloc_cpu` 早于 AP 上线), 调用方应跳过该槽位.
#[inline]
fn rcu_data_allocated(cpu: u32) -> Option<&'static PerCpuRcu> {
    let idx = rcu_slot(cpu);
    if idx == 0 {
        // SAFETY: RCU_BSP 为静态实例; 启动期单线程写入, 运行期只读/原子访问.
        return Some(unsafe { RCU_BSP.get() });
    }
    let ptr = RCU_PER_CPU[idx].load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    // SAFETY: 指针由 rcu_alloc_cpu 以 Release 发布, 指向页池分配且已清零的
    // PerCpuRcu; 高半区直接映射使其运行期可访问; 只读/原子访问.
    Some(unsafe { &*ptr })
}

/// 获取指定 CPU 的 per-CPU RCU 状态引用 (仅限**本 CPU** 路径使用).
///
/// # 不变量
///
/// 调用方保证目标槽位已分配 (启动时序: `rcu_alloc_cpu` 早于 AP 使用 RCU).
/// 若槽位为 `null` (目标 `cpu_index` 尚未分配 RCU 状态), 回退 BSP 状态以保证
/// 内存安全; 该回退只适用于本 CPU 的读写, 跨 CPU 路径必须用
/// [`rcu_data_allocated`].
#[inline]
fn rcu_data(cpu: u32) -> &'static PerCpuRcu {
    if let Some(data) = rcu_data_allocated(cpu) {
        return data;
    }
    // SAFETY: 回退 BSP 仅用于避免 null 解引用 (见 doc 的槽位未分配情形).
    unsafe { RCU_BSP.get() }
}

/// 为指定 `cpu_index` 分配 per-CPU RCU 状态 (幂等).
///
/// 槽位 0 为 BSP 静态实例, 直接返回 `true`. 返回 `false` 表示页池分配失败,
/// 调用方应放弃启动该 AP.
pub fn rcu_alloc_cpu(cpu_index: u32) -> bool {
    let idx = rcu_slot(cpu_index);
    if idx == 0 {
        return true;
    }
    if !RCU_PER_CPU[idx].load(Ordering::Acquire).is_null() {
        return true;
    }
    let Some(ptr) = crate::privileged::mm::alloc_zeroed_page_as::<PerCpuRcu>() else {
        return false;
    };
    // 页已整体清零, PerCpuRcu 全零即 PerCpuRcu::new() 语义, 无需再 write.
    RCU_PER_CPU[idx].store(ptr, Ordering::Release);
    true
}

#[inline]
fn current_rcu() -> &'static PerCpuRcu {
    let cpu = crate::privileged::smp::get_current_cpu();
    rcu_data(cpu)
}

#[inline]
fn rcu_read_lock_impl() {
    let data = current_rcu();
    let nesting = data.nesting.fetch_add(1, Ordering::Acquire);
    if nesting == 0 {
        fence(Ordering::Acquire);
    }
}

#[inline]
fn rcu_read_unlock_impl() {
    let data = current_rcu();
    fence(Ordering::Release);
    let nesting = data.nesting.fetch_sub(1, Ordering::Release);
    if nesting == 1 {
        fence(Ordering::Release);
    }
}

/// 安全读取 RCU 保护的指针
///
/// # Safety
/// 调用者必须在 RCU 读临界区内
#[inline]
pub unsafe fn rcu_dereference<T>(ptr: *const T) -> *const T {
    unsafe {
        fence(Ordering::Acquire);
        ptr::read_volatile(&raw const ptr)
    }
}

/// 安全更新 RCU 保护的指针
///
/// # Safety
/// 调用者确保旧值在所有 RCU 读者退出前不被释放
#[inline]
pub unsafe fn rcu_assign_pointer<T>(slot: *mut *const T, new_val: *const T) {
    unsafe {
        fence(Ordering::Release);
        ptr::write_volatile(slot, new_val);
    }
}

/// 阻塞直到所有 CPU 上已有 RCU 读者退出
///
/// 宽限期流程 (epoch 化, P-76 甲):
/// 1. 取全局唯一序号 `ticket`, 为每个在线且已分配槽位的远端 CPU 登记本轮
///    `GP_WAIT` (槽位序号只升不降), 并发送重调度 IPI 促使目标报告静止;
/// 2. 等待每个远端 CPU 给出「序号不早于本轮且状态非 `GP_WAIT`」的静止证明;
///    等待循环内顺带为**本 CPU** 上报静止, 打破两个 CPU 并发进入宽限期时
///    互相等待、双双自旋到超时的死锁;
/// 3. 等待本 CPU 的读临界区嵌套归零;
/// 4. 只复位序号仍属本轮的槽位, 处理回调.
///
/// 离线 CPU 与未分配槽位的 CPU 不参与本轮宽限期 (前者无读者, 后者尚未完成
/// RCU 初始化); 跨 CPU 槽位一律经 [`rcu_data_allocated`] 获取, 不回落 BSP 槽位.
fn synchronize_rcu_impl() {
    const SYNC_TIMEOUT_SPINS: u32 = 50_000_000;

    // 每轮宽限期的唯一序号; 并发轮次凭此在槽位上取最大, 互不覆盖.
    // 序号 0 保留给「尚无轮次」的初始槽位字, 回绕到 0 时跳过再取一号.
    let mut ticket = RCU_GP_COUNTER
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    if ticket == 0 {
        ticket = RCU_GP_COUNTER
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
    }

    let cpu_count = crate::privileged::smp::get_cpu_count();
    let current_cpu = crate::privileged::smp::get_current_cpu();

    for i in 0..cpu_count {
        if i == current_cpu || !crate::privileged::smp::is_cpu_online(i) {
            continue;
        }
        let Some(data) = rcu_data_allocated(i) else {
            // 槽位未分配 ⇒ 该 AP 尚未完成 RCU 初始化, 不可能有 RCU 读者;
            // 不得回退 BSP 槽位, 否则会改写本 CPU 自身的宽限期状态.
            continue;
        };
        request_gp(data, ticket);
        // 以逻辑索引为键发送重调度 IPI; 目标未登记时内部静默返回。
        crate::privileged::smp::send_reschedule_ipi(i);
    }

    // 本 CPU 的槽位可能被并发轮次登记为 `GP_WAIT`; 立即上报自身静止.
    report_qs(current_rcu());

    let mut spins = 0u32;
    loop {
        let mut pending = false;
        for i in 0..cpu_count {
            if i == current_cpu || !crate::privileged::smp::is_cpu_online(i) {
                continue;
            }
            let Some(data) = rcu_data_allocated(i) else {
                continue;
            };
            if !gp_satisfied(data, ticket) {
                pending = true;
                break;
            }
        }
        if !pending {
            break;
        }
        // 并发宽限期的双方都可能停在本循环自旋, 不会发生上下文切换去触发
        // `rcu_note_quiescent_state`; 这里主动上报本 CPU 的静止状态, 使对方的
        // 等待条件可在本循环内满足.
        report_qs(current_rcu());
        core::hint::spin_loop();
        spins += 1;
        if spins >= SYNC_TIMEOUT_SPINS {
            break;
        }
    }

    {
        let data = current_rcu();
        let mut spins = 0u32;
        while data.nesting.load(Ordering::Acquire) > 0 {
            core::hint::spin_loop();
            spins += 1;
            if spins >= SYNC_TIMEOUT_SPINS {
                break;
            }
        }
    }

    for i in 0..cpu_count {
        if let Some(data) = rcu_data_allocated(i) {
            finish_gp(data, ticket);
        }
    }

    process_callbacks();
}

/// 注册 RCU 回调, 在宽限期结束后调用
///
/// `func` 在 `synchronize_rcu()` 或 `process_callbacks()` 时被调用。
///
/// # Safety
/// `head` 必须是从分配器分配的有效指针, `func` 必须正确处理 `head` 指向的内存
pub unsafe fn call_rcu(head: *mut RcuHead, func: unsafe fn(*mut RcuHead)) {
    if head.is_null() {
        return;
    }

    unsafe {
        (*head).next = ptr::null_mut();
        (*head).func = Some(func);
    }

    let data = current_rcu();
    let flags = crate::privileged::sync::disable_interrupts();

    // SAFETY: Interrupts disabled — callback list manipulation is atomic
    let tail = unsafe { *data.callback_tail.get() };
    if tail.is_null() {
        // SAFETY: Callbacks list empty — write to head via UnsafeCell
        unsafe {
            *data.callbacks.get() = head;
        }
    } else {
        // SAFETY: tail != null → dereference safe; writing next ptr
        unsafe {
            (*tail).next = head;
        }
    }
    // SAFETY: data.callback_tail is an UnsafeCell, interrupts disabled
    unsafe {
        *data.callback_tail.get() = head;
    }

    data.callback_count.fetch_add(1, Ordering::Relaxed);
    data.need_callback_process.store(true, Ordering::Release);

    crate::privileged::sync::restore_interrupts(flags);

    crate::privileged::irq::raise_softirq(crate::privileged::irq::SoftirqVec::High);
}

/// 检查当前上下文是否在 RCU 读临界区内
#[must_use]
pub fn rcu_read_lock_held() -> bool {
    current_rcu().nesting.load(Ordering::Acquire) > 0
}

/// 处理所有挂起的 RCU 回调 (当前 CPU)
pub fn process_callbacks() {
    let data = current_rcu();

    if !data.need_callback_process.load(Ordering::Acquire) {
        return;
    }

    let flags = crate::privileged::sync::disable_interrupts();

    // SAFETY: Interrupts disabled — exclusive access to callback list
    let head = unsafe { *data.callbacks.get() };
    // SAFETY: Clearing callbacks under interrupt lock
    unsafe {
        *data.callbacks.get() = ptr::null_mut();
        *data.callback_tail.get() = ptr::null_mut();
    }
    data.callback_count.store(0, Ordering::Relaxed);
    data.need_callback_process.store(false, Ordering::Release);

    crate::privileged::sync::restore_interrupts(flags);

    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: cur was in the callback linked list, each node is valid
        let next = unsafe { (*cur).next };
        let func = unsafe { (*cur).func };

        if let Some(f) = func {
            // SAFETY: f is the callback registered by call_rcu; cur is the RcuHead
            unsafe {
                f(cur);
            }
        }

        cur = next;
    }
}

/// 标记静止状态 (由调度器在上下文切换时调用)
pub fn rcu_note_quiescent_state() {
    let data = current_rcu();

    // 读临界区未退出时 report_qs 内部直接返回, 不会误报静止.
    report_qs(data);

    if data.need_callback_process.load(Ordering::Acquire)
        && data.nesting.load(Ordering::Acquire) == 0
    {
        process_callbacks();
    }
}

/// 通知所有 CPU 处理各自挂起的 RCU 回调 (由同步宽限期调用)
///
/// 本 CPU 的回调就地处理; 远程 CPU 只发送重调度 IPI 通知, 由目标 CPU 在
/// 下一次上下文切换时经 [`rcu_note_quiescent_state`] 本地处理. 回调链表节点
/// (`callbacks` / `callback_tail`) 只允许所属 CPU 访问: 跨 CPU 清链会与目标
/// CPU 关中断下的 `call_rcu` 链表操作竞争.
pub fn rcu_process_all_callbacks() {
    process_callbacks();

    let cpu_count = crate::privileged::smp::get_cpu_count();
    let current_cpu = crate::privileged::smp::get_current_cpu();
    for i in 0..cpu_count {
        if i == current_cpu || !crate::privileged::smp::is_cpu_online(i) {
            continue;
        }
        let Some(data) = rcu_data_allocated(i) else {
            continue;
        };
        if data.need_callback_process.load(Ordering::Acquire) {
            crate::privileged::smp::send_reschedule_ipi(i);
        }
    }
}

/// 返回已完成的 RCU 宽限期 (grace period) 计数.
pub fn rcu_gp_count() -> u32 {
    RCU_GP_COUNTER.load(Ordering::Relaxed)
}

/// 返回当前 CPU 待处理的 RCU 回调数.
#[must_use]
pub fn rcu_callback_count() -> u32 {
    let cpu = crate::privileged::smp::get_current_cpu();
    rcu_data(cpu).callback_count.load(Ordering::Relaxed)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 进入 RCU 读临界区 (C ABI 导出)
#[unsafe(no_mangle)]
pub extern "C" fn rcu_read_lock() {
    rcu_read_lock_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 退出 RCU 读临界区 (C ABI 导出)
#[unsafe(no_mangle)]
pub extern "C" fn rcu_read_unlock() {
    rcu_read_unlock_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 阻塞直到当前宽限期结束 (C ABI 导出)
#[unsafe(no_mangle)]
pub extern "C" fn synchronize_rcu() {
    synchronize_rcu_impl();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 初始化 RCU 宽限期计数器 (C ABI 导出)
#[unsafe(no_mangle)]
pub extern "C" fn rcu_init() {
    RCU_GP_COUNTER.store(1, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    // host 侧 `current_cpu_index()` 恒回退 0, 所有测试线程共用 BSP 槽位; 而宽限期
    // 会清空「当前 CPU」的回调链表. 并行用例下, 另一用例的 `synchronize_rcu` 会
    // 清掉本用例刚注册的回调 (P-35 flaky 根因). 三个用例以本闸门互斥执行;
    // 真机各 CPU 持独立槽位, 无此共享面.
    static TEST_SERIAL: AtomicBool = AtomicBool::new(false);

    /// RAII 串行闸门: 构造即获取, 析构 (含 panic 展开) 释放.
    struct SerialGuard;

    impl SerialGuard {
        fn acquire() -> Self {
            while TEST_SERIAL
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            Self
        }
    }

    impl Drop for SerialGuard {
        fn drop(&mut self) {
            TEST_SERIAL.store(false, Ordering::Release);
        }
    }

    #[test]
    fn test_rcu_read_lock_unlock() {
        let _serial = SerialGuard::acquire();
        rcu_read_lock();
        assert!(rcu_read_lock_held());
        rcu_read_lock();
        rcu_read_unlock();
        assert!(rcu_read_lock_held());
        rcu_read_unlock();
        assert!(!rcu_read_lock_held());
    }

    #[test]
    fn test_rcu_gp_counter() {
        let _serial = SerialGuard::acquire();
        let before = rcu_gp_count();
        synchronize_rcu();
        let after = rcu_gp_count();
        assert!(after > before);
    }

    #[test]
    fn test_call_rcu() {
        let _serial = SerialGuard::acquire();
        static CALLED: AtomicBool = AtomicBool::new(false);

        // SAFETY: `mut` 由调用方保证为有效指针; 只读访问
        // call_rcu 的 func 形参为 `unsafe fn(*mut RcuHead)` (Rust ABI), 非 C fn 指针.
        unsafe fn callback(_head: *mut RcuHead) {
            CALLED.store(true, Ordering::Release);
        }

        let mut head = RcuHead::new();
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            call_rcu(&mut head as *mut RcuHead, callback);
        }

        assert!(rcu_callback_count() == 1);

        synchronize_rcu();

        assert!(rcu_callback_count() == 0);
        assert!(CALLED.load(Ordering::Acquire));
    }

    // 以下用例只操作栈上独立槽位, 不触碰全局 RCU 状态, 无需串行闸门.

    #[test]
    fn test_gp_epoch_state_machine_cycle() {
        let cpu = PerCpuRcu::new();

        request_gp(&cpu, 1);
        assert_eq!(
            gp_state(cpu.gp_epoch_state.load(Ordering::Acquire)),
            GP_WAIT
        );
        assert!(!gp_satisfied(&cpu, 1), "已登记等待但尚未上报静止");

        report_qs(&cpu);
        assert!(gp_satisfied(&cpu, 1), "上报静止后本轮应获证明");

        finish_gp(&cpu, 1);
        assert_eq!(gp_epoch(cpu.gp_epoch_state.load(Ordering::Acquire)), 1);
        assert!(gp_satisfied(&cpu, 1), "复位只把状态降回 IDLE, 序号保留");
    }

    #[test]
    fn test_gp_pending_newer_request_not_downgraded() {
        let cpu = PerCpuRcu::new();

        request_gp(&cpu, 1);
        request_gp(&cpu, 2);
        let word = cpu.gp_epoch_state.load(Ordering::Acquire);
        assert_eq!(gp_epoch(word), 2, "更晚轮次的待办请求不得被降级");
        assert_eq!(gp_state(word), GP_WAIT);

        report_qs(&cpu);
        assert!(gp_satisfied(&cpu, 1), "更晚轮次的静止同样证明更早轮次");
        assert!(gp_satisfied(&cpu, 2));
        assert!(!gp_satisfied(&cpu, 3), "尚未发生的轮次不满足");

        finish_gp(&cpu, 1);
        let word = cpu.gp_epoch_state.load(Ordering::Acquire);
        assert_eq!(gp_epoch(word), 2, "早轮次收尾不得清除晚轮次记录");
        assert_eq!(gp_state(word), GP_DONE);

        finish_gp(&cpu, 2);
        let word = cpu.gp_epoch_state.load(Ordering::Acquire);
        assert_eq!(gp_epoch(word), 2);
        assert_eq!(gp_state(word), GP_IDLE);
    }

    #[test]
    fn test_gp_completed_epoch_requires_fresh_quiescent() {
        let cpu = PerCpuRcu::new();

        request_gp(&cpu, 2);
        report_qs(&cpu);
        assert_eq!(
            gp_state(cpu.gp_epoch_state.load(Ordering::Acquire)),
            GP_DONE
        );

        // 晚到的更早轮次 (ticket 1): DONE@2 产生于本轮登记之前, 不能作为
        // 本轮的静止证明, 必须覆盖为 WAIT@1 等待本轮之后的静止点.
        request_gp(&cpu, 1);
        let word = cpu.gp_epoch_state.load(Ordering::Acquire);
        assert_eq!(gp_epoch(word), 1);
        assert_eq!(gp_state(word), GP_WAIT);
        assert!(!gp_satisfied(&cpu, 1), "覆盖后不得直接判定满足");

        request_gp(&cpu, 7);
        request_gp(&cpu, 5);
        let word = cpu.gp_epoch_state.load(Ordering::Acquire);
        assert_eq!(gp_epoch(word), 7, "待办请求只升不降");

        report_qs(&cpu);
        assert!(gp_satisfied(&cpu, 1));
        finish_gp(&cpu, 7);
        assert_eq!(
            gp_state(cpu.gp_epoch_state.load(Ordering::Acquire)),
            GP_IDLE
        );
    }

    #[test]
    fn test_gp_timeout_leaves_wait_unclaimed() {
        let cpu = PerCpuRcu::new();
        request_gp(&cpu, 9);

        // 未获静止证明就收尾 (超时路径): 不得复位成可被误读的完成态.
        finish_gp(&cpu, 9);
        let word = cpu.gp_epoch_state.load(Ordering::Acquire);
        assert_eq!(gp_epoch(word), 9);
        assert_eq!(gp_state(word), GP_WAIT);
        assert!(!gp_satisfied(&cpu, 9), "无静止证明的轮次不得被判满足");

        report_qs(&cpu);
        assert!(gp_satisfied(&cpu, 9));
        finish_gp(&cpu, 9);
        assert_eq!(
            gp_state(cpu.gp_epoch_state.load(Ordering::Acquire)),
            GP_IDLE
        );
    }

    #[test]
    fn test_gp_quiescent_report_blocked_by_nesting() {
        let cpu = PerCpuRcu::new();
        request_gp(&cpu, 7);
        cpu.nesting.store(1, Ordering::Release);

        report_qs(&cpu);
        assert_eq!(
            gp_state(cpu.gp_epoch_state.load(Ordering::Acquire)),
            GP_WAIT,
            "读者未退出时不得上报静止"
        );

        cpu.nesting.store(0, Ordering::Release);
        report_qs(&cpu);
        assert_eq!(
            gp_state(cpu.gp_epoch_state.load(Ordering::Acquire)),
            GP_DONE
        );
    }

    #[test]
    fn test_gp_epoch_wrap_comparison() {
        assert!(epoch_at_or_after(0, 0));
        assert!(epoch_at_or_after(3, 2));
        assert!(!epoch_at_or_after(2, 3));
        // 序号回绕: u32::MAX 的下一轮是 0.
        assert!(epoch_at_or_after(0, u32::MAX));
        assert!(!epoch_at_or_after(u32::MAX, 0));
        // 恰好相差半个序号空间时保守判「不晚于」.
        assert!(!epoch_at_or_after(0, 1u32 << 31));
    }
}
