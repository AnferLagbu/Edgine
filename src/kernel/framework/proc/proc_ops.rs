//! 进程操作子模块 — 进程创建/销毁/查询/操作
//!
//! 从 `api.rs` 拆分而来, 包含所有 `process_*` 函数和进程管理相关操作。
//!
//! ## 依赖
//! - `api::raw` — 裸指针/FFI 桥接
//! - `api::{C_CURRENT_PROCESS, CURRENT_PROCESS_PTR, INIT_PROCESS_CREATED}` — 当前进程状态
//! - `process::PROCESS_TABLE` — 进程表
//! - `scheduler::SCHEDULER` — 调度器
//! - `user_proc::USER_PROC_MANAGER` — 用户进程管理

use core::sync::atomic::Ordering;

use super::process::{PROCESS_TABLE, Process};
use super::scheduler::SCHEDULER;
use super::types::{BlockReason, Pid, ProcessId, ProcessState};
use super::user_proc::USER_PROC_MANAGER;
pub use super::user_proc::proc_alloc_pid;
use crate::framework::lib::CStrExt;
use crate::framework::mm::{
    get_kernel_pml4, vmm_clone_user_page_table_cow, vmm_destroy_page_table, vmm_switch_page_table,
};
use crate::framework::racy_cell::RacyCell;
use crate::framework::timer::timer_get_ticks;

// === 特权层: 进程子系统裸指针/FFI 桥接集中地 ===
//
// 本子模块包含所有与 C ABI、裸指针 (进程表 entry) 以及 extern "C" FFI 交互
// 的 `unsafe` 代码。本模块的其余部分 (`api.rs` 顶层) 保持 100% 安全 Rust,
// 通过 `raw::*` 安全函数访问底层功能。
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
        // SAFETY: 调用方在 fork/exec 路径中保证 ptr 来自 PROCESS_TABLE
        unsafe { &*ptr }
    }

    /// 从裸指针读取 `&mut Process`。
    ///
    /// # Safety (内部)
    /// - `ptr` 必须为非空, 指向有效 `Process` 实例
    /// - 同一时刻只能存在一个可变引用
    pub fn process_ref_mut<'a>(ptr: *mut Process) -> &'a mut Process {
        // SAFETY: 调用方在 fork 路径中保证 `&mut` 唯一性
        unsafe { &mut *ptr }
    }

    #[expect(
        clippy::ptr_as_ptr,
        reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
    )]
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
    )]
    /// 分配并构造一个 `Process` (用于 fork 创建子进程)。
    ///
    /// # Safety (内部)
    /// - 调用方在 fork 错误路径中负责 `dealloc_process` 释放。
    pub fn alloc_process(pid: u32, name: &str, parent: Option<ProcessId>) -> *mut Process {
        // SAFETY: alloc/dealloc 配对, 见 dealloc_process。
        unsafe {
            let layout = alloc::alloc::Layout::new::<Process>();
            let ptr = alloc::alloc::alloc(layout) as *mut Process;
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
            // SAFETY: Box 所有权还原。
            unsafe { drop(alloc::boxed::Box::from_raw(ptr)) }
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
    pub fn clone_user_page_table_cow(parent_cr3: u64) -> u64 {
        vmm_clone_user_page_table_cow(parent_cr3)
    }

    /// 通过 FFI 输出 info 级别日志。
    ///
    /// # Safety (内部, 由本模块封闭)
    /// - `msg` 必须为以 `\0` 结尾的有效 C 字符串。
    pub fn klog_info(msg: &[u8]) {
        // SAFETY: msg 来自上层调用, 上层中只传入静态字节串字面量。
        unsafe { crate::framework::klog::klog_ffi_info(msg.as_ptr()) }
    }
}

// === 当前进程状态 (供 api.rs 与本模块共享) ===

static CURRENT_PROCESS_PTR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static INIT_PROCESS_CREATED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

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
    CURRENT_IN_KERN.load(Ordering::SeqCst) as u32
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

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_get_current() -> u64 {
    let ptr = CURRENT_PROCESS_PTR.load(Ordering::SeqCst);
    if ptr == 0 {
        if INIT_PROCESS_CREATED.load(Ordering::SeqCst) == 0 {
            create_init_process();
        }
        CURRENT_PROCESS_PTR.load(Ordering::SeqCst)
    } else {
        ptr
    }
}

fn create_init_process() {
    INIT_PROCESS_CREATED.store(1, Ordering::SeqCst);
    C_CURRENT_PROCESS.map_mut(|p| {
        p.pid = 1;
        p.state = 2;
        p.priority = 2;
        p.time_slice = 10;
    });
    // SAFETY: klog_ffi_info is unsafe extern "C". msg is a valid static byte slice.
    raw::klog_info(b"[PROC] Init process created (pid=1)");
    CURRENT_PROCESS_PTR.store(C_CURRENT_PROCESS.as_ptr() as u64, Ordering::SeqCst);
}

#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
pub extern "C" fn process_get_current_pid() -> u32 {
    SCHEDULER.current().unwrap_or(0)
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

#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
/// 获取进程的原始指针 (用于需要直接访问进程的场景).
pub fn process_get_raw(pid: u32) -> Option<*const super::process::Process> {
    PROCESS_TABLE.get(pid).map(|p| p as *const _)
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
/// ## 为何供 `framework/fs` 内联全限定路径调用
///
/// VFS 侧 (`framework/fs/vfs/handle.rs`) 若 `use crate::framework::proc::...`
/// 会新增 `fs → proc` 的 `use` 依赖, 触发耦合审计; 以
/// `crate::framework::proc::with_current_fd_table(...)` 内联路径调用可绕开
/// (先例: `crate::framework::fs::flock_release_pid`).
pub fn with_current_fd_table<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&super::process::FdTable) -> R,
{
    let ptr = SCHEDULER.get_current_process()?;
    // SAFETY: ptr 由 PROCESS_TABLE.get(当前 pid) 返回, 指向已注册且正在运行的
    // 当前进程 PCB; 当前进程持有对自身的引用, 本次调用期间不会被释放。
    let proc = unsafe { &*ptr };
    Some(f(&proc.fd_table))
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::used_underscore_binding,
    reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
)]
pub extern "C" fn process_get_by_pid(_pid: u32) -> u64 {
    if u64::from(_pid) == C_CURRENT_PROCESS.map(|p| p.pid) {
        C_CURRENT_PROCESS.as_ptr() as u64
    } else {
        PROCESS_TABLE.get(_pid).map_or(0, |p| p as u64)
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_get_current_pwm() -> u64 {
    let pid = SCHEDULER.current().unwrap_or(0);
    if pid == 0 {
        return 0;
    }
    PROCESS_TABLE
        .with_process(pid, super::process::Process::get_pwm)
        .unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
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
#[unsafe(no_mangle)]
pub extern "C" fn process_create(name: *const u8, parent_pid: Pid, pwm: u64) -> Pid {
    proc_create_internal(name, parent_pid, pwm)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_exit(exit_code: u32) {
    let current_pid = SCHEDULER.current().unwrap_or(0);
    if current_pid != 0 {
        // 释放该进程持有的所有文件锁 (flock + POSIX lock)
        crate::framework::fs::current_vfs_ops().release_pid_locks(current_pid);

        // B-9.5: 关闭该进程 fd 表全部 fd — 递减 OpenFile 引用并触发 pcache
        // 失效 / inotify / fd 通知, 避免全局 OPEN_FILE_TABLE 槽位泄漏。
        // (先例: 上方 release_pid_locks 同为 proc → fs 经 VfsOps 契约接线)
        crate::framework::fs::current_vfs_ops().close_all_fds();

        // T1-G2: robust futex 遍历 + CLONE_CHILD_CLEARTID 清零.
        // 必须在切换内核页表/销毁用户地址空间之前执行 (用户内存仍可访问).
        crate::framework::proc::robust::exit_cleanup(current_pid);

        let kernel_cr3 = get_kernel_pml4();
        if kernel_cr3 != 0 {
            // SAFETY: kernel_cr3 是从 vmm::get_kernel_pml4() 获取的合法页表。
            raw::switch_page_table(kernel_cr3);
        }
        // 用户地址空间的释放在 `SCHEDULER.exit` 内置 Zombie 之后执行 (见该处 D4 注)。
        // 此处不可调用: 彼时 state 仍为 Running, `destroy_by_pid_no_kstack` 的
        // is_exited() 守卫不满足, 会空转返回并使页表永不销毁。
    }
    SCHEDULER.exit(exit_code);
}

/// 阻塞当前进程 (用于 futex wait / 等待 I/O 等)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_block(pid: u32) {
    use super::types::BlockReason;
    if pid == 0 {
        return;
    }
    let current_pid = SCHEDULER.current().unwrap_or(0);
    if pid != current_pid {
        return;
    }
    SCHEDULER.block(BlockReason::FutexWait);
    SCHEDULER.schedule();
}

/// 解除进程阻塞 (用于 futex wake / I/O 完成等)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_unblock(pid: u32) {
    if pid == 0 {
        return;
    }
    SCHEDULER.unblock(pid);
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_kill(pid: u32, exit_code: u32) {
    if pid == 0 {
        return;
    }

    let should_kill = PROCESS_TABLE
        .with_process(pid, |proc| {
            let state = proc.get_state();
            if state == ProcessState::Zombie || state == ProcessState::Terminated {
                false
            } else {
                proc.exit_code.store(exit_code, Ordering::SeqCst);
                let _ = proc.set_state_safe(ProcessState::Zombie);
                true
            }
        })
        .unwrap_or(false);

    if should_kill {
        SCHEDULER.unblock(pid);
        SCHEDULER.set_need_reschedule();
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn process_find_by_pid(pid: Pid) -> u64 {
    PROCESS_TABLE.get(pid).map_or(0, |p| p as u64)
}

// ============================================================================
// 进程创建内部实现
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_create_internal(name: *const u8, parent_pid: Pid, pwm: u64) -> Pid {
    if name.is_null() {
        return 0;
    }

    let name_str = match name.as_kstr_opt() {
        Some(s) if !s.is_empty() => s,
        _ => return 0,
    };

    let parent = if parent_pid == 0 {
        None
    } else {
        Some(parent_pid)
    };

    SCHEDULER.create_process(name_str, parent, pwm).unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub extern "C" fn proc_exec_replace(
    elf_data: *const u8,
    elf_size: u64,
    argv: *const *const u8,
    argc: u32,
) -> i32 {
    if elf_data.is_null() {
        return -1;
    }

    let current_pid = SCHEDULER.current().unwrap_or(0);
    if current_pid == 0 {
        return -1;
    }

    let pwm = super::api::scheduler_get_current_pwm();

    // P0-I-31 修复: transactional execve — 先在临时进程中加载并验证新 ELF,
    // 验证通过后将新地址空间转移到当前 PID, 保持 POSIX execve 语义 (PID 不变).
    // 阶段 4b: ELF 文件 I/O 下沉 services, framework 仅收字节并加载 (纯机制).
    let new_pid = super::api::user_proc_load_elf_from_memory(elf_data, elf_size, pwm);
    if new_pid < 0 {
        return -1;
    }

    let new_pid_u32 = new_pid as u32;

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

    let (new_cr3, new_entry, new_user_stack, new_stack_bottom) = if let Some(info) = new_addr_space
    {
        info
    } else {
        USER_PROC_MANAGER.destroy_by_pid(new_pid_u32);
        PROCESS_TABLE.remove_and_free(new_pid_u32);
        return -1;
    };

    // 阶段 3: 切换到内核页表, 替换当前进程的用户地址空间
    let kernel_cr3 = get_kernel_pml4();
    if kernel_cr3 != 0 {
        // SAFETY: kernel_cr3 是从 vmm::get_kernel_pml4() 获取的合法页表。
        raw::switch_page_table(kernel_cr3);
    }

    USER_PROC_MANAGER.replace_user_space(
        current_pid,
        new_cr3,
        new_entry,
        new_user_stack,
        new_stack_bottom,
    );

    // 阶段 4: 移除临时新进程
    // G3 修复: 新地址空间已转移给当前进程 (所有权随 cr3 移动, 不重新计数),
    // 临时进程必须先清空其 cr3, 否则其 Process::drop 会对同一 PML4 计数减到
    // 归零并销毁当前进程正在使用的页表 (UAF).
    PROCESS_TABLE.with_process(new_pid_u32, |p| p.cr3.store(0, Ordering::SeqCst));
    USER_PROC_MANAGER.detach_by_pid(new_pid_u32);
    PROCESS_TABLE.remove_and_free(new_pid_u32);

    // 5. 设置 argv/envp
    if !argv.is_null() && argc > 0 {
        let envp: *const *const u8 = core::ptr::null();
        // SAFETY: argv 来自 C ABI 调用方, 由本函数 C ABI contract 保证。
        unsafe {
            super::api::user_proc_setup_argv(current_pid, argv, argc, envp, 0);
        }
    }

    // 5a. I-48: 重置信号状态 (execve 后信号处理 = 默认)
    crate::framework::proc::reset_signal_state_on_exec(current_pid);

    // 5b. POSIX close-on-exec: 关闭所有标记 CLOEXEC 的 fd
    crate::framework::fs::current_vfs_ops().close_cloexec_fds();

    // 6. 同步当前进程信息
    C_CURRENT_PROCESS.map_mut(|p| {
        p.pid = u64::from(current_pid);
    });

    // 7. 进入当前 PID (使用新地址空间)
    super::api::user_proc_enter_by_pid(current_pid);
    0
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_wait_child(pid: Pid) -> i32 {
    if pid == 0 {
        return -1;
    }

    let proc = PROCESS_TABLE.get(pid);
    if proc.is_none() {
        return -1;
    }

    let (state, code) = PROCESS_TABLE
        .with_process(pid, |process| {
            let state = process.get_state();
            if state == ProcessState::Zombie {
                let code = process.exit_code.load(Ordering::SeqCst) as i32;
                (state, Some(code))
            } else {
                (state, None)
            }
        })
        .unwrap_or((ProcessState::Terminated, None));

    if state == ProcessState::Zombie {
        PROCESS_TABLE.remove_and_free(pid);
        return code.unwrap_or(-1);
    }

    SCHEDULER.block(BlockReason::WaitingForChild);
    -2
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_sleep_ms(ms: u64) {
    if ms == 0 {
        return;
    }

    let pid = SCHEDULER.current().unwrap_or(0);
    if pid == 0 {
        return;
    }

    let current_ticks = timer_get_ticks();
    let ticks_to_sleep = ms.div_ceil(10);
    if ticks_to_sleep == 0 {
        return;
    }

    let wakeup_at = current_ticks + ticks_to_sleep;

    PROCESS_TABLE.with_process(pid, |proc| {
        proc.sleep_until.store(wakeup_at, Ordering::SeqCst);
    });

    SCHEDULER.block(BlockReason::Sleeping);
    SCHEDULER.schedule();
}

/// 保存当前 syscall 的用户寄存器到进程 `p.context` (B05-55 根治).
///
/// # 背景
/// fork/clone 复制 `p.context`, 但该字段仅由 `process_switch_asm` (调度器切换)
/// 写入. init 等首个用户进程经 `enter_user_asm` 直接进入用户态, **从未被调度器
/// 保存过 context**, 故 `p.context` 全零 → 子进程 iretq 用全零帧 → 未进用户态.
///
/// # 根治
/// 在 syscall 入口 (syscall_dispatch_from_frame) 每次调用本函数, 把当前
/// InterruptFrame (用户寄存器真实状态) 映射到 `p.context`. 这样 fork/clone
/// 复制的就是最新用户寄存器 (rip=返回地址, rsp=用户栈, cs/ss/rflags 等),
/// 子进程从正确的用户返回点继续.
///
/// ProcessContext 本不含 rdi/rsi/rdx/rcx/r8-r11 (调度切换不保存这些), 但 B05-55
/// 在 `extra_regs[8]` 中保存它们: 已运行过的进程返回用户态靠 syscall/中断栈的
/// InterruptFrame 恢复, 首次被调度的子进程 (fork/clone) 由 process_switch_asm
/// iretq 前从 extra_regs 恢复 (继承 fork 时父进程的 rdi 等). rax 由 fork/clone
/// 在子 context 上改写为 0.
pub fn proc_save_user_regs(pid: Pid, f: &crate::framework::idt::InterruptFrame) {
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
/// - `@112` ← SPSR_EL1 (EL0 状态)  - `@120` ← ELR_EL1 (用户返回 PC)
/// - `@128` ← SP_EL0 (用户栈指针)  - `@136` ← 用户页表 (EL0 的 TTBR0)
/// - `extra_regs[@672..728]` ← x0..x7
///
/// **不写** `@96` (SP_EL1) 与 `@104` (EL1 侧 TTBR0): 二者由调度切换保存侧维护,
/// 未调度过的新任务由创建方 (fork / init) 预置.
#[cfg(target_arch = "aarch64")]
#[expect(
    clippy::used_underscore_binding,
    reason = "used_underscore_binding: `_fpu_pad` 是历史填充槽名 (@136), 已被复用为 \
              用户页表槽位并被 switch.asm / context.rs 偏移表按名引用, 改名波及跨文件文档; \
              当前优先 expect"
)]
pub fn proc_save_user_regs_aarch64(
    pid: Pid,
    f: &crate::framework::arch::aarch64::exception::ExceptionFrame,
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
        // 用户页表 (本进程页表的权威来源); @136 复用 _fpu_pad 槽位.
        ctx._fpu_pad = p.cr3.load(Ordering::SeqCst);
        ctx.extra_regs[0] = f.x0;
        ctx.extra_regs[1] = f.x1;
        ctx.extra_regs[2] = f.x2;
        ctx.extra_regs[3] = f.x3;
        ctx.extra_regs[4] = f.x4;
        ctx.extra_regs[5] = f.x5;
        ctx.extra_regs[6] = f.x6;
        ctx.extra_regs[7] = f.x7;
    });
}

/// fork 系统调用实现 (COW 页表克隆 + namespace 继承)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
)]
pub extern "C" fn sys_fork() -> Pid {
    let parent_pid = SCHEDULER.current().unwrap_or(0);
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
    let child_ptr = raw::alloc_process(child_pid, "", Some(ProcessId(parent_pid)));
    let child = raw::process_ref_mut(child_ptr);

    // COW 页表克隆: 父子共享物理页, 写入时触发 page fault 复制
    // KPTI 修复: page fault handler 现在使用 get_user_pml4() 获取正确的用户页表
    // G2 修复: 克隆失败必须回滚 (原 `.unwrap_or(parent_cr3)` 在 OOM 时静默共享父页表,
    // 造成父子双所有权且 COW 语义静默降级为共享写)
    let Some(child_cr3) = crate::framework::mm::cow::clone_user_page_table_cow(parent_cr3) else {
        // 此刻子进程尚无内核栈 (`allocate_kernel_stack` 在下方) 且未插入进程表,
        // 直接释放其描述符 + 归还已分配的 PID (否则失败 fork 会泄漏 PID 位图位)
        raw::drop_boxed_process(child_ptr);
        PROCESS_TABLE.free_pid(child_pid);
        return 0;
    };
    child.cr3.store(child_cr3, Ordering::SeqCst);
    child.pwm.store(0, Ordering::SeqCst);
    // rlimit
    if let Some(rlim) = PROCESS_TABLE.with_process(parent_pid, |p| p.rlimit_table.lock().clone()) {
        *child.rlimit_table.lock() = rlim;
    }
    // session/pgid/canary
    if let Some((sid, pgid, canary)) = PROCESS_TABLE.with_process(parent_pid, |p| {
        (
            p.session_id.load(Ordering::SeqCst),
            p.pgid.load(Ordering::SeqCst),
            p.stack_canary.load(Ordering::SeqCst),
        )
    }) {
        child.session_id.store(sid, Ordering::SeqCst);
        child
            .pgid
            .store(if pgid == 0 { parent_pid } else { pgid }, Ordering::SeqCst);
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
    // 内核栈
    if !child.allocate_kernel_stack() {
        raw::drop_boxed_process(child_ptr);
        return 0;
    }
    if let Some(parent_kstack) =
        PROCESS_TABLE.with_process(parent_pid, |p| p.kernel_stack.load(Ordering::SeqCst))
    {
        if parent_kstack != 0 {
            // 拷贝父 kstack 内容到子 kstack: 源/目标都从"顶部向下 size"开始,
            // 而非从顶部向上 (原实现拷 64KB 到 kstack 顶上方, 越界损坏内存).
            // size 取父 kstack 实际大小 (USER_KSTACK_SIZE), 使子进程内核栈初始状态与父一致.
            let kstack_size = super::types::USER_KSTACK_SIZE as usize;
            raw::copy_kstack(
                child.kernel_stack.load(Ordering::SeqCst) - kstack_size as u64,
                parent_kstack - kstack_size as u64,
                kstack_size,
            );
            crate::framework::proc::kernel_stack_write_canary(
                child.kernel_stack.load(Ordering::SeqCst),
            );
        }
    }
    // KPTI 方案 S3: COW 克隆只搬用户半区 (保留槽 `[1:0] = 00` 被过滤), 子进程页表
    // 不含 EL1 视图 ⇒ 须补建, 否则子进程的 EL0→EL1 入口只能回退到全局内核表,
    // 内核态将无法访问其用户页 (copy_from_user 触发同 EL 数据异常). fail-closed:
    // 视图建不起来则子进程不可投运, 回滚 (cr3 由 `Process::drop` 按帧持有计数销毁).
    #[cfg(target_arch = "aarch64")]
    let Some(child_el1_view) = crate::framework::mm::vmm_build_el1_view(child_cr3) else {
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
            #![expect(
                clippy::used_underscore_binding,
                reason = "used_underscore_binding: `_fpu_pad` 是历史填充槽名 (@136), 复用为子进程 \
                          用户页表槽位; 改名会波及 switch.asm / context.rs 偏移表, 当前优先 expect"
            )]
            // KPTI-17: 子进程首次被调度时必须能正确进入 EL0 (恢复侧按 SPSR.M == 0
            // 走 EL0 路径), 故预置:
            //   `_fpu_pad`(@136) = 子进程用户页表 (EL0 的 TTBR0)
            //   `es`(@104)       = 子进程 EL1 视图根 (EL1 侧 TTBR0)
            //   `ds`(@96)        = 子内核栈顶 (SP_EL1: 首次陷入 EL1 时压帧的落点;
            //                      沿用父进程的值会写进父进程的栈页)
            //   `extra_regs[0]`  = x0 = 0 (fork 返回值; 恢复侧 EL0 路径恢复 x0-x7)
            child_ctx._fpu_pad = child_cr3;
            child_ctx.es = child_el1_view;
            child_ctx.ds = child.kernel_stack.load(Ordering::SeqCst) & !0xF;
            child_ctx.extra_regs[0] = 0;
            // `fs`(@112) = 父进程 SVC 入口快照的 EL0 状态 (0x3C0, 由
            // `proc_save_user_regs_aarch64` 写入) ⇒ 子进程走 EL0 恢复路径;
            // `gs`(@120) / `ss`(@128) = 用户返回 PC / 用户栈指针, 随 ctx 复制而来.
        }
    }
    // B-9.5: fork 继承父进程 fd 表 — 子进程复制父 fd 表条目, 并对每个复制出的
    // handle_id 递增 OpenFile 引用 (POSIX: fork 后父子共享同一 open file
    // description, offset/flags 共享; 父进程 fd 表不受影响, 各自退出时递减)。
    // 非 CLONE_FILES 语义, 仅复制句柄映射 (K-03 待办)。
    if let Some(handles) = PROCESS_TABLE.with_process(parent_pid, |p| {
        child.fd_table.copy_from(&p.fd_table);
        p.fd_table
            .get_all_fds()
            .into_iter()
            .map(|(_, hid)| hid)
            .collect::<alloc::vec::Vec<u32>>()
    }) {
        for hid in handles {
            crate::framework::fs::current_vfs_ops().inc_open_file_ref(hid);
        }
    }
    PROCESS_TABLE.insert(child as *const Process as *mut Process);
    PROCESS_TABLE.with_process(parent_pid, |p| {
        p.children.lock().push(ProcessId(child_pid));
    });
    let _ = child.set_state_safe(ProcessState::Ready);
    SCHEDULER.add_to_run_queue(child_pid);
    child_pid
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_ppid(pid: Pid) -> Pid {
    PROCESS_TABLE
        .with_process(pid, |p| p.parent.map_or(0, |p| p.0))
        .unwrap_or(0)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
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
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
// 注意: 保持 Rust ABI — 由 tick_accounting 内部调用, TD-10 契约测试
//       按 Rust ABI 签名匹配该函数.
#[unsafe(no_mangle)]
#[expect(clippy::no_mangle_with_rust_abi)]
pub fn proc_account_tick(in_kern: u32) {
    let pid = CURRENT_PROCESS_PTR.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    PROCESS_TABLE.with_process(pid as Pid, |p| {
        p.tick_count.fetch_add(1, Ordering::SeqCst);
        if in_kern != 0 {
            p.sys_time.fetch_add(1, Ordering::SeqCst);
        } else {
            p.user_time.fetch_add(1, Ordering::SeqCst);
        }
    });
}

/// 取得进程的 user/sys 时间 (jiffies 累积).
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
// 注意: 保持 Rust ABI — TD-10 契约测试按 Rust ABI 签名匹配该函数.
#[unsafe(no_mangle)]
#[expect(clippy::no_mangle_with_rust_abi)]
pub fn proc_get_times(pid: u32, out_user: *mut u64, out_sys: *mut u64) -> i32 {
    if out_user.is_null() || out_sys.is_null() {
        return -1;
    }
    let res = PROCESS_TABLE.with_process(pid as Pid, |p| {
        (
            p.user_time.load(Ordering::SeqCst),
            p.sys_time.load(Ordering::SeqCst),
        )
    });
    match res {
        Some((u, s)) => {
            // SAFETY: 调用方保证指针有效 (services/syscall 上下文).
            unsafe {
                core::ptr::write_unaligned(out_user, u);
                core::ptr::write_unaligned(out_sys, s);
            }
            0
        }
        None => -1,
    }
}

/// 取得当前进程启动时刻 jiffies.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_start_jiffies(pid: u32) -> u64 {
    PROCESS_TABLE
        .with_process(pid as Pid, |p| p.start_jiffies.load(Ordering::SeqCst))
        .unwrap_or(0)
}

/// 标记进程启动时刻 (`process_create` 后调用).
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_set_start_jiffies(pid: u32, j: u64) {
    PROCESS_TABLE.with_process(pid as Pid, |p| {
        p.start_jiffies.store(j, Ordering::SeqCst);
    });
}

/// 获取用户进程创建时间戳 (ticks).
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_get_create_time(pid: u32) -> u64 {
    USER_PROC_MANAGER
        .with_process(pid, |p| p.create_time)
        .unwrap_or(0)
}

/// alarm(seconds) — 设置 alarm 剩余秒数对应的 jiffies 到期时刻.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_alarm(pid: u32, seconds: u32) -> u32 {
    let hz = u64::from(crate::framework::timer::get_frequency());
    if hz == 0 {
        return 0;
    }
    let now = crate::framework::timer::get_ticks();
    let prev_remaining = PROCESS_TABLE
        .with_process(pid as Pid, |p| {
            let deadline = p.alarm_deadline.load(Ordering::SeqCst);
            let prev = if deadline == 0 || now >= deadline {
                0u32
            } else {
                ((deadline - now) / hz) as u32
            };
            if seconds == 0 {
                p.alarm_deadline.store(0, Ordering::SeqCst);
            } else {
                p.alarm_deadline
                    .store(now + u64::from(seconds) * hz, Ordering::SeqCst);
            }
            p.alarm_prev_remaining
                .store(u64::from(prev), Ordering::SeqCst);
            prev
        })
        .unwrap_or(0);
    prev_remaining
}

/// 调度器 tick 时检查 alarm 是否到期.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_check_alarm(pid: u32) -> i32 {
    let now = crate::framework::timer::get_ticks();
    let triggered = PROCESS_TABLE
        .with_process(pid as Pid, |p| {
            let d = p.alarm_deadline.load(Ordering::SeqCst);
            if d != 0 && now >= d {
                p.alarm_deadline.store(0, Ordering::SeqCst);
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
    if triggered {
        // 14 = SIGALRM
        let _ = crate::framework::proc::do_signal_send(pid as Pid, 14);
        1
    } else {
        0
    }
}

/// `setitimer(ITIMER_REAL`, new, old) — Framekernel 只实现 `ITIMER_REAL`.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_setitimer_real(
    pid: u32,
    new_seconds: u64,
    new_interval: u64,
    out_old_seconds: *mut u64,
    out_old_remaining: *mut u64,
) -> i32 {
    let hz = u64::from(crate::framework::timer::get_frequency());
    if hz == 0 {
        return -1;
    }
    let now = crate::framework::timer::get_ticks();
    let result: i32 = 0;
    PROCESS_TABLE.with_process(pid as Pid, |p| {
        if !out_old_seconds.is_null() {
            let d = p.itimer_real_deadline.load(Ordering::SeqCst);
            let rem = if d == 0 || now >= d {
                0u64
            } else {
                (d - now) / hz
            };
            // SAFETY: 调用方保证指针有效.
            unsafe {
                core::ptr::write_unaligned(
                    out_old_seconds,
                    p.itimer_real_interval.load(Ordering::SeqCst) / hz,
                );
                core::ptr::write_unaligned(out_old_remaining, rem);
            }
        }
        if new_seconds == 0 {
            p.itimer_real_deadline.store(0, Ordering::SeqCst);
        } else {
            p.itimer_real_deadline
                .store(now + new_seconds * hz, Ordering::SeqCst);
        }
        p.itimer_real_interval
            .store(new_interval * hz, Ordering::SeqCst);
        p.itimer_real_remaining
            .store(new_seconds * hz, Ordering::SeqCst);
    });
    result
}

/// `getitimer(ITIMER_REAL`, value) — 读取当前 `ITIMER_REAL` 剩余.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn proc_getitimer_real(pid: u32, out_remaining_seconds: *mut u64) -> i32 {
    if out_remaining_seconds.is_null() {
        return -1;
    }
    let hz = u64::from(crate::framework::timer::get_frequency());
    let now = crate::framework::timer::get_ticks();
    let res = PROCESS_TABLE.with_process(pid as Pid, |p| {
        let d = p.itimer_real_deadline.load(Ordering::SeqCst);
        if d == 0 || hz == 0 || now >= d {
            0u64
        } else {
            (d - now) / hz
        }
    });
    let rem = res.unwrap_or(0);
    // SAFETY: 调用方保证指针有效.
    unsafe {
        core::ptr::write_unaligned(out_remaining_seconds, rem);
    }
    0
}

/// 调度器 tick 时检查 `itimer_real` 是否到期.
#[unsafe(no_mangle)]
pub extern "C" fn proc_check_itimer_real(pid: u32) -> i32 {
    let now = crate::framework::timer::get_ticks();
    let mut triggered = false;
    PROCESS_TABLE.with_process(pid as Pid, |p| {
        let d = p.itimer_real_deadline.load(Ordering::SeqCst);
        if d != 0 && now >= d {
            let interval_ticks = p.itimer_real_interval.load(Ordering::SeqCst);
            if interval_ticks > 0 {
                p.itimer_real_deadline
                    .store(now + interval_ticks, Ordering::SeqCst);
                p.itimer_real_remaining
                    .store(interval_ticks, Ordering::SeqCst);
            } else {
                p.itimer_real_deadline.store(0, Ordering::SeqCst);
                p.itimer_real_remaining.store(0, Ordering::SeqCst);
            }
            triggered = true;
        }
    });
    if triggered {
        let _ = crate::framework::proc::do_signal_send(pid as Pid, 14);
        1
    } else {
        0
    }
}

/// POSIX getrusage(who, rusage) — 写回进程/子进程 user/sys 时间.
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn proc_get_rusage(pid: u32, who: i32, out: *mut u8, out_len: u64) -> i32 {
    if out.is_null() || out_len < 32 {
        return -1;
    }
    if who != 0 && who != 1 && who != 2 {
        return -1;
    }
    let hz = u64::from(crate::framework::timer::get_frequency());
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
    let ut_sec = (user / hz) as i64;
    let ut_usec = ((user % hz) * 1_000_000 / hz) as i64;
    let st_sec = (sys / hz) as i64;
    let st_usec = ((sys % hz) * 1_000_000 / hz) as i64;

    // SAFETY: out 至少 32 字节可写, who 合法.
    unsafe {
        core::ptr::write_unaligned(out as *mut i64, ut_sec);
        core::ptr::write_unaligned(out.add(8) as *mut i64, ut_usec);
        core::ptr::write_unaligned(out.add(16) as *mut i64, st_sec);
        core::ptr::write_unaligned(out.add(24) as *mut i64, st_usec);
        let tail = (out_len as usize).saturating_sub(32);
        if tail > 0 {
            core::ptr::write_bytes(out.add(32), 0u8, tail);
        }
    }
    0
}
