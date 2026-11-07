//! Futex — 快速用户空间互斥锁
//!
//! 实现 Linux 风格的 futex 系统调用, 支持用户态高效同步原语.
//!
//! ## 核心语义
//!
//! - `FUTEX_WAIT(op=0)`: 原子比较 *uaddr == val, 若相等则阻塞当前线程
//! - `FUTEX_WAIT_BITSET(op=9)`: 同上, 且接受 `CLOCK_MONOTONIC` **绝对**超时
//! - `FUTEX_WAKE(op=1)`: 唤醒最多 val 个等待在 uaddr 上的线程
//! - `FUTEX_REQUEUE(op=3)`: 将最多 val 个等待者从 uaddr 迁移到 uaddr2
//!
//! ## 等待队列
//!
//! 使用全局哈希表将 uaddr 映射到等待者, 每个桶由一把 `PiMutex` 守护: 桶内临界区
//! 极短, 但持有者可能被抢占, 自旋等待既浪费 CPU 又无法应对持锁线程退出, 故等待
//! 者按 EEVDF 权重阻塞排队, 由定向移交唤醒; 持锁线程退出时由 PI 退出登记强制释放.
//! 每个桶使用固定大小数组, 避免动态内存分配.
//!
//! ## 丢唤醒防线 (桶序号)
//!
//! 每个桶带一个唤醒序号: 唤醒/迁移在桶锁内推进它, 等待者按"快照序号 → 读等待字
//! → 持桶锁复查序号并复查后入队"三步入睡. 只要快照之后发生过一次唤醒, 复查即
//! 失败并返回 `EAGAIN` 重试 —— 唤醒不会落在"读字"与"入队"之间被丢掉.
//! "入队 + 置 Blocked"在同一桶锁域内完成: 否则两者之间的唤醒会被
//! `Scheduler::unblock` 的 Running 判据丢弃, 等待者永久睡在队列外. 置 Blocked 到
//! 释放桶锁之间还要关中断: 已 Blocked 的线程若被本核中断退出路径切走, 它会带着
//! 桶锁离核且不在任何运行队列, 唤醒方再也拿不到桶锁.
//!
//! # Safety
//!
//! - `uaddr` 必须是合法的用户空间指针, 入队/读字前经 `super::raw::check_user_ptr`
//!   限定在 `[1, USER_ADDR_MAX)`; 页表映射有效性由当前进程地址空间保证
//! - 原子比较使用 `AtomicU32` 访问用户空间, 需确保页表映射有效

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::privileged::sync::pi_mutex::PiMutex;
use crate::privileged::sync::{OnceLock, disable_interrupts, restore_interrupts};
use crate::privileged::timer::{get_ticks, get_uptime_ns, ms_to_ticks};

// ============================================================================
// Futex 操作码 (单一权威源 `edgine_abi::thread`)
// ============================================================================

/// 等待: 若 *uaddr == val, 阻塞当前线程
pub const FUTEX_WAIT: i32 = edgine_abi::thread::FUTEX_WAIT;
/// 唤醒: 唤醒最多 val 个等待者
pub const FUTEX_WAKE: i32 = edgine_abi::thread::FUTEX_WAKE;
/// 带绝对超时的等待: 超时参数是 `CLOCK_MONOTONIC` 绝对时刻 (`timespec`)
pub const FUTEX_WAIT_BITSET: i32 = edgine_abi::thread::FUTEX_WAIT_BITSET;
/// 唤醒指定位集 (暂等同于 `FUTEX_WAKE`)
pub const FUTEX_WAKE_BITSET: i32 = edgine_abi::thread::FUTEX_WAKE_BITSET;
/// 迁移等待者: 将最多 val 个等待者从 uaddr 迁移到 uaddr2
pub const FUTEX_REQUEUE: i32 = edgine_abi::thread::FUTEX_REQUEUE;

/// 私有标志: futex 位于进程内共享内存 (同一地址空间)
pub const FUTEX_PRIVATE_FLAG: i32 = edgine_abi::thread::FUTEX_PRIVATE_FLAG;

/// 提取基础操作码 (去掉 `PRIVATE/CLOCK_REALTIME` 等标志)
fn futex_op(op: i32) -> i32 {
    op & !FUTEX_PRIVATE_FLAG & !0x100 // 去掉 FUTEX_CLOCK_REALTIME
}

// ============================================================================
// 等待者
// ============================================================================

/// 一个等待在 futex 上的线程
struct FutexWaiter {
    /// 等待的 futex 用户空间地址
    uaddr: u64,
    /// 等待线程的 PID
    pid: u32,
    /// 是否已被唤醒
    woken: bool,
}

impl FutexWaiter {
    const fn empty() -> Self {
        Self {
            uaddr: 0,
            pid: 0,
            woken: false,
        }
    }

    fn is_occupied(&self) -> bool {
        self.pid != 0
    }
}

// ============================================================================
// 全局等待队列
// ============================================================================

/// 哈希桶数量 (2 的幂, 方便取模)
const FUTEX_HASH_BUCKETS: usize = 64;

/// 每个桶的最大等待者数量
const FUTEX_BUCKET_CAPACITY: usize = 16;

/// 一个哈希桶: 固定大小等待者数组
struct FutexBucket {
    waiters: [FutexWaiter; FUTEX_BUCKET_CAPACITY],
    count: usize,
}

impl FutexBucket {
    /// 创建空桶 (零等待者, `count=0`)。
    const fn empty() -> Self {
        FutexBucket {
            waiters: [const { FutexWaiter::empty() }; FUTEX_BUCKET_CAPACITY],
            count: 0,
        }
    }

    /// 添加等待者, 返回是否成功
    fn push(&mut self, waiter: FutexWaiter) -> bool {
        if self.count >= FUTEX_BUCKET_CAPACITY {
            return false;
        }
        for slot in &mut self.waiters {
            if !slot.is_occupied() {
                *slot = waiter;
                self.count += 1;
                return true;
            }
        }
        false
    }

    /// 移除指定 PID 的等待者, 返回是否确实移除
    fn remove_by_pid(&mut self, pid: u32) -> bool {
        for slot in &mut self.waiters {
            if slot.pid == pid {
                *slot = FutexWaiter::empty();
                self.count -= 1;
                return true;
            }
        }
        false
    }

    /// 唤醒最多 `max_count` 个等待在 uaddr 上的线程, 返回实际唤醒数
    fn wake(&mut self, uaddr: u64, max_count: u32) -> u32 {
        let mut woken = 0u32;
        for slot in &mut self.waiters {
            if slot.is_occupied() && slot.uaddr == uaddr && !slot.woken {
                slot.woken = true;
                crate::privileged::proc::process_unblock(slot.pid);
                woken += 1;
                if woken >= max_count {
                    break;
                }
            }
        }
        // 清理已唤醒的槽位
        for slot in &mut self.waiters {
            if slot.woken {
                self.count -= 1;
                *slot = FutexWaiter::empty();
            }
        }
        woken
    }

    /// 迁移等待者: 唤醒 `max_wake` 个, 迁移 `max_requeue` 个到 uaddr2
    fn requeue(&mut self, uaddr: u64, max_wake: u32, uaddr2: u64, max_requeue: u32) -> (u32, u32) {
        let mut woken = 0u32;
        let mut requeued = 0u32;

        for slot in &mut self.waiters {
            if !slot.is_occupied() || slot.uaddr != uaddr || slot.woken {
                continue;
            }

            if woken < max_wake {
                slot.woken = true;
                crate::privileged::proc::process_unblock(slot.pid);
                woken += 1;
            } else if requeued < max_requeue {
                slot.uaddr = uaddr2;
                requeued += 1;
            } else {
                break;
            }
        }

        // 清理已唤醒的
        for slot in &mut self.waiters {
            if slot.woken {
                self.count -= 1;
                *slot = FutexWaiter::empty();
            }
        }

        (woken, requeued)
    }
}

/// 全局 futex 哈希表 (桶数组与序号数组均在堆上, 避免约 16.5 KB 常驻 BSS)
struct FutexHashTable {
    /// 每桶唤醒序号: 唤醒/迁移在桶锁内推进, 等待者在同一锁域内复查 ——
    /// 序号变化即"快照之后发生过唤醒", 等待者必须放弃入睡并重试.
    seqs: Box<[AtomicU64]>,
    /// 每桶一把 PI 互斥锁: 桶数据由它守护, 争用时按 EEVDF 权重阻塞排队.
    buckets: Box<[PiMutex<FutexBucket>]>,
}

impl FutexHashTable {
    /// 逐元素构造空表 (堆分配桶数组 + 序号数组; 桶锁不额外分配).
    fn new() -> Self {
        Self {
            seqs: (0..FUTEX_HASH_BUCKETS).map(|_| AtomicU64::new(0)).collect(),
            buckets: (0..FUTEX_HASH_BUCKETS)
                .map(|_| PiMutex::new(FutexBucket::empty()))
                .collect(),
        }
    }
}

/// 全局 futex 哈希表 — 惰性堆置, `OnceLock` 内联仅数十字节。
///
/// 首次 futex 系统调用时在 kmalloc 堆上逐元素构造桶数组、序号数组与桶锁。
static FUTEX_TABLE: OnceLock<FutexHashTable> = OnceLock::new();

/// 获取全局 futex 哈希表 (首次调用时惰性分配)。
fn futex_table() -> &'static FutexHashTable {
    FUTEX_TABLE.get_or_init(|slot| {
        slot.write(FutexHashTable::new());
    })
}

/// 计算哈希桶索引
fn hash_uaddr(uaddr: u64) -> usize {
    let hash = (uaddr.wrapping_mul(0x9E37_79B9_7F4A_7C15)) >> 58;
    (hash as usize) & (FUTEX_HASH_BUCKETS - 1)
}

/// futex 桶锁的基础权重缺省值 (nice 0 的 EEVDF 权重 = 1024); 进程表查不到时兜底.
const FUTEX_DEFAULT_WEIGHT: u32 = 1024;

/// 调用线程的基础 EEVDF 权重, 作为桶锁排队的捐赠输入.
///
/// 权重快照与 PI 等待者入队同源: 入队后线程再改 nice 不会刷新本桶的等待权重
/// (与 `PiMutex` 的等待者权重快照口径一致).
fn futex_task_weight(pid: u32) -> u32 {
    crate::privileged::proc::PROCESS_TABLE
        .with_process(pid, |p| p.cfs_weight.load(Ordering::Acquire))
        .and_then(|weight| u32::try_from(weight).ok())
        .unwrap_or(FUTEX_DEFAULT_WEIGHT)
}

// ============================================================================
// 序号守卫与超时换算
// ============================================================================

/// 唤醒 uaddr 上的等待者, 并在桶锁内推进桶序号 (调用方已持有该桶锁).
///
/// 序号推进必须与唤醒扫描同处一个锁域: 等待者以"快照序号 → 读等待字 → 持锁
/// 复查序号"三步入队, 只要快照之后发生过一次唤醒, 复查即失败并返回 `EAGAIN`
/// 重试. 唤醒按 uaddr 匹配, 但序号是桶级的 —— 同桶不同地址的唤醒会带来一次
/// 多余的 `EAGAIN` 重试, 换取的是一把锁内的单一序号, 无额外扫描.
fn wake_with_seq(seq: &AtomicU64, bucket: &mut FutexBucket, uaddr: u64, max_count: u32) -> u32 {
    seq.fetch_add(1, Ordering::SeqCst);
    bucket.wake(uaddr, max_count)
}

/// 迁移 uaddr 上的等待者, 并在源桶锁内推进序号 (调用方已持有该桶锁).
fn requeue_with_seq(
    seq: &AtomicU64,
    bucket: &mut FutexBucket,
    uaddr: u64,
    max_wake: u32,
    uaddr2: u64,
    max_requeue: u32,
) -> (u32, u32) {
    seq.fetch_add(1, Ordering::SeqCst);
    bucket.requeue(uaddr, max_wake, uaddr2, max_requeue)
}

/// 入队等待者: 序号仍等于快照值才写入 (调用方已持有该桶锁).
///
/// 返回 `false` 表示快照之后发生过唤醒 (序号已推进) 或桶已满 —— 调用方必须
/// 放弃本次等待并返回 `EAGAIN`.
fn enqueue_if_seq_unchanged(
    seq: &AtomicU64,
    snapshot: u64,
    bucket: &mut FutexBucket,
    waiter: FutexWaiter,
) -> bool {
    if seq.load(Ordering::SeqCst) != snapshot {
        return false;
    }
    bucket.push(waiter)
}

/// 解析 `FUTEX_WAIT_BITSET` 的绝对超时 (`timespec` 秒/纳秒 → 纳秒).
///
/// `timeout_ptr == 0` 表示无超时; 指针越界返回 `-EFAULT`, 字段非法返回 `-EINVAL`.
fn parse_abs_deadline_ns(timeout_ptr: u64) -> Result<Option<u64>, i64> {
    const EFAULT: i64 = -14;

    /// `struct timespec` (Linux 用户 ABI, 16 字节)
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    if timeout_ptr == 0 {
        return Ok(None);
    }
    if !super::raw::check_user_buf(timeout_ptr, 16) {
        return Err(EFAULT);
    }
    // SAFETY: check_user_buf 已确认 [timeout_ptr, timeout_ptr+16) 落在用户地址
    // 空间, 且当前进程页表处于激活状态 (系统调用上下文); 仅只读拷贝一次.
    let ts = unsafe { core::ptr::read_unaligned(timeout_ptr as *const Timespec) };
    abs_deadline_ns(ts.tv_sec, ts.tv_nsec).map(Some)
}

/// 校验并换算绝对超时: 负值或越界纳秒字段按 `-EINVAL` 拒绝.
fn abs_deadline_ns(sec: i64, nsec: i64) -> Result<u64, i64> {
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return Err(-22); // -EINVAL
    }
    Ok(sec
        .cast_unsigned()
        .saturating_mul(1_000_000_000)
        .saturating_add(nsec.cast_unsigned()))
}

/// 距离截止时刻的剩余毫秒 (向上取整; 已过期返回 0).
fn remaining_ms_ceil(deadline_ns: u64, now_ns: u64) -> u64 {
    deadline_ns.saturating_sub(now_ns).div_ceil(1_000_000)
}

/// 绝对截止时刻 → 定时唤醒的 tick 截止值.
///
/// 已经过期的截止时刻换算为"当前 tick", tick 扫描会在下一轮立即唤醒.
fn sleep_until_ticks(deadline_ns: u64) -> u64 {
    get_ticks().saturating_add(ms_to_ticks(remaining_ms_ceil(deadline_ns, get_uptime_ns())))
}

// ============================================================================
// Futex 系统调用实现
// ============================================================================

/// futex 系统调用入口
#[must_use]
pub fn sys_futex(uaddr: u64, op: i32, val: i32, timeout_or_uaddr2: u64, val2: u32) -> i64 {
    let base_op = futex_op(op);

    match base_op {
        FUTEX_WAIT => futex_wait(uaddr, val, timeout_or_uaddr2, false),
        FUTEX_WAIT_BITSET => futex_wait(uaddr, val, timeout_or_uaddr2, true),
        FUTEX_WAKE | FUTEX_WAKE_BITSET => futex_wake(uaddr, val.cast_unsigned()),
        FUTEX_REQUEUE => futex_requeue(uaddr, val.cast_unsigned(), timeout_or_uaddr2, val2),
        _ => {
            crate::klog_warn!(Sync, "[FUTEX] Unknown op {}", op);
            -(35i64) // -EAGAIN
        }
    }
}

/// `FUTEX_WAIT` / `FUTEX_WAIT_BITSET`: 原子比较并阻塞
///
/// 睡前的三个动作 —— 复查桶序号、入队等待者、置 Blocked —— 在同一个桶锁域内
/// 完成. 任何一项拆到锁外都会留下丢唤醒窗口:
/// - 只查序号不入队: 唤醒仍可落在检查与入队之间;
/// - 先入队后阻塞: 落在两步之间的唤醒被 `Scheduler::unblock` 的 Running 判据
///   丢弃 (线程仍在核上), 等待者随后永久睡在队列外;
/// - 不关中断: 置 Blocked 后若被本核中断退出路径切走, 本线程既不在运行队列也没有
///   唤醒者, 却仍持着桶锁 —— 唤醒方拿不到桶锁, 永久死锁. 故置 Blocked 到释放桶锁
///   之间关中断 (桶锁本身是阻塞锁, 关中断只能包住这段收尾, 不能包住获取).
///
/// `FUTEX_WAIT_BITSET` 的绝对超时按 `CLOCK_MONOTONIC` 解释, 与 `get_uptime_ns`
/// 同纪元; 到期由 tick 睡眠扫描兜底唤醒, 醒来后仍留在队列中即返回 `-ETIMEDOUT`.
fn futex_wait(uaddr: u64, val: i32, timeout_ptr: u64, is_bitset: bool) -> i64 {
    const EAGAIN: i64 = -11;
    const EFAULT: i64 = -14;
    const EINVAL: i64 = -22;
    const ETIMEDOUT: i64 = -110;

    if !super::raw::check_user_ptr(uaddr) {
        return EFAULT;
    }
    let current_pid = crate::privileged::proc::process_get_current_pid();
    if current_pid == 0 {
        return EINVAL;
    }

    let table = futex_table();
    let bucket_idx = hash_uaddr(uaddr);
    // 序号快照必须早于等待字读取: 唤醒方先改字再唤醒; 若快照晚于该唤醒,
    // 读取一定晚于用户存储 (Release/Acquire 链), 会看到新值并直接走 EAGAIN.
    let seq_snapshot = table.seqs[bucket_idx].load(Ordering::SeqCst);

    // SAFETY: check_user_ptr 已限定 uaddr ∈ [1, USER_ADDR_MAX); 4 字节对齐由
    // functions 层 `futex_validate_uaddr` 保证; 当前进程页表处于激活状态.
    let current_val = unsafe { (*(uaddr as *const AtomicU32)).load(Ordering::Acquire) };
    if current_val != val.cast_unsigned() {
        return EAGAIN;
    }

    // SIMPLIFIED: 仅 FUTEX_WAIT_BITSET 携带绝对超时; FUTEX_WAIT 的相对超时、
    // FUTEX_CLOCK_REALTIME 时钟选择与 val2 位集过滤本轮均不接线 (等待按全体
    // 等待者语义). 影响面: 用 FUTEX_WAIT+相对超时的用户态库仍会无限等待, 需
    // 改用 BITSET 变体; 位集过滤缺省等于全 1. 何时需扩展: 出现依赖相对超时或
    // 位集分组的用户态同步库时补接线.
    let deadline_ns = if is_bitset {
        match parse_abs_deadline_ns(timeout_ptr) {
            Ok(deadline) => deadline,
            Err(errno) => return errno,
        }
    } else {
        None
    };
    // 已过期的绝对超时: 值仍等于 val 时按超时返回 (Linux 语义: 不入睡).
    if deadline_ns.is_some_and(|deadline| deadline <= get_uptime_ns()) {
        return ETIMEDOUT;
    }

    let mut bucket = table.buckets[bucket_idx].lock(current_pid, futex_task_weight(current_pid));
    let mut queued = enqueue_if_seq_unchanged(
        &table.seqs[bucket_idx],
        seq_snapshot,
        &mut bucket,
        FutexWaiter {
            uaddr,
            pid: current_pid,
            woken: false,
        },
    );
    if queued {
        let reason = if deadline_ns.is_some() {
            crate::privileged::proc::BlockReason::Sleeping
        } else {
            crate::privileged::proc::BlockReason::FutexWait
        };
        let sleep_until = deadline_ns.map_or(0, sleep_until_ticks);
        // 置 Blocked 与释放桶锁必须原子于中断: 中间被切走会带着桶锁离开运行队列.
        let flags = disable_interrupts();
        queued = crate::privileged::proc::process_block_prepare(current_pid, reason, sleep_until);
        if !queued {
            bucket.remove_by_pid(current_pid);
        }
        drop(bucket);
        restore_interrupts(flags);
    }

    if !queued {
        return EAGAIN; // 快照后被唤醒 / 桶满 / 非本核当前线程
    }

    // 阻塞切换 + 唤醒返回处的致命信号收口 (组退出 / de_thread).
    crate::privileged::proc::process_block_finish(current_pid);

    // 醒来后移除自己: 唤醒路径已在同一锁域内清槽, 仍留在队列里说明是定时扫描
    // 唤醒; 再按截止时刻给出超时结论, 否则按被唤醒 (或伪唤醒) 返回 0.
    let mut bucket = table.buckets[bucket_idx].lock(current_pid, futex_task_weight(current_pid));
    let still_queued = bucket.remove_by_pid(current_pid);
    drop(bucket);

    if still_queued && deadline_ns.is_some_and(|deadline| get_uptime_ns() >= deadline) {
        return ETIMEDOUT;
    }
    0
}

/// `FUTEX_WAKE`: 唤醒等待者
///
/// 公开给 `privileged::proc::robust` 退出路径使用 (robust futex / CLEARTID 唤醒),
/// 故**绝不睡眠**: 退出路径不可阻塞, 只能以 `pid 0` (内核上下文) 非阻塞抢桶锁,
/// 抢不到就自旋重试. 桶内临界区本身只有等待者扫描与唤醒, 但 `PiMutex` 的定向
/// 移交会把已释放的锁挂在尚未上核的目标名下; 内核上下文因此被允许接管未接收的
/// 移交 (见 `PiMutex::try_acquire`), 重试循环只会撞上真正在执行临界区的持有者,
/// 不会因移交目标等不到 CPU 而永久自旋 (A-7 冻结归因).
#[must_use]
pub fn futex_wake(uaddr: u64, max_count: u32) -> i64 {
    if max_count == 0 {
        return 0;
    }

    let bucket_idx = hash_uaddr(uaddr);
    let table = futex_table();
    let woken = loop {
        let Some(mut bucket) = table.buckets[bucket_idx].try_lock_guard(0, 0) else {
            core::hint::spin_loop();
            continue;
        };
        break wake_with_seq(&table.seqs[bucket_idx], &mut bucket, uaddr, max_count);
    };

    i64::from(woken)
}

/// `FUTEX_REQUEUE`: 迁移等待者
fn futex_requeue(uaddr: u64, max_wake: u32, uaddr2: u64, max_requeue: u32) -> i64 {
    let bucket_idx = hash_uaddr(uaddr);
    let table = futex_table();
    let current_pid = crate::privileged::proc::process_get_current_pid();
    let mut bucket = table.buckets[bucket_idx].lock(current_pid, futex_task_weight(current_pid));
    let (woken, requeued) = requeue_with_seq(
        &table.seqs[bucket_idx],
        &mut bucket,
        uaddr,
        max_wake,
        uaddr2,
        max_requeue,
    );

    i64::from(woken + requeued)
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_futex_op_mask() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, assert_eq_test};
    assert_eq_test!(futex_op(0), 0, "FUTEX_WAIT");
    assert_eq_test!(futex_op(1), 1, "FUTEX_WAKE");
    assert_eq_test!(futex_op(128), 0, "FUTEX_WAIT_PRIVATE");
    assert_eq_test!(futex_op(129), 1, "FUTEX_WAKE_PRIVATE");
    assert_eq_test!(futex_op(3), 3, "FUTEX_REQUEUE");
    assert_eq_test!(futex_op(131), 3, "FUTEX_REQUEUE_PRIVATE");
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_futex_hash() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    for &addr in &[0x1000u64, 0x2000, 0x7FFF_0000_0000, 0xDEAD_BEEF] {
        let idx = hash_uaddr(addr);
        check!(idx < FUTEX_HASH_BUCKETS, "hash in range");
    }
    check!(
        hash_uaddr(0x1000) != hash_uaddr(0x2000) || hash_uaddr(0x1000) != hash_uaddr(0x3000),
        "hashes differ"
    );
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_futex_bucket_push_remove() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};
    let mut bucket = FutexBucket::empty();
    check!(bucket.count == 0, "empty bucket");

    let ok = bucket.push(FutexWaiter {
        uaddr: 0x1000,
        pid: 1,
        woken: false,
    });
    check!(ok, "push succeeds");
    check!(bucket.count == 1, "count after push");

    let ok2 = bucket.push(FutexWaiter {
        uaddr: 0x1000,
        pid: 2,
        woken: false,
    });
    check!(ok2, "push 2 succeeds");
    check!(bucket.count == 2, "count after push 2");

    bucket.remove_by_pid(1);
    check!(bucket.count == 1, "count after remove");

    bucket.remove_by_pid(2);
    check!(bucket.count == 0, "count after remove all");
    TestResult::Pass
}

/// 注册 futex 子系统的内核测试用例.
#[cfg(feature = "kernel_test")]
pub fn register_futex_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("futex", "op_mask", test_futex_op_mask);
    r.register("futex", "hash", test_futex_hash);
    r.register("futex", "bucket_push_remove", test_futex_bucket_push_remove);
}

// ============================================================================
// 主机侧确定性用例 (host-test: `make test-kernel-host`)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn waiter(uaddr: u64, pid: u32) -> FutexWaiter {
        FutexWaiter {
            uaddr,
            pid,
            woken: false,
        }
    }

    /// 丢唤醒复现: 等待者快照之后、入队之前发生的唤醒即使队列为空也必须留下
    /// 序号痕迹, 使入队复查失败并让等待者重试.
    #[test]
    fn wake_bumps_bucket_seq_even_without_waiters() {
        let seq = AtomicU64::new(0);
        let mut bucket = FutexBucket::empty();
        let snapshot = seq.load(Ordering::SeqCst);

        let woken = wake_with_seq(&seq, &mut bucket, 0x1000, 1);
        assert_eq!(woken, 0);
        assert_eq!(seq.load(Ordering::SeqCst), snapshot + 1);

        assert!(!enqueue_if_seq_unchanged(
            &seq,
            snapshot,
            &mut bucket,
            waiter(0x1000, 7)
        ));
        assert_eq!(bucket.count, 0);
    }

    /// 序号未变时入队成功; 唤醒推进序号后, 同一快照不再允许入队.
    #[test]
    fn enqueue_after_unchanged_snapshot_succeeds() {
        let seq = AtomicU64::new(5);
        let mut bucket = FutexBucket::empty();
        let snapshot = seq.load(Ordering::SeqCst);
        assert!(enqueue_if_seq_unchanged(
            &seq,
            snapshot,
            &mut bucket,
            waiter(0x2000, 8)
        ));
        assert_eq!(bucket.count, 1);

        assert_eq!(wake_with_seq(&seq, &mut bucket, 0x2000, 1), 1);
        assert_eq!(bucket.count, 0);
        assert!(!enqueue_if_seq_unchanged(
            &seq,
            snapshot,
            &mut bucket,
            waiter(0x2000, 9)
        ));
    }

    /// 迁移推进源桶序号并改写等待者地址; 迁移不产生唤醒.
    #[test]
    fn requeue_bumps_source_seq_and_moves_waiters() {
        let seq = AtomicU64::new(0);
        let mut bucket = FutexBucket::empty();
        assert!(bucket.push(waiter(0x3000, 10)));

        let (woken, requeued) = requeue_with_seq(&seq, &mut bucket, 0x3000, 0, 0x4000, 1);
        assert_eq!((woken, requeued), (0, 1));
        assert_eq!(seq.load(Ordering::SeqCst), 1);
        assert_eq!(bucket.waiters[0].uaddr, 0x4000);
    }

    /// 自移除返回是否命中: 唤醒路径已清槽时必须返回 `false` (用于超时判据).
    #[test]
    fn remove_by_pid_reports_removal() {
        let mut bucket = FutexBucket::empty();
        assert!(!bucket.remove_by_pid(11));

        assert!(bucket.push(waiter(0x5000, 11)));
        assert!(bucket.remove_by_pid(11));
        assert_eq!(bucket.count, 0);
        assert!(!bucket.remove_by_pid(11));
    }

    /// 绝对超时字段校验: 负值/越界纳秒按 EINVAL 拒绝, 大幅值按 u64 饱和.
    #[test]
    fn abs_deadline_validation() {
        assert_eq!(abs_deadline_ns(0, 0), Ok(0));
        assert_eq!(abs_deadline_ns(2, 500_000_000), Ok(2_500_000_000));
        assert_eq!(abs_deadline_ns(-1, 0), Err(-22));
        assert_eq!(abs_deadline_ns(0, -1), Err(-22));
        assert_eq!(abs_deadline_ns(0, 1_000_000_000), Err(-22));
        assert_eq!(abs_deadline_ns(i64::MAX, 999_999_999), Ok(u64::MAX));
    }

    /// 剩余毫秒向上取整: 1ns 也占 1ms, 已过期钳到 0.
    #[test]
    fn remaining_ms_rounds_up() {
        assert_eq!(remaining_ms_ceil(0, 0), 0);
        assert_eq!(remaining_ms_ceil(1, 0), 1);
        assert_eq!(remaining_ms_ceil(1_000_000, 0), 1);
        assert_eq!(remaining_ms_ceil(1_000_001, 0), 2);
        assert_eq!(remaining_ms_ceil(10, 20), 0);
    }

    /// 新建表的桶序号全部从 0 起步 (与 `futex_table()` 惰性初始化同源).
    #[test]
    fn table_starts_with_zeroed_bucket_seqs() {
        let table = FutexHashTable::new();
        assert_eq!(table.seqs.len(), FUTEX_HASH_BUCKETS);
        assert!(table.seqs.iter().all(|s| s.load(Ordering::SeqCst) == 0));
    }
}
