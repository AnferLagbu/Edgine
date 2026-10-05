#![deny(unsafe_code)]
//! @SAFE: 本文件不含 unsafe 代码。所有 unsafe 操作已委托至 privileged API。
//! cgroup (Control Group) — 资源限制、统计与隔离 — privileged 机制实现
//!
//! ## DECISION-J 归属反转记录 (2026-09-13)
//!
//! 策略代码 (控制器 + cgroup 实例 + 全局管理器 + syscall) 于 T1-4
//! (2026-06-16) 迁至 `functions::proc::cgroup`, 本文件仅 re-export。按统一
//! 判据反转：cgroup 层级管理器是进程资源控制机制状态, 被 privileged proc
//! 顶层导出消费 — 属机制项, 迁回。依赖闭包仅 privileged。
//!
//! functions 侧改 `pub use crate::privileged::proc::cgroup::*`
//! 保持 API 兼容 (functions→privileged 合法方向)。
//! 日志使用 privileged::klog::serial_write_bytes (safe API).

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::privileged::klog::serial_write_bytes;
use crate::privileged::proc::Pid;
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::sync::OnceLock;

// ============================================================================
// 常量
// ============================================================================

pub const CGROUP_MAX_DEPTH: usize = 2;
pub const CGROUP_MAX_PROCS: u32 = 4096;
pub const CPU_CFS_PERIOD_DEFAULT_US: u64 = 1_000_000;
pub const CPU_CFS_QUOTA_MAX: u64 = u64::MAX;
pub const MEMORY_LIMIT_MAX: u64 = u64::MAX;
pub const PIDS_MAX_DEFAULT: u64 = u64::MAX;

// ============================================================================
// Cgroup ID 分配
// ============================================================================

static NEXT_CGROUP_ID: AtomicU64 = AtomicU64::new(1);

fn alloc_cgroup_id() -> u64 {
    NEXT_CGROUP_ID.fetch_add(1, Ordering::Relaxed)
}

// ============================================================================
// CPU 控制器
// ============================================================================

/// CPU 控制器 — 按 CFS 配额与周期限制 CPU 带宽, 并统计运行时长与节流次数。
#[derive(Debug)]
pub struct CpuController {
    pub cfs_quota_us: AtomicU64,
    pub cfs_period_us: AtomicU64,
    pub runtime_used: AtomicU64,
    pub nr_throttled: AtomicU64,
    pub throttled_time: AtomicU64,
}

impl CpuController {
    pub fn new() -> Self {
        Self {
            cfs_quota_us: AtomicU64::new(CPU_CFS_QUOTA_MAX),
            cfs_period_us: AtomicU64::new(CPU_CFS_PERIOD_DEFAULT_US),
            runtime_used: AtomicU64::new(0),
            nr_throttled: AtomicU64::new(0),
            throttled_time: AtomicU64::new(0),
        }
    }

    pub fn check_budget(&self, delta_us: u64) -> bool {
        let quota = self.cfs_quota_us.load(Ordering::Acquire);
        if quota == CPU_CFS_QUOTA_MAX {
            return true;
        }
        let used = self.runtime_used.fetch_add(delta_us, Ordering::AcqRel);
        used + delta_us <= quota
    }

    pub fn period_reset(&self) {
        let quota = self.cfs_quota_us.load(Ordering::Acquire);
        if quota != CPU_CFS_QUOTA_MAX {
            let prev = self.runtime_used.swap(0, Ordering::AcqRel);
            if prev > quota {
                self.nr_throttled.fetch_add(1, Ordering::Relaxed);
                self.throttled_time
                    .fetch_add(prev - quota, Ordering::Relaxed);
            }
        }
    }

    pub fn set_quota(&self, quota_us: u64) {
        self.cfs_quota_us.store(quota_us, Ordering::Release);
    }

    pub fn set_period(&self, period_us: u64) {
        if period_us > 0 {
            self.cfs_period_us.store(period_us, Ordering::Release);
        }
    }
}

// ============================================================================
// 内存控制器
// ============================================================================

/// 内存控制器 — 限制 cgroup 内存用量, 并统计峰值用量与 OOM 击杀次数。
#[derive(Debug)]
pub struct MemoryController {
    pub limit_in_bytes: AtomicU64,
    pub usage_in_bytes: AtomicU64,
    pub max_usage_in_bytes: AtomicU64,
    pub oom_kill_count: AtomicU64,
    pub oom_kill_disable: AtomicU32,
}

impl MemoryController {
    pub fn new() -> Self {
        Self {
            limit_in_bytes: AtomicU64::new(MEMORY_LIMIT_MAX),
            usage_in_bytes: AtomicU64::new(0),
            max_usage_in_bytes: AtomicU64::new(0),
            oom_kill_count: AtomicU64::new(0),
            oom_kill_disable: AtomicU32::new(0),
        }
    }

    #[expect(
        clippy::needless_continue,
        reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
    )]
    pub fn try_charge(&self, bytes: u64) -> bool {
        let limit = self.limit_in_bytes.load(Ordering::Acquire);
        if limit == MEMORY_LIMIT_MAX {
            let new = self.usage_in_bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
            self.update_max(new);
            return true;
        }
        loop {
            let current = self.usage_in_bytes.load(Ordering::Acquire);
            if current + bytes > limit {
                return false;
            }
            match self.usage_in_bytes.compare_exchange_weak(
                current,
                current + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.update_max(current + bytes);
                    return true;
                }
                Err(_) => continue,
            }
        }
    }

    pub fn uncharge(&self, bytes: u64) {
        let prev = self.usage_in_bytes.fetch_sub(bytes, Ordering::AcqRel);
        if prev < bytes {
            self.usage_in_bytes.store(0, Ordering::Release);
        }
    }

    pub fn set_limit(&self, limit_bytes: u64) {
        self.limit_in_bytes.store(limit_bytes, Ordering::Release);
    }

    pub fn is_over_limit(&self) -> bool {
        let limit = self.limit_in_bytes.load(Ordering::Acquire);
        if limit == MEMORY_LIMIT_MAX {
            return false;
        }
        self.usage_in_bytes.load(Ordering::Acquire) > limit
    }

    #[expect(
        clippy::needless_continue,
        reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
    )]
    fn update_max(&self, current: u64) {
        loop {
            let max = self.max_usage_in_bytes.load(Ordering::Acquire);
            if current <= max {
                break;
            }
            match self.max_usage_in_bytes.compare_exchange_weak(
                max,
                current,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }
}

// ============================================================================
// PID 控制器
// ============================================================================

/// PID 控制器 — 限制 cgroup 内进程/线程总数, 并统计 fork 失败次数。
#[derive(Debug)]
pub struct PidsController {
    pub pids_max: AtomicU64,
    pub current: AtomicU64,
    pub events_fork_fail: AtomicU64,
}

impl PidsController {
    pub fn new() -> Self {
        Self {
            pids_max: AtomicU64::new(PIDS_MAX_DEFAULT),
            current: AtomicU64::new(0),
            events_fork_fail: AtomicU64::new(0),
        }
    }

    #[expect(
        clippy::needless_continue,
        reason = "needless_continue: continue 提升循环可读性; 当前优先 expect"
    )]
    pub fn try_fork(&self) -> bool {
        let max = self.pids_max.load(Ordering::Acquire);
        if max == PIDS_MAX_DEFAULT {
            self.current.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        loop {
            let cur = self.current.load(Ordering::Acquire);
            if cur >= max {
                self.events_fork_fail.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            match self.current.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue,
            }
        }
    }

    pub fn exit(&self) {
        let prev = self.current.fetch_sub(1, Ordering::AcqRel);
        if prev == 0 {
            self.current.store(0, Ordering::Release);
        }
    }

    pub fn set_max(&self, max: u64) {
        self.pids_max.store(max, Ordering::Release);
    }
}

// ============================================================================
// IO 控制器
// ============================================================================

/// IO 控制器 — 统计 cgroup 的读写字节数与 IO 次数, 并提供带宽/IOPS 上限字段。
#[derive(Debug)]
pub struct IoController {
    pub read_bps_max: AtomicU64,
    pub write_bps_max: AtomicU64,
    pub read_iops_max: AtomicU64,
    pub write_iops_max: AtomicU64,
    pub stat_read_bytes: AtomicU64,
    pub stat_write_bytes: AtomicU64,
    pub stat_read_ios: AtomicU64,
    pub stat_write_ios: AtomicU64,
}

impl IoController {
    pub fn new() -> Self {
        Self {
            read_bps_max: AtomicU64::new(u64::MAX),
            write_bps_max: AtomicU64::new(u64::MAX),
            read_iops_max: AtomicU64::new(u64::MAX),
            write_iops_max: AtomicU64::new(u64::MAX),
            stat_read_bytes: AtomicU64::new(0),
            stat_write_bytes: AtomicU64::new(0),
            stat_read_ios: AtomicU64::new(0),
            stat_write_ios: AtomicU64::new(0),
        }
    }

    pub fn account_read(&self, bytes: u64) {
        self.stat_read_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.stat_read_ios.fetch_add(1, Ordering::Relaxed);
    }

    pub fn account_write(&self, bytes: u64) {
        self.stat_write_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.stat_write_ios.fetch_add(1, Ordering::Relaxed);
    }
}

// ============================================================================
// CgroupRq — cgroup 实例
// ============================================================================

/// cgroup 运行时实例 — 保存层级关系、成员进程及各资源控制器。
#[derive(Debug)]
pub struct CgroupRq {
    pub id: u64,
    pub name: IrqSpinLock<String>,
    pub parent_id: u64,
    pub children: IrqSpinLock<Vec<u64>>,
    pub procs: IrqSpinLock<Vec<Pid>>,
    pub cpu: CpuController,
    pub memory: MemoryController,
    pub pids: PidsController,
    pub io: IoController,
}

impl CgroupRq {
    pub fn new_root() -> Self {
        Self {
            id: 0,
            name: IrqSpinLock::new(String::from("/")),
            parent_id: 0,
            children: IrqSpinLock::new(Vec::new()),
            procs: IrqSpinLock::new(Vec::new()),
            cpu: CpuController::new(),
            memory: MemoryController::new(),
            pids: PidsController::new(),
            io: IoController::new(),
        }
    }

    pub fn new_child(name: &str, parent_id: u64) -> Self {
        Self {
            id: alloc_cgroup_id(),
            name: IrqSpinLock::new(String::from(name)),
            parent_id,
            children: IrqSpinLock::new(Vec::new()),
            procs: IrqSpinLock::new(Vec::new()),
            cpu: CpuController::new(),
            memory: MemoryController::new(),
            pids: PidsController::new(),
            io: IoController::new(),
        }
    }

    pub fn attach_proc(&self, pid: Pid) -> bool {
        let mut procs = self.procs.lock();
        if procs.len() >= CGROUP_MAX_PROCS as usize {
            return false;
        }
        if procs.contains(&pid) {
            return true;
        }
        procs.push(pid);
        self.pids.try_fork()
    }

    pub fn detach_proc(&self, pid: Pid) {
        let mut procs = self.procs.lock();
        if let Some(pos) = procs.iter().position(|&p| p == pid) {
            procs.swap_remove(pos);
            self.pids.exit();
        }
    }
}

// ============================================================================
// CgroupSubsystem — 全局管理器
// ============================================================================

/// cgroup 子系统全局管理器 — 维护 id 到 cgroup 的映射, 负责创建/删除/迁移。
pub struct CgroupSubsystem {
    groups: IrqSpinLock<BTreeMap<u64, Arc<CgroupRq>>>,
    root: Arc<CgroupRq>,
}

impl CgroupSubsystem {
    pub fn new() -> Self {
        let root = Arc::new(CgroupRq::new_root());
        let mut groups = BTreeMap::new();
        groups.insert(0, Arc::clone(&root));

        Self {
            groups: IrqSpinLock::new(groups),
            root,
        }
    }

    pub fn root(&self) -> &Arc<CgroupRq> {
        &self.root
    }

    pub fn create_cgroup(&self, parent_id: u64, name: &str) -> u64 {
        let mut groups = self.groups.lock();

        if !groups.contains_key(&parent_id) {
            return 0;
        }

        if parent_id != 0 {
            if let Some(parent) = groups.get(&parent_id) {
                if parent.parent_id != 0 {
                    return 0;
                }
            }
        }

        let cg = Arc::new(CgroupRq::new_child(name, parent_id));
        let id = cg.id;

        if let Some(parent) = groups.get(&parent_id) {
            parent.children.lock().push(id);
        }

        groups.insert(id, cg);
        id
    }

    /// 删除指定 cgroup.
    ///
    /// 要求该 cgroup 不存在存活进程且没有子 cgroup.
    ///
    /// # Errors
    ///
    /// - `id == 0`(根 cgroup)或组内仍有进程/子组 → `EBUSY`
    /// - 指定的 cgroup 不存在 → `ENOENT`
    pub fn remove_cgroup(&self, id: u64) -> Result<(), Errno> {
        if id == 0 {
            return Err(Errno::EBUSY);
        }

        let mut groups = self.groups.lock();
        let cg = match groups.get(&id) {
            Some(c) => Arc::clone(c),
            None => return Err(Errno::ENOENT),
        };

        if !cg.procs.lock().is_empty() {
            return Err(Errno::EBUSY);
        }

        if !cg.children.lock().is_empty() {
            return Err(Errno::EBUSY);
        }

        if let Some(parent) = groups.get(&cg.parent_id) {
            let mut children = parent.children.lock();
            if let Some(pos) = children.iter().position(|&c| c == id) {
                children.swap_remove(pos);
            }
        }

        groups.remove(&id);
        Ok(())
    }

    pub fn find(&self, id: u64) -> Option<Arc<CgroupRq>> {
        self.groups.lock().get(&id).map(Arc::clone)
    }

    /// 将进程迁移到目标 cgroup.
    ///
    /// 先从原所在组分离, 再附加到目标组.
    ///
    /// # Errors
    ///
    /// - 目标 cgroup 不存在 → `ENOENT`
    /// - 目标组附加失败(如容量已满) → `EAGAIN`
    pub fn migrate(&self, pid: Pid, target_id: u64) -> Result<(), Errno> {
        let target = self.find(target_id).ok_or(Errno::ENOENT)?;

        {
            let groups = self.groups.lock();
            for cg in groups.values() {
                let procs = cg.procs.lock();
                if procs.contains(&pid) {
                    drop(procs);
                    cg.detach_proc(pid);
                    break;
                }
            }
        }

        if !target.attach_proc(pid) {
            return Err(Errno::EAGAIN);
        }

        Ok(())
    }

    pub fn cgroup_of(&self, pid: Pid) -> Option<Arc<CgroupRq>> {
        let groups = self.groups.lock();
        for cg in groups.values() {
            if cg.procs.lock().contains(&pid) {
                return Some(Arc::clone(cg));
            }
        }
        None
    }
}

// ============================================================================
// Errno
// ============================================================================

/// cgroup 子系统错误码 (与 POSIX errno 数值对齐)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
#[allow(clippy::upper_case_acronyms)]
pub enum Errno {
    EPERM = 1,
    ENOENT = 2,
    EAGAIN = 11,
    ENOMEM = 12,
    EBUSY = 16,
    EINVAL = 22,
}

// ============================================================================
// 全局 cgroup 子系统实例
// ============================================================================

static CGROUP_SUBSYSTEM: OnceLock<CgroupSubsystem> = OnceLock::new();
static CGROUP_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// 初始化全局 cgroup 子系统, 并创建根 cgroup (id=0)。
pub fn cgroup_init() {
    CGROUP_SUBSYSTEM.get_or_init(|slot| {
        slot.write(CgroupSubsystem::new());
    });
    CGROUP_INITIALIZED.store(true, Ordering::Release);
    serial_write_bytes(b"[CGROUP] subsystem initialized (root cgroup id=0)\n");
}

/// 获取全局 cgroup 子系统实例的引用.
///
/// # Panics
///
/// 当子系统尚未通过 `cgroup_init()` 初始化时 panic.
pub fn cgroup_subsystem() -> &'static CgroupSubsystem {
    CGROUP_SUBSYSTEM
        .get()
        .expect("cgroup subsystem not initialized")
}

/// 返回 cgroup 子系统是否已完成初始化。
pub fn cgroup_is_initialized() -> bool {
    CGROUP_INITIALIZED.load(Ordering::Acquire)
}

// ============================================================================
// 系统调用
// ============================================================================

/// 在指定父 cgroup 下创建子 cgroup。
///
/// 成功返回新 cgroup id, 初始化未完成或父组非法时返回负错误码。
pub fn sys_cgroup_create(parent_id: u64, _name_ptr: u64, _name_len: u64) -> i64 {
    if !cgroup_is_initialized() {
        return -(Errno::EINVAL as i64);
    }

    let name = alloc::format!("cg_{parent_id}");
    let id = cgroup_subsystem().create_cgroup(parent_id, &name);
    if id == 0 {
        return -(Errno::EINVAL as i64);
    }
    id as i64
}

/// 销毁指定 cgroup。
///
/// 成功返回 0, 初始化未完成或销毁失败时返回负错误码。
pub fn sys_cgroup_destroy(cg_id: u64) -> i64 {
    if !cgroup_is_initialized() {
        return -(Errno::EINVAL as i64);
    }
    match cgroup_subsystem().remove_cgroup(cg_id) {
        Ok(()) => 0,
        Err(e) => -(e as i64),
    }
}

/// 将进程 `pid` 迁移到指定 cgroup。
///
/// 成功返回 0, 初始化未完成或迁移失败时返回负错误码。
pub fn sys_cgroup_attach(cg_id: u64, pid: u64) -> i64 {
    if !cgroup_is_initialized() {
        return -(Errno::EINVAL as i64);
    }
    match cgroup_subsystem().migrate(pid as Pid, cg_id) {
        Ok(()) => 0,
        Err(e) => -(e as i64),
    }
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 设置指定 cgroup 的资源上限。
///
/// `controller`: 0=CFS 配额, 1=CFS 周期, 2=内存, 3=PID 上限。
/// 成功返回 0, 初始化未完成或控制器非法时返回负错误码。
pub fn sys_cgroup_set_limit(cg_id: u64, controller: u64, value: u64) -> i64 {
    if !cgroup_is_initialized() {
        return -(Errno::EINVAL as i64);
    }

    let cg = match cgroup_subsystem().find(cg_id) {
        Some(c) => c,
        None => return -(Errno::ENOENT as i64),
    };

    match controller {
        0 => cg.cpu.set_quota(value),
        1 => cg.cpu.set_period(value),
        2 => cg.memory.set_limit(value),
        3 => cg.pids.set_max(value),
        _ => return -(Errno::EINVAL as i64),
    }

    0
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
/// 读取指定 cgroup 的统计值。
///
/// `stat_type` 选择统计项, 成功返回非负值, 初始化未完成或项非法时返回负错误码。
pub fn sys_cgroup_get_stat(cg_id: u64, stat_type: u64) -> i64 {
    if !cgroup_is_initialized() {
        return -(Errno::EINVAL as i64);
    }

    let cg = match cgroup_subsystem().find(cg_id) {
        Some(c) => c,
        None => return -(Errno::ENOENT as i64),
    };

    match stat_type {
        0 => cg.cpu.runtime_used.load(Ordering::Acquire) as i64,
        1 => cg.memory.usage_in_bytes.load(Ordering::Acquire) as i64,
        2 => cg.pids.current.load(Ordering::Acquire) as i64,
        3 => cg.io.stat_read_bytes.load(Ordering::Acquire) as i64,
        4 => cg.io.stat_write_bytes.load(Ordering::Acquire) as i64,
        5 => cg.cpu.nr_throttled.load(Ordering::Acquire) as i64,
        6 => cg.memory.max_usage_in_bytes.load(Ordering::Acquire) as i64,
        7 => cg.pids.events_fork_fail.load(Ordering::Acquire) as i64,
        _ => -(Errno::EINVAL as i64),
    }
}
