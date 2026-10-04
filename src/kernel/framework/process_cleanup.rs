//! 进程退出清理回调接口 — 解耦 proc 与 egdf
//!
//! proc 在进程退出时需要通知 egdf 清理用户驱动资源,
//! 但不应直接依赖 egdf::user_driver.
//! 本模块提供全局函数指针注册机制: egdf 初始化时注册回调, proc 通过此模块调用.
//!
//! # 安全契约
//!
//! - 注册的回调必须在中断上下文安全 (不睡眠)
//! - 注册的回调接收 pid 参数

use core::sync::atomic::{AtomicPtr, Ordering};

/// 进程退出清理回调类型: `fn(pid: u32)`
type ProcessCleanupFn = fn(u32);

/// 全局回调函数指针. 初始为 null, egdf 初始化时注册.
static PROCESS_CLEANUP_FN: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// 注册进程退出清理回调. 由 egdf 子系统在初始化时调用.
///
/// # Safety
///
/// 调用方必须确保 `func` 是有效的函数指针, 且在内核运行期间始终有效.
pub unsafe fn register_process_cleanup(func: ProcessCleanupFn) {
    PROCESS_CLEANUP_FN.store(func as *mut (), Ordering::Release);
}

/// 通知进程退出事件. 由 proc 在进程退出时调用.
///
/// 若 egdf 未注册回调, 则静默跳过.
pub fn notify_process_exit(pid: u32) {
    // B03-08 返工: 进程退出时强制释放该进程持有的所有 PI Mutex,
    // 防止"持锁进程退出 → 锁永久不释放 → 后续获取者死锁" (TOP 20 #6).
    crate::framework::sync::pi_mutex::pi_mutex_process_exit(pid);

    let ptr = PROCESS_CLEANUP_FN.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: ptr 由 register_process_cleanup 注册, 是有效的 ProcessCleanupFn 函数指针.
        let func: ProcessCleanupFn = unsafe { core::mem::transmute(ptr) };
        func(pid);
    }
}
