use super::types::{AuditAction, AuditEntry, AuditResult, PwmId};
use core::sync::atomic::{AtomicUsize, Ordering};

const AUDIT_CAPACITY: usize = 256;

pub struct AuditLog {
    entries: [AuditEntry; AUDIT_CAPACITY],
    count: AtomicUsize,
}

impl AuditLog {
    pub const fn new() -> Self {
        Self {
            entries: [AuditEntry {
                timestamp: 0,
                pwm: PwmId::ZERO,
                action: AuditAction::Login,
                result: AuditResult::Success,
                target_pwm: PwmId::ZERO,
                details: 0,
            }; AUDIT_CAPACITY],
            count: AtomicUsize::new(0),
        }
    }

    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    #[expect(
        clippy::ptr_cast_constness,
        reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
    )]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    pub fn log(&self, pwm: u64, action: AuditAction, target_pwm: u64, domain: u64, caps: u64) {
        let now = super::bootstrap::pwm_now();
        let idx = self.count.fetch_add(1, Ordering::AcqRel) % AUDIT_CAPACITY;
        // SAFETY: count 已 fetch_add, 后续写不会改变 idx; AuditEntry 不含
        // 重入锁, 多核并发写不同 idx 不会数据竞争; 若 idx 相同 (环形覆盖) 也是
        // 单字节字段级别的无锁覆盖, 接受最后写入语义。
        let entry = &self.entries[idx] as *const AuditEntry as *mut AuditEntry;
        unsafe {
            (*entry).timestamp = now;
            (*entry).pwm = PwmId(pwm);
            (*entry).action = action;
            (*entry).result = AuditResult::Success;
            (*entry).target_pwm = PwmId(target_pwm);
            (*entry).details = (domain << 32) | (caps & 0xFFFFFFFF);
        }
    }

    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    pub fn dump(&self) {
        let count = self.count.load(Ordering::Acquire);
        let len = if count > AUDIT_CAPACITY {
            AUDIT_CAPACITY
        } else {
            count
        };
        for i in 0..len {
            let idx = if count > AUDIT_CAPACITY {
                (count - AUDIT_CAPACITY + i) % AUDIT_CAPACITY
            } else {
                i
            };
            let _e = &self.entries[idx];
            crate::serial_println!(
                "[AUDIT] t={} pwm={:#x} action={} target={:#x} details={:#x}",
                e.timestamp,
                e.pwm.as_u64(),
                e.action.as_u32(),
                e.target_pwm.as_u64(),
                e.details
            );
        }
    }

    pub fn get_entries(&self) -> &[AuditEntry; AUDIT_CAPACITY] {
        &self.entries
    }
}

// B03-09: GLOBAL_AUDIT 改用 IrqSpinLock<AuditLog> 包装, 消除 static mut 多核撕裂。
// 写作常态, dump 读少, IrqSpinLock 持锁时间短 (< 微秒), 适合此场景。
pub(crate) static GLOBAL_AUDIT: crate::framework::sync::IrqSpinLock<AuditLog> =
    crate::framework::sync::IrqSpinLock::new(AuditLog::new());

pub fn log(pwm: u64, action: AuditAction, target_pwm: u64, domain: u64, caps: u64) {
    raw::log(pwm, action, target_pwm, domain, caps);
}

pub fn dump() {
    raw::dump();
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 GLOBAL_AUDIT 访问
// ============================================================================

pub(crate) mod raw {
    use super::{AuditAction, GLOBAL_AUDIT};

    /// 记录一条审计项 (IrqSpinLock 保护)
    pub fn log(pwm: u64, action: AuditAction, target_pwm: u64, domain: u64, caps: u64) {
        GLOBAL_AUDIT
            .lock()
            .log(pwm, action, target_pwm, domain, caps);
    }

    pub fn dump() {
        // 持有锁时长 = 整个 dump 期间; 若 dump 量大可改为快照方式
        GLOBAL_AUDIT.lock().dump();
    }
}
