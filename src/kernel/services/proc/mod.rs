#![deny(unsafe_code)]
//! 进程管理子系统 — services 层策略主体
//!
//! 进程生命周期 / 调度策略 / 信号 / namespace / cgroup / seccomp / rlimit
//! / session / coredump / fd_table / clone / elf / canary 等
//! 子模块. 0 unsafe, 全部上下文切换/页表/调度底层走 framework.
//!
//! 历史: 2026-06 之前 v2.11 状态评估已过时, 当前已远超当时范围.
//! 详细进度见 docs/plan/progress-active-tasks.md.

/// CPU 亲和性策略 — sched_setaffinity / sched_getaffinity
pub mod affinity;
pub mod canary;
/// D2: cgroup 安全封装
pub mod cgroup;
pub mod clone;
pub mod coredump;
pub mod elf;
/// execve 系统调用策略 — 指针校验 / argv 扫描 / SUID 提权
pub mod exec;
/// TD-02: 全局统一 FD 分配器 (范围规划 + 分配/释放/反查)
pub mod fd_alloc;
/// D8: FD Table 分配策略 (first-fit, 上限 64)
pub mod fd_table;
pub mod info;
/// 进程生命周期策略 — fork / exit / sched_yield
pub mod lifecycle;
pub mod madvise_mlock;
/// memfd_create — 匿名内存文件
pub mod memfd;
/// D1: Namespace 安全封装
pub mod namespace;
/// OOMD — 内存不足守护进程策略
pub mod oomd;
/// pidfd 系统调用 — pidfd_open / pidfd_send_signal / pidfd_getfd
pub mod pidfd;
/// 进程优先级策略 — nice / getpriority / setpriority
pub mod priority;
/// 进程管理策略 — proc_list / proc_setpri / credo_proc_cputime
pub mod proc_mgmt;
pub mod rlimit;
pub mod sched;
/// D3: CFS 调度策略 (权重表 + vruntime + 时间片 + CFS/DL 运行队列)
pub mod sched_policy;
pub mod seccomp;
pub mod session;
/// D7: Shadow Stack (CET) 安全封装
pub mod shadow_stack;
pub mod signal;
/// 系统信息策略 — getrusage / sysinfo / getrlimit / hostname / boot_check
pub mod sysinfo;
pub mod table;
pub mod types;
pub mod wait4;

// ============================================================================
// ID 与状态 (直接 re-export 本地 types 模块)
// ============================================================================

/// 进程 ID (新类型, 替代裸 `u32`)
pub use types::{Pid, ProcessId, ThreadId, Tid};

/// 进程状态 (七状态模型)
pub use types::ProcessState;

/// 进程优先级
pub use types::ProcessPriority;

// sched_policy 公共接口 re-export — 策略-机制分离
pub use sched_policy::{DefaultPolicy, register_default_policy};

// namespace 公共接口 re-export — 避免跨层直接访问 services::proc::namespace 内部
pub use namespace::NamespaceSet;

use crate::framework::syscall::Errno;

// ============================================================================
// 错误
// ============================================================================

/// 进程操作错误 — TD-19: 收敛到 `KernelError`, 1 字段 proc 特有 + 1 共享包装.
///
/// 字段说明:
///   - `Exited`: 进程已退出, 走 ESRCH (POSIX 中 ESRCH=3 兼含 "no such process" 语义)
///   - `Kernel(KernelError)`: 共享错误 (`NotFound` → `NoSuchProcess` / `PermissionDenied` /
///     `NoResources` → `WouldBlock` / `InvalidArgument`) 全部走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcError {
    /// 进程已退出
    Exited,
    /// 共享 `KernelError` 包装
    Kernel(crate::services::error::KernelError),
}

impl ProcError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::Exited => E::ESRCH,
            Self::Kernel(e) => e.as_errno(),
        }
    }

    pub fn from_i32(rc: i32) -> Self {
        use crate::services::error::KernelError as K;
        match rc {
            -1 => Self::Kernel(K::NoSuchProcess),
            -2 => Self::Kernel(K::PermissionDenied),
            -3 => Self::Kernel(K::WouldBlock),
            -4 => Self::Exited,
            -22 => Self::Kernel(K::InvalidArgument),
            _ => Self::Kernel(K::Other(rc)),
        }
    }
}

pub type ProcResult<T> = Result<T, ProcError>;

// ============================================================================
// 初始化入口
// ============================================================================

/// 初始化进程子系统 (按依赖顺序调用各 init)
///
/// **调用顺序** (依赖链):
/// 1. 进程表 (`PROCESS_TABLE` 常量初始化自动完成, 无需显式 init)
/// 2. 线程表 (`THREAD_MANAGER.init` → `THREAD_MANAGER` 通过 `pub fn init()`)
/// 3. 主调度器 (`SCHEDULER.init` + `SCHEDULER_EX.init`)
/// 4. Session 管理器 (`SESSION_MANAGER.init` → `pub fn init()`)
///
/// 由启动期 `kernel::init` 调用一次。
#[expect(
    clippy::missing_panics_doc,
    reason = "missing_panics_doc: init 在调度/信号策略重复注册时 panic 以暴露启动期配置错误 (framework 契约: 仅注册一次, 见 B05-36/B05-44)"
)]
pub fn init() {
    // 注册 services 层调度策略 (在 framework 调度器初始化之前)
    // 启动期重复注册是配置错误, panic 暴露 (B05-36 5.9 + B05-44 返工).
    sched_policy::register_default_policy()
        .expect("proc::init: 调度策略重复注册 (framework 契约: 仅注册一次)");
    // REVAL-1: 注册 services 层信号策略
    // 启动期重复注册是配置错误, panic 暴露 (B05-44 返工).
    signal::register_standard_signal_policy()
        .expect("proc::init: 信号策略重复注册 (framework 契约: 仅注册一次)");

    crate::framework::proc::thread::init();
    crate::framework::proc::scheduler::init();
    crate::framework::proc::scheduler_ex::init();
    crate::framework::proc::session::init();
}

// ============================================================================
// 调度器状态
// ============================================================================

/// 触发调度 (在 timer tick 或阻塞唤醒后调用)
///
/// 由架构中断处理代码调用。
pub fn schedule() {
    crate::framework::proc::SCHEDULER.schedule();
}

// ============================================================================
// 进程状态查询
// ============================================================================

/// 从 u8 值构造 `ProcessState` (安全转换)
pub fn state_from_u8(v: u8) -> ProcessState {
    ProcessState::from_u8(v)
}

/// 从 u32 值构造 `ProcessState` (兼容 `AtomicU32` 存储)
pub fn state_from_u32(v: u32) -> ProcessState {
    ProcessState::from_u32(v)
}

/// 从优先级数值构造 `ProcessPriority`
pub fn priority_from_u32(v: u32) -> ProcessPriority {
    ProcessPriority::from_u32(v)
}
