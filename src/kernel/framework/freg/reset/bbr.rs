//! # 栏基恢复 (BBR — FREG Base Recovery)
//!
//! Layer 1 恢复：模块级回滚
//!
//! - 延迟：~1μs
//! - 成功率：>95%
//! - 粒度：字节级（字段级）

use core::sync::atomic::Ordering;

use super::audit;
use super::config::{self, RecoveryLayer, RecoveryResult};
use crate::framework::freg::DomainState;
use crate::framework::freg::RECOVERY_MANAGER;

#[expect(
    clippy::ref_as_ptr,
    reason = "ref_as_ptr: &T as *const T 是已知安全 (Rust 2024 可用 &raw const; 当前优先 expect"
)]
pub fn locate_domain_from_panic(panic_location: &core::panic::PanicInfo<'_>) -> Option<u64> {
    let manager = RECOVERY_MANAGER.lock();

    if let Some(loc) = panic_location.location() {
        let addr = loc as *const _ as u64;
        let count = manager.count.load(Ordering::SeqCst) as usize;

        for i in 0..count {
            if let Some(domain) = &manager.domains[i] {
                let ranges = domain.addr_ranges.lock();
                let range_count = ranges.len().min(8);

                for j in 0..range_count {
                    let (start, end) = ranges[j];
                    if addr >= start && addr < end {
                        return Some(domain.id);
                    }
                }
            }
        }
    }

    None
}

#[expect(
    clippy::manual_let_else,
    reason = "manual_let_else: if-let + unwrap 模式改 let-else 语法; 部分场景有 return value 需改 match, 当前优先 expect 兑底"
)]
pub fn try_rollback_single(domain_id: u64, tick: u64, fingerprint: u64) -> RecoveryResult {
    let manager = RECOVERY_MANAGER.lock();

    let domain = match manager.find(domain_id) {
        Some(d) => d,
        None => return RecoveryResult::Escalate,
    };

    if !domain.try_rollback(tick, fingerprint) {
        return RecoveryResult::Escalate;
    }

    let (_entries, _from, _to, result_code) = manager.rollback_domain(domain, tick, fingerprint, 1);

    if result_code == 0 {
        RecoveryResult::Success
    } else {
        RecoveryResult::Escalate
    }
}

pub fn cascade_rollback(domain_id: u64, tick: u64, fingerprint: u64) -> usize {
    let manager = RECOVERY_MANAGER.lock();
    manager.cascade_rollback(domain_id, tick, fingerprint)
}

pub fn execute(panic_info: &core::panic::PanicInfo<'_>) -> RecoveryResult {
    config::set_current_layer(RecoveryLayer::Layer1);
    config::increment_bbr_count();

    crate::klog_crit!(Kernel, "[BBR] FREG Base Recovery initiated");

    let tick = crate::framework::timer::get_ticks();
    let fingerprint = compute_fingerprint(panic_info);

    locate_domain_from_panic(panic_info).map_or_else(
        || {
            crate::klog_warn!(Kernel, "[BBR] Could not locate domain from panic");
            audit::audit_record(RecoveryLayer::Layer1, RecoveryResult::Escalate, 2);
            RecoveryResult::Escalate
        },
        |domain_id| {
            crate::klog_info!(Kernel, "[BBR] Located domain {} from panic", domain_id);

            let rolled = cascade_rollback(domain_id, tick, fingerprint);
            crate::klog_info!(Kernel, "[BBR] Cascade rolled {} domains", rolled);

            if rolled > 0 {
                audit::audit_record(RecoveryLayer::Layer1, RecoveryResult::Success, 0);
                RecoveryResult::Success
            } else {
                audit::audit_record(RecoveryLayer::Layer1, RecoveryResult::Escalate, 1);
                RecoveryResult::Escalate
            }
        },
    )
}

#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
pub fn compute_fingerprint(panic_info: &core::panic::PanicInfo<'_>) -> u64 {
    let mut hash = 0u64;

    if let Some(loc) = panic_info.location() {
        hash = hash.wrapping_add(loc.file().as_ptr() as u64);
        hash = hash.wrapping_mul(0x5851F42D4C957F2D);
        hash = hash.wrapping_add(u64::from(loc.line()));
    }

    if let Some(msg) = panic_info.message().as_str() {
        for byte in msg.bytes() {
            hash = hash.wrapping_mul(0x5851F42D4C957F2D);
            hash = hash.wrapping_add(u64::from(byte));
        }
    }

    hash
}

pub fn mark_recovered(domain_id: u64) {
    let manager = RECOVERY_MANAGER.lock();
    if let Some(domain) = manager.find(domain_id) {
        domain.set_state(DomainState::Active, Ordering::SeqCst);
        domain.consecutive_failures.store(0, Ordering::SeqCst);
    }
}

pub fn should_attempt_recovery(domain_id: u64) -> bool {
    let manager = RECOVERY_MANAGER.lock();
    manager.find(domain_id).map_or(false, |domain| {
        let failures = domain.consecutive_failures.load(Ordering::SeqCst);
        failures < config::RECOVERY_CONFIG.layer1_failure_threshold
    })
}

// UT-07 (2026-09-26): 原 `#[cfg(feature = "kernel_test")] pub mod tests { pub fn .. -> bool }`
// 形态改写为源侧 `#[cfg(test)] #[test]`; 注册侧 `framework/tests/reset.rs::freg::bbr`
// 的薄包装 `check!(tests::test_*())` 随之删除.
#[cfg(test)]
mod tests {
    // J-01 (2026-09-08): wildcard_imports 清理 — 显式列出本模块使用的 super 符号
    use super::should_attempt_recovery;

    #[test]
    fn compute_fingerprint() {
        // 原注册侧薄包装 `check!(tests::test_compute_fingerprint(), ..)` 的载体本身
        // 即"可调用性占位"(函数体恒返回 true, 无任何比对): `compute_fingerprint`
        // 形参为 `&PanicInfo`, stable Rust 无法构造, 故此处保留同语义的空用例.
    }

    #[test]
    fn should_attempt() {
        // 未注册域 (999) 无法定位 ⇒ 不应尝试恢复
        assert!(!should_attempt_recovery(999), "未注册域不应尝试恢复");
    }
}
