//! 系统调用分发决策 trait — 策略-机制分离接口
//!
//! T-03: 系统调用分发策略 (编号→实现映射) 由 functions 实现,
//! privileged 仅保留寄存器保存/恢复、seccomp 检查等机制.
//!
//! ## 设计
//!
//! - trait 定义在 privileged (引用 privileged 类型)
//! - 实现在 functions (100% safe Rust, `#![deny(unsafe_code)]`)
//! - privileged 提供默认回退策略 (`FallbackSyscallDispatch`), 早期启动阶段使用
//! - functions 在 `init()` 中通过 `register_syscall_dispatch()` 注册自己的策略实现
//!
//! ## 策略边界
//!
//! privileged 保留 (机制):
//! - 寄存器保存/恢复 (syscall_dispatch_from_frame)
//! - in_kern 标记 (proc_set_in_kern)
//! - seccomp 过滤检查
//! - rt_sigreturn 特殊处理
//!
//! functions 实现 (策略):
//! - syscall 编号→具体实现的分发
//! - 参数验证与转换
//! - 返回值处理

/// 系统调用分发接口 — functions 实现, privileged 调用
///
/// 所有方法均为纯决策逻辑, 不涉及硬件操作或 unsafe.
pub trait SyscallDispatch: Send + Sync {
    /// 分发系统调用
    ///
    /// `num` 为翻译后的 `Edgine` 原生编号 (EG_*).
    /// `args` 为 6 个系统调用参数.
    /// 返回值: 正数为成功返回值, 负数为 -errno.
    fn dispatch(&self, num: u64, args: [u64; 6]) -> i64;
}

// ============================================================================
// 默认回退策略 (早期启动阶段, functions 尚未注册时使用)
// ============================================================================

/// 框架内建回退策略 — 所有 syscall 返回 -ENOSYS
///
/// 在 functions 注册策略之前, 所有系统调用返回"功能未实现".
pub struct FallbackSyscallDispatch;

impl SyscallDispatch for FallbackSyscallDispatch {
    fn dispatch(&self, _num: u64, _args: [u64; 6]) -> i64 {
        // ENOSYS = 38, 返回 -38 (ENOSYS_RET 哨兵, DECISION-J 后定义于 privileged types)
        crate::privileged::syscall::types::ENOSYS_RET
    }
}

static FALLBACK_DISPATCH: FallbackSyscallDispatch = FallbackSyscallDispatch;

/// 全局策略注册表 — functions 通过 `register_syscall_dispatch` 注册
static SYSCALL_DISPATCH: crate::privileged::sync::OnceLock<&'static dyn SyscallDispatch> =
    crate::privileged::sync::OnceLock::new();

/// 注册系统调用分发策略 (由 `functions::syscall::init` 调用)
///
/// 只能注册一次; 重复注册返回 `Err`.
///
/// # Errors
/// 当策略已注册时, 返回 `Err`, 其中携带已注册的旧策略指针.
pub fn register_syscall_dispatch(
    policy: &'static dyn SyscallDispatch,
) -> Result<(), &'static dyn SyscallDispatch> {
    match SYSCALL_DISPATCH.set(policy) {
        Ok(()) => Ok(()),
        Err(existing) => Err(existing),
    }
}

/// 获取当前注册的系统调用分发策略 (未注册时返回内建回退)
#[inline]
pub fn current_syscall_dispatch() -> &'static dyn SyscallDispatch {
    match SYSCALL_DISPATCH.get() {
        Some(&p) => p,
        None => &FALLBACK_DISPATCH,
    }
}
