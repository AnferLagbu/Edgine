//! 资源限制查询接口 — 解耦 mm 与 proc::rlimit
//!
//! mm 在 mlock 时需要查询当前进程的 memlock 限制, 但 mm 不应直接依赖 proc 子系统.
//! 本模块提供全局函数指针注册机制: proc 初始化时注册回调, mm 通过此模块调用.
//!
//! # 安全契约
//!
//! - 注册的回调必须在中断上下文安全 (不睡眠)
//! - 注册的回调返回当前进程的 memlock 限制字节数

use core::sync::atomic::{AtomicPtr, Ordering};

/// memlock 限制查询回调类型: `fn() -> u64`
type MemlockLimitFn = fn() -> u64;

/// 全局回调函数指针. 初始为 null, proc 初始化时注册.
static MEMLOCK_LIMIT_FN: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// 注册 memlock 限制查询回调. 由 proc 子系统在初始化时调用.
///
/// # Safety
///
/// 调用方必须确保 `func` 是有效的函数指针, 且在内核运行期间始终有效.
pub unsafe fn register_memlock_limit(func: MemlockLimitFn) {
    MEMLOCK_LIMIT_FN.store(func as *mut (), Ordering::Release);
}

/// 获取当前进程的 memlock 限制字节数.
///
/// 若 proc 未注册回调, 返回默认值 64KB.
pub fn get_memlock_limit() -> u64 {
    let ptr = MEMLOCK_LIMIT_FN.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: ptr 由 register_memlock_limit 注册, 是有效的 MemlockLimitFn 函数指针.
        let func: MemlockLimitFn = unsafe { core::mem::transmute(ptr) };
        return func();
    }
    // 默认值: 64KB (POSIX 常见默认)
    64 * 1024
}

/// 检查 mlock 锁定字节数是否超 `RLIMIT_MEMLOCK`.
///
/// 返回 true 表示超额, mlock 应失败.
pub fn check_memlock_exceeded(current_locked: u64, additional_bytes: u64) -> bool {
    let limit = get_memlock_limit();
    if limit == u64::MAX {
        // RLIM_INFINITY
        return false;
    }
    current_locked.saturating_add(additional_bytes) > limit
}
