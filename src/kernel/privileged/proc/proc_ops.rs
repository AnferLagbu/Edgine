//! 进程操作子模块 — 进程创建/销毁/查询/操作
//!
//! 从 `api.rs` 拆分而来, 包含所有 `process_*` 函数和进程管理相关操作。
//!
//! ## 依赖
//! - `api::raw` — 裸指针/FFI 桥接
//! - `api::{C_CURRENT_PROCESS, CURRENT_PROCESS_PTR}` — 当前进程状态
//! - `process::PROCESS_TABLE` — 进程表
//! - `scheduler::SCHEDULER` — 调度器
//! - `user_proc::USER_PROC_MANAGER` — 用户进程管理
#![deny(missing_docs)]

use core::sync::atomic::Ordering;

use alloc::sync::Arc;

use super::process::{PROCESS_TABLE, Process};
use super::scheduler::{SCHEDULER, Scheduler};
use super::types::{BlockReason, Pid, ProcessId, ProcessState};
pub use super::user_proc::proc_alloc_pid;
use super::user_proc::{USER_PROC_MANAGER, UserProcManager, user_proc_clone};
use crate::privileged::mm::{
    get_kernel_pml4, vmm_clone_user_page_table_cow, vmm_destroy_page_table, vmm_switch_page_table,
};
use crate::privileged::racy_cell::RacyCell;

// === 特权层: 进程子系统裸指针/FFI 桥接集中地 ===
//
// 本子模块包含所有与 C ABI、裸指针 (进程表 entry) 以及 extern "C" FFI 交互
// 的 `unsafe` 代码。本模块的其余部分 (`api.rs` 顶层) 保持 100% 安全 Rust,
// 通过 `raw::*` 安全函数访问底层功能。
/// 特权层裸指针 / C ABI FFI 桥接子模块.
pub mod raw {
    use super::{
        Process, ProcessId, vmm_clone_user_page_table_cow, vmm_destroy_page_table,
        vmm_switch_page_table,
    };

    /// 从裸指针读取 `&Process`。
    ///
    /// # Safety (内部)
    /// - `ptr` 必须为非空, 指向有效 `Process` 实例
    pub fn process_ref<'a>(ptr: *const Process) -> &'a Process {
        // 以地址值中转: 裸指针参数不直接解引用, 解引用集中在本函数的 unsafe 块内.
        let addr = ptr as usize;
        // SAFETY: 调用方在 fork/exec 路径中保证 ptr 来自 PROCESS_TABLE
        unsafe { &*(addr as *const Process) }
    }

    /// 从裸指针读取 `&mut Process`。
    ///
    /// # Safety (内部)
    /// - `ptr` 必须为非空, 指向有效 `Process` 实例
    /// - 同一时刻只能存在一个可变引用
    pub fn process_ref_mut<'a>(ptr: *mut Process) -> &'a mut Process {
        // 以地址值中转: 裸指针参数不直接解引用, 解引用集中在本函数的 unsafe 块内.
        let addr = ptr as usize;
        // SAFETY: 调用方在 fork 路径中保证 `&mut` 唯一性
        unsafe { &mut *(addr as *mut Process) }
    }

    /// 分配并构造一个 `Process` (用于 fork 创建子进程)。
    ///
    /// # Safety (内部)
    /// - 调用方在 fork 错误路径中负责 `dealloc_process` 释放。
    #[must_use]
    pub fn alloc_process(pid: u32, name: &str, parent: Option<ProcessId>) -> *mut Process {
        // SAFETY: alloc/dealloc 配对, 见 dealloc_process。
        unsafe {
            let layout = alloc::alloc::Layout::new::<Process>();
            let ptr = alloc::alloc::alloc(layout) as usize as *mut Process;
            core::ptr::write(ptr, Process::new(pid, name, parent));
            ptr
        }
    }

    /// 释放 `alloc_process` 分配的内存。
    /// 从裸指针 (Box 所有权) 还原 Box 并 drop。
    ///
    /// # Safety (内部)
    /// - `ptr` 必须由 `Box::into_raw` 产生且未被 drop。
    pub fn drop_boxed_process(ptr: *mut Process) {
        if !ptr.is_null() {
            // 以地址值中转: 裸指针参数不直接传给 Box::from_raw, 转换集中在本 unsafe 块内.
            let addr = ptr as usize;
            // SAFETY: Box 所有权还原。
            unsafe { drop(alloc::boxed::Box::from_raw(addr as *mut Process)) }
        }
    }

    /// 复制父进程内核栈内容到子进程。
    ///
    /// # Safety (内部)
    /// - `dst` 和 `src` 必须都是有效的、已映射的、可写的内核栈地址。
    /// - 区间 `[src, src+size)` 与 `[dst, dst+size)` 不得重叠。
    pub fn copy_kstack(dst: u64, src: u64, size: usize) {
        // SAFETY: 调用方在 fork 路径中保证栈映射有效且区间不重叠。
        unsafe { core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, size) }
    }

    /// 释放 `vmm_clone_user_page_table_cow` 产生的用户页表。
    ///
    /// # Safety (内部)
    /// - `cr3` 必须为 `vmm_clone_user_page_table_cow` 返回的非零物理地址。
    pub fn destroy_user_page_table(cr3: u64) {
        vmm_destroy_page_table(cr3);
    }

    /// 调用 `vmm_switch_page_table` (特权级页表切换)。
    ///
    /// # Safety (内部)
    /// - `cr3` 必须为有效的已建立物理页表基址。
    pub fn switch_page_table(cr3: u64) {
        vmm_switch_page_table(cr3);
    }

    /// 调用 `vmm_clone_user_page_table_cow` (fork 路径, COW 共享页表)。
    ///
    /// # Safety (内部)
    /// - `parent_cr3` 必须是有效的、已建立的用户页表基址 (由 process.cr3 提供)。
    /// - 调用方在子进程使用完毕后, 通过 `destroy_user_page_table` 释放。
    #[must_use]
    pub fn clone_user_page_table_cow(parent_cr3: u64) -> u64 {
        vmm_clone_user_page_table_cow(parent_cr3)
    }

    /// 通过 FFI 输出 info 级别日志。
    ///
    /// # Safety (内部, 由本模块封闭)
    /// - `msg` 必须为以 `\0` 结尾的有效 C 字符串。
    pub fn klog_info(msg: &[u8]) {
        // SAFETY: msg 来自上层调用, 上层中只传入静态字节串字面量。
        unsafe { crate::privileged::klog::klog_ffi_info(msg.as_ptr()) }
    }
}

// === 当前进程状态 (供 api.rs 与本模块共享) ===

static CURRENT_PROCESS_PTR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// TD-10: 当前 CPU 是否处于内核态 (syscall / 中断 / 异常).
///
/// - 0: 用户态 (正常运行)
/// - 1: 内核态
///
/// 单一全局变量, 单核模型. 调度器每 tick 在 `tick_accounting` 读取,
/// syscall dispatch 入口设 1, 出口设 0.
static CURRENT_IN_KERN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// TD-10: 设置当前 CPU 是否处于内核态.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_set_in_kern(v: u32) {
    CURRENT_IN_KERN.store(u64::from(v), Ordering::SeqCst);
}

/// TD-10: 读取当前 CPU 是否处于内核态.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_in_kern() -> u32 {
    crate::privileged::glue::low_u32_from_u64(CURRENT_IN_KERN.load(Ordering::SeqCst))
}

#[repr(C)]
#[derive(Copy, Clone)]
pub(crate) struct CProcess {
    pub(crate) pid: u64,
    pub(crate) session_id: u64,
    pub(crate) parent_pid: u64,
    pub(crate) pwm: u64,
    pub(crate) state: u32,
    pub(crate) exit_code: u64,
    pub(crate) priority: i32,
    pub(crate) cpu_time: u64,
    pub(crate) start_time: u64,
    pub(crate) time_slice: u64,
}

impl CProcess {
    const fn zero() -> Self {
        Self {
            pid: 0,
            session_id: 0,
            parent_pid: 0,
            pwm: 0,
            state: 0,
            exit_code: 0,
            priority: 2,
            cpu_time: 0,
            start_time: 0,
            time_slice: 10,
        }
    }
}

pub(crate) static C_CURRENT_PROCESS: RacyCell<CProcess> = RacyCell::new(CProcess::zero());

// ============================================================================
// 进程查询与操作
// ============================================================================

#[unsafe(no_mangle)]
/// 更新当前进程指针缓存 (`C_CURRENT_PROCESS`) 为给定地址.
pub extern "C" fn update_current_process_ptr(ptr: u64) {
    CURRENT_PROCESS_PTR.store(ptr, Ordering::SeqCst);
    if ptr != 0 {
        let proc_ptr = ptr as *const Process;
        // SAFETY: proc_ptr is a valid Process pointer from the process table.
        let proc = raw::process_ref(proc_ptr);
        let pwm_val = proc.get_pwm();
        let pid_val = u64::from(proc.pid.0);
        C_CURRENT_PROCESS.map_mut(|p| {
            p.pid = pid_val;
            p.pwm = pwm_val;
        });
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 返回当前调度进程的 PID (无则返回 0).
#[unsafe(no_mangle)]
pub extern "C" fn process_get_current_pid() -> u32 {
    Scheduler::current().unwrap_or(0)
}

/// 返回当前调度进程的 TGID (线程组 ID, 无则返回 0).
///
/// 单线程进程的 TGID 等于其 PID; 组内各线程返回同一值 (`getpid` 语义).
#[must_use]
pub fn process_get_current_tgid() -> u32 {
    Scheduler::current()
        .and_then(|pid| PROCESS_TABLE.with_process(pid, super::process::Process::tgid))
        .unwrap_or(0)
}

/// 检查指定 PID 的进程是否存在.
#[inline]
pub fn process_exists(pid: u32) -> bool {
    PROCESS_TABLE.get(pid).is_some()
}

/// 尝试增加进程引用计数, 返回是否成功.
#[inline]
pub fn process_try_inc_ref(pid: u32) -> bool {
    PROCESS_TABLE.try_inc_ref(pid)
}

/// 减少进程引用计数, 若计数归零则释放.
#[inline]
pub fn process_dec_ref(pid: u32) {
    PROCESS_TABLE.dec_ref_and_maybe_free(pid);
}

/// 在持有引用期间读取进程的 CR3 页表基址, 返回 None 表示进程不存在或 cr3 为 0.
pub fn process_get_cr3(pid: u32) -> Option<u64> {
    PROCESS_TABLE
        .with_process(pid, |proc| proc.cr3.load(Ordering::SeqCst))
        .filter(|&c| c != 0)
}

/// 读取进程的 PWM (凭证标识), 返回 None 表示进程不存在.
pub fn process_get_pwm(pid: u32) -> Option<u64> {
    PROCESS_TABLE.with_process(pid, super::process::Process::get_pwm)
}

/// 设置进程的信号 pending 位.
pub fn process_signal_pending_set(pid: u32, sig: u32) {
    PROCESS_TABLE.with_process_mut(pid, |proc| {
        proc.signal_pending_set(sig);
    });
}

/// 对指定进程执行只读闭包操作, 返回闭包结果.
pub fn process_with<F, R>(pid: u32, f: F) -> Option<R>
where
    F: FnOnce(&super::process::Process) -> R,
{
    PROCESS_TABLE.with_process(pid, f)
}

/// 对指定进程执行可变闭包操作, 返回闭包结果.
pub fn process_with_mut<F, R>(pid: u32, f: F) -> Option<R>
where
    F: FnOnce(&mut super::process::Process) -> R,
{
    PROCESS_TABLE.with_process_mut(pid, f)
}

/// 遍历所有进程, 对每个进程执行闭包.
pub fn process_for_each<F>(f: F)
where
    F: FnMut(&super::process::Process) -> bool,
{
    PROCESS_TABLE.for_each(f);
}

/// 获取进程的原始指针 (用于需要直接访问进程的场景).
pub fn process_get_raw(pid: u32) -> Option<*const super::process::Process> {
    PROCESS_TABLE
        .get(pid)
        .map(<*mut super::process::Process>::cast_const)
}

/// 释放子进程 PCB (wait4 回收).
pub fn process_remove_and_free(pid: u32) {
    PROCESS_TABLE.remove_and_free(pid);
}

/// 将进程注册到进程表.
pub fn process_insert(process: *mut super::process::Process) -> bool {
    PROCESS_TABLE.insert(process)
}

/// 对当前进程的 per-process FD 表执行只读闭包操作.
///
/// 返回 `None` 表示当前无进程上下文 (内核早期启动阶段 / host 侧单元测试).
///
/// ## 为何经 `SCHEDULER.get_current_process()`
///
/// `get_current_process` 内部经 `PROCESS_TABLE.get(pid)` 短暂持锁即取到裸指针,
/// 不长期持有进程表锁; 本函数仅在闭包执行期间解引用该指针, 避免
/// `PROCESS_TABLE → FdTable` 长持锁嵌套.
///
/// ## 为何供 `functions::fs` 内联全限定路径调用
///
/// VFS 侧 (`functions/fs/handle.rs`) 若 `use crate::privileged::proc::...`
/// 会新增 `fs → proc` 的 `use` 依赖, 触发耦合审计; 以
/// `crate::privileged::proc::with_current_fd_table(...)` 内联路径调用可绕开
/// (先例: `crate::functions::fs::flock_release_pid`).
pub fn with_current_fd_table<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&super::process::FdTable) -> R,
{
    let ptr = Scheduler::get_current_process()?;
    // SAFETY: ptr 由 PROCESS_TABLE.get(当前 pid) 返回, 指向已注册且正在运行的
    // 当前进程 PCB; 当前进程持有对自身的引用, 本次调用期间不会被释放。
    let proc = unsafe { &*ptr };
    Some(f(&proc.fd_table))
}

/// 对当前进程的 FS 上下文 (cwd / 根前缀 / umask) 执行闭包操作.
///
/// 返回 `None` 表示当前无进程上下文 (内核早期启动阶段 / host 侧单元测试),
/// 调用方此时回落到全局缺省 (见 `functions::fs::VfsManager`).
///
/// 取指针方式与 [`with_current_fd_table`] 相同: 仅取裸指针后在闭包内使用,
/// 不长持进程表锁.
pub fn with_current_fs_context<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&super::fs_context::FsContext) -> R,
{
    let ptr = Scheduler::get_current_process()?;
    // SAFETY: 同 `with_current_fd_table`: ptr 指向已注册的当前进程 PCB,
    // 当前进程在本次调用期间不会被释放.
    let proc = unsafe { &*ptr };
    Some(f(&proc.fs_context))
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// 按 PID 返回进程表项地址 (0 = 未找到); 命中当前进程时返回缓存指针.
pub extern "C" fn process_get_by_pid(pid: u32) -> u64 {
    if u64::from(pid) == C_CURRENT_PROCESS.map(|p| p.pid) {
        C_CURRENT_PROCESS.as_ptr() as u64
    } else {
        PROCESS_TABLE.get(pid).map_or(0, |p| p as u64)
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 返回当前调度进程的 PWM 值 (无则 0).
#[unsafe(no_mangle)]
pub extern "C" fn process_get_current_pwm() -> u64 {
    let pid = Scheduler::current().unwrap_or(0);
    if pid == 0 {
        return 0;
    }
    PROCESS_TABLE
        .with_process(pid, super::process::Process::get_pwm)
        .unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 按 PID 返回进程的 PWM 值 (无则 0).
#[unsafe(no_mangle)]
pub extern "C" fn process_get_pwm_by_pid(pid: u32) -> u64 {
    if pid == 0 {
        return 0;
    }
    PROCESS_TABLE
        .with_process(pid, super::process::Process::get_pwm)
        .unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 终止当前进程: 释放文件锁与 fd 表, 回收进程表项.
#[unsafe(no_mangle)]
pub extern "C" fn process_exit(exit_code: u32) {
    let current_pid = Scheduler::current().unwrap_or(0);
    if current_pid != 0 {
        // A-7: PI Mutex 退出清理前移到线程 exit —— 先撤销本线程的等待登记
        // (否则持有者退出后无人再向它移交), 再强制释放它仍持有的锁. 收割期的
        // `process_cleanup` 调用保留为兜底 (两边均幂等).
        crate::privileged::sync::pi_mutex::pi_mutex_process_exit(current_pid);

        // 释放该进程持有的所有文件锁 (flock + POSIX lock)
        crate::privileged::fs::current_vfs_ops().release_pid_locks(current_pid);

        // B-9.5: 关闭该进程 fd 表全部 fd — 递减 OpenFile 引用并触发 pcache
        // 失效 / inotify / fd 通知, 避免全局 OPEN_FILE_TABLE 槽位泄漏。
        // K-03: 表可被同组线程共享 (CLONE_FILES); 只有最后一个持有者退出才关闭,
        // 先退出的线程不得截断其余线程仍在使用的句柄.
        let last_fd_user = PROCESS_TABLE
            .with_process(current_pid, |p| p.fd_table.release_user())
            .unwrap_or(false);
        if last_fd_user {
            crate::privileged::fs::current_vfs_ops().close_all_fds();
        }

        // T1-G2: robust futex 遍历 + CLONE_CHILD_CLEARTID 清零.
        // 必须在切换内核页表/销毁用户地址空间之前执行 (用户内存仍可访问).
        crate::privileged::proc::robust::exit_cleanup(current_pid);

        let kernel_cr3 = get_kernel_pml4();
        if kernel_cr3 != 0 {
            // SAFETY: kernel_cr3 是从 vmm::get_kernel_pml4() 获取的合法页表。
            raw::switch_page_table(kernel_cr3);
        }
        // 用户地址空间的释放在 `SCHEDULER.exit` 内置 Zombie 之后执行 (见该处 D4 注)。
        // 此处不可调用: 彼时 state 仍为 Running, `destroy_by_pid` 的
        // is_running() 守卫会空转返回并使页表永不销毁。
    }
    SCHEDULER.exit(exit_code);
}

/// 终止整个线程组: 登记组退出码, 强制其余线程退出, 再让当前线程退出.
///
/// `exit_group(2)` 与致命信号默认动作的统一入口. 兄弟线程收到不可屏蔽的
/// `SIGKILL` 后在返回用户态前走组退出路径; 正在睡眠的线程由 `Scheduler::unblock`
/// 重新入队. 首个 `request_exit` 的退出码是组内全部 TCB 最终 `exit_code` 的权威值.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_exit_group(exit_code: u32) {
    let current_pid = Scheduler::current().unwrap_or(0);
    if current_pid == 0 {
        return;
    }
    // 退出请求在进程表锁内发布: `Scheduler::exit` 与 `yield_current` 读取
    // (`exit_requested` + `requested_exit_code`) 同样持该锁, 成对读取不会看到
    // "标志已置、码未写"的中间态; `request_exit` 内部再取成员表锁, 与并发
    // `clone` 入组互斥 (入组者要么进本快照, 要么被 `join` 拒绝).
    let Some(group) = PROCESS_TABLE.with_process(current_pid, |p| {
        let group = Arc::clone(p.group());
        group.request_exit(exit_code);
        group
    }) else {
        return;
    };

    for tid in group.members() {
        if tid == current_pid {
            continue;
        }
        // 仅置 pending 不直接改状态: 目标若在核上运行, 由其中断/系统调用返回路径
        // 投递; 若在睡眠, 由 `unblock` 按调度策略搬回运行队列.
        let _ = PROCESS_TABLE.with_process(tid, |p| {
            p.signal_pending_set(u32::from(super::signal::SIGKILL));
        });
        Scheduler::unblock(tid);
    }

    process_exit(exit_code);
}

/// 子线程组已整体退出且全部 TCB 落 Zombie 时返回组退出码, 否则 `-1`.
///
/// 返回 `i64` 而非 `Option<u32>`: 本函数是 `extern "C"` 导出点, `Option<u32>`
/// 无 FFI 表示; `-1` 在合法退出码空间 (0..=255) 之外, 不构成歧义.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_group_exit_code(tgid: Pid) -> i64 {
    PROCESS_TABLE
        .group_zombie_exit_code(tgid)
        .map_or(-1, i64::from)
}

/// 收割已整体退出的线程组 (全部僵尸 TCB); 返回是否由本次调用完成回收.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_reap_group(tgid: Pid) -> bool {
    PROCESS_TABLE.reap_zombie_group(tgid)
}

/// 阻塞当前进程 (用于 futex wait / 等待 I/O 等)
///
/// 先经 [`process_block_prepare`] 置 Blocked (不切换), 再由 [`process_block_finish`]
/// 完成切换并在唤醒返回处收口致命信号.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_block(pid: u32) {
    if process_block_prepare(pid, BlockReason::FutexWait, 0) {
        process_block_finish(pid);
    }
}

/// 置当前线程为阻塞态 (原因 + 可选的定时唤醒截止 tick), 不切换 CPU.
///
/// futex 等待必须把"等待者入队"与"置 Blocked"放在同一锁域内原子完成: 若先入队
/// 后阻塞, 落在两步之间的唤醒会被 [`Scheduler::unblock`] 的 Running 判据丢弃
/// (线程仍在核上运行), 等待者随后永久睡在队列外. 调用方因此需在自己的锁域内
/// 调用本函数, 解锁后再经 [`process_block_finish`] 切换.
///
/// `sleep_until` 按 tick 解释 (0 = 不定时); 非零时配合 [`BlockReason::Sleeping`]
/// 由 tick 扫描到期唤醒.
///
/// 返回 `false` 表示 `pid` 不是本核当前线程 —— 调用方应回滚已入队的等待者.
#[must_use]
pub fn process_block_prepare(pid: u32, reason: BlockReason, sleep_until: u64) -> bool {
    if pid == 0 || pid != Scheduler::current().unwrap_or(0) {
        return false;
    }
    if sleep_until != 0 {
        let _ = PROCESS_TABLE.with_process(pid, |p| {
            p.sleep_until.store(sleep_until, Ordering::SeqCst);
        });
    }
    Scheduler::block(reason);
    true
}

/// 在阻塞态已置位后完成调度切换, 并收口组退出 / `de_thread` 的致命信号.
///
/// 唤醒返回处收口致命信号: `process_exit_group` 对睡眠中的兄弟线程只投递一次
/// `SIGKILL` + `unblock`, 该线程醒来后若立刻重睡同一个等待字, 而本架构尚未接线
/// 「返回用户态前信号投递」(aarch64), 它就永远停在队列外的 Blocked 态 —— 整组
/// 收不齐, 等待方 (父进程 `wait4` / `de_thread`) 永久挂死. 判据与
/// [`Scheduler::yield_current`] 的致命信号检查同源: 内核内等待循环都不是信号投递点.
pub fn process_block_finish(pid: u32) {
    if pid == 0 || pid != Scheduler::current().unwrap_or(0) {
        return;
    }
    Scheduler::schedule();

    let kill_requested = PROCESS_TABLE
        .with_process(pid, |p| {
            p.signal_pending_get() & (1u64 << super::signal::SIGKILL) != 0
                && (p.group().exit_requested() || p.group().exec_de_thread())
        })
        .unwrap_or(false);
    if kill_requested {
        process_exit(u32::from(super::signal::SIGKILL) << 8 | 0x7f);
    }
}

/// 解除进程阻塞 (用于 futex wake / I/O 完成等)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_unblock(pid: u32) {
    if pid == 0 {
        return;
    }
    Scheduler::unblock(pid);
}

// ============================================================================
// 进程创建内部实现
// ============================================================================

/// exec 前结束同组其余线程 (`de_thread`), 使调用线程成为组内唯一存活者.
///
/// 流程: 在同一锁域内置组 `exec_de_thread` 标记并取成员快照 (此后 `clone`
/// 入组一律被拒) → 对每个兄弟线程登记 SIGKILL 并唤醒 → 等待全部落 Zombie →
/// 回收非组长 TCB. 组长 TCB 若不是调用线程则保留为僵尸锚点: `tgid` 仍可经
/// 进程表解析 (`getpid` / `kill` / `wait4` 均按 tgid 定位组), 直到调用线程
/// 退出后整组由父进程收割.
///
/// 返回 `false` 表示调用线程已不在进程表中 (调用方回滚临时新进程).
///
/// 标记必须先于 SIGKILL 置位: 兄弟线程的致命信号处理据此走单线程退出
/// (`terminate_from_fatal_signal`), 否则会触发组退出并终止正在 exec 的调用方.
fn exec_de_thread(caller: Pid) -> bool {
    let Some(group) = PROCESS_TABLE.with_process(caller, |p| Arc::clone(p.group())) else {
        return false;
    };
    let tgid = group.tgid();
    // 标记与快照同一锁域完成: 快照之后的新入组会被 `join` 拒绝 (EAGAIN),
    // 不会留下仍运行旧映像的幽灵线程.
    let members = group.begin_exec_de_thread_snapshot();
    if members.len() <= 1 {
        group.end_exec_de_thread();
        return true;
    }

    for &tid in &members {
        if tid == caller {
            continue;
        }
        let _ = PROCESS_TABLE.with_process(tid, |p| {
            p.signal_pending_set(u32::from(super::signal::SIGKILL));
        });
        Scheduler::unblock(tid);
    }

    // 等待兄弟线程自行退出 — TCB 只能由自身经调度器退出 (Scheme B). 轮询期间
    // 每轮重发一次 SIGKILL 唤醒: 目标可能恰在"置 pending 之后、block 之前"完成
    // 入睡, 单次 unblock 会落空 (Running 状态不投送). 内核内等待循环不经过信号
    // 投递点, 由 `Scheduler::yield_current` 的致命信号检查保证最终退出.
    loop {
        let current_members = group.members();
        let mut all_gone = true;
        for &tid in &members {
            if tid == caller {
                continue;
            }
            let still_member = current_members.contains(&tid);
            match PROCESS_TABLE.with_process(tid, Process::get_state) {
                Some(ProcessState::Zombie | ProcessState::Terminated) => {}
                // 已离开成员账且不在表中 = 入组失败/已被回滚; 无可回收对象.
                None if !still_member => {}
                Some(_) => {
                    all_gone = false;
                    // 仍在成员账才需要补发; 已离开者正在退出途中, 等它落 Zombie.
                    if still_member {
                        let _ = PROCESS_TABLE.with_process(tid, |p| {
                            p.signal_pending_set(u32::from(super::signal::SIGKILL));
                        });
                        Scheduler::unblock(tid);
                    }
                }
                // 已入组但尚未插入进程表 (clone 登记途中): 等它落表再终止.
                None => all_gone = false,
            }
        }
        if all_gone {
            break;
        }
        Scheduler::yield_current();
    }

    // 回收非组长兄弟: 线程没有 `UserProcess` 镜像, `Process::drop` 按各自
    // 登记的 cr3 持有计数归还引用并释放内核栈.
    for &tid in &members {
        if tid != caller && tid != tgid {
            PROCESS_TABLE.remove_and_free(tid);
        }
    }

    group.end_exec_de_thread();
    true
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
/// 用给定 ELF 镜像替换当前进程映像 (exec), 成功返回 0, 失败返回 -1.
///
/// `argv_addr` / `envp_addr` 指向调用进程旧地址空间中 `NULL` 结尾的用户指针
/// 数组, 由 functions 层先行校验并计数; 本函数在旧空间销毁前把字符串复制进
/// 新进程栈 (K-07b 批2).
pub extern "C" fn proc_exec_replace(
    elf_addr: u64,
    elf_size: u64,
    argv_addr: u64,
    arg_count: u32,
    envp_addr: u64,
    env_count: u32,
) -> i32 {
    if elf_addr == 0 {
        return -1;
    }
    // 地址值参数在本函数内转回指针, 指针不跨越 ABI 边界 (FFI 安全类型).
    let elf_data = elf_addr as *const u8;

    let current_pid = Scheduler::current().unwrap_or(0);
    if current_pid == 0 {
        return -1;
    }

    let pwm = super::api::scheduler_get_current_pwm();

    // P0-I-31 修复: transactional execve — 先在临时进程中加载并验证新 ELF,
    // 验证通过后将新地址空间转移到当前 PID, 保持 POSIX execve 语义 (PID 不变).
    // 阶段 4b: ELF 文件 I/O 下沉 functions, privileged 仅收字节并加载 (纯机制).
    let new_pid = super::api::user_proc_load_elf_from_memory(elf_data, elf_size, pwm);
    if new_pid < 0 {
        return -1;
    }

    let new_pid_u32 = new_pid.cast_unsigned();

    // 阶段 2: 读取新进程的地址空间信息 (CR3/entry/stack)
    let new_addr_space = USER_PROC_MANAGER
        .with_process(new_pid_u32, |proc| {
            let state = proc.process().state.load(Ordering::SeqCst);
            if state == 0 {
                return None;
            }
            Some((
                proc.process().cr3.load(Ordering::SeqCst),
                proc.entry,
                proc.process().user_stack.load(Ordering::SeqCst),
                proc.stack_bottom.load(Ordering::SeqCst),
            ))
        })
        .flatten();

    let Some((new_cr3, new_entry, mut new_user_stack, new_stack_bottom)) = new_addr_space else {
        USER_PROC_MANAGER.discard_unrun(new_pid_u32);
        return -1;
    };

    // 阶段 2c: argv/envp 必须在旧地址空间销毁前写入新栈 —— 参数字符串位于
    // 调用进程的旧用户内存, 换表/销毁后再读即 UAF (旧实现把该步放在
    // `replace_user_space` 之后). 写侧按 new_cr3 寻址, 与旧空间无耦合;
    // 失败保持镜像原栈指针, 与原"忽略失败"语义一致.
    if (argv_addr != 0 && arg_count > 0) || (envp_addr != 0 && env_count > 0) {
        // SAFETY: argv_addr/arg_count 与 envp_addr/env_count 由 C ABI 调用方保证
        // 有效, 且此刻旧地址空间仍存活; new_cr3 是刚加载完成的新用户页表.
        let sp = unsafe {
            UserProcManager::setup_user_stack_in_table(
                new_cr3,
                new_user_stack,
                argv_addr as *const *const u8,
                arg_count as usize,
                envp_addr as *const *const u8,
                env_count as usize,
            )
        };
        if sp != 0 {
            new_user_stack = sp;
        }
    }

    // 阶段 2b (K-06): 结束同组其余线程 — exec 后地址空间/身份归调用线程独有.
    // 必须在换页表前完成: 兄弟线程仍在旧地址空间执行, 换表后它们会以旧上下文
    // 落入新映像. 失败 (调用线程已不在表中) 时临时进程照常回滚.
    if !exec_de_thread(current_pid) {
        USER_PROC_MANAGER.discard_unrun(new_pid_u32);
        return -1;
    }

    // 阶段 3: 切换到内核页表, 替换当前进程的用户地址空间
    let kernel_cr3 = get_kernel_pml4();
    if kernel_cr3 != 0 {
        // SAFETY: kernel_cr3 是从 vmm::get_kernel_pml4() 获取的合法页表。
        raw::switch_page_table(kernel_cr3);
    }

    // K-06: 调用线程可能没有镜像 (CLONE_THREAD 线程不建镜像); `replace_user_space`
    // 会先补建镜像再替换, 补建失败时保持原映像 (临时新进程回滚).
    if !USER_PROC_MANAGER.replace_user_space(
        current_pid,
        new_cr3,
        new_entry,
        new_user_stack,
        new_stack_bottom,
    ) {
        USER_PROC_MANAGER.discard_unrun(new_pid_u32);
        return -1;
    }

    // 阶段 4: 移除临时新进程
    // G3 修复: 新地址空间已转移给当前进程 (所有权随 cr3 移动, 不重新计数),
    // 临时进程必须先清空其 cr3, 否则其 Process::drop 会对同一 PML4 计数减到
    // 归零并销毁当前进程正在使用的页表 (UAF).
    // 临时进程从未运行, 与装配失败回滚同走 `discard_unrun` 销毁单源:
    // 写 Zombie → 镜像摘除并归还分配 → PCB 释放 → PID 回收.
    PROCESS_TABLE.with_process(new_pid_u32, |p| p.cr3.store(0, Ordering::SeqCst));
    USER_PROC_MANAGER.discard_unrun(new_pid_u32);

    // 5a. I-48: 重置信号状态 (execve 后信号处理 = 默认)
    //
    // K-03/K-06: 先换新 Arc 再清零 —— exec 后动作表必须是本进程独有实例.
    // de_thread 已结束其余线程, 清零不会波及任何存活共享者.
    PROCESS_TABLE.with_process_mut(current_pid, |p| {
        p.sigaction_table =
            alloc::sync::Arc::new(crate::privileged::sync::IrqSpinLock::new([0u64; 64]));
    });
    crate::privileged::proc::reset_signal_state_on_exec(current_pid);

    // 5a2. K-06: FS 上下文 unshare — exec 后 cwd / 根前缀 / umask 归本进程独有.
    // de_thread 已保证组内无其他线程, 但 CLONE_FS 的独立进程共享仍可能存在于
    // 别组; 复制到新 Arc 后, 后续 chdir/umask 不再影响共享者 (POSIX exec 语义).
    let old_fs = PROCESS_TABLE.with_process(current_pid, |p| Arc::clone(&p.fs_context));
    if let Some(old_fs) = old_fs {
        let new_fs = Arc::new(super::fs_context::FsContext::new_default());
        new_fs.copy_from(&old_fs);
        PROCESS_TABLE.with_process_mut(current_pid, |p| p.fs_context = new_fs);
    }

    // 5b. POSIX close-on-exec: 关闭所有标记 CLOEXEC 的 fd
    crate::privileged::fs::current_vfs_ops().close_cloexec_fds();

    // 5c. execve 后旧映像的 TLS 基址不保留: 新映像从 0 开始,
    // 硬件寄存器同步清除 (随后经 `user_proc_enter_by_pid` 的入口参数再次装载).
    let _ = PROCESS_TABLE.with_process(current_pid, |p| {
        p.tls_base.store(0, Ordering::Release);
    });
    crate::arch!(set_user_tls_base(0));

    // 6. 同步当前进程信息
    C_CURRENT_PROCESS.map_mut(|p| {
        p.pid = u64::from(current_pid);
    });

    // 7. 进入当前 PID (使用新地址空间)
    super::api::user_proc_enter_by_pid(current_pid);
    0
}

/// 保存当前 syscall 的用户寄存器到进程 `p.context` (B05-55 根治).
///
/// # 背景
/// fork/clone 复制 `p.context`, 但该字段仅由 `process_switch_asm` (调度器切换)
/// 写入. init 等首个用户进程经 `enter_user_asm` 直接进入用户态, **从未被调度器
/// 保存过 context**, 故 `p.context` 全零 → 子进程 iretq 用全零帧 → 未进用户态.
///
/// # 根治
/// 在 syscall 入口 (`syscall_dispatch_from_frame`) 每次调用本函数, 把当前
/// `InterruptFrame` (用户寄存器真实状态) 映射到 `p.context`. 这样 fork/clone
/// 复制的就是最新用户寄存器 (rip=返回地址, rsp=用户栈, cs/ss/rflags 等),
/// 子进程从正确的用户返回点继续.
///
/// `ProcessContext` 本不含 rdi/rsi/rdx/rcx/r8-r11 (调度切换不保存这些), 但 B05-55
/// 在 `extra_regs[8]` 中保存它们: 已运行过的进程返回用户态靠 syscall/中断栈的
/// `InterruptFrame` 恢复, 首次被调度的子进程 (fork/clone) 由 `process_switch_asm`
/// iretq 前从 `extra_regs` 恢复 (继承 fork 时父进程的 rdi 等). rax 由 fork/clone
/// 在子 context 上改写为 0.
pub fn proc_save_user_regs(pid: Pid, f: &crate::privileged::idt::InterruptFrame) {
    if pid == 0 {
        return;
    }
    // InterruptFrame 字段均为 Copy 值类型, 闭包内直接引用 `f` 读取即可
    PROCESS_TABLE.with_process(pid, |p| {
        let mut ctx = p.context.lock();
        ctx.r15 = f.r15;
        ctx.r14 = f.r14;
        ctx.r13 = f.r13;
        ctx.r12 = f.r12;
        ctx.rbx = f.rbx;
        ctx.rbp = f.rbp;
        ctx.rip = f.rip;
        ctx.rsp = f.rsp;
        ctx.rflags = f.rflags;
        ctx.cr3 = p.cr3.load(Ordering::SeqCst);
        ctx.cs = f.cs;
        ctx.ss = f.ss;
        ctx.ds = 0x1B;
        ctx.es = 0x1B;
        ctx.fs = 0x1B;
        ctx.gs = 0x1B;
        // B05-55 修复: 保存 caller-saved 寄存器 (调度切换不保存, 供首次被调度
        // 的子进程继承 fork 时的 rdi 等, 见 ProcessContext::extra_regs 文档).
        ctx.extra_regs[0] = f.rdi;
        ctx.extra_regs[1] = f.rsi;
        ctx.extra_regs[2] = f.rdx;
        ctx.extra_regs[3] = f.rcx;
        ctx.extra_regs[4] = f.r8;
        ctx.extra_regs[5] = f.r9;
        ctx.extra_regs[6] = f.r10;
        ctx.extra_regs[7] = f.r11;
    });
}

/// 保存 aarch64 syscall 入口的用户寄存器到进程 `p.context` (KPTI-17 D1).
///
/// # 背景
/// x86_64 侧在 `syscall_dispatch_from_frame` 内调 `proc_save_user_regs` 捕获
/// 用户寄存器 (B05-55); aarch64 侧 `svc_handler` 直接调架构中立的
/// `syscall_dispatch` (无 frame) ⇒ `p.context` 恒全 0 ⇒ fork/clone 复制出零
/// 上下文的子进程. 本函数在 aarch64 SVC 入口补齐同一职责.
///
/// # 字段语义 (aarch64, 见 `arch/aarch64/context.rs` 头注释)
/// - `@0..72`   ← x19..x28      - `@80` ← x29 (FP)      - `@88` ← x30 (LR)
/// - `@112` ← `SPSR_EL1` (EL0 状态)  - `@120` ← `ELR_EL1` (用户返回 PC)
/// - `@128` ← `SP_EL0` (用户栈指针)  - `@136` ← 用户页表 (EL0 的 TTBR0)
/// - `extra_regs[@672..728]` ← x0..x7  - `extra_regs_x8[@736..824]` ← x8..x18
///
/// **不写** `@96` (`SP_EL1`) 与 `@104` (EL1 侧 TTBR0): 二者由调度切换保存侧维护,
/// 未调度过的新任务由创建方 (fork / init) 预置.
#[cfg(target_arch = "aarch64")]
pub fn proc_save_user_regs_aarch64(
    pid: Pid,
    f: &crate::privileged::arch::aarch64::exception::ExceptionFrame,
) {
    if pid == 0 {
        return;
    }
    PROCESS_TABLE.with_process(pid, |p| {
        let mut ctx = p.context.lock();
        ctx.r15 = f.x19;
        ctx.r14 = f.x20;
        ctx.r13 = f.x21;
        ctx.r12 = f.x22;
        ctx.rbx = f.x23;
        ctx.rbp = f.x24;
        ctx.rax = f.x25;
        ctx.rip = f.x26;
        ctx.rsp = f.x27;
        ctx.rflags = f.x28;
        ctx.cr3 = f.x29;
        ctx.cs = f.x30;
        ctx.fs = f.spsr;
        ctx.gs = f.elr;
        ctx.ss = f.sp;
        // 用户页表 (本进程页表的权威来源); @136 复用 fpu_pad 槽位.
        ctx.fpu_pad = p.cr3.load(Ordering::SeqCst);
        ctx.extra_regs[0] = f.x0;
        ctx.extra_regs[1] = f.x1;
        ctx.extra_regs[2] = f.x2;
        ctx.extra_regs[3] = f.x3;
        ctx.extra_regs[4] = f.x4;
        ctx.extra_regs[5] = f.x5;
        ctx.extra_regs[6] = f.x6;
        ctx.extra_regs[7] = f.x7;
        // 用户库 syscall 汇编只声明 x0 为输出 ⇒ x8-x18 是编译器可跨 `svc`
        // 保留的活跃值, 必须全量快照供 fork/clone 子进程首次进入 EL0 时恢复.
        ctx.extra_regs_x8 = [
            f.x8, f.x9, f.x10, f.x11, f.x12, f.x13, f.x14, f.x15, f.x16, f.x17, f.x18,
        ];
    });
}

/// fork 系统调用实现 (COW 页表克隆 + namespace 继承)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn sys_fork() -> Pid {
    let parent_pid = Scheduler::current().unwrap_or(0);
    if parent_pid == 0 {
        return 0;
    }
    let parent_cr3 = PROCESS_TABLE
        .with_process(parent_pid, |p| p.cr3.load(Ordering::SeqCst))
        .unwrap_or(0);
    if parent_cr3 == 0 {
        return 0;
    }
    let child_pid = proc_alloc_pid();
    if child_pid == 0 {
        return 0;
    }
    // K-04: 父进程关系是线程组属性; 子进程组挂在父线程组 (tgid) 名下.
    let parent_tgid = PROCESS_TABLE
        .with_process(parent_pid, Process::tgid)
        .unwrap_or(parent_pid);
    let child_ptr = raw::alloc_process(child_pid, "", Some(ProcessId(parent_tgid)));
    let child = raw::process_ref_mut(child_ptr);

    // COW 页表克隆: 父子共享物理页, 写入时触发 page fault 复制
    // KPTI 修复: page fault handler 现在使用 get_user_pml4() 获取正确的用户页表
    // G2 修复: 克隆失败必须回滚 (原 `.unwrap_or(parent_cr3)` 在 OOM 时静默共享父页表,
    // 造成父子双所有权且 COW 语义静默降级为共享写)
    let Some(child_cr3) = crate::privileged::mm::cow::clone_user_page_table_cow(parent_cr3) else {
        // 此刻子进程尚无内核栈 (`allocate_kernel_stack` 在下方) 且未插入进程表,
        // 直接释放其描述符 + 归还已分配的 PID (否则失败 fork 会泄漏 PID 位图位)
        raw::drop_boxed_process(child_ptr);
        PROCESS_TABLE.free_pid(child_pid);
        return 0;
    };
    child.cr3.store(child_cr3, Ordering::SeqCst);
    child.pwm.store(0, Ordering::SeqCst);
    // 继承父进程可继承状态 (rlimit / session / seccomp / 域标志 / cgroup / namespace)
    inherit_parent_state(child, parent_pid);
    // 内核栈 (用户 TCB 统一 USER_KSTACK_SIZE; 内核线程 idle 才用 KERNEL_STACK_SIZE)
    if !child.allocate_user_kernel_stack() {
        raw::drop_boxed_process(child_ptr);
        PROCESS_TABLE.free_pid(child_pid);
        return 0;
    }
    if let Some(parent_kstack) =
        PROCESS_TABLE.with_process(parent_pid, |p| p.kernel_stack.load(Ordering::SeqCst))
        && parent_kstack != 0
    {
        // 拷贝父 kstack 内容到子 kstack: 源/目标都从"顶部向下 size"开始,
        // 而非从顶部向上 (原实现拷 64KB 到 kstack 顶上方, 越界损坏内存).
        // size 取父 kstack 实际大小 (USER_KSTACK_SIZE), 使子进程内核栈初始状态与父一致.
        let kstack_size = crate::privileged::glue::usize_from_u64(super::types::USER_KSTACK_SIZE);
        raw::copy_kstack(
            child.kernel_stack.load(Ordering::SeqCst) - kstack_size as u64,
            parent_kstack - kstack_size as u64,
            kstack_size,
        );
        crate::privileged::proc::kernel_stack_write_canary(
            child.kernel_stack.load(Ordering::SeqCst),
        );
    }
    // KPTI 方案 S3: COW 克隆只搬用户半区 (保留槽 `[1:0] = 00` 被过滤), 子进程页表
    // 不含 EL1 视图 ⇒ 须补建, 否则子进程的 EL0→EL1 入口只能回退到全局内核表,
    // 内核态将无法访问其用户页 (copy_from_user 触发同 EL 数据异常). fail-closed:
    // 视图建不起来则子进程不可投运, 回滚 (cr3 由 `Process::drop` 按帧持有计数销毁).
    #[cfg(target_arch = "aarch64")]
    let Some(child_el1_view) = crate::privileged::mm::vmm_build_el1_view(child_cr3) else {
        raw::drop_boxed_process(child_ptr);
        PROCESS_TABLE.free_pid(child_pid);
        return 0;
    };
    // 上下文初始化 (分架构: x86_64 的 cr3/rax/rsp 与 aarch64 的 x29/x25/x27 复用
    // 同一偏移, 见 `arch/aarch64/context.rs` 头注释, 故必须按架构分别写).
    if let Some(ctx) = PROCESS_TABLE.with_process(parent_pid, |p| *p.context.lock()) {
        let mut child_ctx = child.context.lock();
        *child_ctx = ctx;
        #[cfg(target_arch = "x86_64")]
        {
            child_ctx.cr3 = child_cr3;
            child_ctx.rax = 0;
        }
        #[cfg(target_arch = "aarch64")]
        {
            // KPTI-17: 子进程首次被调度时必须能正确进入 EL0 (恢复侧按 SPSR.M == 0
            // 走 EL0 路径), 故预置:
            //   `fpu_pad`(@136) = 子进程用户页表 (EL0 的 TTBR0)
            //   `es`(@104)       = 子进程 EL1 视图根 (EL1 侧 TTBR0)
            //   `ds`(@96)        = 子内核栈顶 (SP_EL1: 首次陷入 EL1 时压帧的落点;
            //                      沿用父进程的值会写进父进程的栈页)
            //   `extra_regs[0]`  = x0 = 0 (fork 返回值; 恢复侧 EL0 路径恢复 x0-x7)
            child_ctx.fpu_pad = child_cr3;
            child_ctx.es = child_el1_view;
            child_ctx.ds = child.kernel_stack.load(Ordering::SeqCst) & !0xF;
            child_ctx.extra_regs[0] = 0;
            // `fs`(@112) = 父进程 SVC 入口快照的 EL0 状态 (0x340, 由
            // `proc_save_user_regs_aarch64` 写入) ⇒ 子进程走 EL0 恢复路径;
            // `gs`(@120) / `ss`(@128) = 用户返回 PC / 用户栈指针, 随 ctx 复制而来.
        }
    }
    // B-9.5: fork 继承父进程 fd 表 — 子进程复制父 fd 表条目, 并对每个复制出的
    // handle_id 递增 OpenFile 引用 (POSIX: fork 后父子共享同一 open file
    // description, offset/flags 共享; 父进程 fd 表不受影响, 各自退出时递减)。
    // 非 CLONE_FILES 语义, 仅复制句柄映射 (K-03 待办)。
    inherit_parent_fds(child, parent_pid);
    if !PROCESS_TABLE.insert(core::ptr::from_ref::<Process>(child).cast_mut()) {
        // 登记失败: 撤销 fd 继承取得的 OpenFile 引用, 再释放半成品 PCB 与 PID.
        release_inherited_fd_refs(child);
        raw::drop_boxed_process(child_ptr);
        PROCESS_TABLE.free_pid(child_pid);
        return 0;
    }
    // 为子进程创建 UserProc 镜像记录, 使 fork 子进程与父进程同源注册.
    //
    // 该注册是调度切换路径装配 per-CPU 用户 CR3 与"任务内核栈顶页"的前置条件:
    // `Scheduler::schedule` 的装配块以 `USER_PROC_MANAGER.get(next)` 为门控,
    // 未注册的子进程被投送到次核时 per-CPU user CR3 滞留旧值、其内核栈顶页也不
    // 在其用户页表内 ⇒ 用户态被硬件中断/异常打断时按 TSS.RSP0 压帧即 #PF
    // (cr2=内核栈顶-8). 这正是 APS-05 x86_64 偶发未成对观测的根因 —— 该注册块
    // 在 4557cd30 重写 `sys_fork` (COW 崩溃临时改用共享页表) 时被附带删除.
    // K-06: 父线程可能没有自己的镜像 (CLONE_THREAD 线程), `user_proc_clone`
    // 回落到同组组长的镜像; 仍取不到源时 fail-closed (无镜像的子进程无法投运).
    if user_proc_clone(parent_pid, child_pid) < 0 {
        // PCB 即将被回收, 先撤销 fd 继承取得的 OpenFile 引用, 避免全局句柄泄漏.
        release_inherited_fd_refs(child);
        PROCESS_TABLE.remove_and_free(child_pid);
        return 0;
    }
    PROCESS_TABLE.with_process(parent_pid, |p| {
        p.group().add_child(child_pid);
    });
    let _ = child.set_state_safe(ProcessState::Ready);
    Scheduler::add_to_run_queue(child_pid);
    child_pid
}

/// fork: 子进程继承父进程的 rlimit / session / pgid / canary / seccomp /
/// 域标志 / namespace / cgroup / 调度权重链路 (`nice` / `cfs_weight` /
/// `ProcessPriority`).
///
/// `with_process` 返回 `None` 表示父进程已不在进程表, 此时静默跳过对应继承.
fn inherit_parent_state(child: &mut Process, parent_pid: Pid) {
    // rlimit
    if let Some(rlim) = PROCESS_TABLE.with_process(parent_pid, |p| p.rlimit_table.lock().clone()) {
        *child.rlimit_table.lock() = rlim;
    }
    // session/pgid/canary: 会话与进程组是线程组属性 (K-04)
    if let Some((sid, pgid, canary)) = PROCESS_TABLE.with_process(parent_pid, |p| {
        (
            p.group().session_id(),
            p.group().pgid(),
            p.stack_canary.load(Ordering::SeqCst),
        )
    }) {
        child.group().set_session_id(sid);
        child.group().set_pgid(pgid);
        child.stack_canary.store(canary, Ordering::SeqCst);
    }
    // seccomp
    if let Some((mode, nnp, filters)) = PROCESS_TABLE.with_process(parent_pid, |p| {
        (
            p.seccomp.get_mode(),
            p.seccomp.is_no_new_privs(),
            p.seccomp.filters.lock().clone(),
        )
    }) {
        child.seccomp.mode.store(mode as u8, Ordering::SeqCst);
        child
            .seccomp
            .no_new_privs
            .store(u8::from(nnp), Ordering::SeqCst);
        *child.seccomp.filters.lock() = filters;
    }
    // domain flags: 与 seccomp 同口径, 子进程继承父进程域级行为门控标志
    if let Some(df) =
        PROCESS_TABLE.with_process(parent_pid, |p| p.domain_flags.load(Ordering::SeqCst))
    {
        child.domain_flags.store(df, Ordering::SeqCst);
    }
    // namespace: 从父进程继承 (fork_from 仅 7 个 Arc::clone, 无锁交互)
    // 先提取 NamespaceSet 到局部变量, 再赋值给 child, 避免在 with_process 闭包内操作 child
    if let Some(parent_ns) = PROCESS_TABLE.with_process(parent_pid, |p| {
        super::NamespaceSet::fork_from(&p.namespaces.lock())
    }) {
        *child.namespaces.lock() = parent_ns;
    }
    // cgroup
    if let Some(cg) =
        PROCESS_TABLE.with_process(parent_pid, |p| p.cgroup_id.load(Ordering::Acquire))
    {
        child.cgroup_id.store(cg, Ordering::Release);
    }
    // TLS 基址: fork 子进程继承父进程的用户态 TLS 指针 (POSIX fork 语义)
    if let Some(tls) =
        PROCESS_TABLE.with_process(parent_pid, |p| p.tls_base.load(Ordering::Acquire))
    {
        child.tls_base.store(tls, Ordering::Release);
    }
    // 地址空间描述符: fork 子进程按指针继承父进程的 `MmStruct` (VMA 描述符
    // 在 fork 中不复制, 与页表 COW 克隆分层; 见 `MmStruct::locked_vm` 注).
    if let Some(mm) = PROCESS_TABLE.with_process(parent_pid, |p| p.mm.load(Ordering::Acquire)) {
        child.mm.store(mm, Ordering::Release);
    }
    // K-03: 信号动作表与 FS 上下文按 fork 语义深拷贝 —— fork 出的子进程此后
    // 与父进程各自独立 (CLONE_SIGHAND / CLONE_FS 的共享只在 clone 线程路径).
    let _ = PROCESS_TABLE.with_process(parent_pid, |p| {
        child.copy_signal_actions_from(&p.sigaction_table);
        child.copy_fs_context_from(&p.fs_context);
    });
    // 调度权重链路: POSIX fork 继承父进程 nice / cfs_weight / ProcessPriority.
    // nice 与其派生权重成对复制, 避免子进程以默认权重起算拉低父进程份额.
    // 每轮 EEVDF 账目 (vruntime / vd / 切片余额) 与请求切片不继承 —— 子进程
    // 由入队建账重新起算, 请求切片属调用任务自身的单次请求.
    if let Some((priority, nice, weight)) = PROCESS_TABLE.with_process(parent_pid, |p| {
        (
            p.priority.load(Ordering::Acquire),
            p.nice.load(Ordering::Acquire),
            p.cfs_weight.load(Ordering::Acquire),
        )
    }) {
        child.priority.store(priority, Ordering::Release);
        child.nice.store(nice, Ordering::Release);
        child.cfs_weight.store(weight, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileged::glue::{low_i8_from_u32, u32_from_i8};
    use crate::privileged::proc::cfs::nice_to_weight;
    use crate::privileged::proc::types::ProcessPriority;

    /// 临时进程装卸: 分配 PID + 构造进程 + 插入进程表.
    fn install(name: &str) -> (Pid, *mut Process) {
        let pid = PROCESS_TABLE.allocate_pid().expect("测试进程: 无空闲 pid");
        let ptr = raw::alloc_process(pid, name, None);
        assert!(PROCESS_TABLE.insert(ptr), "测试进程: 插入进程表失败");
        (pid, ptr)
    }

    /// 拆除临时进程 (先从进程表摘除, 再释放堆内存).
    fn uninstall(pid: Pid, ptr: *mut Process) {
        let _ = PROCESS_TABLE.remove(pid);
        raw::drop_boxed_process(ptr);
        PROCESS_TABLE.free_pid(pid);
    }

    /// fork 继承语义: nice / cfs_weight / ProcessPriority 逐字段复制,
    /// 每轮 EEVDF 账目不得随之复制.
    #[test]
    fn test_inherit_parent_state_copies_sched_weight_chain() {
        let (parent_pid, parent_ptr) = install("inherit-sched-parent");
        let (child_pid, child_ptr) = install("inherit-sched-child");

        let nice = 10i8;
        let weight = nice_to_weight(nice);
        // SAFETY: parent_ptr 由 raw::alloc_process 分配且尚未释放, 测试内独占该
        // 进程槽位, 无其它引用同时写入.
        let parent = raw::process_ref_mut(parent_ptr);
        parent.nice.store(u32_from_i8(nice), Ordering::Release);
        parent.cfs_weight.store(weight, Ordering::Release);
        parent
            .priority
            .store(ProcessPriority::High as u32, Ordering::Release);
        parent.cfs_vruntime.store(0x1234, Ordering::Release);
        parent.eevdf_vdeadline.store(0x5678, Ordering::Release);

        // SAFETY: child_ptr 由 raw::alloc_process 分配且尚未释放, 测试内独占该
        // 进程槽位; inherit_parent_state 只读取 PROCESS_TABLE 中的父进程.
        let child = raw::process_ref_mut(child_ptr);
        inherit_parent_state(child, parent_pid);

        let inherited_nice = low_i8_from_u32(child.nice.load(Ordering::Acquire));
        assert_eq!(inherited_nice, nice);
        assert_eq!(child.cfs_weight.load(Ordering::Acquire), weight);
        assert_eq!(
            child.cfs_weight.load(Ordering::Acquire),
            nice_to_weight(inherited_nice)
        );
        assert_eq!(
            child.priority.load(Ordering::Acquire),
            ProcessPriority::High as u32
        );
        // 每轮账目与请求切片从零起算, 不与父进程共享.
        assert_eq!(child.cfs_vruntime.load(Ordering::Acquire), 0);
        assert_eq!(child.eevdf_vdeadline.load(Ordering::Acquire), 0);
        assert_eq!(child.eevdf_requested_slice.load(Ordering::Acquire), 0);

        uninstall(child_pid, child_ptr);
        uninstall(parent_pid, parent_ptr);
    }
}

/// fork: 子进程复制父进程 fd 表, 并对复制出的每个句柄递增 `OpenFile` 引用.
///
/// 递增在 `FdTable::copy_from` 的源表锁内完成, 与并发 close 互斥.
fn inherit_parent_fds(child: &mut Process, parent_pid: Pid) {
    let ops = crate::privileged::fs::current_vfs_ops();
    let _ = PROCESS_TABLE.with_process(parent_pid, |p| {
        child.copy_fd_table_from(&p.fd_table, |handle| ops.inc_open_file_ref(handle));
    });
}

/// 撤销 [`inherit_parent_fds`] 取得的 `OpenFile` 引用 (fork 失败回滚).
///
/// 子进程从未投运, 其 fd 表条目未暴露给用户, 故只递减引用计数, 不触发
/// pcache 失效 / inotify / fd 通知等关闭副作用.
fn release_inherited_fd_refs(child: &Process) {
    let ops = crate::privileged::fs::current_vfs_ops();
    for (_, handle) in child.fd_table.get_all_fds() {
        ops.dec_open_file_ref(handle);
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 返回指定进程的父进程 PID (无父进程则 0).
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_ppid(pid: Pid) -> Pid {
    PROCESS_TABLE
        .with_process(pid, |p| p.group().parent())
        .unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
/// 设置指定进程的 PWM 值, 成功返回 0, 进程不存在返回 -1.
#[unsafe(no_mangle)]
pub extern "C" fn proc_set_pwm(pid: Pid, pwm: u64) -> i32 {
    if PROCESS_TABLE
        .with_process(pid, |p| p.pwm.store(pwm, Ordering::SeqCst))
        .is_some()
    {
        0
    } else {
        -1
    }
}

// ============================================================================
// times / alarm / setitimer / getitimer — POSIX 进程时间与定时器 API
// ============================================================================

/// 累加当前进程的 user/sys 时间
// 由 tick_accounting / scheduler 内部以 Rust 路径调用, TD-10 契约测试按 Rust ABI
// 签名匹配, 无 C 符号导出需求, 故不标 `no_mangle` (P-64 第十五批).
pub fn proc_account_tick(in_kern: u32) {
    let pid = CURRENT_PROCESS_PTR.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    PROCESS_TABLE.with_process(crate::privileged::glue::low_u32_from_u64(pid), |p| {
        p.tick_count.fetch_add(1, Ordering::SeqCst);
        if in_kern != 0 {
            p.sys_time.fetch_add(1, Ordering::SeqCst);
        } else {
            p.user_time.fetch_add(1, Ordering::SeqCst);
        }
    });
}

/// 取得进程的 user/sys 时间 (jiffies 累积).
// TD-10 契约测试按 Rust ABI 签名匹配该函数, 无 C 符号导出需求,
// 故不标 `no_mangle` (P-64 第十五批).
pub fn proc_get_times(pid: u32, out_user: &mut u64, out_sys: &mut u64) -> i32 {
    let res = PROCESS_TABLE.with_process(pid as Pid, |p| {
        (
            p.user_time.load(Ordering::SeqCst),
            p.sys_time.load(Ordering::SeqCst),
        )
    });
    match res {
        Some((u, s)) => {
            *out_user = u;
            *out_sys = s;
            0
        }
        None => -1,
    }
}

/// 获取用户进程创建时间戳 (ticks).
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_create_time(pid: u32) -> u64 {
    USER_PROC_MANAGER
        .with_process(pid, |p| p.create_time)
        .unwrap_or(0)
}

/// POSIX getrusage(who, rusage) — 写回进程/子进程 user/sys 时间.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_rusage(pid: u32, who: i32, out: *mut u8, out_len: u64) -> i32 {
    if out.is_null() || out_len < 32 {
        return -1;
    }
    if who != 0 && who != 1 && who != 2 {
        return -1;
    }
    let hz = u64::from(crate::privileged::timer::get_frequency());
    if hz == 0 {
        return -1;
    }
    let (user, sys) = if who == 0 {
        PROCESS_TABLE
            .with_process(pid as Pid, |p| {
                (
                    p.user_time.load(Ordering::SeqCst),
                    p.sys_time.load(Ordering::SeqCst),
                )
            })
            .unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    let ut_sec = (user / hz).cast_signed();
    let ut_micros = ((user % hz) * 1_000_000 / hz).cast_signed();
    let st_sec = (sys / hz).cast_signed();
    let st_micros = ((sys % hz) * 1_000_000 / hz).cast_signed();

    // 经 UserWritePtr 把用户裸指针收敛为切片, 写入全部走安全索引 (I4 安全代理).
    let Some(mut user_buf) = crate::privileged::userptr::UserWritePtr::checked_new(
        out,
        crate::privileged::glue::usize_from_u64(out_len),
    ) else {
        return -1;
    };
    let buf = user_buf.as_mut_slice();
    buf[0..8].copy_from_slice(&ut_sec.to_ne_bytes());
    buf[8..16].copy_from_slice(&ut_micros.to_ne_bytes());
    buf[16..24].copy_from_slice(&st_sec.to_ne_bytes());
    buf[24..32].copy_from_slice(&st_micros.to_ne_bytes());
    // 其余 rusage 字段按 0 填充 (out_len >= 32 已在上方校验).
    buf[32..].fill(0);
    0
}
