//! Tick 中断处理程序 (IDT 集成)
//!
//! 提供与 IDT 系统集成的周期 tick 中断处理：
//! - **tick Handler**: 周期 tick 入口点 (本核 LAPIC 定时器投递)
//! - **Tick 更新**: 递增全局计数器
//! - **调度触发**: 支持时间片轮转调度
//! - **续期**: 尾部装填本核下一个到期 (one-shot 语义)
//!
//! ## 集成方式 (`x86_64`)
//!
//! ```text
//! LAPIC Timer (本核, 向量 TICK_VECTOR = 0x30)
//!   ↓
//! [isr.asm] → irq_handler()
//!   ↓
//! [IdtManager::handle_irq()]  (irq = TICK_IRQ = 16 ⇒ LAPIC EOI 分支)
//!   ↓
//! [timer_tick_handler()]  ← 本模块
//!   ├── timer::on_timer_interrupt()
//!   ├── net::poll_network()
//!   ├── scheduler::tick() (可选, 仅调度器就绪后)
//!   └── clockevent::rearm_local()  (装填下一周期)
//! ```
//!
//! PIT 不再作为 tick 源 (E-II-b): 本模块不使能 IRQ0, 并显式屏蔽 PIC 的 bit0 ——
//! PIT 通道 0 仍自由跑作为启动期频率测定的参考门 (纯端口读, 不依赖中断投递)。

#[cfg(target_arch = "x86_64")]
use crate::privileged::idt::InterruptFrame;

/// 周期 tick 处理程序 (仅 `x86_64`)
///
/// aarch64 定时器中断由 exception.rs 的 `irq_handler_el1` 处理。
///
/// 不导 `extern "C"` 符号: handler 经 `register_irq` 存入 descriptor 表, 由 Rust
/// 侧调用; 保留 `no_mangle` 只会留下一个无生产调用者的 FFI 孤儿入口 (§5 F9)。
#[cfg(target_arch = "x86_64")]
pub extern "C" fn timer_tick_handler(_frame: *mut InterruptFrame) {
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

    // 6. 续期: LAPIC 定时器是 one-shot 语义 (到期即停), 每次 tick 尾部装填下一
    // 周期. 置于末尾使周期从"本轮工作完成"起算, 不被本轮耗时吃掉.
    crate::privileged::timer::clockevent::rearm_local();
}

/// 注册 tick handler 到 IDT 系统 (仅 `x86_64`)
///
/// 只登记 descriptor, 不装填硬件: 实际起 tick 由启动编排的 clockevent 装配步
/// (排在时基登记之后) 完成 —— 二者分开使"表项就绪"与"设备运行"各有单一责任点.
///
/// # Errors
/// 当向 `IdtManager` 注册 handler 失败时返回 `Err`, 错误信息由
/// `IdtManager::register_irq` 提供 (如 irq 槽越界等).
#[cfg(target_arch = "x86_64")]
pub fn register_timer_irq() -> Result<(), &'static str> {
    use crate::privileged::idt::{IdtManager, TICK_IRQ};

    let manager = IdtManager::instance();

    // 本地向量段: 向量 0x30 只由本核 LAPIC 定时器投递, EOI 由 handle_irq 的
    // irq>=16 分支统一走 LAPIC (一个向量一个源).
    manager.register_irq(
        TICK_IRQ,
        timer_tick_handler,
        "LAPIC Timer",
        0, // flags
    )?;

    // PIT 退役: 显式屏蔽 PIC bit0. boot.asm 启动即全 mask 8259, 旧写法靠此处
    // enable_irq(0) 解 mask 才能收到 tick; 现在不得再解, 且不依赖上游状态.
    IdtManager::disable_irq(0);

    Ok(())
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn test_timer_tick_handler_exists() {
        // 验证函数存在且可调用 (不会 panic)
        // 注意: 实际调用需要有效的 InterruptFrame

        // 函数指针类型检查
        let _handler: extern "C" fn(*mut InterruptFrame) = timer_tick_handler;

        // 如果编译通过，说明函数签名正确
    }

    // UT-06 (2026-09-24): test_register_timer_irq_interface 已迁出 —
    // register_timer_irq() 依赖 IDT 就绪, host 下不可用; 等价断言迁至
    // register_timer_irq_tests 的 "register_interface" 用例 (QEMU 裸机).
}

/// 注册定时器 IRQ 测试用例 (kernel_test, 仅 x86_64)
#[cfg(all(feature = "kernel_test", target_arch = "x86_64"))]
pub fn register_timer_irq_tests() {
    use crate::privileged::tests::{TestFn, TestResult, runner};

    fn timer_tick_handler_signature() -> TestResult {
        // J-01 (2026-09-08): used_underscore_binding 清理 — 类型注解断言函数签名
        // 编译期兼容, 无需 _handler 绑定.
        let _: extern "C" fn(*mut InterruptFrame) = timer_tick_handler;
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
        timer_tick_handler_signature as TestFn,
    );
    r.register(
        "timer::irq",
        "register_interface",
        register_interface as TestFn,
    );
}
