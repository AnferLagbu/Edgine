//! 全局 tick 查询接口 — 解耦 freg 与 timer::tick
//!
//! freg 在故障恢复时需要读取当前 tick, 但不应直接访问 timer 内部. 本模块提供
//! 全局函数指针注册机制: 启动编排注册回调, freg 通过本模块调用.
//!
//! # 安全契约
//!
//! - 注册的回调必须在中断上下文安全 (不睡眠)
//! - 注册的回调返回当前全局 tick 计数 (单一时基, 即 `timer::get_ticks`)

use core::sync::atomic::{AtomicPtr, Ordering};

/// tick 查询回调类型: `fn() -> u64`
type TickFn = fn() -> u64;

/// 全局回调函数指针. 初始为 null, 启动编排注册.
static TICK_FN: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// 注册 tick 查询回调. 由启动编排 (`install_builtin_tick_query`) 调用.
///
/// # Safety
///
/// 调用方必须确保 `func` 是有效的函数指针, 且在内核运行期间始终有效.
pub unsafe fn register_tick_query(func: TickFn) {
    TICK_FN.store(func as *mut (), Ordering::Release);
}

/// 安装内核内置 tick 查询回调 (`timer::get_ticks`).
///
/// 机制侧封装 safe API: 编排入口 (`lib.rs::kernel_init`) 在 scheduler 初始化之后
/// 直接调本函数, 编排层保持 0 unsafe (AGENTS.md §4.1 “privileged 留机制原语 +
/// 封装 safe API”). 此前该注册只存在于 `sched_ops::scheduler_init` 这一无生产
/// 调用者的 FFI 孤儿入口, 导致 `current_tick()` 恒返 0。登记见
/// docs/plan/net-e2e-tcp-echo-flakiness.md §6 P-1 / P-8（本注册已接线）.
///
/// P-1 收敛后回调源不再指向 `proc::scheduler` 自存的 tick 副本, 而是直接指向
/// 单一时基的权威读数 (两个视图已合一, 不存在"调度器眼中的时间").
pub fn install_builtin_tick_query() {
    // SAFETY: `timer::get_ticks` 是 'static 自由函数 (无捕获、不依赖可释放状态),
    // 只读全局时基原子量 ⇒ 中断上下文安全, 满足本模块安全契约。
    unsafe { register_tick_query(crate::privileged::timer::get_ticks) };
}

/// 获取当前全局 tick 计数 (单一时基读数).
///
/// 若启动编排尚未注册回调, 返回 0.
pub fn current_tick() -> u64 {
    let ptr = TICK_FN.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: ptr 由 register_tick_query 注册, 是有效的 TickFn 函数指针.
        let func: TickFn = unsafe { core::mem::transmute(ptr) };
        return func();
    }
    0
}
