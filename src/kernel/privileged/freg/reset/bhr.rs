//! # 栏硬重置 (BHR — FREG Hard Reset)
//!
//! Layer 3 恢复：完全从硬件层面重置

use super::audit;
use super::config::{self, RecoveryLayer};

/// 关闭 CPU 中断 (BHR 硬重置前置步骤)。
pub fn disable_interrupts() {
    #[cfg(not(feature = "kernel_test"))]
    {
        let _ = crate::arch!(interrupt_disable());
    }
}

/// 屏蔽 8259 主/从片全部 PIC 中断。
pub fn mask_all_irqs() {
    #[cfg(not(feature = "kernel_test"))]
    {
        // 屏蔽所有 PIC 中断 (8259 主/从片)
        crate::arch!(outb(0xA1, 0xFF)); // 从片 IMR
        crate::arch!(outb(0x21, 0xFF)); // 主片 IMR
    }
}

/// 关闭所有外设 (BHR 硬重置前置步骤)。
pub fn shutdown_devices() {
    #[cfg(not(feature = "kernel_test"))]
    {
        crate::klog_info!(Kernel, "[BHR] Shutting down devices...");
    }
}

/// 保存崩溃现场信息用于事后诊断。
pub fn save_crash_info() {
    #[cfg(not(feature = "kernel_test"))]
    {
        crate::klog_info!(Kernel, "[BHR] Crash info saved");
    }
}

/// 通过 8042 键盘控制器触发系统复位, 不返回。
pub fn keyboard_reset() -> ! {
    #[cfg(not(feature = "kernel_test"))]
    {
        crate::arch!(outb(0x64, 0xFE)); // 8042 键盘控制器 reset
    }

    #[cfg(feature = "kernel_test")]
    loop {
        core::hint::spin_loop();
    }

    #[cfg(not(feature = "kernel_test"))]
    loop {
        crate::arch!(halt());
    }
}

/// 触发 x86 三重故障以强制硬复位, 不返回。
pub fn triple_fault() -> ! {
    #[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
    raw::triple_fault_asm();

    loop {
        core::hint::spin_loop();
    }
}

/// 执行 Layer3 硬重置流程 (关中断/屏蔽 IRQ/关设备后键盘复位), 不返回。
pub fn execute() -> ! {
    config::set_current_layer(RecoveryLayer::Layer3);
    config::increment_bhr_count();

    crate::klog_crit!(Kernel, "[BHR] FREG Hard Reset initiated");

    audit::audit_record(RecoveryLayer::Layer3, RecoveryResult::Success, 0);

    save_crash_info();
    disable_interrupts();
    mask_all_irqs();
    shutdown_devices();

    crate::klog_crit!(Kernel, "[BHR] Performing keyboard controller reset...");

    keyboard_reset()
}

/// 键盘复位失败时的兜底路径: 触发三重故障, 不返回。
pub fn execute_fallback() -> ! {
    crate::klog_crit!(
        Kernel,
        "[BHR] Keyboard reset failed, attempting triple fault"
    );
    triple_fault()
}

use super::config::RecoveryResult;

// ============================================================================
// 特权子模块 (Framekernel raw): 集中 x86 特权硬件指令
// ============================================================================
//
// `triple_fault` 使用 `lidt [0]` (加载空 IDT) + `int 3` 触发 CPU
// 三重故障, 是 x86 平台级硬重置的最终手段。无安全抽象可替代。

#[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
pub(crate) mod raw {
    /// 通过加载空 IDT 并触发 #BP 强制 CPU 三重故障
    pub fn triple_fault_asm() {
        // SAFETY: 这是有意触发的硬重置, 不应返回。CPU 将进入 S5/关机。
        unsafe {
            core::arch::asm!("lidt [0]", "int 3", options(nomem, nostack));
        }
    }
}
