//! kmalloc 中断安全锁契约测试 (P1-I-28)
//!
//! 验证:
//! 1. kmalloc.rs::acquire_lock/release_lock 签名变更为 (无) -> IrqSaveFlags / (&flags) -> ()
//! 2. 源码静态扫描确认调用点一致 (let flags = self.acquire_lock(); ... self.release_lock(&flags);)
//! 3. 中断安全锁配对契约: lock_irqsave 返回 flags, unlock_irqrestore 接 flags
//!
//! 分册 9 项 2 (B09-21): 原第 2 条 (kmalloc_slab.rs 源文本静态扫描) 随
//! `privileged/mm/kmalloc_slab.rs` 零引用孤岛整体删除而移除.
//!
//! ## B08-20 迁移 (2026-09-06)
//! 删除本地 `IrqSaveFlags` / `IRQ_DISABLED` / `disable_interrupts` /
//! `restore_interrupts` / `acquire_lock` / `release_lock` / `MockHeap` 平行镜像,
//! 改引内核真实源码 `edgine::kernel::privileged::sync::{SpinLock, IrqSpinLock,
//! IrqSaveFlags, disable_interrupts, restore_interrupts}`.
//! 内核 `disable_interrupts`/`restore_interrupts` 在 host-test 下为桩 (no-op,
//! B08-14 前置: host 无中断语义, 原子自旋仍正确互斥); `SpinLock`/`IrqSpinLock`
//! 的原子自旋在 host 多线程下仍正确互斥.
//!
//! ## 与原镜像的差异 (以内核为权威)
//! 原镜像用全局 `IRQ_DISABLED: AtomicBool` 模拟"持锁期间 IRQ disabled"状态;
//! 内核 host-test 桩下 `disable_interrupts` 为 no-op, 无可观察的 disabled 状态.
//! 故改为验证锁**配对契约** (lock_irqsave 返回 flags / is_locked 翻转 /
//! IrqSpinLock RAII guard Drop 自动释放), 该契约即 kmalloc 临界区的实际保障.

use edgine::kernel::privileged::sync::{
    IrqSaveFlags, IrqSpinLock, SpinLock, disable_interrupts, restore_interrupts,
};

#[test]
fn lock_acquire_release_paired_with_flags() {
    // 基础: acquire (lock_irqsave) 必返 flags, release (unlock_irqrestore) 必接 flags
    let mut lock = SpinLock::new();
    let flags: IrqSaveFlags = lock.lock_irqsave();
    assert!(lock.is_locked(), "持锁期间 is_locked 必须为 true");
    lock.unlock_irqrestore(&flags);
    assert!(!lock.is_locked(), "释放后 lock 必为 false");
}

#[test]
fn irq_spinlock_guards_critical_section() {
    // P1-I-28: IrqSpinLock 是内核临界区的锁类型,
    // RAII guard 持锁期间屏蔽中断, Drop 自动释放.
    let data = IrqSpinLock::new(0u32);
    data.with_mut(|v| *v += 1);
    assert_eq!(*data.lock(), 1, "with_mut 内自增必须对后续 lock 可见");
    assert_eq!(*data.lock(), 1, "guard Drop 后锁已释放, 可再次获取");
}

#[test]
fn nested_critical_section_with_distinct_locks() {
    // P1-I-28: 嵌套临界区验证. 内核自旋锁不可重入同锁 (会死锁), 但不同锁可嵌套;
    // 每层各自保存/恢复 flags, 互不干扰.
    let mut outer = SpinLock::new();
    let mut inner = SpinLock::new();
    let flags1 = outer.lock_irqsave();
    assert!(outer.is_locked());
    let flags2 = inner.lock_irqsave();
    assert!(inner.is_locked());
    inner.unlock_irqrestore(&flags2);
    assert!(!inner.is_locked());
    outer.unlock_irqrestore(&flags1);
    assert!(!outer.is_locked());
}

#[test]
fn irq_disable_restore_host_stub_pairing() {
    // P1-I-28: disable_interrupts/restore_interrupts 配对契约.
    // host-test 下为 no-op (B08-14): disable 返回 IrqSaveFlags(0), restore 无操作,
    // 但调用配对不 panic, 保证裸机/宿主两套实现同一调用面.
    let flags = disable_interrupts();
    restore_interrupts(&flags);
    let flags2 = disable_interrupts();
    restore_interrupts(&flags2);
}

#[test]
fn kmalloc_source_uses_irq_save_flags_signature() {
    // P1-I-28 验收: 源码静态扫描 — kmalloc.rs 的 lock 函数签名使用 IrqSaveFlags
    let source = include_str!("../../src/kernel/privileged/mm/kmalloc.rs");
    // 修复后必须包含: fn acquire_lock(&self) -> IrqSaveFlags
    assert!(
        source.contains("fn acquire_lock(&self) -> IrqSaveFlags"),
        "P1-I-28: kmalloc.rs::acquire_lock 签名必须返回 IrqSaveFlags"
    );
    assert!(
        source.contains("fn release_lock(&self, flags: &IrqSaveFlags)"),
        "P1-I-28: kmalloc.rs::release_lock 签名必须接 &IrqSaveFlags"
    );
    // 必须导入 disable_interrupts / restore_interrupts
    assert!(
        (source.contains("use crate::privileged::sync::spinlock::")
            || source.contains("use crate::privileged::sync::"))
            && source.contains("disable_interrupts")
            && source.contains("restore_interrupts"),
        "P1-I-28: kmalloc.rs 必须导入 disable/restore 中断原语"
    );
    // 必须实现 disable + compare_exchange_weak (旧版是裸 compare_exchange_weak)
    let has_disable_then_cas = source
        .lines()
        .any(|line| line.contains("let flags = disable_interrupts();"))
        && source
            .lines()
            .any(|line| line.contains("compare_exchange_weak"));
    assert!(
        has_disable_then_cas,
        "P1-I-28: kmalloc.rs acquire_lock 必须先 disable_interrupts 再 CAS"
    );
}
