//! inotify — 文件系统事件通知机制
//!
//! ## 阶段 4b 归属 (VFS 完整下沉 · DECISION-W)
//!
//! inotify 实例表/事件队列随 VFS 完整下沉迁入 services (原 framework
//! `vfs/inotify.rs`)。事件队列被 VFS 路径机制内联消费 (vfs_path/handle
//! 在文件操作路径直接调用 `inotify_notify`)。依赖闭包仅为 framework 公开
//! API (IrqSpinLock/Errno/userptr 均为 framework 安全出口)。
//!
//! ## 安全契约
//!
//! - 全局状态由 `IrqSpinLock` 守护
//! - `inotify_notify` 在 VFS 路径调用, 持锁时不可睡眠
//! - 事件队列使用 `IrqSpinLock` 保护, 中断安全
//! - 用户缓冲区写入经 framework `write_struct_to_user` 安全包装 (0 unsafe)

use alloc::vec::Vec;

use crate::framework::errno::Errno;
use crate::framework::sync::IrqSpinLock as Mutex;
use core::sync::atomic::Ordering;

// ============================================================================
// inotify 常量
// ============================================================================

/// `IN_ACCESS`: 文件被访问
pub const IN_ACCESS: u32 = 0x0000_0001;
/// `IN_MODIFY`: 文件被修改
pub const IN_MODIFY: u32 = 0x0000_0002;
/// `IN_ATTRIB`: 文件属性变化
pub const IN_ATTRIB: u32 = 0x0000_0004;
/// `IN_CLOSE_WRITE`: 可写文件被关闭
pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
/// `IN_CLOSE_NOWRITE`: 不可写文件被关闭
pub const IN_CLOSE_NOWRITE: u32 = 0x0000_0010;
/// `IN_OPEN`: 文件被打开
pub const IN_OPEN: u32 = 0x0000_0020;
/// `IN_MOVED_FROM`: 文件被移出监控目录
pub const IN_MOVED_FROM: u32 = 0x0000_0040;
/// `IN_MOVED_TO`: 文件被移入监控目录
pub const IN_MOVED_TO: u32 = 0x0000_0080;
/// `IN_CREATE`: 在监控目录中创建文件
pub const IN_CREATE: u32 = 0x0000_0100;
/// `IN_DELETE`: 在监控目录中删除文件
pub const IN_DELETE: u32 = 0x0000_0200;
/// `IN_DELETE_SELF`: 被监控文件自身被删除
pub const IN_DELETE_SELF: u32 = 0x0000_0400;
/// `IN_MOVE_SELF`: 被监控文件自身被移动
pub const IN_MOVE_SELF: u32 = 0x0000_0800;

/// `IN_ISDIR`: 事件对象是目录
pub const IN_ISDIR: u32 = 0x4000_0000;
/// `IN_Q_OVERFLOW`: 事件队列溢出
pub const IN_Q_OVERFLOW: u32 = 0x0000_4000;
/// `IN_IGNORED`: watch 被移除 (内核自动发送)
pub const IN_IGNORED: u32 = 0x0000_8000;

/// `IN_NONBLOCK`: 非阻塞模式 (`inotify_init1` 标志)
pub const IN_NONBLOCK: i32 = 0x0800;
/// `IN_CLOEXEC`: 执行时关闭 (`inotify_init1` 标志)
pub const IN_CLOEXEC: i32 = 0x0200_0000;

/// `IN_ALL_EVENTS`: 所有事件的掩码
pub const IN_ALL_EVENTS: u32 = IN_ACCESS
    | IN_MODIFY
    | IN_ATTRIB
    | IN_CLOSE_WRITE
    | IN_CLOSE_NOWRITE
    | IN_OPEN
    | IN_MOVED_FROM
    | IN_MOVED_TO
    | IN_CREATE
    | IN_DELETE
    | IN_DELETE_SELF
    | IN_MOVE_SELF;

/// 最大 inotify 实例数
const INOTIFY_MAX_INSTANCES: usize = 8;
/// 每实例最大 watch 数
const INOTIFY_MAX_WATCHES: usize = 16;
/// 每实例最大事件队列深度
const INOTIFY_MAX_EVENTS: usize = 64;
/// 文件名最大长度 (`inotify_event.name`)
const INOTIFY_MAX_NAME: usize = 32;
/// TD-02: 基址来源已迁移至 `framework::proc::FdPlan::INOTIFY` 单一来源, 不再硬编码.
pub const INOTIFY_FD_BASE: i32 = crate::framework::proc::FdPlan::INOTIFY.base;

// ============================================================================
// inotify 数据结构
// ============================================================================

/// `inotify_event` — 用户空间事件结构 (与 Linux ABI 兼容)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InotifyEvent {
    /// watch 描述符
    pub wd: i32,
    /// 事件掩码
    pub mask: u32,
    /// 关联的 cookie (用于关联 `IN_MOVED_FROM/TO`, v1 暂为 0)
    pub cookie: u32,
    /// name 字段长度 (含 \0)
    pub len: u32,
    /// 可选文件名 (目录事件时为被操作的文件名)
    pub name: [u8; INOTIFY_MAX_NAME],
}

impl Default for InotifyEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl InotifyEvent {
    pub const fn new() -> Self {
        Self {
            wd: 0,
            mask: 0,
            cookie: 0,
            len: 0,
            name: [0; INOTIFY_MAX_NAME],
        }
    }
    /// 事件结构体的固定部分大小 (不含 name)
    pub const FIXED_SIZE: usize = 16; // wd/mask/cookie/len 各 4 字节

    /// 总大小 (含 name)
    pub const FULL_SIZE: usize = Self::FIXED_SIZE + INOTIFY_MAX_NAME;

    /// 设置 name 字段
    fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(INOTIFY_MAX_NAME - 1);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name[len] = 0;
        self.len = if name.is_empty() { 0 } else { (len + 1) as u32 };
    }
}

/// watch 条目
#[derive(Debug, Clone, Copy)]
struct WatchEntry {
    /// watch 描述符 (wd)
    wd: i32,
    /// 被监控的 inode 号
    ino: u32,
    /// 事件掩码
    mask: u32,
    /// 是否有效
    valid: bool,
}

impl Default for WatchEntry {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchEntry {
    const fn new() -> Self {
        Self {
            wd: 0,
            ino: 0,
            mask: 0,
            valid: false,
        }
    }
}

/// inotify 实例
struct InotifyInstance {
    /// 实例 slot 索引 (fd = `INOTIFY_FD_BASE` + `slot_idx`)
    slot_idx: usize,
    /// watch 表
    watches: [WatchEntry; INOTIFY_MAX_WATCHES],
    /// watch 计数
    watch_count: usize,
    /// 下一个 wd (从 1 开始)
    next_wd: i32,
    /// 事件环形队列
    events: [InotifyEvent; INOTIFY_MAX_EVENTS],
    /// 队列头 (读位置)
    event_head: usize,
    /// 队列尾 (写位置)
    event_tail: usize,
    /// 队列中事件数
    event_count: usize,
    /// 是否有效
    valid: bool,
}

impl InotifyInstance {
    const fn new() -> Self {
        Self {
            slot_idx: 0,
            watches: [WatchEntry::new(); INOTIFY_MAX_WATCHES],
            watch_count: 0,
            next_wd: 1,
            events: [InotifyEvent::new(); INOTIFY_MAX_EVENTS],
            event_head: 0,
            event_tail: 0,
            event_count: 0,
            valid: false,
        }
    }

    fn init(&mut self, slot_idx: usize) {
        self.slot_idx = slot_idx;
        self.watch_count = 0;
        self.next_wd = 1;
        self.event_head = 0;
        self.event_tail = 0;
        self.event_count = 0;
        self.valid = true;
        for w in &mut self.watches {
            *w = WatchEntry::default();
        }
    }

    /// 获取该实例的 fd
    fn fd(&self) -> i32 {
        // TD-02 V3: 通过 fd_alloc 集中计算 FD 编号
        crate::framework::proc::fd_at(
            crate::framework::proc::FdSubsystem::Inotify,
            self.slot_idx as usize,
        )
    }

    /// 检查 fd 是否有效
    fn is_fd_valid(&self) -> bool {
        self.fd() >= 0
    }

    /// 入队一个事件, 队列满时丢弃最旧事件
    fn push_event(&mut self, event: InotifyEvent) {
        // 检查 fd 是否有效
        if !self.is_fd_valid() {
            return;
        }

        if self.event_count == INOTIFY_MAX_EVENTS {
            self.event_head = (self.event_head + 1) % INOTIFY_MAX_EVENTS;
            self.event_count -= 1;
        }
        self.events[self.event_tail] = event;
        self.event_tail = (self.event_tail + 1) % INOTIFY_MAX_EVENTS;
        self.event_count += 1;
    }

    /// 出队一个事件
    fn pop_event(&mut self) -> Option<InotifyEvent> {
        if self.event_count == 0 {
            return None;
        }
        let event = self.events[self.event_head];
        self.event_head = (self.event_head + 1) % INOTIFY_MAX_EVENTS;
        self.event_count -= 1;
        Some(event)
    }

    /// 查找指定 inode 上的 watch
    fn find_watch_by_ino(&self, ino: u32) -> Option<usize> {
        self.watches.iter().position(|w| w.valid && w.ino == ino)
    }

    /// 查找指定 wd 的 watch
    fn find_watch_by_wd(&self, wd: i32) -> Option<usize> {
        self.watches.iter().position(|w| w.valid && w.wd == wd)
    }

    /// 添加 watch, 返回 wd
    fn add_watch(&mut self, ino: u32, mask: u32) -> Result<i32, Errno> {
        if let Some(idx) = self.find_watch_by_ino(ino) {
            self.watches[idx].mask = mask;
            return Ok(self.watches[idx].wd);
        }

        let idx = self
            .watches
            .iter()
            .position(|w| !w.valid)
            .ok_or(Errno::ENOSPC)?;

        let wd = self.next_wd;
        self.next_wd += 1;
        self.watches[idx] = WatchEntry {
            wd,
            ino,
            mask,
            valid: true,
        };
        self.watch_count += 1;
        Ok(wd)
    }

    /// 移除 watch
    fn remove_watch(&mut self, wd: i32) -> Result<(), Errno> {
        let idx = self.find_watch_by_wd(wd).ok_or(Errno::EINVAL)?;
        self.watches[idx] = WatchEntry::default();
        self.watch_count -= 1;
        Ok(())
    }
}

// ============================================================================
// 全局状态
// ============================================================================

/// inotify 实例表
static INOTIFY_INSTANCES: Mutex<[InotifyInstance; INOTIFY_MAX_INSTANCES]> = Mutex::new([
    InotifyInstance::new(),
    InotifyInstance::new(),
    InotifyInstance::new(),
    InotifyInstance::new(),
    InotifyInstance::new(),
    InotifyInstance::new(),
    InotifyInstance::new(),
    InotifyInstance::new(),
]);

/// 统计: inotify 操作计数
static INOTIFY_OPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// ============================================================================
// inotify 系统调用实现
// ============================================================================

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `inotify_init1` — 创建 inotify 实例
pub fn sys_inotify_init1(flags: i32) -> i64 {
    INOTIFY_OPS.fetch_add(1, Ordering::Relaxed);

    if flags & !(IN_NONBLOCK | IN_CLOEXEC) != 0 {
        return Errno::EINVAL.as_ret();
    }

    // V2: 使用集中分配器获取 FD
    let fd = match crate::framework::proc::fd_alloc::alloc_fd(
        crate::framework::proc::fd_alloc::FdSubsystem::Inotify,
    ) {
        Some(f) => f,
        None => return Errno::EMFILE.as_ret(),
    };

    let slot_idx = match crate::framework::proc::fd_alloc::idx_of(fd) {
        Some((_sub, s)) => s,
        None => return Errno::EBADF.as_ret(),
    };

    let mut instances = INOTIFY_INSTANCES.lock();
    let slot = match instances.iter_mut().find(|i| !i.valid) {
        Some(s) => s,
        None => return Errno::EMFILE.as_ret(),
    };
    slot.init(slot_idx);

    i64::from(fd)
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `inotify_add_watch` — 添加 watch
pub fn sys_inotify_add_watch(fd: i64, ino: u32, mask: u32) -> i64 {
    INOTIFY_OPS.fetch_add(1, Ordering::Relaxed);

    if !is_inotify_fd(fd as i32) || ino == 0 {
        return Errno::EBADF.as_ret();
    }

    if mask & IN_ALL_EVENTS == 0 {
        return Errno::EINVAL.as_ret();
    }

    let slot = fd_to_slot(fd as i32);
    let mut instances = INOTIFY_INSTANCES.lock();

    let instance = match instances.iter_mut().find(|i| i.valid && i.slot_idx == slot) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    match instance.add_watch(ino, mask) {
        Ok(wd) => i64::from(wd),
        Err(e) => e.as_ret(),
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `inotify_rm_watch` — 移除 watch
pub fn sys_inotify_rm_watch(fd: i64, wd: i32) -> i64 {
    INOTIFY_OPS.fetch_add(1, Ordering::Relaxed);

    if !is_inotify_fd(fd as i32) {
        return Errno::EBADF.as_ret();
    }

    let slot = fd_to_slot(fd as i32);
    let mut instances = INOTIFY_INSTANCES.lock();

    let instance = match instances.iter_mut().find(|i| i.valid && i.slot_idx == slot) {
        Some(i) => i,
        None => return Errno::EBADF.as_ret(),
    };

    if instance.find_watch_by_wd(wd).is_some() {
        let ignored_event = InotifyEvent {
            wd,
            mask: IN_IGNORED,
            cookie: 0,
            len: 0,
            name: [0; INOTIFY_MAX_NAME],
        };
        instance.push_event(ignored_event);
    }

    match instance.remove_watch(wd) {
        Ok(()) => 0,
        Err(e) => e.as_ret(),
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `inotify_read` — 从 inotify fd 读取事件 (safe 部分)
///
/// 返回事件列表和总字节数。用户指针写入由 framework 层处理。
pub fn inotify_read_events(fd: i64, max_count: usize) -> Option<(Vec<InotifyEvent>, usize)> {
    if !is_inotify_fd(fd as i32) || max_count < InotifyEvent::FULL_SIZE {
        return None;
    }

    let slot = fd_to_slot(fd as i32);
    let mut instances = INOTIFY_INSTANCES.lock();

    let instance = match instances.iter_mut().find(|i| i.valid && i.slot_idx == slot) {
        Some(i) => i,
        None => return None,
    };

    let mut events = Vec::new();
    let mut written = 0usize;

    while written + InotifyEvent::FULL_SIZE <= max_count {
        let event = match instance.pop_event() {
            Some(e) => e,
            None => break,
        };
        events.push(event);
        written += InotifyEvent::FULL_SIZE;
    }

    if events.is_empty() {
        None
    } else {
        Some((events, written))
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// `inotify_read` — 从 inotify fd 读取事件 (用户缓冲区写入)
///
/// 用户缓冲区写入经 framework `write_struct_to_user` 安全包装 (0 unsafe)。
/// 事件出队策略委托到同模块 `inotify_read_events`。
pub fn sys_inotify_read(fd: i64, buf: *mut u8, count: usize) -> i64 {
    if buf.is_null() || count < InotifyEvent::FULL_SIZE {
        return Errno::EINVAL.as_ret();
    }

    let (events, _written) = match inotify_read_events(fd, count) {
        Some(r) => r,
        None => return Errno::EAGAIN.as_ret(),
    };

    let mut written = 0usize;
    for event in &events {
        // framework 安全包装: 校验 dst 范围 + SMAP, 不再需要本地 unsafe
        if !crate::framework::userptr::write_struct_to_user(buf as u64 + written as u64, event) {
            break;
        }
        written += InotifyEvent::FULL_SIZE;
    }

    written as i64
}

// ============================================================================
// VFS 事件通知接口
// ============================================================================

/// 判断 fd 是否属于 inotify FD 空间
pub fn is_inotify_fd(fd: i32) -> bool {
    fd >= INOTIFY_FD_BASE && fd < INOTIFY_FD_BASE + INOTIFY_MAX_INSTANCES as i32
}

/// fd → slot 索引
fn fd_to_slot(fd: i32) -> usize {
    (fd - INOTIFY_FD_BASE) as usize
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 通知所有监控指定 inode 的 inotify 实例
pub fn inotify_notify(ino: u32, mask: u32, name: &str, is_dir: bool) {
    if ino == 0 {
        return;
    }

    let mut instances = INOTIFY_INSTANCES.lock();
    let mut notified_fds = [false; INOTIFY_MAX_INSTANCES];
    let mut notified_count = 0usize;

    for (i, instance) in instances.iter_mut().enumerate() {
        if !instance.valid {
            continue;
        }

        let watch_idx = match instance.find_watch_by_ino(ino) {
            Some(idx) => idx,
            None => continue,
        };

        let watch = &instance.watches[watch_idx];

        if watch.mask & mask == 0 {
            continue;
        }

        let mut event = InotifyEvent {
            wd: watch.wd,
            mask,
            cookie: 0,
            len: 0,
            name: [0; INOTIFY_MAX_NAME],
        };

        if is_dir {
            event.mask |= IN_ISDIR;
        }

        if !name.is_empty() {
            event.set_name(name);
        }

        instance.push_event(event);

        if notified_count < INOTIFY_MAX_INSTANCES {
            notified_fds[i] = true;
            notified_count += 1;
        }
    }

    if notified_count > 0 {
        drop(instances);
        for i in 0..INOTIFY_MAX_INSTANCES {
            if notified_fds[i] {
                // TD-02 V3: 通过 fd_alloc 集中计算 FD 编号
                crate::framework::syscall::epoll::epoll_pwake(crate::framework::proc::fd_at(
                    crate::framework::proc::FdSubsystem::Inotify,
                    i,
                ));
            }
        }
    }
}

/// 释放指定 inotify 实例的所有资源
pub fn inotify_release(fd: i64) {
    if !is_inotify_fd(fd as i32) {
        return;
    }

    let slot = fd_to_slot(fd as i32);
    let mut instances = INOTIFY_INSTANCES.lock();

    if let Some(instance) = instances.iter_mut().find(|i| i.valid && i.slot_idx == slot) {
        instance.valid = false;
        instance.watch_count = 0;
    }
}

/// 检查指定 inotify fd 是否有事件可读 (epoll 集成用)
pub fn inotify_fd_readable(fd: i64) -> bool {
    if !is_inotify_fd(fd as i32) {
        return false;
    }

    let slot = fd_to_slot(fd as i32);
    let instances = INOTIFY_INSTANCES.lock();

    instances
        .iter()
        .find(|i| i.valid && i.slot_idx == slot)
        .map_or(false, |i| i.event_count > 0)
}

/// 获取统计信息
pub fn inotify_stats() -> (u64, u64) {
    let instances = INOTIFY_INSTANCES.lock();
    let active = instances.iter().filter(|i| i.valid).count() as u64;
    let ops = INOTIFY_OPS.load(Ordering::Relaxed);
    (active, ops)
}

// ============================================================================
// 单元测试 (DECISION-080 双轨: 纯逻辑测试归源侧 #[cfg(test)])
// ============================================================================

#[cfg(test)]
mod tests {
    use super::{IN_NONBLOCK, inotify_release, is_inotify_fd, sys_inotify_init1};

    /// `inotify_init` 遗留接口等价 `inotify_init1(0)`
    #[test]
    fn test_inotify_init_legacy() {
        let fd = sys_inotify_init1(0);
        assert!(fd > 0, "inotify_init1(0) 应返回有效 fd");
        assert!(is_inotify_fd(fd as i32), "fd 应为 inotify fd");
        inotify_release(fd);

        // flags 保留位非零 → EINVAL
        assert_eq!(
            sys_inotify_init1(IN_NONBLOCK | 0x10),
            -22,
            "非法 flags 应返回 EINVAL"
        );
        // IN_NONBLOCK 单独合法
        let fd2 = sys_inotify_init1(IN_NONBLOCK);
        assert!(fd2 > 0, "IN_NONBLOCK 应为合法 flags");
        inotify_release(fd2);
    }
}
