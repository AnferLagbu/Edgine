//! # 栏软重置 (BSR — FREG Soft Reset)
//!
//! Layer 2 恢复：回滚到初始栏并重置设备

use core::sync::atomic::Ordering;

use super::audit;
use super::config::{self, RecoveryLayer, RecoveryResult};
use crate::privileged::freg::DomainState;
use crate::privileged::freg::PANIC_FLAG;
use crate::privileged::freg::RECOVERY_MANAGER;
use crate::privileged::freg::RecoveryManager;

/// 冻结所有恢复域, 使其在软重置期间不接受新操作。
///
/// P-30 止血: 改为接受调用方已持有的 `&RecoveryManager` (不再自行 `lock()`),
/// 使 `execute()` 能单次持锁原子跑完整 freeze/rollback/unfreeze 序列, 不再逐步
/// 重复加锁 (消除单核故障经恢复风暴逐步加锁放大成持锁窗口膨胀).
pub fn freeze_all_domains(manager: &RecoveryManager) {
    let count = manager.count.load(Ordering::SeqCst) as usize;
    for i in 0..count {
        if let Some(domain) = &manager.domains[i] {
            domain.set_state(DomainState::Freezing, Ordering::SeqCst);
        }
    }
}

/// 解冻所有恢复域: 恢复为 Active 并清零连续失败计数。
pub fn unfreeze_all_domains(manager: &RecoveryManager) {
    let count = manager.count.load(Ordering::SeqCst) as usize;
    for i in 0..count {
        if let Some(domain) = &manager.domains[i] {
            domain.set_state(DomainState::Active, Ordering::SeqCst);
            domain.consecutive_failures.store(0, Ordering::SeqCst);
        }
    }
}

/// 回滚所有恢复域到初始状态, 返回累计回滚的撤销条目数。
pub fn rollback_to_init(manager: &RecoveryManager) -> usize {
    let mut total_rolled = 0usize;
    let count = manager.count.load(Ordering::SeqCst) as usize;
    for i in 0..count {
        if let Some(domain) = &manager.domains[i] {
            let mut undo = domain.undo.lock();
            let rolled = undo.rollback_to(0);
            total_rolled += rolled;
            drop(undo);
            domain.freg_generation.store(1, Ordering::SeqCst);
        }
    }
    total_rolled
}

/// 从快照恢复全部设备寄存器, 失败时返回 Escalate。
pub fn reset_devices() -> RecoveryResult {
    use crate::privileged::freg::snapshot;

    fn mmio_write32(base: u64, offset: u32, value: u32) {
        raw::mmio_write32(base, offset, value);
    }

    let (success, failed) = snapshot::snapshot_restore_all(mmio_write32);

    if failed == 0 || success > 0 {
        RecoveryResult::Success
    } else {
        RecoveryResult::Escalate
    }
}

/// 关闭 CPU 中断 (软重置期间隔离外部事件)。
pub fn reset_interrupts() {
    #[cfg(not(feature = "kernel_test"))]
    {
        let _ = crate::arch!(interrupt_disable());
    }
}

/// 清除 panic 标志与重置进行中状态, 结束软重置流程。
pub fn clear_panic_state() {
    PANIC_FLAG.store(false, Ordering::SeqCst);
    config::set_reset_in_progress(false);
}

/// 执行 Layer2 软重置: 冻结域/回滚到初始态/恢复设备, 失败则升级。
pub fn execute() -> RecoveryResult {
    if config::is_reset_in_progress() {
        return RecoveryResult::Escalate;
    }

    config::set_reset_in_progress(true);
    config::set_current_layer(RecoveryLayer::Layer2);
    config::increment_bsr_count();

    crate::klog_crit!(Kernel, "[BSR] FREG Soft Reset initiated");

    // P-30 止血: 单次持锁跑完整序列 (freeze/rollback/unfreeze 现接受 &RecoveryManager),
    // 不再逐步重复加锁.
    let manager = RECOVERY_MANAGER.lock();
    freeze_all_domains(&manager);

    let rolled = rollback_to_init(&manager);
    crate::klog_info!(Kernel, "[BSR] Rolled back {} undo entries", rolled);

    let device_result = reset_devices();
    if device_result.should_escalate() {
        crate::klog_err!(Kernel, "[BSR] Device reset failed, escalating to BHR");
        audit::audit_record(RecoveryLayer::Layer2, RecoveryResult::Escalate, 1);
        return RecoveryResult::Escalate;
    }

    reset_interrupts();
    unfreeze_all_domains(&manager);
    clear_panic_state();

    audit::audit_record(RecoveryLayer::Layer2, RecoveryResult::Success, 0);
    crate::klog_crit!(Kernel, "[BSR] FREG Soft Reset completed successfully");

    RecoveryResult::Success
}

// UT-07 (2026-09-26): 原 `#[cfg(feature = "kernel_test")] pub mod tests { pub fn .. -> bool }`
// 形态改写为源侧 `#[cfg(test)] #[test]`; 注册侧 `privileged/tests/reset.rs::freg::bsr`
// 的薄包装 `check!(tests::test_freeze_unfreeze())` 随之删除.
#[cfg(test)]
mod tests {
    // J-01 (2026-09-08): wildcard_imports 清理 — 显式列出本模块使用的 super 符号
    // P-30 止血: 测试同样经 super 引入 RECOVERY_MANAGER (子模块不继承父 use)
    use super::{RECOVERY_MANAGER, freeze_all_domains, unfreeze_all_domains};

    #[test]
    fn freeze_unfreeze() {
        // 全域冻结/解冻为纯簿记 (遍历 RECOVERY_MANAGER 域状态), 断言"可调用不 panic"
        let manager = RECOVERY_MANAGER.lock();
        freeze_all_domains(&manager);
        unfreeze_all_domains(&manager);
    }
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 MMIO 写操作
// ============================================================================
//
// BSR (FREG Soft Reset) 通过 MMIO 写恢复设备寄存器, 这是
// 硬件 I/O 的本质需求。本子模块集中 `write_volatile` 调用。

pub(crate) mod raw {
    /// MMIO 32位写
    ///
    /// # SAFETY
    /// 调用方必须确保 `base + offset` 指向有效的设备寄存器地址。
    pub fn mmio_write32(base: u64, offset: u32, value: u32) {
        // SAFETY: 见函数契约。
        unsafe {
            core::ptr::write_volatile((base + u64::from(offset)) as *mut u32, value);
        }
    }
}
