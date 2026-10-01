use alloc::boxed::Box;

use super::types::{CapBits, CapDomain, GrantRecord, MAX_GRANT_RECORDS, PwmError};
use crate::framework::sync::{IrqSpinLock, OnceLock};

/// Grant 记录表 — 惰性堆置, 避免 `[GrantRecord; 1024]` (约 40 KB) 常驻 BSS。
///
/// `OnceLock` 内联仅数十字节; 首次访问时在 kmalloc 堆上构造 `Box<[GrantRecord]>`。
/// 元素数组经 `(0..N).map()` 逐元素生成, 不产生大栈临时对象, 也不留 `.rodata` 模板。
static GRANT_RECORDS: OnceLock<IrqSpinLock<Box<[GrantRecord]>>> = OnceLock::new();

/// 获取 Grant 记录表 (首次调用时惰性分配)。
fn grant_records() -> &'static IrqSpinLock<Box<[GrantRecord]>> {
    GRANT_RECORDS.get_or_init(|slot| {
        slot.write(IrqSpinLock::new(
            (0..MAX_GRANT_RECORDS)
                .map(|_| GrantRecord::EMPTY)
                .collect::<Box<[_]>>(),
        ));
    })
}

/// 向授权记录表追加一条授权记录, 供后续授权校验使用。
/// # Errors
/// 授权记录表已满时返回 Err。
pub fn add_record(record: GrantRecord) -> Result<(), PwmError> {
    let mut guard = grant_records().lock();
    guard
        .iter_mut()
        .find(|r| r.is_empty())
        .map_or(Err(PwmError::TableFull), |slot| {
            *slot = record;
            Ok(())
        })
}

pub fn is_grantor(grantor_pwm: u64, grantee_pwm: u64, domain: CapDomain, caps: CapBits) -> bool {
    let guard = grant_records().lock();
    guard.iter().any(|r| {
        r.grantor_pwm.0 == grantor_pwm
            && r.grantee_pwm.0 == grantee_pwm
            && r.domain == domain
            && (r.caps & caps) == caps
    })
}

pub fn clear_records(revoker_pwm: u64, target_pwm: u64, domain: CapDomain, caps: CapBits) {
    let mut guard = grant_records().lock();
    for record in guard.iter_mut() {
        if record.grantor_pwm.0 == revoker_pwm
            && record.grantee_pwm.0 == target_pwm
            && record.domain == domain
        {
            record.caps = CapBits(record.caps.0 & !caps.0);
            if record.caps == CapBits::NONE {
                *record = GrantRecord::EMPTY;
            }
        }
    }
}
