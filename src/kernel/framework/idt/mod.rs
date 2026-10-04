//! # Interrupt Descriptor Table (IDT) - Rust 安全重写
//!
//! `Edgine` 操作系统的中断描述符表管理模块。
//!
//! ## 架构概览
//!
//! ```text
//! Hardware Interrupt/Exception
//!   ↓
//! [isr.asm] (汇编 stub)
//!   ↓
//! [exception_handler() / irq_handler()]  ← FFI 入口
//!   ↓
//! [IdtManager] (Rust 核心逻辑)
//!   ├── ExceptionDispatcher
//!   │   ├── DivisionByZeroHandler
//!   │   ├── PageFaultHandler
//!   │   ├── GeneralProtectionFaultHandler
//!   │   └── DoubleFaultHandler
//!   └── IrqManager
//!       ├── register_irq()
//!       ├── unregister_irq()
//!       └── dispatch_irq()
//! ```
//!
//! ## 安全性增强 (相比 C 版本)
//!
//! - ✅ **内存安全**: Ownership + Borrow Checker 消除缓冲区溢出
//! - ✅ **并发安全**: `AtomicU64` / Mutex 保护全局状态
//! - ✅ **类型安全**: Trait 系统替代 void* 函数指针
//! - ✅ **空指针安全**: `Option<T>` 编译期排除 null deref
//!
//! ## 使用示例
//!
//! ```rust,ignore
//! // 注册自定义 IRQ handler
//! idt::register_irq(1, my_keyboard_handler, "keyboard", 0);
//!
//! // 在异常处理中使用
//! fn handle_page_fault(frame: &mut InterruptFrame) -> RecoveryAction {
//!     if frame.is_user_mode() {
//!         RecoveryAction::TerminateProcess(1)
//!     } else {
//!         RecoveryAction::DomainRecovery
//!     }
//! }
//! ```

// 子模块声明
pub mod handlers; // Phase 3: 异常处理器实现
pub mod idt; // Phase 2: 核心管理器
/// T-04: 中断处理决策 trait
pub mod irq_trait;
pub mod safety;
pub mod statistics;
pub mod types; // Phase 3: 统计与 JSON 导出

// 重新导出核心类型 (方便外部使用)
pub use types::{
    ErrorFlags,
    GDT_KERNEL_CODE,
    IDT_DPL_USER,
    // 常量
    IDT_ENTRIES,
    IDT_TYPE_INTERRUPT,
    IDT_TYPE_TRAP,
    IRQ_BASE,
    IdtEntry,
    IdtPtr,
    InterruptFrame,
    InterruptStatistics,
    IrqDescriptor,
    MODULE_INIT_FAILURE,
    MODULE_INIT_SUCCESS,
    // 辅助函数
    get_exception_name,
    get_irq_name,
};

pub use safety::{
    CpuFeatures, disable_interrupts, enable_interrupts, halt_loop, is_null_or_invalid,
    is_valid_kernel_address, is_valid_user_address, rdtsc, read_cr2,
};

pub use idt::IdtManager;

#[cfg(target_arch = "x86_64")]
pub use idt::idt_entries_base_lma;

/// 将已初始化的 IDT 加载到当前 CPU (`lidt`).
///
/// BSP 在 `idt_init()` 内完成 IDT 表构建后加载; AP 在自身 `ap_entry` 中于
/// `gdt_init_ap` 之后、`sti` 之前调用本函数. 若 AP 在 `IDTR.BASE = 0` 状态下
/// 开中断, 首个中断 (定时器向量 0x20) 即触发 #GP → #DF → triple fault.
///
/// # Safety
/// 调用方必须保证:
/// - `IdtManager::init()` 已完成 (IDT 表已填充, 各 CPU 共享同一张表);
/// - 当前 CPU 的中断处于屏蔽状态.
#[cfg(target_arch = "x86_64")]
pub unsafe fn load_idt_on_current_cpu() {
    // SAFETY: 由调用方保证 IDT 表已由 `IdtManager::init()` 初始化完成,
    // 且当前 CPU 中断已屏蔽 (见本函数 # Safety 契约).
    unsafe {
        IdtManager::instance().load_idt();
    }
}

// Phase 3: 异常处理器导出
pub use handlers::{
    DefaultHandler, DivisionByZeroHandler, DoubleFaultHandler, ExceptionCategory, ExceptionHandler,
    GeneralProtectionFaultHandler, PageFaultHandler, PanicInfo, RecoveryAction, Severity,
    create_handler, get_collector,
};

// Phase 3: 统计模块导出
pub use statistics::{DetailedStatistics, InterruptEvent, get_detailed_statistics};

// irq_trait 公共接口 re-export — T-04 策略-机制分离
pub use irq_trait::{
    FallbackIrqDecision, IrqContext, IrqDecision, SoftirqContext, current_irq_decision,
    register_irq_decision,
};

/// 全局 IDT 管理器实例 (Phase 2 已实现)
pub static IDT_MANAGER: () = ();

// ============================================================================
// FFI 接口层 (C ↔ Rust 桥接) - Phase 2 完整实现
// ============================================================================

/// `x86_64` 中断 wrapper 函数指针类型
/// 入口 stub (asm) → wrapper (本函数) → 业务 handler (Rust 普通调用)
/// 使用 `extern "C"` (`x86_64` Linux 上等同 `sysv64`),因为 wrapper 内部需
/// 正常调用业务 handler,不能用 `x86-interrupt` (后者禁止普通函数调用)
pub type CExceptionHandler = extern "C" fn(*mut InterruptFrame);

/// `x86_64` IRQ wrapper 函数指针类型
pub type CIrqHandler = extern "C" fn(*mut InterruptFrame);

/// 初始化 IDT 子系统 (FFI 导出函数)
///
/// # Safety
/// 此函数必须在内核启动早期调用，且只能调用一次
///
/// # Returns
/// - `MODULE_INIT_SUCCESS` (0): 成功
/// - `MODULE_INIT_FAILURE` (-1): 失败
#[unsafe(no_mangle)]
#[cfg(target_arch = "x86_64")]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
pub extern "C" fn idt_init() -> i32 {
    use crate::klog_error;

    let manager = IdtManager::instance();

    // 获取 ISR 地址表 (从 isr.asm 导出的符号, 使用 fn 指针)
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    #[expect(
        clippy::items_after_statements,
        reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
    )]
    unsafe extern "C" {
        fn isr0();
        fn isr1();
        fn isr2();
        fn isr3();
        fn isr4();
        fn isr5();
        fn isr6();
        fn isr7();
        fn isr8();
        fn isr9();
        fn isr10();
        fn isr11();
        fn isr12();
        fn isr13();
        fn isr14();
        fn isr15();
        fn isr16();
        fn isr17();
        fn isr18();
        fn isr19();
        fn isr20();
        fn isr21();
        fn isr22();
        fn isr23();
        fn isr24();
        fn isr25();
        fn isr26();
        fn isr27();
        fn isr28();
        fn isr29();
        fn isr30();
        fn isr31();
        fn irq0();
        fn irq1();
        fn irq2();
        fn irq3();
        fn irq4();
        fn irq5();
        fn irq6();
        fn irq7();
        fn irq8();
        fn irq9();
        fn irq10();
        fn irq11();
        fn irq12();
        fn irq13();
        fn irq14();
        fn irq15();
        // MSI 向量 stub (0x40-0x9F → irq16-irq127)
        fn irq16();
        fn irq17();
        fn irq18();
        fn irq19();
        fn irq20();
        fn irq21();
        fn irq22();
        fn irq23();
        fn irq24();
        fn irq25();
        fn irq26();
        fn irq27();
        fn irq28();
        fn irq29();
        fn irq30();
        fn irq31();
        fn irq32();
        fn irq33();
        fn irq34();
        fn irq35();
        fn irq36();
        fn irq37();
        fn irq38();
        fn irq39();
        fn irq40();
        fn irq41();
        fn irq42();
        fn irq43();
        fn irq44();
        fn irq45();
        fn irq46();
        fn irq47();
        fn irq48();
        fn irq49();
        fn irq50();
        fn irq51();
        fn irq52();
        fn irq53();
        fn irq54();
        fn irq55();
        fn irq56();
        fn irq57();
        fn irq58();
        fn irq59();
        fn irq60();
        fn irq61();
        fn irq62();
        fn irq63();
        fn irq64();
        fn irq65();
        fn irq66();
        fn irq67();
        fn irq68();
        fn irq69();
        fn irq70();
        fn irq71();
        fn irq72();
        fn irq73();
        fn irq74();
        fn irq75();
        fn irq76();
        fn irq77();
        fn irq78();
        fn irq79();
        // MSI 向量 stub 续 (0x80-0x9F → irq80-irq127)
        fn irq80();
        fn irq81();
        fn irq82();
        fn irq83();
        fn irq84();
        fn irq85();
        fn irq86();
        fn irq87();
        fn irq88();
        fn irq89();
        fn irq90();
        fn irq91();
        fn irq92();
        fn irq93();
        fn irq94();
        fn irq95();
        fn irq96();
        fn irq97();
        fn irq98();
        fn irq99();
        fn irq100();
        fn irq101();
        fn irq102();
        fn irq103();
        fn irq104();
        fn irq105();
        fn irq106();
        fn irq107();
        fn irq108();
        fn irq109();
        fn irq110();
        fn irq111();
        fn irq112();
        fn irq113();
        fn irq114();
        fn irq115();
        fn irq116();
        fn irq117();
        fn irq118();
        fn irq119();
        fn irq120();
        fn irq121();
        fn irq122();
        fn irq123();
        fn irq124();
        fn irq125();
        fn irq126();
        fn irq127();
        // IPI 向量 stub (0xFD TLB 失效 / 0xFE reschedule)
        fn irq253();
        fn irq254();
        fn syscall_handler();
        fn isr0x82();
    }

    // KPTI: x86_64 上 ISR stub 由链接器分配在低半部分地址,
    // 但用户页表只映射了高半部分内核空间. 中断在用户态触发时,
    // CPU 从 IDT 取 handler 地址并跳转, 必须是高半部分地址.
    // aarch64 不需要此偏移 (KERNEL_BASE=0).
    macro_rules! addr {
        ($f:ident) => {{
            let lo = ($f as *const ()) as usize as u64;
            #[cfg(target_arch = "x86_64")]
            {
                lo + crate::framework::mm::KERNEL_BASE
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                lo
            }
        }};
    }
    let isr_table: [u64; 32] = [
        addr!(isr0),
        addr!(isr1),
        addr!(isr2),
        addr!(isr3),
        addr!(isr4),
        addr!(isr5),
        addr!(isr6),
        addr!(isr7),
        addr!(isr8),
        addr!(isr9),
        addr!(isr10),
        addr!(isr11),
        addr!(isr12),
        addr!(isr13),
        addr!(isr14),
        addr!(isr15),
        addr!(isr16),
        addr!(isr17),
        addr!(isr18),
        addr!(isr19),
        addr!(isr20),
        addr!(isr21),
        addr!(isr22),
        addr!(isr23),
        addr!(isr24),
        addr!(isr25),
        addr!(isr26),
        addr!(isr27),
        addr!(isr28),
        addr!(isr29),
        addr!(isr30),
        addr!(isr31),
    ];

    let irq_table: [u64; 16] = [
        addr!(irq0),
        addr!(irq1),
        addr!(irq2),
        addr!(irq3),
        addr!(irq4),
        addr!(irq5),
        addr!(irq6),
        addr!(irq7),
        addr!(irq8),
        addr!(irq9),
        addr!(irq10),
        addr!(irq11),
        addr!(irq12),
        addr!(irq13),
        addr!(irq14),
        addr!(irq15),
    ];

    // MSI 向量 stub 表 (0x40-0x9F → irq16-irq127, 112 个 stub)
    let msi_table: [u64; 112] = [
        addr!(irq16),
        addr!(irq17),
        addr!(irq18),
        addr!(irq19),
        addr!(irq20),
        addr!(irq21),
        addr!(irq22),
        addr!(irq23),
        addr!(irq24),
        addr!(irq25),
        addr!(irq26),
        addr!(irq27),
        addr!(irq28),
        addr!(irq29),
        addr!(irq30),
        addr!(irq31),
        addr!(irq32),
        addr!(irq33),
        addr!(irq34),
        addr!(irq35),
        addr!(irq36),
        addr!(irq37),
        addr!(irq38),
        addr!(irq39),
        addr!(irq40),
        addr!(irq41),
        addr!(irq42),
        addr!(irq43),
        addr!(irq44),
        addr!(irq45),
        addr!(irq46),
        addr!(irq47),
        addr!(irq48),
        addr!(irq49),
        addr!(irq50),
        addr!(irq51),
        addr!(irq52),
        addr!(irq53),
        addr!(irq54),
        addr!(irq55),
        addr!(irq56),
        addr!(irq57),
        addr!(irq58),
        addr!(irq59),
        addr!(irq60),
        addr!(irq61),
        addr!(irq62),
        addr!(irq63),
        addr!(irq64),
        addr!(irq65),
        addr!(irq66),
        addr!(irq67),
        addr!(irq68),
        addr!(irq69),
        addr!(irq70),
        addr!(irq71),
        addr!(irq72),
        addr!(irq73),
        addr!(irq74),
        addr!(irq75),
        addr!(irq76),
        addr!(irq77),
        addr!(irq78),
        addr!(irq79),
        addr!(irq80),
        addr!(irq81),
        addr!(irq82),
        addr!(irq83),
        addr!(irq84),
        addr!(irq85),
        addr!(irq86),
        addr!(irq87),
        addr!(irq88),
        addr!(irq89),
        addr!(irq90),
        addr!(irq91),
        addr!(irq92),
        addr!(irq93),
        addr!(irq94),
        addr!(irq95),
        addr!(irq96),
        addr!(irq97),
        addr!(irq98),
        addr!(irq99),
        addr!(irq100),
        addr!(irq101),
        addr!(irq102),
        addr!(irq103),
        addr!(irq104),
        addr!(irq105),
        addr!(irq106),
        addr!(irq107),
        addr!(irq108),
        addr!(irq109),
        addr!(irq110),
        addr!(irq111),
        addr!(irq112),
        addr!(irq113),
        addr!(irq114),
        addr!(irq115),
        addr!(irq116),
        addr!(irq117),
        addr!(irq118),
        addr!(irq119),
        addr!(irq120),
        addr!(irq121),
        addr!(irq122),
        addr!(irq123),
        addr!(irq124),
        addr!(irq125),
        addr!(irq126),
        addr!(irq127),
    ];

    match manager.init(
        &isr_table,
        &irq_table,
        addr!(syscall_handler),
        addr!(isr0x82),
    ) {
        Ok(()) => {}
        Err(msg) => {
            klog_error!("IDT init failed: {}", msg);
            return MODULE_INIT_FAILURE;
        }
    }

    // 编程 MSI 向量 IDT 条目 (0x40-0x7F)
    manager.init_msi_idt(&msi_table);

    // 编程 IPI 向量 IDT 门 (0xFD TLB 失效 / 0xFE reschedule)
    let ipi_table: [u64; 2] = [addr!(irq253), addr!(irq254)];
    manager.init_ipi_idt(&ipi_table);

    MODULE_INIT_SUCCESS
}

/// 异常处理主入口 (从 isr.asm 调用)
///
/// # Arguments
/// * `frame` - 中断帧指针 (由 isr.asm 构建)
///
/// # Safety
/// 此函数在中断上下文中调用，必须快速执行
#[unsafe(no_mangle)]
#[unsafe(link_section = ".kpti_trampoline")]
pub unsafe extern "C" fn exception_handler(frame: *mut InterruptFrame) {
    let manager = IdtManager::instance();
    manager.handle_exception(frame);
}

/// IRQ 处理主入口 (从 isr.asm 调用)
///
/// # Arguments
/// * `frame` - 中断帧指针
///
/// # Safety
/// 此函数在中断上下文中调用，需要发送 EOI
#[unsafe(no_mangle)]
#[cfg(target_arch = "x86_64")]
#[unsafe(link_section = ".kpti_trampoline")]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub unsafe extern "C" fn irq_handler(frame: *mut InterruptFrame) {
    unsafe {
        if frame.is_null() {
            return;
        }

        let manager = IdtManager::instance();
        let frame_ref = &*frame;
        let vector = frame_ref.int_no as u8;

        manager.handle_irq(frame, vector);
    }
}

/// 设置 IDT 门描述符 (FFI 兼容接口)
///
/// # Arguments
/// * `num` - 向量号 (0-255)
/// * `handler` - handler 地址
/// * `selector` - 代码段选择子
/// * `type_attr` - 类型属性标志
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
// 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
#[expect(clippy::cast_possible_truncation)]
pub extern "C" fn idt_set_gate(num: u8, handler: u64, selector: u16, type_attr: u8) {
    let manager = IdtManager::instance();

    // 直接修改 entries 数组 (需要 Mutex 保护)
    let mut state = manager.state.lock();
    if num < IDT_ENTRIES as u8 {
        state.entries[num as usize] = IdtEntry::new(handler, selector, type_attr);
    }
}

/// 注册 IRQ handler (FFI 兼容接口)
///
/// # Arguments
/// * `irq` - IRQ 号 (0-15)
/// * `handler` - C 函数指针
/// * `name` - handler 名称 (用于日志)
/// * `flags` - 标志位
///
/// # Returns
/// - `0`: 成功
/// - `-1`: 参数无效
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub extern "C" fn idt_register_irq(
    irq: u8,
    handler: CIrqHandler,
    name: *const u8,
    flags: u32,
) -> i32 {
    let manager = IdtManager::instance();

    // 将 C 字符串转换为 Rust &str
    let name_str = if name.is_null() {
        ""
    } else {
        // 简单处理：假设 name 指向静态字符串
        // SAFETY: `const` 由调用方保证为有效指针; 只读访问
        unsafe {
            core::ffi::CStr::from_ptr(name as *const core::ffi::c_char)
                .to_str()
                .unwrap_or("")
        }
    };

    match manager.register_irq(irq, handler, name_str, flags) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// 注销 IRQ handler (FFI 兼容接口)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn idt_unregister_irq(irq: u8, handler: CIrqHandler) -> i32 {
    let manager = IdtManager::instance();

    match manager.unregister_irq(irq, handler) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// 启用指定 IRQ
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn idt_enable_irq(irq: u8) {
    let manager = IdtManager::instance();
    manager.enable_irq(irq);
}

/// 禁用指定 IRQ
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn idt_disable_irq(irq: u8) {
    let manager = IdtManager::instance();
    manager.disable_irq(irq);
}

/// 导出 IDT 状态 (用于调试)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn idt_dump_state() {
    let manager = IdtManager::instance();
    manager.dump_state();
}

/// 获取中断计数统计
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn idt_get_interrupt_count(vector: u8) -> u64 {
    let manager = IdtManager::instance();
    manager.get_interrupt_count(vector)
}

/// 打印详细的中断统计信息
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn idt_print_interrupt_stats() {
    let manager = IdtManager::instance();
    manager.print_statistics();
}

// ============================================================================
// 测试模块
// ============================================================================

#[cfg(test)]
mod integration_tests {
    use super::*;

    // host 轨仅保留 host 适用的纯逻辑冒烟 (klog 在 host-test 下为 no-op, 见 klog/mod.rs).
    // idt_init 不作断言: 其符号面依赖 isr.asm 的 164 个汇编 stub, 且函数体内经
    // remap_pic(outb) 与 load_idt(lidt) 执行特权指令, 属硬件路径 (DECISION-080:
    // 硬件路径归 kernel_test 载体); 其真实覆盖由 BSP 启动路径
    // (arch/x86_64/mod.rs 的 idt_init 调用) + QEMU boot + 双架构链接阶段承担.
    // 不为此新增 host 符号桩 —— 遵守 T1「消灭 host 占位符号, 守卫交还链接器」的方向.
    #[test]
    fn test_dump_functions_no_panic() {
        // 测试 dump 函数不会 panic
        idt_dump_state();
        idt_print_interrupt_stats();
    }

    #[test]
    fn test_exception_names_coverage() {
        // 确保所有标准异常都有名称
        for i in 0..32u8 {
            let name = get_exception_name(i);
            assert!(!name.is_empty(), "Exception {} should have a name", i);
            assert_ne!(name, "Unknown", "Exception {} should be known", i);
        }
    }

    #[test]
    fn test_constants_sanity() {
        assert_eq!(IDT_ENTRIES, 256);
        assert_eq!(IRQ_BASE, 32);
        assert!(MODULE_INIT_SUCCESS != MODULE_INIT_FAILURE);
    }
}
