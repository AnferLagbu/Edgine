//! userfaultfd — 用户态缺页处理机制 (T1 G4 实装)
//!
//! ## 语义
//!
//! 进程注册一段已映射区间 (MISSING 模式) 后, 该区间的匿名页首次访问不再由
//! 内核 demand paging 直接满足, 而是:
//!
//! ```text
//! 缺页进程 (#PF, 用户态)            服务线程 (持有 uffd fd)
//!   fault_notify(addr)
//!     ├── 事件入队 (UFFD_EVENT_PAGEFAULT)
//!     ├── 唤醒 read() 阻塞的服务线程
//!     ── 返回 Waiting → PfResult::UffdWait
//!         (scheduler_block, 由 tick 抢占切走)     read() 返回事件
//!                                                UFFDIO_COPY(dst, src)
//!                                                  ├── 数据暂存
//!                                                  └── scheduler_unblock(缺页 pid)
//!   #PF 重入 → fault_notify 返回 Ready
//!     ├── 分配物理页 + 填充暂存数据
//!     └── 映射 → PfResult::Fixed
//! ```
//!
//! ## 阻塞落点 (关键设计)
//!
//! #PF 在 x86_64 上走 IDT IST=4 专用栈 (见 `idt::idt::IdtManager::init` 注释),
//! **不能**在缺页现场就地切换上下文 (会覆盖 IST 上挂起的异常帧). 因此
//! `PfResult::UffdWait` 的处理落点为: 仅标记当前进程 `Blocked` + 置
//! `need_reschedule`, iretq 回用户态; 由下一次 tick 抢占切走 (见
//! `idt::handlers::PageFaultHandler`)。
//!
//! ## SIMPLIFIED (相对完整路径下的定点简化)
//!
//! - 单挂起页模型: 一个实例同时只跟踪一个待处理缺页页 (`PA_UFD`); `UFFDIO_COPY` /
//!   `UFFDIO_ZEROPAGE` 必须针对该页, 否则 `EINVAL`. 影响面: 服务线程无法预填
//!   非当前缺页页; 何时需扩展: 改为 per-page 挂起表 (Vec/HashMap) 后可支持多页并发.
//! - 事件队列为定长环形缓冲 (`MAX_EVENTS`), 满则丢弃新事件 (`fault_notify` 返回
//!   `NotRegistered` 由内核 demand paging 兜底, 不永久挂起缺页进程).
//! - 仅支持 `UFFDIO_REGISTER_MODE_MISSING`; WP/MINOR 模式与 fork/remap/remove
//!   事件未实装 (`UFFDIO_API.features` 回填 0).
//! - `UFFDIO_COPY` 单次仅支持一页 (`len == PAGE_SIZE`).
//! - 本模块 0 unsafe (仅用 `PhysAddr::to_virt` + `copy_nonoverlapping` 于
//!   `fill_provided_page`, 已带 SAFETY 注释).

use alloc::boxed::Box;

use super::{PAGE_SIZE, PhysAddr};
use crate::privileged::sync::{IrqSpinLock, OnceLock};

// ============================================================================
// Linux ABI 常量 (真实编码, 与 uapi/linux/userfaultfd.h 一致)
// ============================================================================

/// `UFFDIO_API`
pub const UFFDIO_API: u64 = 0xC018_AA3F;
/// `UFFDIO_REGISTER`
pub const UFFDIO_REGISTER: u64 = 0xC020_AA00;
/// `UFFDIO_UNREGISTER`
pub const UFFDIO_UNREGISTER: u64 = 0x8010_AA01;
/// `UFFDIO_WAKE`
pub const UFFDIO_WAKE: u64 = 0x8010_AA02;
/// `UFFDIO_COPY`
pub const UFFDIO_COPY: u64 = 0xC028_AA03;
/// `UFFDIO_ZEROPAGE`
pub const UFFDIO_ZEROPAGE: u64 = 0xC020_AA04;

/// `UFFD_API` (API 版本号)
pub const UFFD_API: u64 = 0xAA;
/// `UFFDIO_REGISTER_MODE_MISSING`
pub const UFFDIO_REGISTER_MODE_MISSING: u64 = 1;
/// `UFFD_EVENT_PAGEFAULT`
pub const UFFD_EVENT_PAGEFAULT: u8 = 0x12;
/// `UFFD_PAGEFAULT_FLAG_WRITE`
pub const UFFD_PAGEFAULT_FLAG_WRITE: u64 = 0x0002;

/// `struct uffd_msg` 大小 (ABI 固定 32 字节)
pub const UFFD_MSG_SIZE: usize = 32;
/// `struct uffdio_range` 大小
pub const UFFDIO_RANGE_SIZE: usize = 16;

// ============================================================================
// ABI 结构体
// ============================================================================

/// `UFFDIO_API` 命令参数: 协商 API 版本与可用特性.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UffdIoApi {
    pub api: u64,
    pub features: u64,
    pub ioctls: u64,
}

/// userfaultfd 地址区间 (起始地址 + 长度).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UffdIoRange {
    pub start: u64,
    pub len: u64,
}

/// `UFFDIO_REGISTER` 命令参数: 注册区间及缺页模式.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UffdIoRegister {
    pub range: UffdIoRange,
    pub mode: u64,
    pub ioctls: u64,
}

/// `UFFDIO_COPY` 命令参数: 把用户数据拷贝进缺页区间.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UffdIoCopy {
    pub dst: u64,
    pub src: u64,
    pub len: u64,
    pub mode: u64,
    /// 内核回填: 成功拷贝的字节数
    pub copy: i64,
}

/// `UFFDIO_ZEROPAGE` 命令参数: 把缺页区间填零.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UffdIoZeropage {
    pub range: UffdIoRange,
    pub mode: u64,
    /// 内核回填: 成功置零的字节数
    pub zeropage: i64,
}

/// `struct uffd_msg` (仅 `UFFD_EVENT_PAGEFAULT` 变体)
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UffdMsgPagefault {
    pub event: u8,
    pub reserved1: u8,
    pub reserved2: u16,
    pub reserved3: u32,
    pub flags: u64,
    pub address: u64,
    /// 缺页进程 pid (`feat.ptid`)
    pub ptid: u32,
    pub reserved4: u32,
}

// ============================================================================
// 实例状态
// ============================================================================

/// 每实例最大注册区间数
const MAX_RANGES: usize = 4;
/// 每实例事件队列容量
const MAX_EVENTS: usize = 16;
/// 实例数 = `USERFAULT_FD` 范围容量 (16)
const MAX_INSTANCES: usize = crate::privileged::proc::fd_alloc::max_slots(
    crate::privileged::proc::fd_alloc::FdSubsystem::UserFaultFd,
);

/// 已注册区间
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct UffdRange {
    start: u64,
    end: u64,
    mode: u64,
}

/// 挂起页状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingState {
    /// 无挂起缺页
    Idle,
    /// 已入队事件, 等待服务线程提供数据
    Waiting,
    /// 服务线程已提供数据, 等待缺页进程重入 #PF 完成映射
    Provided,
}

/// userfaultfd 实例
struct UffdInstance {
    /// 是否已分配
    used: bool,
    /// 创建者 pid
    owner_pid: u32,
    /// 是否完成 `UFFDIO_API` 握手
    api_negotiated: bool,
    /// 阻塞在 `read()` 的服务线程 pid (0 = 无)
    reader_pid: u32,
    /// 当前挂起缺页的进程 pid (0 = 无)
    fault_pid: u32,
    /// 当前挂起缺页的页地址 (0 = 无)
    fault_page: u64,
    /// 挂起页状态
    page_state: PendingState,
    /// `UFFDIO_ZEROPAGE` 语义标记 (暂存区无数据, 填零)
    staged_zero: bool,
    /// `UFFDIO_COPY` 暂存数据 (单页)
    staged: [u8; PAGE_SIZE as usize],
    /// 已注册区间
    ranges: [UffdRange; MAX_RANGES],
    /// 已注册区间数
    range_count: usize,
    /// 事件环形队列
    events: [UffdMsgPagefault; MAX_EVENTS],
    /// 队首偏移
    ev_head: usize,
    /// 队内事件数
    ev_count: usize,
}

impl UffdInstance {
    /// 空实例 (const, 用于静态表初始化)
    const fn empty() -> Self {
        Self {
            used: false,
            owner_pid: 0,
            api_negotiated: false,
            reader_pid: 0,
            fault_pid: 0,
            fault_page: 0,
            page_state: PendingState::Idle,
            staged_zero: false,
            staged: [0u8; PAGE_SIZE as usize],
            ranges: [UffdRange {
                start: 0,
                end: 0,
                mode: 0,
            }; MAX_RANGES],
            range_count: 0,
            events: [UffdMsgPagefault {
                event: 0,
                reserved1: 0,
                reserved2: 0,
                reserved3: 0,
                flags: 0,
                address: 0,
                ptid: 0,
                reserved4: 0,
            }; MAX_EVENTS],
            ev_head: 0,
            ev_count: 0,
        }
    }

    /// 区间是否覆盖 `page_addr` 且模式匹配
    fn covers(&self, page_addr: u64) -> bool {
        self.ranges[..self.range_count].iter().any(|r| {
            r.mode & UFFDIO_REGISTER_MODE_MISSING != 0 && page_addr >= r.start && page_addr < r.end
        })
    }
}

/// 全局实例表 (中断安全: #PF 路径读取) — 惰性堆置, 避免约 74 KB 常驻 BSS。
///
/// `OnceLock` 内联仅数十字节; 首次访问 (创建实例或 #PF 路径查询) 时在 kmalloc
/// 堆上逐元素构造 `Box<[UffdInstance]>`。首次访问恒晚于 kmalloc 初始化。
static UFFD_TABLE: OnceLock<IrqSpinLock<Box<[UffdInstance]>>> = OnceLock::new();

/// 获取全局实例表的锁 (首次调用时惰性分配)。
fn uffd_table() -> &'static IrqSpinLock<Box<[UffdInstance]>> {
    UFFD_TABLE.get_or_init(|slot| {
        slot.write(IrqSpinLock::new(
            (0..MAX_INSTANCES)
                .map(|_| UffdInstance::empty())
                .collect::<Box<[_]>>(),
        ));
    })
}

/// 实例下标 (非 uffd fd 返回 `None`)
fn instance_index(fd: i32) -> Option<usize> {
    use crate::privileged::proc::fd_alloc::{FdSubsystem, idx_of};
    match idx_of(fd) {
        Some((FdSubsystem::UserFaultFd, slot)) => Some(slot),
        _ => None,
    }
}

/// 是否为 userfaultfd
pub fn is_uffd_fd(fd: i32) -> bool {
    instance_index(fd).is_some()
}

// ============================================================================
// 生命周期
// ============================================================================

/// 创建 userfaultfd 实例 (成功返回 fd)
pub fn create(owner_pid: u32) -> Option<i32> {
    use crate::privileged::proc::fd_alloc::{FdSubsystem, alloc_fd};
    let fd = alloc_fd(FdSubsystem::UserFaultFd)?;
    let Some(idx) = instance_index(fd) else {
        let _ = crate::privileged::proc::fd_alloc::free_fd(FdSubsystem::UserFaultFd, fd);
        return None;
    };
    let mut t = uffd_table().lock();
    t[idx] = UffdInstance::empty();
    t[idx].used = true;
    t[idx].owner_pid = owner_pid;
    Some(fd)
}

/// 释放 userfaultfd 实例 (唤醒所有等待者), 返回是否成功释放
pub fn release(fd: i32) -> bool {
    let Some(idx) = instance_index(fd) else {
        return false;
    };
    let (fault_pid, reader_pid) = {
        let mut t = uffd_table().lock();
        if !t[idx].used {
            return false;
        }
        let fp = t[idx].fault_pid;
        let rp = t[idx].reader_pid;
        t[idx] = UffdInstance::empty();
        (fp, rp)
    };
    // 唤醒等待者, 避免进程永久阻塞 (close 语义: 等待者应收到错误并重新尝试)
    if fault_pid != 0 {
        crate::privileged::proc::scheduler_unblock(fault_pid);
    }
    if reader_pid != 0 {
        crate::privileged::proc::scheduler_unblock(reader_pid);
    }
    crate::privileged::proc::fd_alloc::free_fd(
        crate::privileged::proc::fd_alloc::FdSubsystem::UserFaultFd,
        fd,
    )
}

// ============================================================================
// UFFDIO_API / REGISTER / UNREGISTER / WAKE (握手与区间管理)
// ============================================================================

/// `UFFDIO_API` 握手: 校验 `api` 版本并回填 `ioctls` 支持位图
///
/// # Errors
/// `api != UFFD_API` 时返回 `EINVAL` (Linux 语义: 不支持的 API 版本).
pub fn api_negotiate(
    fd: i32,
    api: u64,
    features: u64,
) -> Result<UffdIoApi, crate::privileged::syscall::Errno> {
    use crate::privileged::syscall::Errno;
    let Some(idx) = instance_index(fd) else {
        return Err(Errno::EBADF);
    };
    if api != UFFD_API {
        return Err(Errno::EINVAL);
    }
    let mut t = uffd_table().lock();
    if !t[idx].used {
        return Err(Errno::EBADF);
    }
    t[idx].api_negotiated = true;
    // SIMPLIFIED: 未实装 WP/MINOR 与 fork/remap/remove, 请求特性一律忽略 (Linux 会清零
    // 不支持的位并要求调用方复核). 影响: 调用方拿到 features=0 即知内核只支持 MISSING.
    let _ = features;
    Ok(UffdIoApi {
        api: UFFD_API,
        features: 0,
        ioctls: UFFDIO_REGISTER | UFFDIO_UNREGISTER | UFFDIO_WAKE | UFFDIO_COPY | UFFDIO_ZEROPAGE,
    })
}

/// 注册缺页拦截区间
///
/// SIMPLIFIED: 要求区间被现有 VMA 完整覆盖 (见 `MmStruct::range_is_mapped`, 允许为
/// VMA 的真子集, 但不允许含空洞); 区间独立于 VMA 拆分, 无需 `split_vma`. 模式仅支持
/// MISSING.
///
/// # Errors
/// - fd 无效 → `EBADF`; 未握手 `UFFDIO_API` → `EINVAL`
/// - `len == 0` / 非页对齐 / 模式不含 MISSING → `EINVAL`
/// - 区间未被完整映射 → `EFAULT`; 注册区间数超限 → `ENOSPC`
pub fn register(
    fd: i32,
    start: u64,
    len: u64,
    mode: u64,
) -> Result<(), crate::privileged::syscall::Errno> {
    use crate::privileged::syscall::Errno;
    let Some(idx) = instance_index(fd) else {
        return Err(Errno::EBADF);
    };
    if len == 0 || !start.is_multiple_of(PAGE_SIZE) || !len.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    if mode == 0 || mode & UFFDIO_REGISTER_MODE_MISSING == 0 {
        return Err(Errno::EINVAL);
    }
    let end = start.checked_add(len).ok_or(Errno::EINVAL)?;
    let start_usize = usize::try_from(start).map_err(|_| Errno::EINVAL)?;
    let end_usize = usize::try_from(end).map_err(|_| Errno::EINVAL)?;
    let Some(mm) = crate::privileged::mm::vma_get_current_mm() else {
        return Err(Errno::EFAULT);
    };
    if !mm.range_is_mapped(start_usize, end_usize) {
        return Err(Errno::EFAULT);
    }

    let mut t = uffd_table().lock();
    if !t[idx].used {
        return Err(Errno::EBADF);
    }
    if !t[idx].api_negotiated {
        return Err(Errno::EINVAL);
    }
    if t[idx].range_count >= MAX_RANGES {
        return Err(Errno::ENOSPC);
    }
    let n = t[idx].range_count;
    t[idx].ranges[n] = UffdRange { start, end, mode };
    t[idx].range_count = n + 1;
    Ok(())
}

/// 注销缺页拦截区间 (`start`/`len` 必须与某次注册完全一致)
///
/// # Errors
/// fd 无效 → `EBADF`; 无匹配区间 → `EINVAL`.
pub fn unregister(fd: i32, start: u64, len: u64) -> Result<(), crate::privileged::syscall::Errno> {
    use crate::privileged::syscall::Errno;
    let Some(idx) = instance_index(fd) else {
        return Err(Errno::EBADF);
    };
    let end = start.checked_add(len).ok_or(Errno::EINVAL)?;
    let mut t = uffd_table().lock();
    if !t[idx].used {
        return Err(Errno::EBADF);
    }
    let mut found = None;
    for i in 0..t[idx].range_count {
        if t[idx].ranges[i].start == start && t[idx].ranges[i].end == end {
            found = Some(i);
            break;
        }
    }
    let Some(i) = found else {
        return Err(Errno::EINVAL);
    };
    // 紧凑删除
    let n = t[idx].range_count;
    for j in i..n - 1 {
        let next = t[idx].ranges[j + 1];
        t[idx].ranges[j] = next;
    }
    t[idx].range_count = n - 1;
    t[idx].ranges[n - 1] = UffdRange::default();
    Ok(())
}

/// `UFFDIO_WAKE`: 唤醒当前等待中的缺页进程, 并复位挂起状态
///
/// SIMPLIFIED: 无独立唤醒队列 (单挂起页模型), 仅当 `range` 覆盖当前挂起页时生效;
/// 被唤醒的进程会重新缺页并再次入队事件. 返回是否唤醒了等待者.
///
/// # Errors
/// - fd 无效 → `EBADF`
/// - `start + len` 溢出 → `EINVAL`
pub fn wake(fd: i32, start: u64, len: u64) -> Result<bool, crate::privileged::syscall::Errno> {
    use crate::privileged::syscall::Errno;
    let Some(idx) = instance_index(fd) else {
        return Err(Errno::EBADF);
    };
    let end = start.checked_add(len).ok_or(Errno::EINVAL)?;
    let pid = {
        let mut t = uffd_table().lock();
        if !t[idx].used {
            return Err(Errno::EBADF);
        }
        let page = t[idx].fault_page;
        if t[idx].page_state == PendingState::Idle || page < start || page >= end {
            0
        } else {
            let pid = t[idx].fault_pid;
            t[idx].page_state = PendingState::Idle;
            t[idx].fault_page = 0;
            t[idx].fault_pid = 0;
            pid
        }
    };
    if pid != 0 {
        crate::privileged::proc::scheduler_unblock(pid);
        Ok(true)
    } else {
        Ok(false)
    }
}

// ============================================================================
// 数据提供 (服务线程侧): UFFDIO_COPY / UFFDIO_ZEROPAGE
// ============================================================================

/// 服务线程为挂起缺页页提供数据 (`data == None` 表示填零页)
///
/// 成功后唤醒缺页进程; 该进程重入 #PF 时由 `fill_provided_page` 完成映射.
///
/// # Errors
/// - fd 无效 → `EBADF`; `page_addr` 非页对齐 → `EINVAL`
/// - 无挂起缺页 / 页不匹配 / 已有数据待映射 / 数据长度不为整页 → `EINVAL`
pub fn provide_page(
    fd: i32,
    page_addr: u64,
    data: Option<&[u8]>,
) -> Result<(), crate::privileged::syscall::Errno> {
    use crate::privileged::syscall::Errno;
    let Some(idx) = instance_index(fd) else {
        return Err(Errno::EBADF);
    };
    if !page_addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    if let Some(d) = data {
        if d.len() != PAGE_SIZE as usize {
            return Err(Errno::EINVAL);
        }
    }
    let pid = {
        let mut t = uffd_table().lock();
        if !t[idx].used {
            return Err(Errno::EBADF);
        }
        if t[idx].page_state != PendingState::Waiting || t[idx].fault_page != page_addr {
            return Err(Errno::EINVAL);
        }
        if let Some(d) = data {
            t[idx].staged.copy_from_slice(d);
            t[idx].staged_zero = false;
        } else {
            t[idx].staged_zero = true;
        }
        t[idx].page_state = PendingState::Provided;
        t[idx].fault_pid
    };
    if pid != 0 {
        crate::privileged::proc::scheduler_unblock(pid);
    }
    Ok(())
}

// ============================================================================
// 缺页路径 (#PF)
// ============================================================================

/// `fault_notify` 结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UffdFaultOutcome {
    /// 未注册 (走内核 demand paging)
    NotRegistered,
    /// 已排队事件, 调用方应阻塞当前进程等待数据
    Waiting,
    /// 数据已就绪, 调用方应分配物理页后调用 `fill_provided_page` 完成映射
    Ready,
}

/// #PF 缺页登记 (仅表访问, 无阻塞/无分配)
///
/// `page_addr` 必须为页对齐地址; 返回 `Waiting` 时调用方须返回
/// `PfResult::UffdWait` (由 handlers 层标记阻塞).
pub fn fault_notify(page_addr: u64, flags: u64) -> UffdFaultOutcome {
    let pid = crate::privileged::proc::process_get_current_pid();
    let mut wake_reader = 0u32;

    let outcome = {
        let mut t = uffd_table().lock();
        let mut found = None;
        for i in 0..MAX_INSTANCES {
            if t[i].used && t[i].covers(page_addr) {
                found = Some(i);
                break;
            }
        }
        let Some(idx) = found else {
            return UffdFaultOutcome::NotRegistered;
        };
        let inst = &mut t[idx];

        if inst.page_state == PendingState::Provided && inst.fault_page == page_addr {
            UffdFaultOutcome::Ready
        } else if inst.page_state == PendingState::Waiting && inst.fault_page == page_addr {
            // 同一页重复缺页 (阻塞前的忙重试窗口): 不重复入队事件
            inst.fault_pid = pid;
            UffdFaultOutcome::Waiting
        } else if inst.ev_count >= MAX_EVENTS {
            // SIMPLIFIED: 队列满则放弃拦截, 由内核 demand paging 兜底 (不永久挂起)
            UffdFaultOutcome::NotRegistered
        } else {
            let tail = (inst.ev_head + inst.ev_count) % MAX_EVENTS;
            inst.events[tail] = UffdMsgPagefault {
                event: UFFD_EVENT_PAGEFAULT,
                reserved1: 0,
                reserved2: 0,
                reserved3: 0,
                flags,
                address: page_addr,
                ptid: pid,
                reserved4: 0,
            };
            inst.ev_count += 1;
            inst.fault_pid = pid;
            inst.fault_page = page_addr;
            inst.page_state = PendingState::Waiting;
            // 唤醒阻塞在 read() 的服务线程 (本路径在用户态 #PF 上下文,
            // 被中断的是用户代码, 未持内核锁)
            wake_reader = inst.reader_pid;
            UffdFaultOutcome::Waiting
        }
    };

    if wake_reader != 0 {
        crate::privileged::proc::scheduler_unblock(wake_reader);
    }
    outcome
}

/// 将已就绪的暂存数据填充到 `phys` 页 (`fault_notify` 返回 `Ready` 后调用)
///
/// 成功后复位挂起状态; 返回是否填充成功 (状态不匹配返回 false).
pub fn fill_provided_page(page_addr: u64, phys: PhysAddr) -> bool {
    let mut t = uffd_table().lock();
    let mut found = None;
    for i in 0..MAX_INSTANCES {
        if t[i].used && t[i].covers(page_addr) {
            found = Some(i);
            break;
        }
    }
    let Some(idx) = found else {
        return false;
    };
    let inst = &mut t[idx];
    if inst.page_state != PendingState::Provided || inst.fault_page != page_addr {
        return false;
    }

    // SAFETY: phys 由调用方以 PMM 分配的页帧传入, `to_virt` 得到恒等/高半核映射的
    // 有效内核虚拟地址; 该页尚未映射到任何用户地址空间 (调用方刚分配), 独占写入.
    unsafe {
        let dst = phys.to_virt().0 as *mut u8;
        if inst.staged_zero {
            core::ptr::write_bytes(dst, 0, PAGE_SIZE as usize);
        } else {
            core::ptr::copy_nonoverlapping(inst.staged.as_ptr(), dst, PAGE_SIZE as usize);
        }
    }

    inst.page_state = PendingState::Idle;
    inst.fault_page = 0;
    inst.fault_pid = 0;
    inst.staged_zero = false;
    true
}

// ============================================================================
// 事件读取 (服务线程侧)
// ============================================================================

/// 弹出队首事件 (非阻塞)
pub fn pop_event(fd: i32) -> Option<UffdMsgPagefault> {
    let idx = instance_index(fd)?;
    let mut t = uffd_table().lock();
    if !t[idx].used || t[idx].ev_count == 0 {
        return None;
    }
    let head = t[idx].ev_head;
    let ev = t[idx].events[head];
    t[idx].events[head] = UffdMsgPagefault::default();
    t[idx].ev_head = (head + 1) % MAX_EVENTS;
    t[idx].ev_count -= 1;
    Some(ev)
}

/// 登记 `read()` 阻塞的线程 pid (供 `fault_notify` 唤醒)
pub fn set_reader(fd: i32, pid: u32) {
    if let Some(idx) = instance_index(fd) {
        let mut t = uffd_table().lock();
        if t[idx].used {
            t[idx].reader_pid = pid;
        }
    }
}

/// 注销 `read()` 阻塞的线程 pid
pub fn clear_reader(fd: i32, pid: u32) {
    if let Some(idx) = instance_index(fd) {
        let mut t = uffd_table().lock();
        if t[idx].used && t[idx].reader_pid == pid {
            t[idx].reader_pid = 0;
        }
    }
}

/// 实例是否仍处于打开状态 (用于 `read()` 阻塞后被 close 唤醒时提前返回)
pub fn is_open(fd: i32) -> bool {
    match instance_index(fd) {
        Some(idx) => uffd_table().lock()[idx].used,
        None => false,
    }
}

/// 实例是否已握手 `UFFDIO_API` 且有注册区间 (供测试/诊断)
pub fn is_active(fd: i32) -> bool {
    match instance_index(fd) {
        Some(idx) => {
            let t = uffd_table().lock();
            t[idx].used && t[idx].api_negotiated && t[idx].range_count > 0
        }
        None => false,
    }
}
