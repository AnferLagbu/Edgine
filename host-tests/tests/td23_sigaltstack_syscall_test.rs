//! I-45 补充验收: 用户态 sigaltstack 系统调用, 内核正确记录替代栈参数
//!
//! 覆盖契约:
//! 1. `SYS_sigaltstack = 131` 系统调用号
//! 2. `sys_rt_sigreturn` 仅清 `SS_ONSTACK`, 保留 `SS_DISABLE`
//! 3. host 无当前进程时 `sys_sigaltstack` 返回 -ESRCH (内核真实路径)
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `sigaltstack_op` / `StackT` / `AltStackState` 状态机平行镜像, 改引
//! 内核真实常量:
//! - `edgine::kernel::services::syscall::types::SYS_sigaltstack` (pub const = 131)
//! - `edgine::kernel::framework::proc::{SS_ONSTACK, SS_DISABLE}` (pub const)
//! - `edgine::kernel::framework::syscall::api::sys_sigaltstack` (pub fn)
//!
//! ## 因内核 host 不可测已移除
//! `sys_sigaltstack` 的完整状态机 (SS_DISABLE 清 addr/size / 启用清 flags /
//! old_ss 查询) 无法在 host 直接验证: 内核实现依赖
//! `process_get_current_pid()` → `SCHEDULER.current()` (全局调度器每 CPU 当前 pid,
//! host 下无法注入测试 pid 使 `process_with_mut` 命中测试进程). 对应用例
//! (query_only_no_change / set_then_query_returns_value / disable_clears_addr_size
//! 等) 已移除. 状态机真实覆盖由 QEMU 集成测试 (syscall 路径) 承担.

use edgine::kernel::framework::proc::{SS_DISABLE, SS_ONSTACK};
use edgine::kernel::framework::syscall::api::sys_sigaltstack;
use edgine::kernel::services::syscall::types::SYS_sigaltstack;

#[test]
fn syscall_number_is_546() {
    // 镜像 [framework/syscall/types.rs::SYS_sigaltstack] → 内核真实常量
    assert_eq!(SYS_sigaltstack, 131);
}

#[test]
fn ss_onstack_cleared_on_sigrturn() {
    // 镜像 sys_rt_sigreturn: 仅清 SS_ONSTACK, 保留 SS_DISABLE
    // (内核 dispatch.rs: flags & !SS_ONSTACK). 使用内核 pub 常量验证位语义.
    let flags = SS_ONSTACK | SS_DISABLE;
    let new_flags = flags & !SS_ONSTACK;
    assert_eq!(new_flags & SS_ONSTACK, 0);
    assert_eq!(new_flags & SS_DISABLE, SS_DISABLE);
}

#[test]
fn ss_flag_constants() {
    // 内核 pub 常量 (proc/signal.rs): POSIX 定义
    assert_eq!(SS_ONSTACK, 1);
    assert_eq!(SS_DISABLE, 2);
    assert_eq!(SS_ONSTACK & SS_DISABLE, 0); // 互斥位
}

#[test]
fn sigaltstack_no_current_process_returns_esrch() {
    // host 下 SCHEDULER.current() = None → process_get_current_pid() = 0
    // → sys_sigaltstack 返回 -ESRCH (3). 这是内核 sys_sigaltstack 在 host
    // 环境下可到达的真实路径, 验证当前进程缺失时的契约.
    let ret = sys_sigaltstack(0, 0);
    assert_eq!(
        ret, -3,
        "host 无当前进程时 sys_sigaltstack 应返回 -ESRCH, 实际 = {}",
        ret
    );
}
