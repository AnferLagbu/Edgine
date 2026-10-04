//! 进程管理子系统 API 层
//!
//! 为内核其它模块提供进程/线程/调度的统一入口。
//!
//! ## 模块结构
//! - `proc_ops` — 进程创建/销毁/查询/操作 (`CProcess` + process_* 函数)
//! - `sched_ops` — 调度器操作 (scheduler_* 函数)
//! - `raw` — 裸指针/FFI 桥接
//!
//! ## 调用方契约
//! - `syscall::mod` — fork/execve/exit/wait4/kill/getpid 等系统调用
//! - `syscall::mmap` — mmap 通过 `process_get_current_pid` 获取当前进程
//! - `ipc::pipe/shm/signal` — IPC 操作需关联当前进程 PID 和 PWM
//! - `freg::recovery` — 进程域纳入FREG恢复
//! - `sgeg::session` — 会话管理器注册/注销进程
//! - `fs::procfs` — `/proc` 文件系统读取进程列表
//!
//! ## 安全约束
//! - `CURRENT_PROCESS_PTR` 用 `AtomicU64` 无锁读写,但 `C_CURRENT_PROCESS` 是 unsafe static mut
//! - `process_get_current()` 懒初始化 init 进程 (pid=1)
//! - `process_exit()` 必须在内核态调用,退出前切换到内核 CR3
//! - `PROCESS_TABLE` / `SCHEDULER` 均为全局单例,内部有锁保护
//!
//! ## 性能特征
//! - 进程查找: O(1) 哈希表
//! - 进程创建: O(N) PID 扫描 (N ≤ 65536)
//! - 上下文切换: asm stub, ~200 CPU cycles

use core::sync::atomic::Ordering;

use super::proc_ops;
use super::sched_ops;
use super::scheduler::SCHEDULER;
use super::user_proc::USER_PROC_MANAGER;

// 向后兼容 re-export: 将已拆分至 proc_ops / sched_ops 的函数重新导出
// 使外部代码仍可通过 `proc::api::*` 路径访问
pub use proc_ops::*;
pub use sched_ops::*;

// ============================================================================
// wait_queue 桩函数
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn wait_queue_init(_wq: *mut u8) {}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn wait_queue_add(_wq: *mut u8, _thread: u64) {}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn wait_queue_wake_one(_wq: *mut u8) {}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn wait_queue_wake_all(_wq: *mut u8) {}

// ============================================================================
// 会话/用户进程初始化
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn session_init() {
    super::session::SESSION_MANAGER.init();
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn user_proc_init() {
    USER_PROC_MANAGER.init();
}

// ============================================================================
// init 启动状态查询 (供 functions 包装)
// ============================================================================

/// init 启动状态: 0=未启动, 1=initramfs 解压中, 2=init ELF 加载中, 3=已 Ring 3 进入
static INIT_STATUS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// 获取 init 启动状态
pub fn init_launch_status() -> u32 {
    INIT_STATUS.load(core::sync::atomic::Ordering::Acquire)
}

/// 由 functions 层启动编排 (launch_first_user_process) 设置 init 启动状态
pub fn set_init_status(s: u32) {
    INIT_STATUS.store(s, core::sync::atomic::Ordering::Release);
}

// ============================================================================
// 用户进程加载与进入
// ============================================================================

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn user_proc_load_elf_from_memory(
    elf_data: *const u8,
    elf_size: u64,
    pwm: u64,
) -> i32 {
    USER_PROC_MANAGER.load_elf_from_memory(elf_data, elf_size, pwm)
}

/// 在 ELF 加载完成后, 在用户栈上建立 argv/envp (供 exec 系统调用使用)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
#[expect(
    clippy::similar_names,
    reason = "similar_names: 变量名相似表达同族概念; 当前优先 expect"
)]
///
/// # Safety
///
/// `name` 是合法的 C 字符串 (以 NUL 结尾). 进程表已初始化.
pub unsafe extern "C" fn user_proc_setup_argv(
    pid: u32,
    argv: *const *const u8,
    argc: u32,
    envp: *const *const u8,
    envc: u32,
) -> i32 {
    // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
    unsafe {
        let proc = match USER_PROC_MANAGER.get(pid) {
            Some(p) => p,
            None => return -1,
        };

        let sp = USER_PROC_MANAGER.setup_user_stack(proc, argv, argc as usize, envp, envc as usize);

        if sp == 0 { -1 } else { 0 }
    }
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn user_proc_enter_by_pid(pid: u32) -> i32 {
    crate::klog_boot_info!("[USER] user_proc_enter_by_pid: pid={}", pid);

    let (pid_val, pwm_val, state_val) = USER_PROC_MANAGER
        .with_process(pid, |proc| {
            (
                u64::from(proc.process().pid.0),
                proc.process().pwm.load(Ordering::SeqCst),
                proc.process().state.load(Ordering::SeqCst),
            )
        })
        .unwrap_or((0, 0, 0));

    crate::klog_boot_info!(
        "[USER] with_process result: pid_val={:#X} pwm_val={:#X} state_val={}",
        pid_val,
        pwm_val,
        state_val
    );

    if pid_val == 0 {
        crate::klog_boot_info!("[USER] pid_val is 0, returning -1");
        return -1;
    }

    proc_ops::C_CURRENT_PROCESS.map_mut(|p| {
        p.pid = pid_val;
        p.pwm = pwm_val;
        p.state = state_val;
        p.parent_pid = 1;
    });

    SCHEDULER.set_current(pid);

    crate::klog_boot_info!("[USER] calling USER_PROC_MANAGER.get({})", pid);
    USER_PROC_MANAGER.get(pid).map_or(-1, |proc| {
        // 诊断：打印从 Process 读取的 kernel_stack 值
        // SAFETY: 指针操作在有效范围内，调用方保证指针有效性
        let p_kstack = unsafe {
            (*proc)
                .process()
                .kernel_stack
                .load(core::sync::atomic::Ordering::SeqCst)
        };
        crate::klog_boot_info!(
            "[USER] got proc={:#X}, Process.kstack={:#X}",
            proc as u64,
            p_kstack
        );
        crate::klog_boot_info!("[USER] calling enter()");
        USER_PROC_MANAGER.enter(proc);
        0
    })
}

/// 进入指定用户进程 (Ring 3 / EL0): 设置当前进程槽位 → 加入调度器 → 切换用户态.
///
/// L-02 机制入口: functions 层启动编排 (functions::init::launch_first_user_process)
/// 经此完成进入用户态的最后三步, 无需访问 privileged 内部 `C_CURRENT_PROCESS`.
///
/// 正常路径下 `USER_PROC_MANAGER.enter` 切走不返回; 若返回, 由调用方决定后续.
pub fn enter_user_process(pid: u32) {
    proc_ops::C_CURRENT_PROCESS.map_mut(|p| {
        p.pid = u64::from(pid);
        p.pwm = 0;
        p.state = 2;
        p.parent_pid = 1;
    });

    SCHEDULER.add(pid);

    user_proc_enter_by_pid(pid);
}
