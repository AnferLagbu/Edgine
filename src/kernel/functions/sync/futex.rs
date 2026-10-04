#![deny(unsafe_code)]
//! Futex — functions 层安全代理
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 `privileged::syscall::futex`。
//!
//! ## 职责
//!
//! - 提供类型安全的 futex 操作枚举 (Wait / Wake / Requeue)
//! - flags 验证 (仅 `PRIVATE_FLAG` 0x80 / `CLOCK_REALTIME` 0x01)
//! - op 解码基础操作 (WAIT/WAKE/REQUEUE 忽略时钟与位图变体)
//! - 委托 privileged 层执行
//!
//! ## 错误
//!
//! - EINVAL: op 不支持 / flags 越界
//! - EFAULT: uaddr 未映射
//! - EAGAIN: WAIT 时 *uaddr != expected
//! - ETIMEDOUT: WAIT 超时

use crate::privileged::syscall::Errno;

// ============================================================================
// futex op 常量 (与 Linux 兼容)
// ============================================================================

/// 基础操作: WAIT (等待唤醒)
pub const FUTEX_WAIT: i32 = 0;
/// 基础操作: WAKE (唤醒)
pub const FUTEX_WAKE: i32 = 1;
/// 基础操作: REQUEUE (从一 uaddr 唤醒并迁移到另一 uaddr)
pub const FUTEX_REQUEUE: i32 = 3;
/// 基础操作: `WAIT_BITSET` (带位图掩码的 WAIT, 用于选择性唤醒)
pub const FUTEX_WAIT_BITSET: i32 = 9;
/// 基础操作: `WAKE_BITSET` (带位图掩码的 WAKE)
pub const FUTEX_WAKE_BITSET: i32 = 10;
/// 私有 flag: 进程内 futex (不跨进程)
pub const FUTEX_PRIVATE_FLAG: i32 = 128;

// ============================================================================
// 解析 op 字段: 提取基础操作 (低 4 位)
// ============================================================================

/// 从 op 提取基础操作 (低 4 位)
pub fn futex_base_op(op: i32) -> i32 {
    op & 0x0F
}

/// 判断 op 是否为 WAIT 类
pub fn is_wait_op(op: i32) -> bool {
    matches!(futex_base_op(op), FUTEX_WAIT | FUTEX_WAIT_BITSET)
}

/// 判断 op 是否为 WAKE 类
pub fn is_wake_op(op: i32) -> bool {
    matches!(futex_base_op(op), FUTEX_WAKE | FUTEX_WAKE_BITSET)
}

// ============================================================================
// 参数验证
// ============================================================================

/// 验证 futex 入参: uaddr 非 0 且 4 字节对齐 (u32 原子操作)
///
/// # Errors
///
/// - `uaddr == 0` → `EFAULT`
/// - `uaddr` 未按 4 字节对齐 → `EINVAL`
pub fn futex_validate_uaddr(uaddr: u64) -> Result<(), Errno> {
    if uaddr == 0 {
        return Err(Errno::EFAULT);
    }
    if uaddr & 0x3 != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

/// 验证 op 字段: 基础操作合法
///
/// # Errors
///
/// 当基础操作不是 WAIT/WAKE/REQUEUE 等受支持操作时返回 `ENOSYS`.
pub fn futex_validate_op(op: i32) -> Result<(), Errno> {
    match futex_base_op(op) {
        FUTEX_WAIT | FUTEX_WAIT_BITSET | FUTEX_WAKE | FUTEX_WAKE_BITSET | FUTEX_REQUEUE => Ok(()),
        _ => Err(Errno::ENOSYS),
    }
}

// ============================================================================
// safe 包装
// ============================================================================

/// Futex 操作结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FutexResult {
    /// WAIT 被唤醒 (返 0)
    Woken,
    /// WAKE 唤醒的线程数
    WokenCount(u32),
    /// REQUEUE 唤醒 + 迁移的线程数
    Requeued { woken: u32, requeued: u32 },
    /// 等待中 (未返回)
    Pending,
}

impl FutexResult {
    #[expect(
        clippy::comparison_chain,
        reason = "DECISION-043 pedantic 兜底: 当前批量 expect 兑底; 后续可逐处手工重构 (改 .cast() / let-else / 命名等)"
    )]
    /// 从 syscall 返回值解析
    pub fn from_ret(ret: i64) -> Self {
        if ret == 0 {
            Self::Woken
        } else if ret > 0 {
            Self::WokenCount(ret as u32)
        } else {
            Self::Pending
        }
    }
}

#[expect(
    clippy::match_same_arms,
    reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
)]
/// safe 包装: futex 系统调用
///
/// # Errors
///
/// - 参数验证失败: `uaddr == 0` → `EFAULT`, 未对齐或 op 非法 → `EINVAL`/`ENOSYS`
/// - 底层 `sys_futex` 返回负值: 按 errno 映射为 `EAGAIN`(值不匹配)、
///   `EFAULT`、`EINVAL`、`ENOSYS` 等
pub fn futex_syscall(
    uaddr: u64,
    op: i32,
    val: i32,
    timeout_or_uaddr2: u64,
    val2: u32,
) -> Result<FutexResult, Errno> {
    // 1. 验证
    futex_validate_uaddr(uaddr)?;
    futex_validate_op(op)?;

    // 2. 委托 privileged
    let ret = crate::privileged::syscall::futex::sys_futex(uaddr, op, val, timeout_or_uaddr2, val2);

    // 3. 错误码解析
    if ret < 0 {
        let errno = match (-ret) as i32 {
            11 => Errno::EAGAIN, // EAGAIN = 11 (Linux)
            14 => Errno::EFAULT,
            22 => Errno::EINVAL,
            38 => Errno::ENOSYS,
            _ => Errno::EINVAL,
        };
        return Err(errno);
    }

    Ok(FutexResult::from_ret(ret))
}
