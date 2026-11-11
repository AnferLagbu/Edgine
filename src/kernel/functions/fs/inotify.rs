#![deny(unsafe_code)]
#![deny(missing_docs)]
//! inotify — 文件系统事件通知机制
//!
//! ## fd 对象模型 (阶段 3 第 5 个迁移子系统)
//!
//! inotify 不再是"全局编号区间 + 集中基址规划"的位图模型, 而是
//! [`InotifyFile`] 对象挂在全局 `OpenFileTable`, 用户 fd 由
//! per-process `FdTable` 分配 (与 VFS / socket / UDS fd 同源). 对象内部只持
//! 有策略表 (本模块 `INOTIFY_INSTANCES`) 的**槽位下标** (`idx`), 槽位生灭由
//! 对象 `release()` 驱动: 最后一个 fd 引用关闭 → `release()` →
//! `InotifyInstance::reset_slot()`. 由此与其它 fd 对象同构地获得:
//!
//! - **close / dup / fork**: 引用计数语义与 VFS fd 一致;
//! - **CLOEXEC**: fd 级标志落 per-process fd 表;
//! - **epoll / poll**: 对象实现 [`File::poll`] (事件队列非空 → `IN`), 经通用
//!   对象分发求值;
//! - **阻塞状态**: 由 `OpenFile::nonblock` 单点持有.
//!
//! 对象**不实现** `Inode` (`as_inode() == None`), 不参与 pcache / inotify
//! 命名空间, 也不可作 mmap 后端.
//!
//! ## 安全契约
//!
//! - 全局状态由 `IrqSpinLock` 守护
//! - `inotify_notify` 在 VFS 路径调用, 持锁时不可睡眠; 唤醒回调在实例表
//!   锁释放后执行
//! - 事件队列使用 `IrqSpinLock` 保护, 中断安全
//! - 事件出队后序列化到调用方切片 (`File::read_at` 契约), 用户缓冲区写入
//!   由 VFS 分发层的 privileged 安全代理承担 (0 unsafe)

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::functions::fs::file::{File, PollEvents};
use crate::functions::fs::open_file_table::OPEN_FILE_TABLE;
use crate::functions::fs::vfs_types::{KernelError, KernelResult, OpenFile, VfsFileType};
use crate::privileged::errno::Errno;
use crate::privileged::sync::IrqSpinLock as Mutex;

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

/// 最大 inotify 实例数 (全局策略表容量, 非 fd 编号空间)
const INOTIFY_MAX_INSTANCES: usize = 8;
/// 每实例最大 watch 数
const INOTIFY_MAX_WATCHES: usize = 16;
/// 每实例最大事件队列深度
const INOTIFY_MAX_EVENTS: usize = 64;
/// 文件名最大长度 (`inotify_event.name`)
const INOTIFY_MAX_NAME: usize = 32;

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
    /// 构造一个全零的 inotify 事件 (无 watch、无事件掩码、无文件名)。
    #[must_use]
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
        self.len = if name.is_empty() {
            0
        } else {
            crate::functions::intcast::low_u32_from_usize(len + 1)
        };
    }

    /// 序列化到调用方切片 — `dst` 长度必须恰为 [`Self::FULL_SIZE`].
    fn write_to_slice(&self, dst: &mut [u8]) {
        dst[0..4].copy_from_slice(&self.wd.to_ne_bytes());
        dst[4..8].copy_from_slice(&self.mask.to_ne_bytes());
        dst[8..12].copy_from_slice(&self.cookie.to_ne_bytes());
        dst[12..16].copy_from_slice(&self.len.to_ne_bytes());
        dst[Self::FIXED_SIZE..Self::FULL_SIZE].copy_from_slice(&self.name);
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

/// inotify 实例 (策略表槽位)
struct InotifyInstance {
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
    /// 唤醒用数值 fd (对象挂表时登记; `-1` = 未登记)
    wake_fd: i32,
    /// 是否有效
    valid: bool,
}

impl InotifyInstance {
    const fn new() -> Self {
        Self {
            watches: [WatchEntry::new(); INOTIFY_MAX_WATCHES],
            watch_count: 0,
            next_wd: 1,
            events: [InotifyEvent::new(); INOTIFY_MAX_EVENTS],
            event_head: 0,
            event_tail: 0,
            event_count: 0,
            wake_fd: -1,
            valid: false,
        }
    }

    fn init(&mut self) {
        self.watch_count = 0;
        self.next_wd = 1;
        self.event_head = 0;
        self.event_tail = 0;
        self.event_count = 0;
        self.wake_fd = -1;
        self.valid = true;
        for w in &mut self.watches {
            *w = WatchEntry::default();
        }
    }

    /// 失效槽位 (对象 `release()` 调用) — 清空 watch 与待读事件
    fn reset_slot(&mut self) {
        self.valid = false;
        self.watch_count = 0;
        self.event_head = 0;
        self.event_tail = 0;
        self.event_count = 0;
        self.wake_fd = -1;
    }

    /// 入队一个事件, 队列满时丢弃最旧事件
    fn push_event(&mut self, event: InotifyEvent) {
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
            .ok_or(Errno::Enospc)?;

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
        let idx = self.find_watch_by_wd(wd).ok_or(Errno::Einval)?;
        self.watches[idx] = WatchEntry::default();
        self.watch_count -= 1;
        Ok(())
    }
}

// ============================================================================
// fd 对象
// ============================================================================

/// inotify fd 对象 — 仅持策略表槽位下标
pub struct InotifyFile {
    /// `INOTIFY_INSTANCES` 槽位下标
    idx: u8,
}

impl InotifyFile {
    /// 以策略表槽位下标构造对象 (槽位所有权转移给本对象)
    #[must_use]
    pub fn new(idx: u8) -> Self {
        Self { idx }
    }

    /// 策略表槽位下标 — syscall 层委托 watch 操作时使用
    #[must_use]
    pub fn idx(&self) -> u8 {
        self.idx
    }

    /// 归还策略表槽位 (最后一个 fd 引用关闭时调用)
    fn free_instance(idx: u8) {
        let mut instances = INOTIFY_INSTANCES.lock();
        if let Some(instance) = instances.get_mut(usize::from(idx)) {
            instance.reset_slot();
        }
    }

    /// 对象 → `OpenFileTable` → per-process fd 表; 任一步失败归还槽位
    fn attach(idx: u8, nonblock: bool, cloexec: bool) -> Result<usize, Errno> {
        let pwm = crate::privileged::sgeg::pwm_get_current();
        let open_file = OpenFile::from_file(Arc::new(Self::new(idx)), 0, pwm);
        open_file.set_nonblock(nonblock);
        let Some(handle_id) = OPEN_FILE_TABLE.alloc(open_file) else {
            // 对象未挂表 → release() 不会被调用, 槽位须在此显式归还
            Self::free_instance(idx);
            return Err(Errno::Emfile);
        };
        let Some(fd_idx) =
            crate::privileged::proc::with_current_fd_table(|t| t.alloc_fd(handle_id, cloexec))
                .flatten()
        else {
            // 挂表成功但 fd 表耗尽: close 触发最后一个引用的 release() → 归还槽位
            OPEN_FILE_TABLE.close(handle_id);
            return Err(Errno::Emfile);
        };
        // SIMPLIFIED: epoll 唤醒注册表仍以数值 fd 为键 (跨子系统统一改造归
        // epoll 子系统); 影响面 = 仅登记创建时的首个 fd 号, dup 别名的就绪
        // 唤醒需按该 fd 注册兴趣 (与 socket/UDS 迁移同口径); 何时需扩展:
        // 唤醒注册表改按 OpenFile handle 索引时.
        let wake_fd = crate::functions::intcast::low_i32_from_usize(fd_idx);
        let mut instances = INOTIFY_INSTANCES.lock();
        if let Some(instance) = instances.get_mut(usize::from(idx)) {
            instance.wake_fd = wake_fd;
        }
        Ok(fd_idx)
    }
}

impl File for InotifyFile {
    // SIMPLIFIED: 阻塞读未实装 — 无待读事件时恒返回 EAGAIN (与原 privileged /
    // functions 实现一致, 无等待队列); 影响面 = 未置 IN_NONBLOCK 的读也不睡眠;
    // 何时需扩展: inotify 等待队列接入 epoll 唤醒路径时.
    fn read_at(&self, _offset: u64, buf: &mut [u8], _pwm: u64) -> KernelResult<usize> {
        // Linux: 缓冲区小于 sizeof(struct inotify_event) → EINVAL
        if buf.len() < InotifyEvent::FULL_SIZE {
            return Err(KernelError::InvalidArgument);
        }
        let written = with_instance(self.idx, |instance| {
            let mut written = 0usize;
            while written + InotifyEvent::FULL_SIZE <= buf.len() {
                let Some(event) = instance.pop_event() else {
                    break;
                };
                event.write_to_slice(&mut buf[written..written + InotifyEvent::FULL_SIZE]);
                written += InotifyEvent::FULL_SIZE;
            }
            written
        })
        .ok_or(KernelError::BadFd)?;
        if written == 0 {
            return Err(KernelError::WouldBlock);
        }
        Ok(written)
    }

    fn write_at(&self, _offset: u64, _buf: &[u8], _pwm: u64) -> KernelResult<usize> {
        // inotify fd 只读 (Linux: 向其写入返回 EINVAL)
        Err(KernelError::InvalidArgument)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let readable =
            with_instance(self.idx, |instance| instance.event_count > 0).is_some_and(|r| r);
        if readable {
            events & PollEvents::IN
        } else {
            PollEvents::empty()
        }
    }

    fn io_control(&self, _cmd: u32, _arg: usize) -> KernelResult<usize> {
        Err(KernelError::NotSupported)
    }

    fn release(&self) {
        Self::free_instance(self.idx);
    }

    // SIMPLIFIED: `VfsFileType` 无独立的 inotify 变体 — 以 File 呈现 (与
    // eventfd / signalfd / socket 同构); 影响面 = fstat 的 st_mode 报普通文件;
    // 何时需扩展: VfsFileType 增加匿名 inode 类变体并贯通 stat 编码时.
    fn file_type(&self) -> VfsFileType {
        VfsFileType::File
    }

    fn stream(&self) -> bool {
        // POSIX: inotify fd 不可定位 (lseek → ESPIPE)
        true
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// 在有效槽位上执行闭包; 槽位无效返回 `None`
fn with_instance<R>(idx: u8, f: impl FnOnce(&mut InotifyInstance) -> R) -> Option<R> {
    let mut instances = INOTIFY_INSTANCES.lock();
    let instance = instances.get_mut(usize::from(idx))?;
    if !instance.valid {
        return None;
    }
    Some(f(instance))
}

/// 由用户 fd 解析 inotify 槽位下标, 并在解析期间钉住 `OpenFile`.
///
/// 返回的 `handle_id` 供调用方用毕后 `dec_ref`; 解析与 `inc_ref` 在同一
/// `OpenFileTable` 临界区内完成, 避免 close 竞争导致的槽位 ABA.
fn resolve_inotify_fd(fd: i64) -> Option<(u32, u8)> {
    let fd_idx = usize::try_from(fd).ok()?;
    let handle_id = crate::functions::fs::vfs_get_fd_handle(fd_idx)?;
    let idx = OPEN_FILE_TABLE
        .with_file(handle_id, |of| {
            let idx = of.file().as_any().downcast_ref::<InotifyFile>()?.idx();
            of.inc_ref();
            Some(idx)
        })
        .flatten()?;
    Some((handle_id, idx))
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

/// `inotify_init1` — 创建 inotify 实例
///
/// # Errors
///
/// - `flags` 含未知位 → `EINVAL`
/// - 实例表 / `OpenFileTable` / per-process fd 表耗尽 → `EMFILE`
pub fn sys_inotify_init1(flags: i32) -> i64 {
    INOTIFY_OPS.fetch_add(1, Ordering::Relaxed);

    if flags & !(IN_NONBLOCK | IN_CLOEXEC) != 0 {
        return Errno::Einval.as_ret();
    }

    let idx = {
        let mut instances = INOTIFY_INSTANCES.lock();
        let Some((i, slot)) = instances.iter_mut().enumerate().find(|(_, s)| !s.valid) else {
            return Errno::Emfile.as_ret();
        };
        slot.init();
        crate::functions::intcast::low_u8_from_usize(i)
    };

    match InotifyFile::attach(idx, flags & IN_NONBLOCK != 0, flags & IN_CLOEXEC != 0) {
        Ok(fd_idx) => crate::functions::intcast::i64_from_usize(fd_idx),
        Err(e) => e.as_ret(),
    }
}

/// `inotify_add_watch` — 添加 watch
///
/// # Errors
///
/// - fd 不是 inotify 对象或 `ino == 0` → `EBADF`
/// - `mask` 不含任何事件位 → `EINVAL`
/// - watch 表满 → `ENOSPC`
pub fn sys_inotify_add_watch(fd: i64, ino: u32, mask: u32) -> i64 {
    INOTIFY_OPS.fetch_add(1, Ordering::Relaxed);

    let Some((handle_id, idx)) = resolve_inotify_fd(fd) else {
        return Errno::Ebadf.as_ret();
    };
    let result = if ino == 0 {
        Err(Errno::Ebadf)
    } else if mask & IN_ALL_EVENTS == 0 {
        Err(Errno::Einval)
    } else {
        match with_instance(idx, |instance| instance.add_watch(ino, mask)) {
            Some(result) => result,
            None => Err(Errno::Ebadf),
        }
    };
    OPEN_FILE_TABLE.dec_ref(handle_id);

    match result {
        Ok(wd) => i64::from(wd),
        Err(e) => e.as_ret(),
    }
}

/// `inotify_rm_watch` — 移除 watch
///
/// # Errors
///
/// - fd 不是 inotify 对象 → `EBADF`
/// - `wd` 不存在 → `EINVAL`
pub fn sys_inotify_rm_watch(fd: i64, wd: i32) -> i64 {
    INOTIFY_OPS.fetch_add(1, Ordering::Relaxed);

    let Some((handle_id, idx)) = resolve_inotify_fd(fd) else {
        return Errno::Ebadf.as_ret();
    };
    let result = with_instance(idx, |instance| {
        if instance.find_watch_by_wd(wd).is_some() {
            let mut ignored = InotifyEvent::new();
            ignored.wd = wd;
            ignored.mask = IN_IGNORED;
            instance.push_event(ignored);
        }
        instance.remove_watch(wd)
    });
    OPEN_FILE_TABLE.dec_ref(handle_id);

    match result {
        None => Errno::Ebadf.as_ret(),
        Some(Ok(())) => 0,
        Some(Err(e)) => e.as_ret(),
    }
}

// ============================================================================
// VFS 事件通知接口
// ============================================================================

/// 通知所有监控指定 inode 的 inotify 实例
///
/// 在 VFS 路径 (open/write/close/unlink...) 持 `OpenFileTable` 锁时调用,
/// 因此唤醒回调必须在实例表锁释放后执行 (不嵌套 epoll 锁).
pub fn inotify_notify(ino: u32, mask: u32, name: &str, is_dir: bool) {
    if ino == 0 {
        return;
    }

    let mut instances = INOTIFY_INSTANCES.lock();
    let mut wake_fds = [0i32; INOTIFY_MAX_INSTANCES];
    let mut wake_count = 0usize;

    for instance in instances.iter_mut() {
        if !instance.valid {
            continue;
        }

        let Some(watch_idx) = instance.find_watch_by_ino(ino) else {
            continue;
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

        if instance.wake_fd >= 0 && wake_count < INOTIFY_MAX_INSTANCES {
            wake_fds[wake_count] = instance.wake_fd;
            wake_count += 1;
        }
    }

    drop(instances);

    for &fd in &wake_fds[..wake_count] {
        crate::privileged::syscall::epoll::epoll_pwake(fd);
    }
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
    use super::*;

    /// 临时安装当前进程 (含独立 FdTable), 执行闭包后无条件拆除现场
    ///
    /// per-process fd 分配依赖当前进程上下文; 与 `fs` 模块的单测共用
    /// `FS_GLOBAL_TEST_LOCK` 串行化全局进程表改动.
    fn with_temp_process<F: FnOnce()>(name: &str, f: F) {
        use crate::privileged::proc::PROCESS_TABLE;
        use crate::privileged::proc::raw;
        use crate::privileged::proc::scheduler::Scheduler;

        let Some(pid) = PROCESS_TABLE.allocate_pid() else {
            panic!("临时进程: 无空闲 pid");
        };
        let proc_ptr = raw::alloc_process(pid, name, None);
        if !PROCESS_TABLE.insert(proc_ptr) {
            raw::drop_boxed_process(proc_ptr);
            PROCESS_TABLE.free_pid(pid);
            panic!("临时进程: 插入进程表失败");
        }

        let prev_current = Scheduler::current();
        Scheduler::set_current(pid);

        f();

        Scheduler::set_current(prev_current.unwrap_or(0));
        let _ = PROCESS_TABLE.remove(pid);
        raw::drop_boxed_process(proc_ptr);
        PROCESS_TABLE.free_pid(pid);
    }

    #[test]
    fn object_contract_sentinels() {
        // 纯对象面: 不访问全局实例表 (下标仅在 read/poll 时被解析)
        let file = InotifyFile::new(3);
        assert_eq!(file.idx(), 3, "槽位下标原样保存");
        assert!(file.stream(), "inotify 不可定位 (ESPIPE)");
        assert!(file.as_inode().is_none(), "inotify 非 inode 承载");
        assert!(file.mmap_backing().is_none(), "inotify 不可作 mmap 后端");
        assert!(
            file.inode_namespace_id().is_none(),
            "inotify 不属于 inode 命名空间"
        );
        assert_eq!(file.file_type(), VfsFileType::File);
        assert_eq!(
            file.io_control(0x1234_5678, 0),
            Err(KernelError::NotSupported),
            "inotify 对象不处理通用 ioctl"
        );
        assert_eq!(
            file.write_at(0, &[0u8; InotifyEvent::FULL_SIZE], 0),
            Err(KernelError::InvalidArgument),
            "inotify fd 只读 (写 → EINVAL)"
        );
    }

    #[test]
    fn object_downcast_round_trip() {
        // syscall 层经 `as_any().downcast_ref::<InotifyFile>()` 取槽位
        let file = InotifyFile::new(7);
        let erased: &dyn File = &file;
        let recovered = erased
            .as_any()
            .downcast_ref::<InotifyFile>()
            .expect("InotifyFile 必须可下转型");
        assert_eq!(recovered.idx(), 7);
    }

    #[test]
    fn syscall_create_watch_read_close() {
        use crate::functions::fs::api;
        use crate::functions::fs::{FS_GLOBAL_TEST_LOCK, vfs_dup, vfs_get_fd_handle};

        let _lock = FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("inotify-t", || {
            let fd = sys_inotify_init1(IN_CLOEXEC);
            assert!(fd >= 3, "inotify fd 由 per-process 表分配");
            let fd_idx = usize::try_from(fd).expect("fd 应非负");
            assert_eq!(
                crate::privileged::proc::with_current_fd_table(|t| t.is_cloexec(fd_idx)),
                Some(true),
                "IN_CLOEXEC 应落 per-process fd 表"
            );
            let handle = vfs_get_fd_handle(fd_idx).expect("fd 应有 OpenFile");
            assert!(
                !OPEN_FILE_TABLE
                    .with_file(handle, |of| of.is_nonblock())
                    .unwrap_or(true),
                "未置 IN_NONBLOCK 时应为阻塞位"
            );

            let mut buf = [0u8; InotifyEvent::FULL_SIZE];
            assert_eq!(
                api::vfs_read_safe(crate::functions::intcast::low_u32_from_i64(fd), &mut buf),
                -11,
                "空队列读 → EAGAIN"
            );
            assert_eq!(
                api::vfs_read_safe(
                    crate::functions::intcast::low_u32_from_i64(fd),
                    &mut [0u8; InotifyEvent::FIXED_SIZE]
                ),
                -22,
                "短缓冲读 → EINVAL"
            );

            let wd = sys_inotify_add_watch(fd, 7, IN_MODIFY);
            assert_eq!(wd, 1, "首个 watch 的 wd 从 1 开始");
            assert_eq!(
                sys_inotify_add_watch(fd, 7, IN_ALL_EVENTS),
                1,
                "同 inode 复返原 wd 并更新掩码"
            );
            assert_eq!(sys_inotify_add_watch(fd, 0, IN_MODIFY), -9, "ino 0 → EBADF");
            assert_eq!(sys_inotify_add_watch(fd, 8, 0), -22, "空掩码 → EINVAL");

            // 未就绪 → poll 空; 通知后就绪 → IN
            assert!(
                OPEN_FILE_TABLE
                    .with_file(handle, |of| of.file().poll(PollEvents::IN))
                    .expect("对象应可 poll")
                    .is_empty(),
                "事件队列为空时 poll 不就绪"
            );
            inotify_notify(7, IN_MODIFY, "x.txt", false);
            assert_eq!(
                OPEN_FILE_TABLE
                    .with_file(handle, |of| of.file().poll(PollEvents::IN))
                    .expect("对象应可 poll"),
                PollEvents::IN,
                "事件入队后 poll 报 IN"
            );

            assert_eq!(
                api::vfs_read_safe(crate::functions::intcast::low_u32_from_i64(fd), &mut buf),
                crate::functions::intcast::low_i32_from_usize(InotifyEvent::FULL_SIZE),
                "读取一个完整事件"
            );
            assert_eq!(
                i32::from_ne_bytes(buf[0..4].try_into().expect("wd 字段")),
                1
            );
            assert_eq!(
                u32::from_ne_bytes(buf[4..8].try_into().expect("mask 字段")),
                IN_MODIFY
            );
            assert_eq!(
                u32::from_ne_bytes(buf[12..16].try_into().expect("len 字段")),
                6,
                "name 长度含结尾 \\0"
            );
            assert_eq!(&buf[16..22], b"x.txt\0");
            assert_eq!(
                api::vfs_read_safe(crate::functions::intcast::low_u32_from_i64(fd), &mut buf),
                -11,
                "读空 → EAGAIN"
            );

            // 队列溢出: 65 次通知保留最新 64 条
            for _ in 0..=INOTIFY_MAX_EVENTS {
                inotify_notify(7, IN_MODIFY, "", false);
            }
            let mut drained = 0usize;
            while api::vfs_read_safe(crate::functions::intcast::low_u32_from_i64(fd), &mut buf) > 0
            {
                drained += 1;
            }
            assert_eq!(drained, INOTIFY_MAX_EVENTS, "环形队列深度上限 64");

            // dup 别名共享同一实例
            let dup_fd = vfs_dup(crate::functions::intcast::low_u32_from_i64(fd));
            assert!(dup_fd >= 0, "dup 应成功");
            let dup_idx = crate::functions::intcast::usize_from_i32(dup_fd);
            assert_eq!(
                vfs_get_fd_handle(dup_idx),
                Some(handle),
                "dup 共享同一 OpenFile"
            );
            assert_eq!(
                api::vfs_close_safe(crate::functions::intcast::low_u32_from_i64(fd)),
                0,
                "关闭首个别名应成功"
            );
            let wd2 = sys_inotify_add_watch(i64::from(dup_fd), 9, IN_ACCESS);
            assert_eq!(wd2, 2, "首别名关闭后实例仍有效, wd 继续递增");
            let wd2 = crate::functions::intcast::low_i32_from_i64(wd2);

            // rm_watch: 先产生 IN_IGNORED, 再移除
            assert_eq!(sys_inotify_rm_watch(i64::from(dup_fd), wd2), 0);
            assert_eq!(
                api::vfs_read_safe(
                    crate::functions::intcast::low_u32_from_i64(i64::from(dup_fd)),
                    &mut buf
                ),
                crate::functions::intcast::low_i32_from_usize(InotifyEvent::FULL_SIZE)
            );
            assert_eq!(
                u32::from_ne_bytes(buf[4..8].try_into().expect("mask 字段")),
                IN_IGNORED
            );
            assert_eq!(
                sys_inotify_rm_watch(i64::from(dup_fd), wd2),
                -22,
                "未知 wd → EINVAL"
            );

            // 最后别名关闭 → release() 归还槽位
            assert_eq!(
                api::vfs_close_safe(crate::functions::intcast::low_u32_from_i64(i64::from(
                    dup_fd
                ))),
                0
            );
            let instances = INOTIFY_INSTANCES.lock();
            assert!(
                instances.iter().all(|i| !i.valid),
                "最后一个 fd 关闭后槽位必须归还"
            );
            drop(instances);

            // 非 inotify fd → EBADF
            assert_eq!(sys_inotify_add_watch(0, 7, IN_MODIFY), -9);
            assert_eq!(sys_inotify_rm_watch(0, 1), -9);
        });
    }

    #[test]
    fn syscall_flags_and_capacity() {
        use crate::functions::fs::api;
        use crate::functions::fs::{FS_GLOBAL_TEST_LOCK, vfs_get_fd_handle};

        let _lock = FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("inotify-cap-t", || {
            assert_eq!(sys_inotify_init1(0x10), -22, "未知 flags → EINVAL");

            let mut fds = [0i64; INOTIFY_MAX_INSTANCES];
            for slot in &mut fds {
                let fd = sys_inotify_init1(0);
                assert!(fd >= 0, "实例数未满时应创建成功");
                *slot = fd;
            }
            assert_eq!(sys_inotify_init1(0), -24, "第 9 个实例 → EMFILE");

            for &fd in &fds {
                assert_eq!(
                    api::vfs_close_safe(crate::functions::intcast::low_u32_from_i64(fd)),
                    0
                );
            }

            let fd = sys_inotify_init1(IN_NONBLOCK);
            assert!(fd >= 0, "槽位归还后应可再次创建");
            let fd_idx = usize::try_from(fd).expect("fd 应非负");
            let handle = vfs_get_fd_handle(fd_idx).expect("fd 应有 OpenFile");
            assert!(
                OPEN_FILE_TABLE
                    .with_file(handle, |of| of.is_nonblock())
                    .unwrap_or(false),
                "IN_NONBLOCK 应落 OpenFile"
            );
            assert_eq!(
                api::vfs_close_safe(crate::functions::intcast::low_u32_from_i64(fd)),
                0
            );
        });
    }
}
