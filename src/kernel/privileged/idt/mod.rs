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
    TICK_IRQ,
    TICK_VECTOR,
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

// SAFETY: isr* / irq* / syscall_handler 等符号由 isr.asm 导出, 签名与 C ABI 声明一致
#[cfg(target_arch = "x86_64")]
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
    // 本地向量 stub (0x30 = IRQ_BASE + TICK_IRQ, LAPIC 定时器 tick)
    fn irq128();
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

/// 异常 stub 表 (异常向量 0-31)
// SAFETY: 表内符号均由 `isr.asm` 导出, C ABI 声明见上方 `unsafe extern "C"` 块;
// 此处只取函数指针供 IDT 填补使用, 不在本处调用.
#[cfg(target_arch = "x86_64")]
static ISR_STUBS: [unsafe extern "C" fn(); 32] = [
    isr0, isr1, isr2, isr3, isr4, isr5, isr6, isr7, isr8, isr9, isr10, isr11, isr12, isr13, isr14,
    isr15, isr16, isr17, isr18, isr19, isr20, isr21, isr22, isr23, isr24, isr25, isr26, isr27,
    isr28, isr29, isr30, isr31,
];

/// 传统 IRQ stub 表 (8259/IOAPIC 向量 0x20-0x2F)
// SAFETY: 表内符号均由 `isr.asm` 导出, C ABI 声明见上方 `unsafe extern "C"` 块;
// 此处只取函数指针供 IDT 填补使用, 不在本处调用.
#[cfg(target_arch = "x86_64")]
static IRQ_STUBS: [unsafe extern "C" fn(); 16] = [
    irq0, irq1, irq2, irq3, irq4, irq5, irq6, irq7, irq8, irq9, irq10, irq11, irq12, irq13, irq14,
    irq15,
];

/// MSI 向量 stub 表 (0x40-0x9F → irq16-irq127, 112 个 stub)
// SAFETY: 表内符号均由 `isr.asm` 导出, C ABI 声明见上方 `unsafe extern "C"` 块;
// 此处只取函数指针供 IDT 填补使用, 不在本处调用.
#[cfg(target_arch = "x86_64")]
static MSI_STUBS: [unsafe extern "C" fn(); 112] = [
    irq16, irq17, irq18, irq19, irq20, irq21, irq22, irq23, irq24, irq25, irq26, irq27, irq28,
    irq29, irq30, irq31, irq32, irq33, irq34, irq35, irq36, irq37, irq38, irq39, irq40, irq41,
    irq42, irq43, irq44, irq45, irq46, irq47, irq48, irq49, irq50, irq51, irq52, irq53, irq54,
    irq55, irq56, irq57, irq58, irq59, irq60, irq61, irq62, irq63, irq64, irq65, irq66, irq67,
    irq68, irq69, irq70, irq71, irq72, irq73, irq74, irq75, irq76, irq77, irq78, irq79, irq80,
    irq81, irq82, irq83, irq84, irq85, irq86, irq87, irq88, irq89, irq90, irq91, irq92, irq93,
    irq94, irq95, irq96, irq97, irq98, irq99, irq100, irq101, irq102, irq103, irq104, irq105,
    irq106, irq107, irq108, irq109, irq110, irq111, irq112, irq113, irq114, irq115, irq116, irq117,
    irq118, irq119, irq120, irq121, irq122, irq123, irq124, irq125, irq126, irq127,
];

/// IPI 向量 stub 表 (0xFD TLB 失效 / 0xFE reschedule)
// SAFETY: 表内符号均由 `isr.asm` 导出, C ABI 声明见上方 `unsafe extern "C"` 块;
// 此处只取函数指针供 IDT 填补使用, 不在本处调用.
#[cfg(target_arch = "x86_64")]
static IPI_STUBS: [unsafe extern "C" fn(); 2] = [irq253, irq254];

/// 取 stub 符号的 `KPTI` 高半区地址 (仅 x86_64; aarch64 的 `KERNEL_BASE` 为 0).
#[cfg(target_arch = "x86_64")]
macro_rules! stub_addr {
    ($f:ident) => {{ ($f as *const ()) as usize as u64 + crate::privileged::mm::KERNEL_BASE }};
}

/// 将 stub 函数表转换为带 `KPTI` 高半区偏移的地址表.
///
/// `KPTI`: x86_64 上 ISR stub 由链接器分配在低半部分地址, 但用户页表只映射了
/// 高半部分内核空间. 中断在用户态触发时, CPU 从 IDT 取 handler 地址并跳转,
/// 必须是高半部分地址.
// SAFETY: 入参是上述 `unsafe extern "C"` 块导出的 stub 符号指针, 仅做
// 函数指针→地址的数值换算, 不解引用也不调用.
#[cfg(target_arch = "x86_64")]
fn stub_addrs<const N: usize>(stubs: &[unsafe extern "C" fn(); N]) -> [u64; N] {
    let mut addrs = [0u64; N];
    for (slot, stub) in addrs.iter_mut().zip(stubs.iter()) {
        *slot = (*stub as *const ()) as usize as u64 + crate::privileged::mm::KERNEL_BASE;
    }
    addrs
}

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
pub extern "C" fn idt_init() -> i32 {
    use crate::klog_error;

    let manager = IdtManager::instance();

    let isr_table: [u64; 32] = stub_addrs(&ISR_STUBS);
    let irq_table: [u64; 16] = stub_addrs(&IRQ_STUBS);
    let msi_table: [u64; 112] = stub_addrs(&MSI_STUBS);
    let ipi_table: [u64; 2] = stub_addrs(&IPI_STUBS);

    match manager.init(
        &isr_table,
        &irq_table,
        stub_addr!(syscall_handler),
        stub_addr!(isr0x82),
    ) {
        Ok(()) => {}
        Err(msg) => {
            klog_error!("IDT init failed: {}", msg);
            return MODULE_INIT_FAILURE;
        }
    }

    // 编程 MSI 向量 IDT 条目 (0x40-0x7F)
    manager.init_msi_idt(&msi_table);

    // 编程 tick 本地向量门 (TICK_VECTOR = 0x30): LAPIC 定时器由本核 LVT 直接
    // 投递, 不经 8259/IOAPIC; 与 `register_irq(TICK_IRQ, ..)` 的槽位同一真源.
    // 必须早于任何 `program_timer` 装配 (否则首个 tick 无门 → #GP → #DF).
    manager.init_local_vector(TICK_VECTOR, stub_addr!(irq128));

    // 编程 IPI 向量 IDT 门 (0xFD TLB 失效 / 0xFE reschedule)
    manager.init_ipi_idt(ipi_table);

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
pub unsafe extern "C" fn irq_handler(frame: *mut InterruptFrame) {
    unsafe {
        if frame.is_null() {
            return;
        }

        let manager = IdtManager::instance();
        let frame_ref = &*frame;
        let vector = crate::privileged::lib::low_u8_from_u64(frame_ref.int_no);

        // Lockdep: 标记进入中断上下文 (仅诊断构建; 默认 boot 零开销).
        #[cfg(any(debug_assertions, feature = "lockdep"))]
        crate::privileged::sync::lockdep::irq_enter();
        manager.handle_irq(frame, vector);
        #[cfg(any(debug_assertions, feature = "lockdep"))]
        crate::privileged::sync::lockdep::irq_exit();
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
pub extern "C" fn idt_set_gate(num: u8, handler: u64, selector: u16, type_attr: u8) {
    let manager = IdtManager::instance();

    // 直接修改 entries 数组 (需要 Mutex 保护)
    let mut state = manager.state.lock();
    if num < crate::privileged::lib::low_u8_from_usize(IDT_ENTRIES) {
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
            core::ffi::CStr::from_ptr(name.cast::<core::ffi::c_char>())
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
    IdtManager::enable_irq(irq);
}

/// 禁用指定 IRQ
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn idt_disable_irq(irq: u8) {
    IdtManager::disable_irq(irq);
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
