//! fd 关闭通知接口 — 解耦 fs 与 syscall::epoll
//!
//! fs 在 fd 关闭时需要通知 epoll 等待者, 但 fs 不应直接依赖 syscall 子系统.
//! 本模块提供全局函数指针注册机制: epoll 初始化时注册回调, fs 通过此模块调用.
//!
//! # 安全契约
//!
//! - 注册的回调必须在中断上下文安全 (不睡眠)
//! - 注册的回调不可持有 fd_table 锁 (避免锁顺序倒置)

use core::sync::atomic::{AtomicPtr, Ordering};

/// fd 关闭通知回调类型: `fn(fd: i32)`
type PwakeFn = fn(i32);

/// 全局回调函数指针. 初始为 null, epoll 初始化时注册.
static PWAKE_FN: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// 注册 fd 关闭通知回调. 由 epoll 子系统在初始化时调用.
///
/// # Safety
///
/// 调用方必须确保 `func` 是有效的函数指针, 且在内核运行期间始终有效.
pub unsafe fn register_pwake(func: PwakeFn) {
    PWAKE_FN.store(func as *mut (), Ordering::Release);
}

/// 通知 fd 关闭事件. 由 fs 在 fd 关闭时调用.
///
/// 若 epoll 未注册回调, 此函数为空操作.
pub fn notify_fd_close(fd: i32) {
    let ptr = PWAKE_FN.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: ptr 由 register_pwake 注册, 是有效的 PwakeFn 函数指针.
        let func: PwakeFn = unsafe { core::mem::transmute(ptr) };
        func(fd);
    }
}
