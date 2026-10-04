//! Timer IRQ0 中断处理程序
//!
//! 提供与 IDT 系统集成的定时器中断处理：
//! - **IRQ0 Handler**: 定时器中断入口点
//! - **Tick 更新**: 递增全局计数器
//! - **调度触发**: 支持时间片轮转调度
//!
//! ## 集成方式
//!
//! ```text
//! Hardware IRQ0 (PIT)
//!   ↓
//! [isr.asm] → irq_handler()
//!   ↓
//! [IdtManager::handle_irq()]
//!   ↓
//! [timer_irq0_handler()]  ← 本模块
//!   ├── timer::on_timer_interrupt()
//!   └── scheduler::tick() (可选)
//! ```

#[cfg(target_arch = "x86_64")]
use crate::privileged::idt::InterruptFrame;

/// Timer IRQ0 中断处理程序 (仅 `x86_64`)
/// aarch64 定时器中断由 exception.rs 的 `irq_handler_el1` 处理
#[cfg(target_arch = "x86_64")]
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn timer_irq0_handler(_frame: *mut InterruptFrame) {
    // I-50: hrtimer_run_queues 已在 on_timer_interrupt 内统一触发 (tick.rs),
    // 此处不再显式调用, 避免重复处理 (hrtimer 自身有去重, 但统一入口更清晰).
    crate::privileged::timer::on_timer_interrupt();

    #[cfg(not(feature = "kernel_test"))]
    {
        #[cfg(target_arch = "x86_64")]
        {
            // smoltcp: 始终轮询
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                crate::privileged::net::poll_network();
            }
        }
    }

    // 5. 触发调度器 tick (统一入口: 进程调度器负责线程记账 + 调度决策)
    // ✅ 安全检查: 仅当调度器已初始化时才触发 tick (与 ARM 版本一致, 避免竞态崩溃)
    if crate::privileged::proc::SCHEDULER_READY.load(core::sync::atomic::Ordering::Acquire) {
        // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
        unsafe extern "C" {
            fn scheduler_tick();
        }
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            scheduler_tick();
        }
    }
}

/// 注册 Timer IRQ0 handler 到 IDT 系统 (仅 `x86_64`)
///
/// # Errors
/// 当向 `IdtManager` 注册 IRQ0 handler 失败时返回 `Err`, 错误信息由
/// `IdtManager::register_irq` 提供 (如向量槽位冲突等).
#[cfg(target_arch = "x86_64")]
pub fn register_timer_irq() -> Result<(), &'static str> {
    use crate::privileged::idt::IdtManager;

    let manager = IdtManager::instance();

    // 注册 IRQ0 handler
    manager.register_irq(
        0, // IRQ0 = PIT Timer
        timer_irq0_handler,
        "PIT Timer",
        0, // flags
    )?;

    // 启用 IRQ0
    manager.enable_irq(0);

    Ok(())
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn test_timer_irq0_handler_exists() {
        // 验证函数存在且可调用 (不会 panic)
        // 注意: 实际调用需要有效的 InterruptFrame

        // 函数指针类型检查
        let _handler: extern "C" fn(*mut InterruptFrame) = timer_irq0_handler;

        // 如果编译通过，说明函数签名正确
    }

    // UT-06 (2026-09-24): test_register_timer_irq_interface 已迁出 —
    // register_timer_irq() 依赖 IDT 就绪, host 下不可用; 等价断言迁至
    // register_timer_irq_tests 的 "register_interface" 用例 (QEMU 裸机).
}

#[cfg(all(feature = "kernel_test", target_arch = "x86_64"))]
pub fn register_timer_irq_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};

    fn timer_irq0_handler_signature() -> TestResult {
        // J-01 (2026-09-08): used_underscore_binding 清理 — 类型注解断言函数签名
        // 编译期兼容, 无需 _handler 绑定.
        let _: extern "C" fn(*mut InterruptFrame) = timer_irq0_handler;
        TestResult::Pass
    }

    /// UT-06 (2026-09-24): 自 irq.rs::tests::test_register_timer_irq_interface 迁入 —
    /// register_timer_irq() 依赖 IDT 就绪 (host 不可用), 仅 kernel_test (QEMU 裸机) 执行.
    fn register_interface() -> TestResult {
        // 可能因 IDT 状态返回 Err, 但不应 panic
        let _ = register_timer_irq();
        TestResult::Pass
    }

    let r = runner();
    r.register(
        "timer::irq",
        "handler_signature",
        timer_irq0_handler_signature as TestFn,
    );
    r.register(
        "timer::irq",
        "register_interface",
        register_interface as TestFn,
    );
}
