//! Lockdep 安全代理 — functions 层
//!
//! 将 privileged::sync::lockdep 的 TCB 接口封装为 100% safe Rust API。
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! use crate::functions::sync::lockdep;
//!
//! // 注册锁类 (通常在 static 初始化时)
//! static MY_LOCK_CLASS: LockClassId = LockClassId::INVALID;
//!
//! fn init() {
//!     unsafe { MY_LOCK_CLASS = lockdep::register_class("my_lock", LockKind::Mutex); }
//! }
//!
//! // 在锁获取后通知 lockdep
//! lockdep::acquire(MY_LOCK_CLASS, false);
//!
//! // 在锁释放前通知 lockdep
//! lockdep::release(MY_LOCK_CLASS);
//! ```
//!
//! ## 安全契约
//!
//! - 本模块零 unsafe, 所有 unsafe 操作在 privileged::sync::lockdep 中完成
//! - `register_class` 接受 `&'static str`, 保证名称生命周期

#![deny(unsafe_code)]

// Re-export 类型
pub use crate::privileged::sync::{LockClassDesc, LockClassId, LockKind};

/// 注册锁类
///
/// 返回全局唯一的 `LockClassId`, 后续 acquire/release 使用此 ID。
/// 同名锁类只注册一次 (幂等)。
pub fn register_class(name: &'static str, kind: LockKind) -> LockClassId {
    crate::privileged::sync::register_class(LockClassDesc { name, kind })
}

/// 锁获取通知
///
/// 在锁成功获取后调用。检测:
/// 1. AB-BA 死锁 (锁序反转)
/// 2. 中断上下文获取睡眠锁
/// 3. 递归获取非递归锁
///
/// # 参数
/// - `class_id`: 锁类 ID
/// - `irq_context`: 是否在中断上下文中获取
///
/// # 返回
/// - `true`: 正常
/// - `false`: 检测到违规 (已打印警告)
pub fn acquire(class_id: LockClassId, irq_context: bool) -> bool {
    crate::privileged::sync::acquire(class_id, irq_context)
}

/// 锁释放通知
///
/// 在锁释放前调用。从持有栈中移除。
pub fn release(class_id: LockClassId) {
    crate::privileged::sync::release(class_id);
}

/// 标记进入中断上下文
pub fn irq_enter() {
    crate::privileged::sync::irq_enter();
}

/// 标记退出中断上下文
pub fn irq_exit() {
    crate::privileged::sync::irq_exit();
}

/// 当前是否在中断上下文
pub fn in_irq_context() -> bool {
    crate::privileged::sync::in_irq_context()
}

/// 查询当前持有锁深度
pub fn held_depth() -> usize {
    crate::privileged::sync::held_depth()
}

/// 查询已注册锁类数
pub fn num_classes() -> usize {
    crate::privileged::sync::num_classes()
}

/// 查询检测到的违规数
pub fn num_violations() -> u32 {
    crate::privileged::sync::num_violations()
}

/// 检查是否已检测到死锁
pub fn deadlock_detected() -> bool {
    crate::privileged::sync::deadlock_detected()
}

/// 打印当前锁依赖状态 (调试用)
pub fn dump_state() {
    crate::privileged::sync::dump_state();
}
