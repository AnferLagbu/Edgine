//! epoll — 事件轮询机制 (TCB)
//!
//! 实现 Linux epoll API: `epoll_create` / `epoll_ctl` / `epoll_wait`.
//!
//! ## 架构
//!
//! ```text
//! epoll_instance (哈希表存储 fd→event 映射, 完整集成 VFS poll + 阻塞语义)
//!   ├── interest_list: 注册的所有 fd + 事件
//!   ├── ready_list:    就绪的 fd + 事件 (epoll_wait 返回)
//!   └── wait_queue:    epoll_wait 阻塞时挂起当前线程
//!
//! epoll_ctl(ADD):  fd → interest_list
//! epoll_ctl(MOD):  修改 interest_list 中的事件
//! epoll_ctl(DEL):  从 interest_list 移除
//! epoll_wait:      ready_list → 用户空间 (无事件则挂入 wait_queue, 调度让出)
//! epoll_pwake(fd): 任意 fd 状态变化 (write/close) 时调用, 唤醒该 fd 注册的 epfd
//! ```
//!
//! ## VFS poll 集成
//!
//! - `check_fd_ready` 调用 `vfs_is_fd_valid` + `vfs_fd_type`, 推断真实事件:
//!   * `file_type=File/Empty` → EPOLLIN | EPOLLOUT (ramfs 内存常驻, 始终可读写)
//!   * `file_type=Dir`        → EPOLLIN (读目录项, 写 EPOLLOUT 不报告)
//!   * `file_type=Dev`        → EPOLLHUP (设备节点无可读字节流, 需驱动层注册)
//!   * `file_type=Symlink`    → EPOLLIN | EPOLLHUP (读 link target 后挂断)
//!   * 无效 fd             → EPOLLERR | EPOLLHUP
//!
//! ## 与 Linux 的差异
//!
//! - `interest_list` 使用 Vec 而非红黑树 (简化实现, fd 数量有限)
//! - `wait_queue` 容量 4 (复用 `ipc::types::WaitQueue` 简化版)
//! - 不支持 EPOLLEXCLUSIVE / EPOLLWAKEUP
//!
//! # Safety
//!
//! - epoll 实例通过全局 ID 分配, 避免指针悬挂
//! - `epoll_pwake` 在文件 I/O 路径 (write/close) 调用, 持锁时不可睡眠
//! - 阻塞在 `epoll_wait` 中调用 `SCHEDULER.yield_to_wait`, 无需额外锁保护

use crate::privileged::fs::vfs_poll_trait::{VfsPollContext, current_vfs_poll_policy};
use crate::privileged::ipc::{WaitQueue, WaitQueueItem};
use crate::privileged::sync::IrqSpinLock as Mutex;
use crate::privileged::syscall::Errno;
use alloc::vec::Vec;

// ============================================================================
// epoll 常量
// ============================================================================

/// EPOLLIN: 可读事件
pub const EPOLLIN: u32 = 0x001;
/// EPOLLOUT: 可写事件
pub const EPOLLOUT: u32 = 0x004;
/// EPOLLERR: 错误事件
pub const EPOLLERR: u32 = 0x008;
/// EPOLLHUP: 挂断事件
pub const EPOLLHUP: u32 = 0x010;
/// EPOLLRDHUP: 对端关闭
pub const EPOLLRDHUP: u32 = 0x2000;
/// EPOLLET: 边沿触发
pub const EPOLLET: u32 = 1 << 31;
/// EPOLLONESHOT: 一次性事件
pub const EPOLLONESHOT: u32 = 1 << 30;

/// `epoll_ctl` 操作
pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;

// ============================================================================
// epoll 数据结构
// ============================================================================

/// `epoll_event` — 用户空间事件结构
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct EpollEvent {
    /// 事件掩码 (EPOLLIN | EPOLLOUT | ...)
    pub events: u32,
    /// 用户数据 (64-bit, 透传)
    pub data: u64,
}

/// 内核跟踪的 fd 项
#[derive(Debug, Clone, Copy)]
struct EpollItem {
    /// 监控的 fd
    fd: i32,
    /// 事件掩码
    events: u32,
    /// 用户数据
    data: u64,
    /// 是否边沿触发
    is_et: bool,
    /// 是否一次性
    is_oneshot: bool,
    /// 是否已就绪 (oneshot 用)
    ready: bool,
}

/// epoll 实例
struct EpollInstance {
    /// 感兴趣列表 (所有注册的 fd)
    interest_list: Vec<EpollItem>,
    /// 就绪列表 (有事件待处理的 fd)
    ready_list: Vec<EpollEvent>,
    /// 等待队列 (`epoll_wait` 阻塞时挂起线程, `epoll_pwake` 唤醒)
    wait_queue: WaitQueue,
    /// 实例 ID
    id: u64,
}

// ============================================================================
// 全局状态
// ============================================================================

/// epoll 实例表
static EPOLL_INSTANCES: Mutex<Vec<EpollInstance>> = Mutex::new(Vec::new());
/// 下一个 epoll 实例 ID
static NEXT_EPOLL_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

// ============================================================================
// epoll 系统调用实现
// ============================================================================

/// `epoll_create` — 创建 epoll 实例
///
/// `size` 参数在 Linux 2.6.8+ 中被忽略, 但必须 > 0.
/// 返回 epoll fd (当前用实例 ID 代替).
pub fn sys_epoll_create(size: i32) -> i64 {
    if size <= 0 {
        return Errno::EINVAL.as_ret();
    }

    let id = NEXT_EPOLL_ID.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    let instance = EpollInstance {
        interest_list: Vec::new(),
        ready_list: Vec::new(),
        wait_queue: WaitQueue::new(),
        id,
    };

    EPOLL_INSTANCES.lock().push(instance);

    crate::klog_debug!(Sync, "[epoll] Created instance id={}", id);
    id as i64
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `epoll_ctl` — 控制 epoll 实例
///
/// - `EPOLL_CTL_ADD`: 注册 fd
/// - `EPOLL_CTL_MOD`: 修改 fd 的事件
/// - `EPOLL_CTL_DEL`: 移除 fd
pub fn sys_epoll_ctl(epfd: i64, op: i32, fd: i32, event: *const EpollEvent) -> i64 {
    if epfd <= 0 {
        return Errno::EBADF.as_ret();
    }
    if fd < 0 {
        return Errno::EBADF.as_ret();
    }

    let epfd_id = epfd as u64;
    let mut instances = EPOLL_INSTANCES.lock();

    // 查找 epoll 实例
    let idx = match instances.iter().position(|i| i.id == epfd_id) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    match op {
        EPOLL_CTL_ADD => {
            // 检查 fd 是否已存在
            if instances[idx]
                .interest_list
                .iter()
                .any(|item| item.fd == fd)
            {
                return Errno::EEXIST.as_ret();
            }

            // SAFETY: event 指针由 syscall 入口验证
            let ev = if event.is_null() {
                return Errno::EFAULT.as_ret();
            } else {
                unsafe { core::ptr::read(event) }
            };

            let ev_events = ev.events;
            let ev_data = ev.data;

            let item = EpollItem {
                fd,
                events: ev_events & !EPOLLET & !EPOLLONESHOT,
                data: ev_data,
                is_et: (ev_events & EPOLLET) != 0,
                is_oneshot: (ev_events & EPOLLONESHOT) != 0,
                ready: false,
            };

            instances[idx].interest_list.push(item);
            crate::klog_debug!(
                Sync,
                "[epoll] ADD fd={} events=0x{:X} to epfd={}",
                fd,
                ev_events,
                epfd_id
            );
        }
        EPOLL_CTL_MOD => {
            let item = match instances[idx].interest_list.iter_mut().find(|i| i.fd == fd) {
                Some(i) => i,
                None => return Errno::ENOENT.as_ret(),
            };

            let ev = if event.is_null() {
                return Errno::EFAULT.as_ret();
            } else {
                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe { core::ptr::read(event) }
            };

            let ev_events = ev.events;
            let ev_data = ev.data;

            item.events = ev_events & !EPOLLET & !EPOLLONESHOT;
            item.data = ev_data;
            item.is_et = (ev_events & EPOLLET) != 0;
            item.is_oneshot = (ev_events & EPOLLONESHOT) != 0;
            item.ready = false;

            crate::klog_debug!(
                Sync,
                "[epoll] MOD fd={} events=0x{:X} in epfd={}",
                fd,
                ev_events,
                epfd_id
            );
        }
        EPOLL_CTL_DEL => {
            let len_before = instances[idx].interest_list.len();
            instances[idx].interest_list.retain(|i| i.fd != fd);
            if instances[idx].interest_list.len() == len_before {
                return Errno::ENOENT.as_ret();
            }

            // 同时从就绪列表移除
            instances[idx].ready_list.retain(|e| e.data != fd as u64);

            crate::klog_debug!(Sync, "[epoll] DEL fd={} from epfd={}", fd, epfd_id);
        }
        _ => {
            return Errno::EINVAL.as_ret();
        }
    }

    0
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `epoll_wait` — 等待事件
///
/// `maxevents` 必须大于 0.
/// `timeout`: -1=无限等待, 0=非阻塞, >0=毫秒超时.
/// 返回就绪事件数, 或错误码.
pub fn sys_epoll_wait(epfd: i64, events: *mut EpollEvent, maxevents: i32, timeout: i32) -> i64 {
    if epfd <= 0 || events.is_null() || maxevents <= 0 {
        return Errno::EINVAL.as_ret();
    }

    let epfd_id = epfd as u64;
    let mut instances = EPOLL_INSTANCES.lock();

    // 查找 epoll 实例
    let idx = match instances.iter().position(|i| i.id == epfd_id) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    // 扫描 interest_list, 检查哪些 fd 就绪 (完整 VFS poll)
    let mut ready_events = Vec::new();

    for item in &instances[idx].interest_list {
        let revents = check_fd_ready(item.fd, item.events);

        if revents != 0 {
            ready_events.push(EpollEvent {
                events: revents,
                data: item.data,
            });

            // oneshot: 标记已就绪, 不再报告
        }

        if ready_events.len() as i32 >= maxevents {
            break;
        }
    }

    // 如果没有就绪事件且 timeout != 0: 阻塞等待
    if ready_events.is_empty() && timeout != 0 {
        // 完整实现: 挂入 epfd 等待队列, 调度让出
        // epoll_pwake 会在 fd 状态变化时唤醒
        let current_pid = crate::privileged::proc::process_get_current_pid();

        if current_pid != 0 && timeout == -1 {
            // 1. 挂入 wait_queue (持锁, 避免与 epoll_pwake 竞态)
            instances[idx].wait_queue.add(WaitQueueItem {
                pid: current_pid as u32,
            });
            // 2. 释放锁, 再阻塞 (与 futex 模式一致: unlock → block → schedule)
            drop(instances);

            // 3. 阻塞当前线程 + 触发调度
            crate::privileged::proc::process_block(current_pid);

            // 4. 被唤醒: 重新加锁扫描
            let instances = EPOLL_INSTANCES.lock();
            if let Some(idx) = instances.iter().position(|i| i.id == epfd_id) {
                for item in &instances[idx].interest_list {
                    let revents = check_fd_ready(item.fd, item.events);
                    if revents != 0 {
                        ready_events.push(EpollEvent {
                            events: revents,
                            data: item.data,
                        });
                    }
                    if ready_events.len() as i32 >= maxevents {
                        break;
                    }
                }
            }
        }
        // timeout > 0: 当前简化直接返回 0 (无事件)
        // 完整 hrtimer 集成后实现精准定时唤醒
    }

    // 复制到用户空间
    let count = ready_events.len().min(maxevents as usize);
    for i in 0..count {
        // SAFETY: events 指针由 syscall 入口验证
        unsafe {
            core::ptr::write(events.add(i), ready_events[i]);
        }
    }

    crate::klog_debug!(
        Sync,
        "[epoll] WAIT epfd={} returned {} events",
        epfd_id,
        count
    );
    count as i64
}

// ============================================================================
// epoll_pwake — fd 状态变化唤醒 (供 VFS I/O 路径调用)
// ============================================================================

/// 唤醒等待在指定 fd 上的所有 epoll 实例
///
/// 由 VFS I/O 路径 (write/close/fs 变更) 在持锁外调用, 简单遍历所有 epoll 实例
/// 找到包含该 fd 的实例, 加入就绪列表并唤醒等待者.
///
/// 复杂度 O(N×M), N = epoll 实例数, M = 每个实例的 `interest_list` 大小.
/// 单实例 fd 数量受 maxevents 限制, 性能可接受.
///
/// # REVAL-6.2 拆分
///
/// `epoll_pwake` 本身是**机制** (机制层职责):
/// - 遍历所有 epoll 实例
/// - 找到包含 fd 的实例
/// - 唤醒 `wait_queue` 中的等待者
///
/// 策略层职责 (抽到 `enqueue_ready_for_fd`):
/// - 决策 revents (复用 `check_fd_ready`)
/// - 决策 dedup (避免 `ready_list` 重复)
///
/// # Safety
///
/// - 必须在持有 `fd_table` 锁的 VFS 路径外调用 (避免锁顺序倒置)
/// - 不可在中断上下文睡眠 (本函数不睡眠)
pub fn epoll_pwake(fd: i32) {
    let mut instances = EPOLL_INSTANCES.lock();

    for i in 0..instances.len() {
        // 机制: 检查 interest_list 是否包含该 fd
        if !instance_watches_fd(&instances[i], fd) {
            continue;
        }

        // 策略: 把该 fd 就绪事件加入 ready_list (revents + dedup)
        enqueue_ready_for_fd(&mut instances[i], fd);

        // 机制: 唤醒 wait_queue 中的所有等待者
        while let Some(item) = instances[i].wait_queue.wake_one() {
            crate::privileged::proc::scheduler_unblock(item.pid);
        }
    }
}

/// 机制: 检查 epoll 实例是否在监控指定 fd
///
/// REVAL-6.2: 从 `epoll_pwake` 提取, 保持纯函数特性 (0 unsafe, 无副作用).
#[inline]
fn instance_watches_fd(instance: &EpollInstance, fd: i32) -> bool {
    instance.interest_list.iter().any(|item| item.fd == fd)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 策略: 把 fd 的就绪事件加入 epoll 实例的 `ready_list`
///
/// REVAL-6.2: 从 `epoll_pwake` 提取, 封装:
/// - revents 计算 (走 `check_fd_ready` → `VfsPollPolicy`)
/// - dedup 检查 (避免同一 fd 重复入队, 与 edge-trigger 配合)
/// - 一次性 (oneshot) 标记
///
/// 返回: 是否成功入队 (true = 新增, false = 重复跳过)
fn enqueue_ready_for_fd(instance: &mut EpollInstance, fd: i32) -> bool {
    // 找到该 fd 在 interest_list 中的位置
    let pos = match instance.interest_list.iter().position(|item| item.fd == fd) {
        Some(p) => p,
        None => return false,
    };

    let events = instance.interest_list[pos].events;
    let data = instance.interest_list[pos].data;

    // 策略 1: 决策 revents (委托 VfsPollPolicy)
    let revents = check_fd_ready(fd, events);
    if revents == 0 {
        return false;
    }

    // 策略 2: dedup (避免 ready_list 重复)
    if instance.ready_list.iter().any(|e| e.data == data) {
        return false;
    }

    // 入队
    instance.ready_list.push(EpollEvent {
        events: revents,
        data,
    });
    true
}

// ============================================================================
// 辅助函数
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
#[expect(
    clippy::items_after_statements,
    reason = "items_after_statements: item 紧邻使用点声明便于阅读上下文; 当前优先 expect"
)]
/// 检查 fd 是否就绪 (完整集成 VFS)
///
/// REVAL-6.1: 4 种 VFS `file_type` → events 位映射改走 `VfsPollPolicy` trait dispatch
/// (`functions/fs/vfs_poll_policy.rs` 的 `StandardVfsPollPolicy`).
/// 未注册策略时使用 `VfsPollPolicyRef::Fallback` 行为, 与原硬编码一致.
///
/// 仍然在 privileged 处理 (因为这些是 syscall 层的特殊 fd):
///   - eventfd/signalfd/timerfd: 框架内部状态
///   - VFS fd: 委托给 `VfsPollPolicy`
///
/// 与 user 事件掩码做 AND 运算, 只报告 user 关心的位.
fn check_fd_ready(fd: i32, events: u32) -> u32 {
    // 1. eventfd 空间 [200, 216)
    if crate::privileged::syscall::eventfd::is_eventfd_fd(fd) {
        let raw = crate::privileged::syscall::eventfd::eventfd_poll_events(fd);
        return raw & events;
    }

    // 2. signalfd 空间 [220, 236)
    if crate::privileged::syscall::signalfd::is_signalfd_fd(fd) {
        let raw = crate::privileged::syscall::signalfd::signalfd_poll_events(fd);
        return raw & events;
    }

    // 3. timerfd 空间 [240, 256)
    if crate::privileged::syscall::timerfd::is_timerfd_fd(fd) {
        let raw = crate::privileged::syscall::timerfd::timerfd_poll_events(fd);
        return raw & events;
    }

    // 4. VFS fd 空间 — REVAL-6.1: 委托给 VfsPollPolicy
    use crate::privileged::fs::VfsFileType;

    // 查询 VFS 真实状态 (经 VfsOps 契约 → 当前进程 fd 表 → OpenFile 元数据源)
    let (valid, file_type) = match crate::privileged::fs::current_vfs_ops().fd_file_type(fd) {
        Some(ft) => (true, ft),
        None => (false, 0u8),
    };

    // M6: 处理非法 file_type
    let vfs_file_type = match VfsFileType::from_u8(file_type) {
        Some(t) => t,
        None => return 0, // 非法类型视为无事件
    };

    // REVAL-6.1: 通过 VfsPollPolicy trait dispatch 决策
    let ctx = VfsPollContext {
        valid,
        file_type: vfs_file_type,
    };
    let raw_revents = current_vfs_poll_policy().events_for(ctx);

    // 只报告 user 关心的位
    raw_revents & events
}

/// 销毁 epoll 实例
pub fn epoll_destroy(epfd: u64) {
    let mut instances = EPOLL_INSTANCES.lock();
    instances.retain(|i| i.id != epfd);
    crate::klog_debug!(Sync, "[epoll] Destroyed instance id={}", epfd);
}

// ============================================================================
// 内核测试
// ============================================================================

#[cfg(feature = "kernel_test")]
fn test_epoll_create() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    let fd = sys_epoll_create(1);
    check!(fd > 0, "epoll_create returns positive fd");

    let fd2 = sys_epoll_create(0);
    check!(fd2 < 0, "epoll_create(0) returns error");

    TestResult::Pass
}

#[cfg(feature = "kernel_test")]
fn test_epoll_ctl_add_del() -> crate::privileged::tests::TestResult {
    use crate::privileged::tests::{TestResult, check};

    let epfd = sys_epoll_create(4);
    check!(epfd > 0, "epoll_create ok");

    let ev = EpollEvent {
        events: EPOLLIN,
        data: 42,
    };
    let ret = sys_epoll_ctl(epfd, EPOLL_CTL_ADD, 3, &raw const ev);
    check!(ret == 0, "epoll_ctl ADD ok");

    // 重复添加应失败
    let ret2 = sys_epoll_ctl(epfd, EPOLL_CTL_ADD, 3, &raw const ev);
    check!(ret2 < 0, "epoll_ctl ADD duplicate fails");

    // 删除
    let ret3 = sys_epoll_ctl(epfd, EPOLL_CTL_DEL, 3, core::ptr::null());
    check!(ret3 == 0, "epoll_ctl DEL ok");

    TestResult::Pass
}

/// 注册 epoll 子系统的内核测试用例.
#[cfg(feature = "kernel_test")]
pub fn register_epoll_tests() {
    use crate::privileged::tests::runner;
    let r = runner();
    r.register("epoll", "create", test_epoll_create);
    r.register("epoll", "ctl_add_del", test_epoll_ctl_add_del);
}
