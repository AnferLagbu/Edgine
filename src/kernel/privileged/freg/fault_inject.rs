#[cfg(feature = "fault_injection")]
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "fault_injection")]
pub static FAULT_INJECTION_RATE: AtomicU32 = AtomicU32::new(0);

/// 按注入率随机触发恢复域 panic, 用于故障注入测试。
#[cfg(feature = "fault_injection")]
pub fn maybe_inject_fault(domain_id: u64) {
    let rate = FAULT_INJECTION_RATE.load(Ordering::Relaxed);
    if rate > 0 {
        let r = crate::arch!(timestamp()) as u32;
        if r % 1000 < rate {
            panic!(
                "[FAULT-INJECT] Domain {} forced panic (rate={}/1000)",
                domain_id, rate
            );
        }
    }
}

/// 未启用故障注入特性时的空实现 (始终不注入)。
#[cfg(not(feature = "fault_injection"))]
#[inline(always)]
pub fn maybe_inject_fault(_domain_id: u64) {}
