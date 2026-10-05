//! Futex — 快速用户空间互斥锁
//!
//! 实现 Linux 风格的 futex 系统调用, 支持用户态高效同步原语.
//!
//! ## 核心语义
//!
//! - `FUTEX_WAIT(op=0)`: 原子比较 *uaddr == val, 若相等则阻塞当前线程
//! - `FUTEX_WAKE(op=1)`: 唤醒最多 val 个等待在 uaddr 上的线程
//! - `FUTEX_REQUEUE(op=3)`: 将最多 val 个等待者从 uaddr 迁移到 uaddr2
//!
//! ## 等待队列
//!
//! 使用全局哈希表将 uaddr 映射到等待者, 哈希表以自旋锁保护.
//! 每个桶使用固定大小数组, 避免动态内存分配.
//!
//! # Safety
//!
//! - `uaddr` 必须是合法的用户空间指针, 在 syscall 入口已通过 `check_user_ptr` 验证
//! - 原子比较使用 `AtomicU32` 访问用户空间, 需确保页表映射有效

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::privileged::sync::OnceLock;

// ============================================================================
// Futex 操作码 (与 Linux 兼容)
// ============================================================================

/// 等待: 若 *uaddr == val, 阻塞当前线程
pub const FUTEX_WAIT: i32 = 0;
/// 唤醒: 唤醒最多 val 个等待者
pub const FUTEX_WAKE: i32 = 1;
/// 带超时等待 (暂不支持超时, 语义同 `FUTEX_WAIT`)
pub const FUTEX_WAIT_BITSET: i32 = 9;
/// 唤醒指定位集 (暂等同于 `FUTEX_WAKE`)
pub const FUTEX_WAKE_BITSET: i32 = 10;
/// 迁移等待者: 将最多 val 个等待者从 uaddr 迁移到 uaddr2
pub const FUTEX_REQUEUE: i32 = 3;

/// 私有标志: futex 位于进程内共享内存 (同一地址空间)
pub const FUTEX_PRIVATE_FLAG: i32 = 128;

/// 提取基础操作码 (去掉 `PRIVATE/CLOCK_REALTIME` 等标志)
fn futex_op(op: i32) -> i32 {
    op & !FUTEX_PRIVATE_FLAG & !0x100 // 去掉 FUTEX_CLOCK_REALTIME
}

// ============================================================================
// 简易自旋锁 (Interior Mutability)
// ============================================================================

/// 基于 `AtomicBool` 的简易自旋锁, 支持 &self 锁定
struct SimpleSpinLock {
    locked: AtomicBool,
}

impl SimpleSpinLock {
    fn lock(&self) {
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
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

    /// 移除指定 PID 的等待者
    fn remove_by_pid(&mut self, pid: u32) {
        for slot in &mut self.waiters {
            if slot.pid == pid {
                *slot = FutexWaiter::empty();
                self.count -= 1;
                return;
            }
        }
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

/// 全局 futex 哈希表 (桶数组与锁数组均在堆上, 避免约 16.5 KB 常驻 BSS)
struct FutexHashTable {
    locks: Box<[SimpleSpinLock]>,
    buckets: Box<[UnsafeCell<FutexBucket>]>,
}

// SAFETY: FutexHashTable 的每个桶由独立的 SimpleSpinLock 保护,
// SAFETY: FutexHashTable 含 UnsafeCell, 但不同桶可以并发访问, 同一桶内的访问通过 lock/unlock 序列化.
unsafe impl Sync for FutexHashTable {}
// SAFETY: 同上, 桶级锁保证并发安全.
unsafe impl Send for FutexHashTable {}

/// 全局 futex 哈希表 — 惰性堆置, `OnceLock` 内联仅数十字节。
///
/// 首次 futex 系统调用时在 kmalloc 堆上逐元素构造两个桶数组。
static FUTEX_TABLE: OnceLock<FutexHashTable> = OnceLock::new();

/// 获取全局 futex 哈希表 (首次调用时惰性分配)。
fn futex_table() -> &'static FutexHashTable {
    FUTEX_TABLE.get_or_init(|slot| {
        slot.write(FutexHashTable {
            locks: (0..FUTEX_HASH_BUCKETS)
                .map(|_| SimpleSpinLock {
                    locked: AtomicBool::new(false),
                })
                .collect(),
            buckets: (0..FUTEX_HASH_BUCKETS)
                .map(|_| UnsafeCell::new(FutexBucket::empty()))
                .collect(),
        });
    })
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
/// 计算哈希桶索引
fn hash_uaddr(uaddr: u64) -> usize {
    let hash = (uaddr.wrapping_mul(0x9E3779B97F4A7C15)) >> 58;
    (hash as usize) & (FUTEX_HASH_BUCKETS - 1)
}

// ============================================================================
// Futex 系统调用实现
// ============================================================================

/// futex 系统调用入口
pub fn sys_futex(uaddr: u64, op: i32, val: i32, timeout_or_uaddr2: u64, val2: u32) -> i64 {
    let base_op = futex_op(op);

    match base_op {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => futex_wait(uaddr, val, timeout_or_uaddr2),
        FUTEX_WAKE | FUTEX_WAKE_BITSET => futex_wake(uaddr, val as u32),
        FUTEX_REQUEUE => futex_requeue(uaddr, val as u32, timeout_or_uaddr2, val2),
        _ => {
            crate::klog_warn!(Sync, "[FUTEX] Unknown op {}", op);
            -(35i64) // -EAGAIN
        }
    }
}

/// `FUTEX_WAIT`: 原子比较并阻塞
fn futex_wait(uaddr: u64, val: i32, _timeout: u64) -> i64 {
    // 1. 原子读取用户空间值
    let uaddr_ptr = uaddr as *const AtomicU32;
    if uaddr_ptr.is_null() {
        return -(14i64); // -EFAULT
    }

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let current_val = unsafe {
        // SAFETY: uaddr 已在 syscall 入口通过 check_user_ptr 验证.
        (*uaddr_ptr).load(Ordering::Acquire)
    };

    // 2. 原子比较: 若不匹配, 立即返回 EAGAIN
    if current_val != val as u32 {
        return -(11i64); // -EAGAIN
    }

    // 3. 获取当前 PID
    let current_pid = crate::privileged::proc::process_get_current_pid();
    if current_pid == 0 {
        return -(22i64); // -EINVAL
    }

    // 4. 加入等待队列
    let bucket_idx = hash_uaddr(uaddr);
    let table = futex_table();
    {
        table.locks[bucket_idx].lock();
        // SAFETY: 我们持有锁, 可以安全访问桶
        let bucket = unsafe { &mut *table.buckets[bucket_idx].get() };
        let added = bucket.push(FutexWaiter {
            uaddr,
            pid: current_pid,
            woken: false,
        });
        table.locks[bucket_idx].unlock();
        if !added {
            return -(11i64); // -EAGAIN: 桶满
        }
    }

    // 5. 阻塞当前线程
    crate::privileged::proc::process_block(current_pid);

    // 6. 被唤醒后, 从等待队列中移除自己
    {
        table.locks[bucket_idx].lock();
        // SAFETY: `table` 由 `futex_table()` 返回 `&'static`, 有效; 持锁下访问桶
        let bucket = unsafe { &mut *table.buckets[bucket_idx].get() };
        bucket.remove_by_pid(current_pid);
        table.locks[bucket_idx].unlock();
    }

    0
}

/// `FUTEX_WAKE`: 唤醒等待者
///
/// 公开给 `privileged::proc::robust` 退出路径使用 (robust futex / CLEARTID 唤醒).
pub fn futex_wake(uaddr: u64, max_count: u32) -> i64 {
    if max_count == 0 {
        return 0;
    }

    let bucket_idx = hash_uaddr(uaddr);
    let table = futex_table();
    let woken = {
        table.locks[bucket_idx].lock();
        // SAFETY: `table` 由 `futex_table()` 返回 `&'static`, 有效; 持锁下访问桶
        let bucket = unsafe { &mut *table.buckets[bucket_idx].get() };
        let w = bucket.wake(uaddr, max_count);
        table.locks[bucket_idx].unlock();
        w
    };

    i64::from(woken)
}

/// `FUTEX_REQUEUE`: 迁移等待者
fn futex_requeue(uaddr: u64, max_wake: u32, uaddr2: u64, max_requeue: u32) -> i64 {
    let bucket_idx = hash_uaddr(uaddr);
    let table = futex_table();
    let (woken, requeued) = {
        table.locks[bucket_idx].lock();
        // SAFETY: `table` 由 `futex_table()` 返回 `&'static`, 有效; 持锁下访问桶
        let bucket = unsafe { &mut *table.buckets[bucket_idx].get() };
        let r = bucket.requeue(uaddr, max_wake, uaddr2, max_requeue);
        table.locks[bucket_idx].unlock();
        r
    };

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
    for &addr in &[0x1000u64, 0x2000, 0x7FFF00000000, 0xDEADBEEF] {
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
