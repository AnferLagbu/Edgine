//! # Privileged 进程子系统 (TCB)
//!
//! 唯一允许 `unsafe` 的进程管理底层实现。对应 `functions/proc/` 安全代理。
//!
//! ## 依赖声明
//!
//! privileged 内部依赖: mm, sync, syscall, config, idt, sched, freg, tests
//! functions 依赖: `functions::proc` (安全代理)
//!
//! ## 架构
//!
//! ```text
//! privileged/proc/  (TCB 进程管理)
//! ├── types.rs          核心数据结构 (Process/Thread/Task/Pid/Tid 等)
//! ├── process.rs        进程表 (PCB/进程 CRUD/状态机)
//! ├── thread.rs         线程 (内核线程/用户线程/上下文)
//! ├── session.rs        会话/进程组/凭证
//! ├── elf.rs            ELF 二进制解析 (TCB 原始)
//! ├── api.rs            C ↔ Rust 桥接 (兼容层)
//! ├── scheduler.rs      主调度器 (CFS + 实时 + 批处理)
//! ├── scheduler_ex.rs   调度器扩展 (SMP 负载均衡/亲和性)
//! ├── cfs.rs            CFS 公平调度算法
//! ├── cpu_queue.rs      每 CPU 运行队列
//! ├── oomd.rs           OOM killer
//! ├── switch.asm        上下文切换汇编
//! └── mod.rs            模块导出
//! ```
//!
//! ## SAFETY 契约
//!
//! - 唯一允许 unsafe 的进程管理实现层
//! - 进程表/线程表的裸指针解引用集中在本模块
//! - functions/proc/ 通过安全 API (`with(pid, |p| ...)`) 调用本模块
//!
//! ## 与 functions/proc/ 的关系
//!
//! - `functions/proc/` 是 100% safe 业务封装, 顶部 `#![deny(unsafe_code)]`
//! - `functions/proc/` 通过 `pub use privileged::proc::*` 暴露底层能力给 syscall
//! - 任何 unsafe 改动必须在 `privileged/proc/` 完成, 业务层禁止穿透

#![allow(ambiguous_glob_reexports)]

pub mod api;
pub mod canary;
pub mod cfs;
pub mod cgroup;
pub mod coredump_trait;
pub mod cpu_queue;
/// SGEG 域级行为门控 (`DomainFlags`) 机制 + 状态读写
pub mod domain;
pub mod elf;
/// D2: cgroup 资源控制器
/// TD-02: 全局统一 FD 分配器与基址规划
pub mod fd_alloc;
pub mod fd_table;
pub mod madvise_mlock;
/// L-02: 机制 API 集中导出 — 供 functions 层策略实现调用
pub mod mechanism;
/// D1: Linux 兼容 Namespace 框架
pub mod namespace;
pub mod oomd;
pub mod posix_timer;
pub mod proc_ops;
pub mod process;
pub mod rlimit;
/// 进程退出清理 (robust futex / CLEARTID) — 机制层
pub mod robust;
pub mod sched_ops;
pub mod sched_trait;
pub mod scheduler;
pub mod scheduler_ex;
pub mod seccomp;
pub mod session;
pub mod signal;
pub mod signal_trait;
pub mod thread;
pub mod types;
pub mod user_proc;

// LATER(polish): 用显式导入替代 glob re-export 消除歧义
// USER_STACK_SIZE: types(usize) 对比 user_proc(u64)
// init: scheduler vs user_proc
pub use crate::privileged::freg::*;
pub use canary::*;
pub use coredump_trait::{
    CoredumpSink, RegSnapshot, current_coredump_sink, read_interrupt_regs, register_coredump_sink,
};
pub use posix_timer::*;
pub use process::*;
pub use scheduler::*;
pub use scheduler_ex::*;
pub use session::*;
pub use signal::*;
pub use signal_trait::*;
pub use thread::*;
pub use types::*;
pub use user_proc::*;

// cpu_queue 公共接口 re-export — 避免跨子系统直接访问 proc::cpu_queue 内部
pub use cpu_queue::init_cpu_queue;

// api 公共接口 re-export — 避免跨子系统直接访问 proc::api 内部
pub use api::*;

// proc_ops / sched_ops 公共接口 re-export
pub use proc_ops::*;
pub use sched_ops::*;

// raw 公共接口 re-export — 避免跨子系统直接访问 proc::proc_ops::raw 内部
pub use proc_ops::raw;

// fd_alloc 公共接口 re-export — 避免跨子系统直接访问 proc::fd_alloc 内部
pub use fd_alloc::{FdPlan, FdSubsystem, alloc_fd, fd_at, free_fd, idx_of};

// madvise_mlock 公共接口 re-export — 避免跨子系统直接访问 proc::madvise_mlock 内部
pub use madvise_mlock::{
    sys_madvise, sys_mincore, sys_mlock, sys_mlockall, sys_munlock, sys_munlockall,
};

// elf 公共接口 re-export — 避免跨子系统直接访问 proc::elf 内部
pub use elf::{Elf64Header, Elf64Phdr, ElfLoadResult, elf_load, elf_validate};

// rlimit 公共接口显式 re-export — glob re-export 可能被遮蔽
// sys_getrlimit / sys_setrlimit 已随 T2 迁至 functions (syscall-followup), 此处仅保留机制字段与查询辅助
pub use rlimit::{RLIM_INFINITY, RLIMIT_CORE, get_memlock_limit};

// seccomp 公共接口 re-export — 避免跨子系统直接访问 proc::seccomp 内部
// sys_seccomp/sys_prctl_prctl 已随 T2 批 2 迁至 functions (syscall-followup);
// 此处仅保留机制类型/常量/检查入口供 functions 策略消费
pub use seccomp::{
    DEFAULT_ACTION, MAX_FILTERS, SeccompAction, SeccompFilter, SeccompMode, SeccompRule,
    SeccompState, seccomp_check,
};

// domain 公共接口 re-export — 避免跨子系统直接访问 proc::domain 内部
pub use domain::{domain_flags_get, domain_flags_set, domain_gate_check};

// namespace 公共接口 re-export — 避免跨子系统直接访问 proc::namespace 内部
// sys_setns/sys_unshare 已随 T2 批 4 迁至 functions (syscall-followup);
// NamespaceSet 机制字段保留
pub use namespace::NamespaceSet;

// cgroup 公共接口 re-export — 避免跨子系统直接访问 proc::cgroup 内部
pub use cgroup::{
    cgroup_is_initialized, cgroup_subsystem, sys_cgroup_attach, sys_cgroup_create,
    sys_cgroup_destroy, sys_cgroup_get_stat, sys_cgroup_set_limit,
};

// sched_trait 公共接口 re-export — 策略-机制分离
pub use sched_trait::{
    FallbackPolicy, SchedDecision, current_sched_decision, register_sched_decision,
};

// mechanism 模块供 functions 层通过 privileged::proc::mechanism::* 访问机制 API
// 不使用 glob re-export 因与现有 api re-export 产生歧义
