//! eventfd — 事件通知文件描述符 (TCB)
//!
//! 实现 Linux eventfd API: eventfd / eventfd2.
//!
//! ## 架构
//!
//! ```text
//! EventFdTable (全局, IrqSpinLock 保护)
//!   └── [EventFdSlot; EFD_MAX_SLOTS]
//!         ├── counter: u64     (内核计数器)
//!         ├── semaphore: bool  (EFD_SEMAPHORE 模式)
//!         └── used: bool
//!
//! FD 空间: [1100, 1100 + EFD_MAX_SLOTS)
//!   fd 200 → slot 0, fd 201 → slot 1, ...
//!
//! read(fd):  semaphore=false → 返回 counter 并清零
//!            semaphore=true  → 返回 1 并 counter -= 1 (若 counter > 0)
//!            counter=0       → EAGAIN (非阻塞) 或阻塞
//!
//! write(fd, value): counter += value; 若溢出 → EAGAIN
//!                   value=0 → EINVAL
//!
//! epoll 集成: counter > 0 → EPOLLIN; counter < U64_MAX → EPOLLOUT
//! ```
//!
//! # Safety
//!
//! - `EventFdTable` 由 `IrqSpinLock` 保护, 中断安全
//! - 用户指针在 functions 层 `check_user_buf` 校验后才进入 TCB
//! - counter 使用 u64, 原子性由锁保证

use core::sync::atomic::{AtomicU32, Ordering};

use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::syscall::Errno;

// ============================================================================
// 常量
// ============================================================================

/// eventfd 最大实例数
pub const EFD_MAX_SLOTS: usize = 16;
/// TD-02: 基址来源已迁移至 `privileged::proc::FdPlan::EVENT_FD` 单一来源, 不再硬编码.
pub const EFD_FD_BASE: i32 = crate::privileged::proc::FdPlan::EVENT_FD.base;
/// `EFD_CLOEXEC` (与 Linux 一致)
pub const EFD_CLOEXEC: i32 = 0o2000000;
/// `EFD_NONBLOCK` (与 Linux 一致)
pub const EFD_NONBLOCK: i32 = 0o4000;
/// `EFD_SEMAPHORE` (与 Linux 一致)
pub const EFD_SEMAPHORE: i32 = 0o1;

// ============================================================================
// 数据结构
// ============================================================================

/// eventfd 槽位
struct EventFdSlot {
    /// 内核计数器
    counter: u64,
    /// 是否为信号量模式
    semaphore: bool,
    /// 是否已使用
    used: bool,
}

impl EventFdSlot {
    const fn new() -> Self {
        Self {
            counter: 0,
            semaphore: false,
            used: false,
        }
    }
}

/// eventfd 全局表
struct EventFdTable {
    slots: [EventFdSlot; EFD_MAX_SLOTS],
}

impl EventFdTable {
    const fn new() -> Self {
        Self {
            slots: [const { EventFdSlot::new() }; EFD_MAX_SLOTS],
        }
    }
}

/// 全局 eventfd 表
static EFD_TABLE: Mutex<EventFdTable> = Mutex::new(EventFdTable::new());

/// 已分配的 eventfd 数量
static EFD_COUNT: AtomicU32 = AtomicU32::new(0);

// ============================================================================
// 系统调用实现
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// eventfd — 创建 eventfd 实例
///
/// `initval`: 初始计数器值
/// `flags`: `EFD_CLOEXEC` | `EFD_NONBLOCK` | `EFD_SEMAPHORE`
/// 返回 fd (≥ 200), 或负 errno
pub fn sys_eventfd(initval: u64, flags: i32) -> i64 {
    // flags 校验: 只允许已知标志
    let valid_flags = EFD_CLOEXEC | EFD_NONBLOCK | EFD_SEMAPHORE;
    if flags & !valid_flags != 0 {
        return Errno::EINVAL.as_ret();
    }

    let semaphore = (flags & EFD_SEMAPHORE) != 0;

    // V2: 使用集中分配器获取 FD
    let fd = match crate::privileged::proc::fd_alloc::alloc_fd(
        crate::privileged::proc::fd_alloc::FdSubsystem::EventFd,
    ) {
        Some(f) => f,
        None => return Errno::EMFILE.as_ret(),
    };

    // V2: FD 编号由 alloc_fd 计算 (底层使用 fd_at(EventFd, slot))
    let slot = match crate::privileged::proc::fd_alloc::idx_of(fd) {
        Some((_sub, s)) => s,
        None => return Errno::EBADF.as_ret(),
    };

    let mut table = EFD_TABLE.lock();
    let s = &mut table.slots[slot];
    s.used = true;
    s.counter = initval;
    s.semaphore = semaphore;
    EFD_COUNT.fetch_add(1, Ordering::Relaxed);

    crate::klog_debug!(
        Sync,
        "[eventfd] Created fd={} initval={} sem={}",
        fd,
        initval,
        semaphore
    );
    i64::from(fd)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// eventfd read — 读取计数器
///
/// - semaphore=false: 返回 counter 并清零
/// - semaphore=true:  返回 1 并 counter -= 1
/// - counter=0:       返回 EAGAIN
///
/// `fd`: eventfd 文件描述符
/// `buf`: 用户空间 8 字节缓冲区指针
/// 返回 8 (成功), 或负 errno
pub fn sys_eventfd_read(fd: i32, buf: u64) -> i64 {
    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    let mut table = EFD_TABLE.lock();
    let slot = &mut table.slots[idx];

    if !slot.used {
        return Errno::EBADF.as_ret();
    }

    if slot.counter == 0 {
        return Errno::EAGAIN.as_ret();
    }

    let value = if slot.semaphore {
        slot.counter -= 1;
        1u64
    } else {
        let v = slot.counter;
        slot.counter = 0;
        v
    };

    drop(table);

    // 写入用户空间 (8 字节 u64)
    if buf == 0 {
        return Errno::EFAULT.as_ret();
    }
    // SAFETY: buf 由 syscall 入口验证, 8 字节对齐写入 u64
    unsafe {
        core::ptr::write(buf as *mut u64, value);
    }

    8 // 成功读取 8 字节
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// eventfd write — 递增计数器
///
/// `fd`: eventfd 文件描述符
/// `value`: 要增加的值 (必须 > 0, 且 ≤ `U64_MAX` - 1)
/// 返回 8 (成功), 或负 errno
pub fn sys_eventfd_write(fd: i32, value: u64) -> i64 {
    if value == 0 {
        return Errno::EINVAL.as_ret();
    }

    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    let mut table = EFD_TABLE.lock();
    let slot = &mut table.slots[idx];

    if !slot.used {
        return Errno::EBADF.as_ret();
    }

    // 溢出检查: counter + value > U64_MAX - 1
    if slot.counter > u64::MAX - 1 - value {
        return Errno::EAGAIN.as_ret();
    }

    slot.counter += value;

    crate::klog_debug!(
        Sync,
        "[eventfd] Write fd={} value={} counter={}",
        fd,
        value,
        slot.counter
    );
    8
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// eventfd close — 关闭 eventfd
///
/// 释放槽位, 返回 0 或负 errno
pub fn sys_eventfd_close(fd: i32) -> i64 {
    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    let mut table = EFD_TABLE.lock();
    let slot = &mut table.slots[idx];

    if !slot.used {
        return Errno::EBADF.as_ret();
    }

    slot.used = false;
    slot.counter = 0;
    slot.semaphore = false;
    EFD_COUNT.fetch_sub(1, Ordering::Relaxed);

    drop(table);

    // TD-04: close 路径必须 epoll_pwake — 否则 epoll_wait 可能永远睡在已关闭 fd 上,
    // 后续 slot 复用时看到的是新 eventfd 的事件, 进程侧拿到 stale fd 句柄.
    // 必须在释放 EFD_TABLE 锁之后再唤醒, 让 epoll_waiter 看到 slot.used=false → EPOLLERR.
    crate::privileged::syscall::epoll_pwake(fd);

    crate::klog_debug!(Sync, "[eventfd] Closed fd={}", fd);
    0
}

// ============================================================================
// epoll 集成
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 检查 eventfd 是否就绪 (供 epoll `check_fd_ready` 调用)
///
/// 返回 EPOLLIN (可读) / EPOLLOUT (可写) 事件掩码
pub fn eventfd_poll_events(fd: i32) -> u32 {
    use crate::privileged::syscall::{EPOLLERR, EPOLLIN, EPOLLOUT};

    let idx = match fd_to_idx(fd) {
        Some(i) => i,
        None => return EPOLLERR,
    };

    let table = EFD_TABLE.lock();
    let slot = &table.slots[idx];

    if !slot.used {
        return EPOLLERR;
    }

    let mut events = 0u32;
    if slot.counter > 0 {
        events |= EPOLLIN; // 可读
    }
    if slot.counter < u64::MAX - 1 {
        events |= EPOLLOUT; // 可写 (不会溢出)
    }

    events
}

// ============================================================================
// 辅助函数
// ============================================================================

/// fd → 槽位索引
///
/// TD-02 V4: 改走 `fd_alloc::idx_of` 集中反查, 本地不再持有 `EFD_FD_BASE` 字面量 +
/// 减法边界检查.
fn fd_to_idx(fd: i32) -> Option<usize> {
    match crate::privileged::proc::idx_of(fd) {
        Some((crate::privileged::proc::FdSubsystem::EventFd, slot)) => Some(slot),
        _ => None,
    }
}

/// 检查 fd 是否属于 eventfd 空间
///
/// TD-02 V4: 改走 `fd_alloc::idx_of`, 不再持有 `EFD_FD_BASE` 字面量 + 算术.
pub fn is_eventfd_fd(fd: i32) -> bool {
    matches!(
        crate::privileged::proc::idx_of(fd),
        Some((crate::privileged::proc::FdSubsystem::EventFd, _))
    )
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_eventfd_create_read_write() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    // 创建 eventfd, initval=5
    let fd = sys_eventfd(5, 0);
    check!(fd >= 200, "eventfd returns fd >= 200");

    // 读取: 非 semaphore 模式, 返回 5 并清零
    let mut val: u64 = 0;
    let ret = sys_eventfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret == 8, "eventfd read returns 8");
    check!(val == 5, "eventfd read value == 5");

    // 再次读取: counter=0, EAGAIN
    let ret2 = sys_eventfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret2 < 0, "eventfd read empty returns error");

    // 写入 10
    let ret3 = sys_eventfd_write(fd as i32, 10);
    check!(ret3 == 8, "eventfd write returns 8");

    // 读取: 返回 10
    let ret4 = sys_eventfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret4 == 8, "eventfd read after write returns 8");
    check!(val == 10, "eventfd read value == 10");

    // 关闭
    let ret5 = sys_eventfd_close(fd as i32);
    check!(ret5 == 0, "eventfd close returns 0");

    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_eventfd_semaphore() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    // 创建 semaphore 模式 eventfd, initval=3
    let fd = sys_eventfd(3, EFD_SEMAPHORE);
    check!(fd >= 200, "eventfd semaphore returns fd >= 200");

    // 读取: 返回 1, counter=2
    let mut val: u64 = 0;
    let ret = sys_eventfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret == 8, "semaphore read returns 8");
    check!(val == 1, "semaphore read value == 1");

    // 再读: 返回 1, counter=1
    let ret2 = sys_eventfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret2 == 8, "semaphore read 2 returns 8");
    check!(val == 1, "semaphore read 2 value == 1");

    // 再读: 返回 1, counter=0
    let ret3 = sys_eventfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret3 == 8, "semaphore read 3 returns 8");

    // 再读: EAGAIN
    let ret4 = sys_eventfd_read(fd as i32, (&raw mut val) as u64);
    check!(ret4 < 0, "semaphore read empty returns error");

    sys_eventfd_close(fd as i32);
    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_eventfd_poll() -> crate::privileged::tests::TestResult {
    use crate::privileged::syscall::{EPOLLIN, EPOLLOUT};
    use crate::privileged::tests::{TestResult, check};

    let fd = sys_eventfd(0, 0);
    check!(fd >= 200, "eventfd for poll ok");

    // counter=0: 仅 EPOLLOUT
    let events = eventfd_poll_events(fd as i32);
    check!(events & EPOLLIN == 0, "counter=0 not readable");
    check!(events & EPOLLOUT != 0, "counter=0 writable");

    // 写入 1: EPOLLIN + EPOLLOUT
    sys_eventfd_write(fd as i32, 1);
    let events2 = eventfd_poll_events(fd as i32);
    check!(events2 & EPOLLIN != 0, "counter>0 readable");
    check!(events2 & EPOLLOUT != 0, "counter>0 writable");

    sys_eventfd_close(fd as i32);
    TestResult::Pass
}

/// 注册 eventfd 子系统的内核测试用例.
#[cfg(feature = "kernel_test")]
pub fn register_eventfd_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register(
        "eventfd",
        "create_read_write",
        test_eventfd_create_read_write,
    );
    r.register("eventfd", "semaphore", test_eventfd_semaphore);
    r.register("eventfd", "poll", test_eventfd_poll);
}
