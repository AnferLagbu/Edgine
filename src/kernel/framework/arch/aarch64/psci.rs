//! PSCI (Power State Coordination Interface) — 电源状态协调接口
//!
//! ARM 电源管理标准接口，通过 SMC/HVC 调用实现关机/重启/次核上电。
//! 调用通道 (conduit) 由设备树 `/psci` 节点的 `method` 属性决定 (见 [`conduit`]);
//! QEMU virt 使用 HVC (PSCI v0.2+), 多数真机使用 SMC。
//! PSCI_VERSION 返回 (major<<16 | minor<<8 | patch), 取高位/低位都是 u32 已知安全.

use crate::framework::dtb::{self, PsciMethod};

/// PSCI 函数 ID (SMC64)
const PSCI_SYSTEM_OFF: u32 = 0x84000008;
const PSCI_SYSTEM_RESET: u32 = 0x84000009;
const PSCI_VERSION: u32 = 0x84000000;

/// `CPU_ON` 的 SMC64 函数 ID (ARM PSCI v0.2+)
///
/// 入参: x1 = target_cpu (MPIDR), x2 = entry_point (物理地址), x3 = context_id.
pub const PSCI_CPU_ON: u32 = 0xC400_0003;

/// 当前平台的 PSCI conduit; 未探测到时回退 SMC (多数真机固件实现于 EL3).
///
/// SIMPLIFIED: 无设备树 `/psci.method` 时按 SMC 回退, 覆盖多数真机与固件约定;
///   影响面: 仅在既无设备树又使用 HVC conduit 的平台上误选 SMC;
///   何时需扩展: 引入启动参数或编译期特性显式指定 conduit 时.
fn conduit() -> PsciMethod {
    dtb::psci_method().unwrap_or(PsciMethod::Smc)
}

/// 按探测到的 conduit 执行 PSCI 调用: x0 = function_id, x1..x3 = 参数, 返回 x0.
///
/// 未使用的参数一律传 0 (PSCI 函数忽略无关寄存器).
///
/// # Safety
///
/// 仅在固件按 [`conduit`] 提供 PSCI 服务时调用; 调用方须保证 `x1..x3` 对目标
/// PSCI 函数合法 (如 `CPU_ON` 的 x2 须为次核入口的物理地址).
unsafe fn invoke(func: u64, x1: u64, x2: u64, x3: u64) -> i64 {
    let ret: i64;
    match conduit() {
        // SAFETY: 探测到 HVC conduit, 固件经 HVC 陷阱提供 PSCI 服务.
        PsciMethod::Hvc => unsafe {
            core::arch::asm!(
                "hvc #0",
                in("x0") func,
                in("x1") x1,
                in("x2") x2,
                in("x3") x3,
                lateout("x0") ret,
                options(nostack),
            );
        },
        // SAFETY: 探测到 SMC conduit, 固件经 SMC 陷阱提供 PSCI 服务.
        PsciMethod::Smc => unsafe {
            core::arch::asm!(
                "smc #0",
                in("x0") func,
                in("x1") x1,
                in("x2") x2,
                in("x3") x3,
                lateout("x0") ret,
                options(nostack),
            );
        },
    }
    ret
}

/// PSCI 返回码 (0 = SUCCESS 由 [`cpu_on`] 内部转为 `Ok(())`)
///
/// 变体与 ARM PSCI v1.x 规范的负错误码一一对应.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsciError {
    /// -1: 该函数不受支持
    NotSupported,
    /// -2: 参数非法 (如目标核 MPIDR 无效)
    InvalidParameters,
    /// -3: 调用被固件拒绝
    Denied,
    /// -4: 目标核已上电
    AlreadyOn,
    /// -5: 目标核正在上电中
    OnPending,
    /// -6: 固件内部失败
    InternalFailure,
    /// -7: 目标核不存在
    NotPresent,
    /// -8: 目标核被禁用
    Disabled,
    /// 其他未列出的返回码
    Other(i64),
}

impl PsciError {
    /// 返回码 → 错误变体; 0 (SUCCESS) 返回 `None`
    fn from_i64(code: i64) -> Option<Self> {
        match code {
            0 => None,
            -1 => Some(Self::NotSupported),
            -2 => Some(Self::InvalidParameters),
            -3 => Some(Self::Denied),
            -4 => Some(Self::AlreadyOn),
            -5 => Some(Self::OnPending),
            -6 => Some(Self::InternalFailure),
            -7 => Some(Self::NotPresent),
            -8 => Some(Self::Disabled),
            other => Some(Self::Other(other)),
        }
    }
}

/// 上电 `mpidr` 指定的 CPU 至物理地址 `entry_pa`, `context_id` 透传至入口 x0
///
/// 前置校验 PSCI 版本; 固件不支持时返回 [`PsciError::NotSupported`].
pub fn cpu_on(mpidr: u64, entry_pa: u64, context_id: u64) -> Result<(), PsciError> {
    if psci_version().is_none() {
        return Err(PsciError::NotSupported);
    }
    // SAFETY: 上游校验 PSCI 可用; entry_pa 为次核低半区 stub 的物理地址 (见 SMP-03),
    // 调用期间该地址恒等映射有效, 不会因 MMU 状态变化而失效.
    let code = unsafe { invoke(u64::from(PSCI_CPU_ON), mpidr, entry_pa, context_id) };
    match PsciError::from_i64(code) {
        None => Ok(()),
        Some(err) => Err(err),
    }
}

/// 检查 PSCI 版本。返回 (major, minor) 或 None。
fn psci_version() -> Option<(u32, u32)> {
    // SAFETY: PSCI_VERSION 为无入参函数; conduit 由设备树探测, 固件保证可调用.
    let ver = unsafe { invoke(u64::from(PSCI_VERSION), 0, 0, 0) } as u64;
    if ver == u64::from(u32::MAX) {
        // PSCI 不可用 (固件返回 u32::MAX)
        None
    } else {
        Some(((ver >> 16) as u32, (ver & 0xFFFF) as u32))
    }
}

/// PSCI 关机 — 不会返回
pub fn system_off() -> ! {
    // 尝试 PSCI
    match psci_version() {
        Some((_major, _minor)) => {
            // SAFETY: PSCI 可用 (上一层已校验); SYSTEM_OFF 无额外入参.
            unsafe { invoke(u64::from(PSCI_SYSTEM_OFF), 0, 0, 0) };
        }
        None => {}
    }

    // PSCI 不可用时，触发异常 (通过写入零地址)
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mov x0, #0; str x0, [x0]", options(nostack));
    }
    loop {}
}

/// PSCI 重启 — 不会返回
pub fn system_reset() -> ! {
    match psci_version() {
        Some((_major, _minor)) => {
            // SAFETY: PSCI 可用 (上一层已校验); SYSTEM_RESET 无额外入参.
            unsafe { invoke(u64::from(PSCI_SYSTEM_RESET), 0, 0, 0) };
        }
        None => {}
    }

    // PSCI 不可用时，fallback
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mov x0, #0; str x0, [x0]", options(nostack));
    }
    loop {}
}
