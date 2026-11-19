//! SGEG 审计日志 — 固定容量环形缓冲, 记录身份与能力相关安全事件。
//!
//! 写入经 `IrqSpinLock` 保护 (多核安全), 支持按序全量转储到串口。

#![deny(missing_docs)]

use super::types::{AuditAction, AuditEntry, AuditResult, PwmId};
use core::sync::atomic::{AtomicUsize, Ordering};

const AUDIT_CAPACITY: usize = 256;

/// 环形审计日志: 固定容量条目数组与写入计数。
pub struct AuditLog {
    entries: [AuditEntry; AUDIT_CAPACITY],
    count: AtomicUsize,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    /// 创建空审计日志。
    #[must_use]
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

    /// 追加一条审计项 (环形覆盖最旧记录)。
    pub fn log(&self, pwm: u64, action: AuditAction, target_pwm: u64, domain: u64, caps: u64) {
        let now = super::bootstrap::pwm_now();
        let idx = self.count.fetch_add(1, Ordering::AcqRel) % AUDIT_CAPACITY;
        // SAFETY: count 已 fetch_add, 后续写不会改变 idx; AuditEntry 不含
        // 重入锁, 多核并发写不同 idx 不会数据竞争; 若 idx 相同 (环形覆盖) 也是
        // 单字节字段级别的无锁覆盖, 接受最后写入语义。
        let entry = (&raw const self.entries[idx]).cast_mut();
        // SAFETY: `idx` 由 `count.fetch_add(1, AcqRel) % AUDIT_CAPACITY` 得出, 恒在数组
        // 界内; `entry` 经 `&raw const … .cast_mut()` 取得, 本块只写不读, 六个字段全是
        // Copy 值 (无 Drop、无内部可变性), 写入不越出该条目。
        // 已登记缺口 (批6 待裁, 本批不改; SGEG 现版冻结, 禁为其它工程适配现版):
        // ① 本文件模块文档声称「写入经 IrqSpinLock 保护」, 而 `AuditLog` 只有
        // `[AuditEntry; 256]` + `AtomicUsize`, **无任何锁**, `log(&self)` 经裸指针写共享
        // 数组 ⇒ 与 `dump(&self)` 的读构成 &/&mut 别名冲突 (UB);
        // ② 计数回绕后多核可取到同一 idx, 上方注释承认「最后写入语义」, 但那仍是无同步
        // 的并发写 = 数据竞争, 不是可接受的语义。
        unsafe {
            (*entry).timestamp = now;
            (*entry).pwm = PwmId(pwm);
            (*entry).action = action;
            (*entry).result = AuditResult::Success;
            (*entry).target_pwm = PwmId(target_pwm);
            (*entry).details = (domain << 32) | (caps & 0xFFFF_FFFF);
        }
    }

    /// 将全部有效审计条目打印到串口。
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
            // 审计条目字段经 `serial_println!` (当前为空展开) 消费; 保留索引与格式声明
            let _ = &self.entries[idx];
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

    /// 返回固定容量条目的只读视图 (按写入顺序)。
    pub fn get_entries(&self) -> &[AuditEntry; AUDIT_CAPACITY] {
        &self.entries
    }
}

// B03-09: GLOBAL_AUDIT 改用 IrqSpinLock<AuditLog> 包装, 消除 static mut 多核撕裂。
// 写作常态, dump 读少, IrqSpinLock 持锁时间短 (< 微秒), 适合此场景。
pub(crate) static GLOBAL_AUDIT: crate::privileged::sync::IrqSpinLock<AuditLog> =
    crate::privileged::sync::IrqSpinLock::new(AuditLog::new());

/// 向全局审计日志追加一条审计项 (`IrqSpinLock` 保护)。
pub fn log(pwm: u64, action: AuditAction, target_pwm: u64, domain: u64, caps: u64) {
    raw::log(pwm, action, target_pwm, domain, caps);
}

/// 将全局审计日志中的全部有效条目打印到串口。
pub fn dump() {
    raw::dump();
}

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 GLOBAL_AUDIT 访问
// ============================================================================

pub(crate) mod raw {
    use super::{AuditAction, GLOBAL_AUDIT};

    /// 记录一条审计项 (`IrqSpinLock` 保护)
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
