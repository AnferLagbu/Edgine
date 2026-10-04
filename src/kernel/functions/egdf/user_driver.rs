#![deny(unsafe_code)]
//! 用户态驱动 (User Driver) — functions 层安全代理
//!
//! 封装 privileged `egdf::user_driver::*`, 向 functions 提供强类型 safe API:
//! - 设备绑定 — `bind` / `unbind`
//! - MMIO 映射 — `map` / `unmap`
//! - 中断转发 — `forward_irq`
//!
//! ## 迁移方法
//!
//! 1. privileged 自定义错误码 (`ERR_*` = 0/-1..-6) → [`UserDriverError`] 强类型
//! 2. 每个函数经 `map_err` 收敛错误码, 不向 functions 泄漏 privileged 内部错误类型
//! 3. [`UserDriverError::to_errno`] 统一映射 POSIX errno 供 syscall 层使用
//! 4. 0 unsafe 出现在 functions 层

use crate::privileged::egdf::NodeId;
use crate::privileged::egdf::user_driver as fw;
use crate::privileged::mm::MmStruct;
use crate::privileged::syscall::Errno;

// ============================================================================
// 错误
// ============================================================================

/// 用户态驱动操作错误 — 忠实映射 privileged `ERR_*` 自定义错误码.
///
/// privileged 侧 `user_driver` 使用 0 / -1..-6 私有码, 与 POSIX 语义不兼容
/// (如 `-1` 表示 `NotFound` 而非 `EPERM`), 故本层独立定义强类型枚举,
/// 并单独提供 [`UserDriverError::to_errno`] 转 POSIX。
///
/// 字段说明:
///   - `NotFound`: 进程或设备树节点不存在
///   - `NotAuthorized`: PWM 缺少所需设备能力
///   - `InvalidState`: 节点状态非法 / 已被占用 / 未绑定
///   - `NoMmio`: 节点缺少 reg 属性或地址 / 大小非法
///   - `PidMismatch`: 节点绑定的 PID 与请求 PID 不匹配
///   - `Oom`: 内存不足或 VMA 插入失败
///   - `Unknown(i32)`: 未识别的 privileged 错误码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserDriverError {
    /// 进程或设备树节点不存在
    NotFound,
    /// PWM 缺少所需设备能力
    NotAuthorized,
    /// 节点状态非法 / 已被占用 / 未绑定
    InvalidState,
    /// 节点缺少 reg 属性或地址 / 大小非法
    NoMmio,
    /// 节点绑定的 PID 与请求 PID 不匹配
    PidMismatch,
    /// 内存不足或 VMA 插入失败
    Oom,
    /// 未识别的 privileged 错误码
    Unknown(i32),
}

impl UserDriverError {
    /// 由 privileged 自定义错误码构造
    pub fn from_code(code: i32) -> Self {
        match code {
            fw::ERR_NOT_FOUND => Self::NotFound,
            fw::ERR_NOT_AUTHORIZED => Self::NotAuthorized,
            fw::ERR_INVALID_STATE => Self::InvalidState,
            fw::ERR_NO_MMIO => Self::NoMmio,
            fw::ERR_PID_MISMATCH => Self::PidMismatch,
            fw::ERR_OOM => Self::Oom,
            other => Self::Unknown(other),
        }
    }

    /// 映射为 POSIX errno
    pub fn to_errno(self) -> Errno {
        use Errno as E;
        match self {
            Self::NotFound => E::ENOENT,
            Self::NotAuthorized | Self::PidMismatch => E::EPERM,
            Self::InvalidState => E::EBUSY,
            Self::NoMmio | Self::Unknown(_) => E::EINVAL,
            Self::Oom => E::ENOMEM,
        }
    }
}

/// functions 层结果类型别名
pub type UserDriverResult<T> = Result<T, UserDriverError>;

// ============================================================================
// 设备绑定
// ============================================================================

/// 将设备树节点绑定到指定用户进程, 使该进程获得该设备的独占访问权。
///
/// # Errors
/// PWM 缺少 `DEVICE_CAP_BIND` 权限、进程不存在、节点不存在、节点状态非法或
/// 节点已被其他进程占用时返回 `Err`.
pub fn bind(node_id: NodeId, pid: u32, pwm: u64) -> UserDriverResult<()> {
    fw::devtree_bind_user_device(node_id, pid, pwm)
        .map_err(|e| UserDriverError::from_code(e.code()))
}

/// 解除设备树节点与用户进程的绑定, 并卸载进程地址空间中映射的设备 MMIO 范围。
///
/// # Errors
/// PWM 缺少 `DEVICE_CAP_BIND` 权限、进程不存在、节点不存在、节点映射的 PID 不匹配
/// 或节点未处于可解绑状态时返回 `Err`.
pub fn unbind(node_id: NodeId, pid: u32, pwm: u64, mm: &MmStruct) -> UserDriverResult<()> {
    fw::devtree_unbind_user_device(node_id, pid, pwm, mm)
        .map_err(|e| UserDriverError::from_code(e.code()))
}

// ============================================================================
// MMIO 映射
// ============================================================================

/// 将设备节点的 MMIO 资源映射到用户进程地址空间, 返回映射基址。
///
/// # Errors
/// PWM 缺少 `DEVICE_CAP_MMIO` 权限、进程或节点不存在、节点映射的 PID 不匹配、
/// 节点状态非法、缺少 reg 属性、物理地址或大小非法或内存不足时返回 `Err`.
pub fn map(node_id: NodeId, pid: u32, pwm: u64, mm: &MmStruct) -> UserDriverResult<usize> {
    fw::devtree_map_user_device(node_id, pid, pwm, mm)
        .map_err(|e| UserDriverError::from_code(e.code()))
}

/// 解除设备节点的 MMIO 映射, 将指定虚拟地址范围从用户进程地址空间移除。
///
/// # Errors
/// PWM 缺少 `DEVICE_CAP_MMIO` 权限、进程不存在、节点不存在或节点映射的 PID
/// 不匹配时返回 `Err`.
pub fn unmap(
    node_id: NodeId,
    pid: u32,
    pwm: u64,
    mm: &MmStruct,
    virt_addr: usize,
    size: usize,
) -> UserDriverResult<()> {
    fw::devtree_unmap_user_device(node_id, pid, pwm, mm, virt_addr, size)
        .map_err(|e| UserDriverError::from_code(e.code()))
}

// ============================================================================
// 中断转发
// ============================================================================

/// 向绑定到该节点的用户进程转发中断 (设置 `SIGUSR1`)。
///
/// 返回 `true` 表示已成功递送信号; `false` 表示节点未绑定用户进程、进程不存在
/// 或 PWM 缺少 `DEVICE_CAP_IRQ` 能力。
pub fn forward_irq(node_id: NodeId) -> bool {
    fw::egdf_forward_irq(node_id)
}
