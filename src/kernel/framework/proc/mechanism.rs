//! 进程管理 — 机制 API 集中导出
//!
//! L-02: 将 framework/proc 的纯机制函数集中导出, 供 services 层策略实现调用.
//!
//! **机制 (Mechanism)**: 直接操作硬件或全局数据结构的底层操作:
//! - 页表切换 (CR3 切换)
//! - 内核栈复制
//! - 上下文切换
//! - 进程表 CRUD (PCB 分配/释放/插入/查找)
//! - 调度器队列操作 (add/block/unblock/schedule)
//! - 用户进程加载/进入
//!
//! **策略 (Policy)**: 决定"何时/如何"使用机制:
//! - fork 的 COW 决策
//! - execve 的地址空间替换策略
//! - 调度优先级计算
//! - 进程创建/退出的资源管理策略
//!
//! services 层通过 `use framework::proc::mechanism::*` 获取机制 API,
//! 在 services 层实现策略逻辑.

// ==================== 页表操作 ====================

pub use super::proc_ops::raw::clone_user_page_table_cow;
pub use super::proc_ops::raw::destroy_user_page_table;
pub use super::proc_ops::raw::switch_page_table;

// ==================== 进程分配/释放 ====================

pub use super::proc_ops::raw::alloc_process;
pub use super::proc_ops::raw::drop_boxed_process;
pub use super::proc_ops::raw::process_ref;
pub use super::proc_ops::raw::process_ref_mut;

// ==================== 内核栈操作 ====================

pub use super::proc_ops::raw::copy_kstack;

// ==================== 进程表操作 ====================

pub use super::proc_ops::process_dec_ref;
pub use super::proc_ops::process_exists;
pub use super::proc_ops::process_for_each;
pub use super::proc_ops::process_get_cr3;
pub use super::proc_ops::process_get_pwm;
pub use super::proc_ops::process_get_raw;
pub use super::proc_ops::process_insert;
pub use super::proc_ops::process_remove_and_free;
pub use super::proc_ops::process_try_inc_ref;
pub use super::proc_ops::process_with;
pub use super::proc_ops::process_with_mut;

// ==================== 调度器操作 ====================

pub use super::sched_ops::scheduler_add;
pub use super::sched_ops::scheduler_add_to_run_queue;
pub use super::sched_ops::scheduler_current_cputime;
pub use super::sched_ops::scheduler_schedule;
pub use super::sched_ops::scheduler_unblock;
pub use super::sched_ops::scheduler_yield;
pub use super::sched_ops::scheduler_yield_ex;

// ==================== 用户进程操作 ====================

pub use super::api::enter_user_process;
pub use super::api::user_proc_enter_by_pid;
pub use super::api::user_proc_load_elf_from_memory;
pub use super::api::user_proc_setup_argv;
pub use super::proc_ops::proc_alloc_pid;

// ==================== 进程信息查询 ====================

pub use super::proc_ops::process_find_by_pid;
pub use super::proc_ops::process_get_by_pid;
pub use super::proc_ops::process_get_current;
pub use super::proc_ops::process_get_current_pid;
pub use super::proc_ops::process_get_current_pwm;
pub use super::proc_ops::process_get_pwm_by_pid;
pub use super::proc_ops::update_current_process_ptr;

// ==================== 信号操作 ====================

pub use super::proc_ops::process_signal_pending_set;

// ==================== 进程状态操作 ====================

pub use super::proc_ops::proc_get_in_kern;
pub use super::proc_ops::proc_set_in_kern;
pub use super::sched_ops::proc_get_exit_code;
pub use super::sched_ops::proc_get_state;
pub use super::sched_ops::proc_is_initialized;
pub use super::sched_ops::proc_set_priority;

// ==================== 初始化 ====================

pub use super::api::session_init;
pub use super::api::user_proc_init;
pub use super::sched_ops::process_init;
pub use super::sched_ops::scheduler_init;
pub use super::sched_ops::scheduler_tick;
pub use super::sched_ops::thread_init;
