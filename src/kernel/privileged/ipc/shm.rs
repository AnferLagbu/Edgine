//! 共享内存 (SHM) FFI 边界 — T6-1 策略已迁移至 functions/ipc/shm.rs
//!
//! 本模块仅保留 FFI 函数 (用户空间指针转换, 委托 functions 策略).
//!
//! ## SAFETY
//!
//! - FFI 函数通过 `RacyCell::get_mut()` 安全访问全局 IPC_NAMESPACE.
//! - 用户空间指针通过 `UserRefMut` 安全访问.

use super::types::IpcId;
use crate::privileged::errno::Errno;
use crate::privileged::ipc::strategy::current_ipc_strategy;
use crate::privileged::proc::process_get_current_pid;
use crate::privileged::userptr::UserRefMut;

// ============================================================================
// FFI 导出函数
// ============================================================================

/// FFI: 创建共享内存段
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ipc_shm_create(size: u64, perm: i32) -> IpcId {
    let ns = super::IPC_NAMESPACE.get_mut();
    let next_id = super::NEXT_IPC_ID.get_mut();
    let pid = process_get_current_pid();
    // DECISION-K: 未注册降级 (返回 0=无效 id, 逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_shm_create");
        return 0;
    };
    s.shm_create(ns, next_id, size, perm, pid).unwrap_or(0)
}

/// FFI: 附加共享内存段。
///
/// # Safety
/// `addr` 必须是有效可写指针, 用于返回映射的虚拟地址。
/// 由 `sys_shmat` 分发, cred 校验已通过。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipc_shm_attach(id: IpcId, addr: *mut *mut u8) -> i32 {
    let ns = super::IPC_NAMESPACE.get_mut();
    let pid = process_get_current_pid();
    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_shm_attach");
        return -(Errno::ENOSYS as i32);
    };
    s.shm_attach(ns, id, pid).map_or(-1, |phys_addr| {
        if !addr.is_null() {
            // SAFETY: caller guarantees addr is a valid pointer to
            // a *mut u8 in user memory.
            let mut out = unsafe { UserRefMut::<*mut u8>::new(addr) };
            *out.as_mut() = phys_addr as *mut u8;
        }
        0
    })
}

/// FFI: 分离共享内存段
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ipc_shm_detach(id: IpcId) -> i32 {
    let ns = super::IPC_NAMESPACE.get_mut();
    let pid = process_get_current_pid();
    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_shm_detach");
        return -(Errno::ENOSYS as i32);
    };
    s.shm_detach(ns, id, pid).map_or(-1, |()| 0)
}

/// FFI: 销毁共享内存段
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn ipc_shm_destroy(id: IpcId) -> i32 {
    let ns = super::IPC_NAMESPACE.get_mut();
    // DECISION-K: 未注册降级 ENOSYS (逻辑错误降级原则, 不 panic)
    let Some(s) = current_ipc_strategy() else {
        crate::klog_warn!(Kernel, "IpcStrategy 未注册: ipc_shm_destroy");
        return -(Errno::ENOSYS as i32);
    };
    match s.shm_destroy(ns, id) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
