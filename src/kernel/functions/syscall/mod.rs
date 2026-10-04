#![deny(unsafe_code)]
//! 系统调用 (Syscall) — functions 层安全代理
//!
//! ## 状态 (v2.18, 2026-06-04)
//!
//! Phase 2.5 syscall 迁移:
//! - [x] 强类型 `SyscallNumber` 替代裸 u64
//! - [x] 强类型 `SyscallArgs` 替代 6 个独立 u64 参数
//! - [x] 强类型 `SyscallResult<T>` (基于 `Errno`)
//! - [x] 用户态指针/缓冲区验证
//! - [x] `UserContext` 入口安全分发
//! - [x] 处理器注册 API (dispatch trait, 见 `dispatch.rs`)
//!
//! ## 迁移方法
//!
//! 1. 通过 `dispatch_trait` 注册 functions 层分发策略, privileged 回退未迁移 syscall
//! 2. functions 层 0 unsafe — 所有 unsafe 在 privileged TCB
//! 3. 强类型 `SyscallResult<T>` 替代 `i64` 返回码 (POSIX 风格: 负数 = -errno)
//!
//! 评估日期: 2026-06-04

pub mod brk;
pub mod canary;
pub mod dispatch;
// 设备固件加载 系统调用 (从 privileged/syscall/firmware.rs 下沉, §6.2)
pub mod firmware;
// ftrace / KGDB 系统调用 (从 privileged/syscall/ftrace_kgdb.rs 下沉, §6.2)
pub mod ftrace;
// POSIX Timer 系统调用包装 (从 privileged/syscall/posix_timer.rs 下沉, §6.1)
pub mod posix_timer;
pub mod types;

use crate::privileged::syscall;
use crate::privileged::userctx::UserContext;
use crate::privileged::usermode;

// ============================================================================
// 强类型 re-export
// ============================================================================

/// POSIX errno 错误码
pub use types::Errno;

/// Syscall 编号 (新类型, 替代裸 u64)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct SyscallNumber(pub u64);

impl SyscallNumber {
    /// 构造
    #[inline]
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    /// 原始值
    #[inline]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl From<u64> for SyscallNumber {
    #[inline]
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<SyscallNumber> for u64 {
    #[inline]
    fn from(s: SyscallNumber) -> Self {
        s.0
    }
}

// ============================================================================
// Syscall 参数 (替代 4 个独立 u64)
// ============================================================================

/// Syscall 参数 (6 个 u64)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyscallArgs {
    /// 第 1 参数 (rdi / x1)
    pub a0: u64,
    /// 第 2 参数 (rsi / x2)
    pub a1: u64,
    /// 第 3 参数 (rdx / x3)
    pub a2: u64,
    /// 第 4 参数 (r10 / x4)
    pub a3: u64,
    /// 第 5 参数 (r8 / x5)
    pub a4: u64,
    /// 第 6 参数 (r9 / x6)
    pub a5: u64,
}

impl SyscallArgs {
    /// 零参数
    pub const NONE: Self = Self {
        a0: 0,
        a1: 0,
        a2: 0,
        a3: 0,
        a4: 0,
        a5: 0,
    };

    /// 构造
    #[inline]
    pub const fn new(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> Self {
        Self {
            a0,
            a1,
            a2,
            a3,
            a4,
            a5,
        }
    }

    /// 1 参数
    #[inline]
    pub const fn one(a0: u64) -> Self {
        Self::new(a0, 0, 0, 0, 0, 0)
    }

    /// 2 参数
    #[inline]
    pub const fn two(a0: u64, a1: u64) -> Self {
        Self::new(a0, a1, 0, 0, 0, 0)
    }

    /// 3 参数
    #[inline]
    pub const fn three(a0: u64, a1: u64, a2: u64) -> Self {
        Self::new(a0, a1, a2, 0, 0, 0)
    }

    /// 4 参数
    #[inline]
    pub const fn four(a0: u64, a1: u64, a2: u64, a3: u64) -> Self {
        Self::new(a0, a1, a2, a3, 0, 0)
    }
}

// ============================================================================
// Syscall 结果 (POSIX 风格, 强类型)
// ============================================================================

/// Syscall 结果 (成功值 或 errno 错误)
pub type SyscallResult<T> = Result<T, Errno>;

// ============================================================================
// 通用 Errno 转换
// ============================================================================

// B09-12/DECISION-H13 P0-1: Errno::try_from_i32 与 errno_from_i64 已迁回
// privileged::errno (原 Errno 定义随迁), 此处 re-export 保持调用方兼容.
pub use crate::privileged::errno::errno_from_i64;

/// `i64` 返回码 → `SyscallResult<u64>` (POSIX 约定)
///
/// # Errors
///
/// 当返回码为负值时返回对应的 `Errno`; 无法识别的 errno 回退为 `EINVAL`.
pub fn parse_return(rc: i64) -> SyscallResult<u64> {
    if rc < 0 {
        Err(Errno::try_from_i32(-rc as i32).unwrap_or(Errno::EINVAL))
    } else {
        Ok(rc as u64)
    }
}

// ============================================================================
// 用户态指针验证
// ============================================================================

/// 验证用户态指针
pub fn check_user_ptr(ptr: u64) -> bool {
    syscall::api::validate_user_ptr(ptr)
}

/// 验证用户态缓冲区
pub fn check_user_buf(ptr: u64, len: u64) -> bool {
    syscall::api::validate_user_buf(ptr, len)
}

// ============================================================================
// 分发
// ============================================================================

/// 通过 `UserContext` 安全分发 syscall
///
/// **参数**:
/// - `ctx`: 用户态上下文 (由中断入口构造, 内核 TCB 保证有效)
///
/// **返回**:
/// - syscall 原始返回值 (POSIX: 0/-errno 约定)
pub fn dispatch_from_ctx(ctx: &UserContext) -> i64 {
    let num = ctx.syscall_number();
    let a0 = ctx.arg0();
    let a1 = ctx.arg1();
    let a2 = ctx.arg2();
    let a3 = ctx.arg3();
    let a4 = ctx.arg4();
    let a5 = ctx.arg5();
    usermode::dispatch_syscall(num, a0, a1, a2, a3, a4, a5)
}

/// 通过 `SyscallNumber` + `SyscallArgs` 强类型分发
pub fn dispatch(num: SyscallNumber, args: SyscallArgs) -> i64 {
    usermode::dispatch_syscall(num.0, args.a0, args.a1, args.a2, args.a3, args.a4, args.a5)
}

/// 通过 `UserContext` 分发并解析为 `SyscallResult<u64>`
///
/// # Errors
///
/// 当底层分发返回负值(即 syscall 失败)时返回对应的 `Errno`.
pub fn dispatch_from_ctx_typed(ctx: &UserContext) -> SyscallResult<u64> {
    parse_return(dispatch_from_ctx(ctx))
}

// ============================================================================
// 初始化
// ============================================================================

/// 初始化 syscall 子系统 (仅注册 functions 层分发策略)
///
/// 注意: privileged 层 MSR/LSTAR 配置由 `privileged::syscall_init::syscall_init()` 单独完成,
/// 此函数仅注册 functions 层系统调用分发策略, 不再回调 privileged 以避免循环依赖。
pub fn init() {
    // T-03: 注册 functions 层系统调用分发策略
    // 注册失败说明策略已注册或状态异常, 启动期必须显式处理, 不能静默吞掉.
    let r = dispatch::register_functions_dispatch();
    if r.is_err() {
        crate::privileged::klog::log_err(
            crate::privileged::klog::LogCategory::Boot,
            format_args!("[SYSCALL] register_functions_dispatch FAILED (重复注册?)"),
        );
    }
}

// Re-export
pub use dispatch::FunctionsSyscallDispatch;

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_number_round_trip() {
        let n = SyscallNumber::new(42);
        assert_eq!(n.raw(), 42);
        assert_eq!(u64::from(n), 42u64);
        let m: SyscallNumber = 99u64.into();
        assert_eq!(m.raw(), 99);
    }

    #[test]
    fn syscall_args_constructors() {
        let a0 = SyscallArgs::one(7);
        assert_eq!(a0.a0, 7);
        assert_eq!(a0.a1, 0);
        assert_eq!(a0.a2, 0);
        assert_eq!(a0.a3, 0);

        let a2 = SyscallArgs::two(1, 2);
        assert_eq!(a2.a0, 1);
        assert_eq!(a2.a1, 2);

        let a3 = SyscallArgs::three(1, 2, 3);
        assert_eq!(a3.a0, 1);
        assert_eq!(a3.a1, 2);
        assert_eq!(a3.a2, 3);
        assert_eq!(a3.a3, 0);

        // 4 参构造走具名构造函数 four (new 需 6 参).
        let a4 = SyscallArgs::four(1, 2, 3, 4);
        assert_eq!(a4.a3, 4);
    }

    #[test]
    fn errno_from_i64_works() {
        assert_eq!(errno_from_i64(-2), Some(Errno::ENOENT));
        assert_eq!(errno_from_i64(-14), Some(Errno::EFAULT));
        assert_eq!(errno_from_i64(0), None);
        assert_eq!(errno_from_i64(42), None);
    }

    #[test]
    fn parse_return_works() {
        assert_eq!(parse_return(0), Ok(0));
        assert_eq!(parse_return(42), Ok(42));
        assert_eq!(parse_return(-2), Err(Errno::ENOENT));
    }

    #[test]
    fn user_ptr_check() {
        use crate::privileged::constants::limits::USER_ADDR_MAX;

        // 零指针应无效
        assert!(!check_user_ptr(0));
        // 合法用户地址应通过
        assert!(check_user_ptr(0x1000));
        // 上界之下 1 字节仍合法 (validate_user_ptr 为 ptr < USER_ADDR_MAX)
        assert!(check_user_ptr(USER_ADDR_MAX - 1));
        // 达到/超过用户态地址上界应失败
        // (原用例用 0x7FFFFFFFE000, 该值低于真实上界 0x7FFFFFFF_F000, 断言与实现不符;
        //  2026-09-24 UT-06 修正为直接以 USER_ADDR_MAX 为判据)
        assert!(!check_user_ptr(USER_ADDR_MAX));
    }

    #[test]
    fn user_buf_check() {
        use crate::privileged::constants::limits::USER_ADDR_MAX;

        // 零长度但 NULL 指针: validate_user_buf 要求 ptr 非 NULL, 故无效
        // (原断言认为 (0, 0) 合法, 与 B03-21 后语义不符; 2026-09-24 UT-06 修正)
        assert!(!check_user_buf(0, 0));
        // 零长度但非 NULL: 合法 (与 validate_user_buf 语义一致)
        assert!(check_user_buf(0x1000, 0));
        // 合法 buf
        assert!(check_user_buf(0x1000, 0x100));
        // ptr + len 越过用户态上界应失败
        assert!(!check_user_buf(USER_ADDR_MAX - 1, 0x1000));
    }
}
