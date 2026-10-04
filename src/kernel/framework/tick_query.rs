//! 全局 tick 查询接口 — 解耦 freg 与 proc::scheduler
//!
//! freg 在故障恢复时需要读取当前 tick, 但不应直接访问 proc::scheduler 内部.
//! 本模块提供全局函数指针注册机制: scheduler 初始化时注册回调, freg 通过此模块调用.
//!
//! # 安全契约
//!
//! - 注册的回调必须在中断上下文安全 (不睡眠)
//! - 注册的回调返回当前全局 tick 计数

use core::sync::atomic::{AtomicPtr, Ordering};

/// tick 查询回调类型: `fn() -> u64`
type TickFn = fn() -> u64;

/// 全局回调函数指针. 初始为 null, scheduler 初始化时注册.
static TICK_FN: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// 注册 tick 查询回调. 由 scheduler 子系统在初始化时调用.
///
/// # Safety
///
/// 调用方必须确保 `func` 是有效的函数指针, 且在内核运行期间始终有效.
pub unsafe fn register_tick_query(func: TickFn) {
    TICK_FN.store(func as *mut (), Ordering::Release);
}

/// 获取当前全局 tick 计数.
///
/// 若 scheduler 未注册回调, 返回 0.
pub fn current_tick() -> u64 {
    let ptr = TICK_FN.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: ptr 由 register_tick_query 注册, 是有效的 TickFn 函数指针.
        let func: TickFn = unsafe { core::mem::transmute(ptr) };
        return func();
    }
    0
}
