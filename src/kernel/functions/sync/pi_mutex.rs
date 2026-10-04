#![deny(unsafe_code)]
//! Priority Inheritance Mutex — functions 层安全代理
//!
//! ## 职责
//!
//! - 0 unsafe, 强类型 RAII API
//! - 委托 `privileged::sync::pi_mutex` 完成所有底层操作
//! - 提供 `lock()` / `try_lock()` 强类型方法, 错误统一返回 [`PiMutexError`]
//!
//! ## 评估日期
//!
//! 2026-06-08

use crate::privileged::sync::pi_mutex as fw;
pub use fw::PiMutex;

// ============================================================================
// 错误
// ============================================================================

/// PI Mutex 操作错误 — TD-20: 收敛到 `KernelError`, 2 字段 PI 特有 + 1 共享包装.
///
/// 字段说明:
///   - `NotOwner`: 当前线程非持有者 (双重释放风险)
///   - `Exhausted`: 资源耗尽 (无空闲槽位)
///   - `Kernel(KernelError)`: 共享错误 (`WouldBlock`) 走单一来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiMutexError {
    /// 当前线程非持有者 (双重释放)
    NotOwner,
    /// 资源耗尽 (无空闲槽位)
    Exhausted,
    /// 共享 `KernelError` 包装
    Kernel(crate::functions::error::KernelError),
}

impl PiMutexError {
    /// 映射为 POSIX errno
    pub fn to_errno(self) -> crate::privileged::syscall::Errno {
        use crate::privileged::syscall::Errno as E;
        match self {
            Self::NotOwner => E::EPERM,
            Self::Exhausted => E::ENOMEM,
            Self::Kernel(e) => e.as_errno(),
        }
    }
}

pub type PiMutexResult<T> = Result<T, PiMutexError>;

// ============================================================================
// 安全 API
// ============================================================================

#[expect(
    clippy::unnecessary_wraps,
    reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
)]
/// 获取锁 (阻塞 + 优先级继承)
///
/// # 参数
/// - `my_pid`: 当前线程 PID
/// - `my_base_priority`: 当前线程的 `base_priority`
///
/// # 返回
/// RAII 守卫, drop 时自动释放
///
/// # Errors
///
/// 当前实现中 `lock()` 内部自旋直到获取成功, 不会返回 `Err`.
pub fn lock<T>(
    mutex: &PiMutex<T>,
    my_pid: u32,
    my_base_priority: u32,
) -> PiMutexResult<fw::PiMutexGuard<'_, T>> {
    // PI Mutex 的 lock 永不返回 WouldBlock, 内部自旋直到获取
    // 但若要区分错误, 可包装一层
    let _ = my_pid;
    let _ = my_base_priority;
    // 当前实现: lock() 不返回错误 (总会自旋直到成功)
    // 此处仅作类型适配, 实际不会有 Err
    Ok(mutex.lock(my_pid, my_base_priority))
}

/// 尝试获取锁 (非阻塞)
///
/// # Errors
///
/// 当锁被其他线程持有时返回 `PiMutexError::Kernel(WouldBlock)`.
pub fn try_lock<T>(
    mutex: &PiMutex<T>,
    my_pid: u32,
    my_base_priority: u32,
) -> PiMutexResult<fw::PiMutexGuard<'_, T>> {
    if mutex.try_lock(my_pid, my_base_priority) {
        Ok(mutex.lock(my_pid, my_base_priority))
    } else {
        Err(PiMutexError::Kernel(
            crate::functions::error::KernelError::WouldBlock,
        ))
    }
}

// 注: set_donation_callback / set_revoke_callback 是启动期 unsafe API,
// 保留在 privileged/sync/pi_mutex 层 (TCB), 不通过 functions 包装.
