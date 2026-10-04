//! # IDT 数据类型定义
//!
//! 安全的 x86-64 中断描述符表数据结构。
//! 所有布局与 C 版本 [idt.h](../../../include/idt.h) 完全兼容，
//! 使用 `#[repr(C, packed)]` 确保内存布局一致。

// 仅测试使用: `is_user_mode` 已改为只依据 CS.RPL, 内核态判据不再需要文本基址;
// 本符号保留供 `test_user_mode_detection` 构造内核态帧.
#[cfg(test)]
use crate::privileged::mm::KERNEL_TEXT_BASE;

/// IDT 条目总数 (Intel 64-bit)
pub const IDT_ENTRIES: usize = 256;

/// IRQ 基础向量号
pub const IRQ_BASE: u8 = 32;

/// IDT 门描述符类型标志
pub const IDT_TYPE_INTERRUPT: u8 = 0x8E; // 中断门 (DPL=0)
pub const IDT_TYPE_TRAP: u8 = 0x8F; // 陷阱门 (DPL=0)
pub const IDT_DPL_USER: u8 = 0x60; // 用户态权限位

/// GDT 内核代码段选择子
pub const GDT_KERNEL_CODE: u16 = 0x08;

/// 模块初始化成功/失败码
pub const MODULE_INIT_SUCCESS: i32 = 0;
pub const MODULE_INIT_FAILURE: i32 = -1;

/// IRQ 标志位
pub const IRQ_FLAG_SHARED: u32 = 0x01;
pub const IRQ_FLAG_EDGE: u32 = 0x02;
pub const IRQ_FLAG_LEVEL: u32 = 0x04;

/// 中断帧结构 (与 isr.asm 的 push 序列完全匹配)
///
/// # Safety
/// 此结构的字段顺序 **必须** 与 [isr.asm](../../kernel/isr.asm) 中的 push 序列保持一致：
/// ```asm
/// push rax, rbx, rcx, rdx, rbp, rsi, rdi, r8..r15
/// ```
/// 任何修改都必须同步更新汇编代码。
#[repr(C, packed)]
pub struct InterruptFrame {
    /// 通用寄存器 (按栈顺序排列)
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,

    /// 中断元数据
    pub int_no: u64, // 中断向量号
    pub err_code: u64, // 错误码 (部分异常有)

    /// 返回地址信息
    pub rip: u64, // 指令指针
    pub cs: u64,     // 代码段选择子
    pub rflags: u64, // RFLAGS 寄存器
    pub rsp: u64,    // 栈指针
    pub ss: u64,     // 栈段选择子
}

// 编译时静态断言：确保结构体大小正确
// C 版本: 15 regs (r15-rax) + 2 (int_no, err_code) + 5 (rip, cs, rflags, rsp, ss) = 22 * 8 = 176 bytes
const _: () = assert!(
    core::mem::size_of::<InterruptFrame>() == 176,
    "InterruptFrame size must be 176 bytes (22 fields * 8 bytes)"
);

impl InterruptFrame {
    /// 创建新的中断帧 (用于测试)
    #[cfg(any(test, feature = "kernel_test"))]
    // J-01 (2026-09-08): 删除 unfulfilled inline_always expect — new_test_frame
    // 无 #[inline(always)] 属性, expect 从不触发 (kernel_test 编译暴露)
    pub fn new_test_frame(int_no: u64, rip: u64, cs: u64) -> Self {
        Self {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rdi: 0,
            rsi: 0,
            rbp: 0,
            rdx: 0,
            rcx: 0,
            rbx: 0,
            rax: 0,
            int_no,
            err_code: 0,
            rip,
            cs,
            rflags: 0x202,
            rsp: 0,
            ss: 0x10,
        }
    }

    /// 判断当前中断是否来自 user-mode
    ///
    /// 唯一判据是 **CS.RPL** (段选择子最低两位): RPL=3 即用户态.
    ///
    /// 曾辅以 "RIP 落在低半区" 的启发式作为第二判据, 但内核 `.text` 实际
    /// 链接在低半区 (`. = 0x100000`), 该条件对内核态异常恒为真 —— 会把内核
    /// 故障 (如 #UD/#GPF) 误判为用户态, 进而在 `handlers.rs` 走
    /// `TerminateProcess` 而非 Panic, 内核现场被静默吞掉. 故删除该判据.
    ///
    /// # Returns
    /// - `true`: user-mode 中断
    /// - `false`: kernel-mode 中断
    #[inline(always)]
    pub fn is_user_mode(&self) -> bool {
        (self.cs & 0x03) == 3
    }

    /// 安全地读取 CR2 寄存器 (Page Fault 地址)
    ///
    /// # Safety
    /// 仅在 Page Fault (#PF) 异常中调用此方法
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub unsafe fn fault_address(&self) -> u64 {
        crate::arch!(read_fault_address()) as u64
    }

    /// 获取错误码的各个位域
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn error_code_flags(&self) -> ErrorFlags {
        ErrorFlags::from_bits_truncate(self.err_code as u32)
    }

    /// 打印寄存器状态 (用于调试)
    #[cfg(feature = "log")]
    pub fn dump_registers(&self) {
        use log::{info, warn};

        warn!("=== Interrupt Frame ===");
        info!("  Vector: {} ({:#x})", self.int_no, self.int_no);
        info!(
            "  RIP={:#016x} CS={:#04x} RFLAGS={:#08x}",
            self.rip, self.cs as u16, self.rflags
        );
        info!("  RSP={:#016x} SS={:#04x}", self.rsp, self.ss as u16);
        info!(
            "  RAX={:#016x} RBX={:#016x} RCX={:#016x}",
            self.rax, self.rcx, self.rcx
        );
        info!(
            "  RDX={:#016x} RSI={:#016x} RDI={:#016x}",
            self.rdx, self.rsi, self.rdi
        );
        info!(
            "  Mode: {}",
            if self.is_user_mode() {
                "USER"
            } else {
                "KERNEL"
            }
        );
    }
}

// Page Fault 错误码位域标志
bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ErrorFlags: u32 {
        const PRESENT     = 1 << 0;  // 页是否存在
        const WRITE       = 1 << 1;  // 是否写操作
        const USER        = 1 << 2;  // 是否 user-mode
        const RESERVED    = 1 << 3;  // 预留位被置位
        const INSTRUCTION = 1 << 4;  // 是否取指令
    }
}

/// IDT 门描述符条目 (128-bit)
///
/// 布局完全匹配 Intel SDM Vol. 3A Figure 6-5:
/// ```text
/// +0  Offset`[15:0]`   | Selector
/// +8  IST            | Type/Attr | Offset`[31:16]`
/// +16 Offset`[63:32]`  | Reserved
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Default, Debug)]
pub struct IdtEntry {
    /// Handler 偏移 `[15:0]`
    pub offset_low: u16,
    /// 代码段选择子
    pub selector: u16,
    /// IST (Interrupt Stack Table) index
    pub ist: u8,
    /// 类型属性 (P/DPL/Type)
    pub type_attr: u8,
    /// Handler 偏移 `[31:16]`
    pub offset_mid: u16,
    /// Handler 偏移 `[63:32]`
    pub offset_high: u32,
    /// 保留 (必须为 0)
    pub reserved: u32,
}

impl IdtEntry {
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 创建新的 IDT 门描述符
    pub fn new(handler: u64, selector: u16, type_attr: u8) -> Self {
        Self {
            offset_low: (handler & 0xFFFF) as u16,
            selector,
            ist: 0,
            type_attr,
            offset_mid: ((handler >> 16) & 0xFFFF) as u16,
            offset_high: ((handler >> 32) & 0xFFFFFFFF) as u32,
            reserved: 0,
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 创建带 IST 索引的 IDT 门描述符
    pub fn new_with_ist(handler: u64, selector: u16, type_attr: u8, ist_index: u8) -> Self {
        Self {
            offset_low: (handler & 0xFFFF) as u16,
            selector,
            ist: ist_index & 0x07,
            type_attr,
            offset_mid: ((handler >> 16) & 0xFFFF) as u16,
            offset_high: ((handler >> 32) & 0xFFFFFFFF) as u32,
            reserved: 0,
        }
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 设置 handler 地址
    pub fn set_handler(&mut self, handler: u64) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = ((handler >> 32) & 0xFFFFFFFF) as u32;
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 判断门是否有效 (Present bit set)
    pub fn is_present(&self) -> bool {
        (self.type_attr & 0x80) != 0
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是 API 约定 (如 impl trait); 当前优先 expect"
    )]
    /// 获取完整的 handler 地址
    pub fn handler_address(&self) -> u64 {
        u64::from(self.offset_high) << 32
            | u64::from(self.offset_mid) << 16
            | u64::from(self.offset_low)
    }
}

/// IDT 指针 (用于 lidt 指令)
///
/// 布局匹配 Intel SDM:
/// ```text
/// +0  Limit (size - 1)
/// +8  Base address
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct IdtPtr {
    /// IDT 大小限制 (字节数 - 1)
    pub limit: u16,
    /// IDT 基址
    pub base: u64,
}

impl IdtPtr {
    /// 创建新的 IDT 指针
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn new(base: u64) -> Self {
        Self {
            limit: (IDT_ENTRIES * core::mem::size_of::<IdtEntry>() - 1) as u16,
            base,
        }
    }
}

/// IRQ 描述符 (扩展信息)
#[derive(Debug)]
pub struct IrqDescriptor {
    /// 处理函数 (`x86_64` 上 wrapper 用 C ABI / sysv64,aarch64 上 AAPCS64)
    pub handler: Option<extern "C" fn(*mut InterruptFrame)>,
    /// 名称 (用于日志和诊断)
    pub name: &'static str,
    /// 详细描述
    pub description: &'static str,
    /// 标志 (`IRQ_FLAG`_*)
    pub flags: u32,
    /// 调用统计
    pub call_count: core::sync::atomic::AtomicU64,
    /// 错误统计
    pub error_count: core::sync::atomic::AtomicU64,
}

impl Clone for IrqDescriptor {
    fn clone(&self) -> Self {
        Self {
            handler: self.handler,
            name: self.name,
            description: self.description,
            flags: self.flags,
            call_count: core::sync::atomic::AtomicU64::new(
                self.call_count.load(core::sync::atomic::Ordering::Relaxed),
            ),
            error_count: core::sync::atomic::AtomicU64::new(
                self.error_count.load(core::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

impl IrqDescriptor {
    /// 创建空的 IRQ 描述符
    pub const fn empty() -> Self {
        Self {
            handler: None,
            name: "",
            description: "",
            flags: 0,
            call_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
        }
    }
}

/// 原子计数器 (用于无锁统计)
use core::sync::atomic::{AtomicU64, Ordering};

/// 中断统计数据 (线程安全)
#[derive(Debug)]
pub struct InterruptStatistics {
    /// 各异常向量的触发次数
    pub exception_counts: [core::sync::atomic::AtomicU64; 32],
    /// 各 IRQ 的触发次数
    pub irq_counts: [core::sync::atomic::AtomicU64; 16],
    /// 嵌套中断总次数
    pub nested_interrupts: core::sync::atomic::AtomicU64,
    /// 上次异常时间戳 (TSC)
    pub last_exception_tsc: core::sync::atomic::AtomicU64,
}

impl Default for InterruptStatistics {
    fn default() -> Self {
        // 使用 array::from_fn 需要 nightly，这里用简单的方式
        let stats = Self {
            exception_counts: [const { core::sync::atomic::AtomicU64::new(0) }; 32],
            irq_counts: [const { core::sync::atomic::AtomicU64::new(0) }; 16],
            nested_interrupts: core::sync::atomic::AtomicU64::new(0),
            last_exception_tsc: core::sync::atomic::AtomicU64::new(0),
        };
        stats
    }
}

impl InterruptStatistics {
    /// 创建新的统计实例
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次异常
    pub fn record_exception(&self, vector: u8) {
        if vector < 32 {
            self.exception_counts[vector as usize].fetch_add(1, Ordering::Relaxed);
        }
        // 更新时间戳 (使用 rdtsc)

        self.last_exception_tsc
            .store(crate::arch!(timestamp()), Ordering::Relaxed);
    }

    /// 记录一次 IRQ
    pub fn record_irq(&self, irq: u8) {
        if irq < 16 {
            self.irq_counts[irq as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 获取指定向量的计数
    pub fn get_count(&self, vector: u8) -> u64 {
        if vector < 32 {
            self.exception_counts[vector as usize].load(Ordering::Relaxed)
        } else if (vector as usize) >= IRQ_BASE as usize
            && (vector as usize) < IRQ_BASE as usize + 16
        {
            self.irq_counts[(vector - IRQ_BASE) as usize].load(Ordering::Relaxed)
        } else {
            0
        }
    }

    /// 导出为 JSON 格式 (用于测试框架)
    #[cfg(feature = "json_export")]
    pub fn export_json(&self) -> alloc::string::String {
        use alloc::format;
        let mut json = String::from("{\"exceptions\":{");
        for i in 0..32u8 {
            if i > 0 {
                json.push_str(",");
            }
            json.push_str(&format!("\"{}\":{}", i, self.get_count(i)));
        }
        json.push_str("},\"irqs\":{");
        for i in 0..16u8 {
            if i > 0 {
                json.push_str(",");
            }
            json.push_str(&format!("\"{}\":{}", i, self.get_count(IRQ_BASE + i)));
        }
        json.push_str("}}");
        json
    }
}

/// 异常名称映射表
pub static EXCEPTION_NAMES: [&str; 32] = [
    "Division By Zero",              // #0
    "Debug",                         // #1
    "Non Maskable Interrupt",        // #2
    "Breakpoint",                    // #3
    "Into Detected Overflow",        // #4
    "Out of Bounds",                 // #5
    "Invalid Opcode",                // #6
    "No Coprocessor",                // #7
    "Double Fault",                  // #8
    "Coprocessor Segment Overrun",   // #9
    "Bad TSS",                       // #10
    "Segment Not Present",           // #11
    "Stack Fault",                   // #12
    "General Protection Fault",      // #13
    "Page Fault",                    // #14
    "Unknown Interrupt",             // #15
    "Coprocessor Fault",             // #16
    "Alignment Check",               // #17
    "Machine Check",                 // #18
    "SIMD Floating-Point Exception", // #19
    "Virtualization Exception",      // #20
    "Control Protection Exception",  // #21
    "Reserved",                      // #22-27
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Hypervisor Injection Exception", // #28
    "VMM Communication Exception",    // #29
    "Security Exception",             // #30
    "Reserved",                       // #31
];

/// IRQ 名称映射表
pub static IRQ_NAMES: [&str; 16] = [
    "Timer",         // IRQ 0
    "Keyboard",      // IRQ 1
    "Cascade",       // IRQ 2
    "COM2",          // IRQ 3
    "COM1",          // IRQ 4
    "LPT2",          // IRQ 5
    "Floppy",        // IRQ 6
    "LPT1/Spurious", // IRQ 7
    "CMOS",          // IRQ 8
    "ACPI",          // IRQ 9
    "PCI",           // IRQ 10
    "NIC",           // IRQ 11
    "CoProcessor",   // IRQ 12
    "Primary ATA",   // IRQ 13
    "Secondary ATA", // IRQ 14
    "Spurious",      // IRQ 15
];

/// 获取异常名称
pub fn get_exception_name(vector: u8) -> &'static str {
    if vector < 32 {
        EXCEPTION_NAMES[vector as usize]
    } else {
        "Unknown"
    }
}

/// 获取 IRQ 名称
pub fn get_irq_name(irq: u8) -> &'static str {
    if irq < 16 {
        IRQ_NAMES[irq as usize]
    } else {
        "Unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interrupt_frame_size() {
        assert_eq!(core::mem::size_of::<InterruptFrame>(), 176);
    }

    #[test]
    fn test_user_mode_detection() {
        // Kernel mode frame (CS = 0x08)
        let kernel_frame = InterruptFrame::new_test_frame(14, KERNEL_TEXT_BASE, 0x08);
        assert!(!kernel_frame.is_user_mode());

        // User mode frame (CS = 0x23)
        let user_frame = InterruptFrame::new_test_frame(14, 0x400000, 0x23);
        assert!(user_frame.is_user_mode());

        // 回归防护: 内核 CS 且 RIP 落在低半区 —— 旧的 RIP 启发式在此会误判为
        // 用户态 (内核 .text 链接于低半区, 该条件对内核态异常恒真).
        let anomalous_frame = InterruptFrame::new_test_frame(0, 0x1221d7, 0x08);
        assert!(!anomalous_frame.is_user_mode()); // 仅按 CS.RPL: 内核 CS 即内核态
    }

    #[test]
    fn test_idt_entry_creation() {
        let entry = IdtEntry::new(0xDEADBEEFCAFEBABE, GDT_KERNEL_CODE, IDT_TYPE_INTERRUPT);
        // IdtEntry 是 packed 结构: &entry.field 会构造未对齐引用 (E0793),
        // 故先按值取出字段再断言.
        assert_eq!({ entry.offset_low }, 0xBABE);
        assert_eq!({ entry.selector }, GDT_KERNEL_CODE);
        assert!(entry.is_present());
        assert_eq!(entry.handler_address(), 0xDEADBEEFCAFEBABE);
    }

    #[test]
    fn test_idt_ptr_creation() {
        let base_addr = 0xFFFF800000001000;
        let ptr = IdtPtr::new(base_addr);
        // 理由同 test_idt_entry_creation: IdtPtr 是 packed 结构.
        assert_eq!({ ptr.base }, base_addr);
        assert_eq!({ ptr.limit }, (IDT_ENTRIES * 16 - 1) as u16);
    }

    #[test]
    fn test_statistics_recording() {
        let stats = InterruptStatistics::new();

        stats.record_exception(0); // Division By Zero
        stats.record_exception(14); // Page Fault
        stats.record_irq(0); // Timer

        assert_eq!(stats.get_count(0), 1);
        assert_eq!(stats.get_count(14), 1);
        assert_eq!(stats.get_count(IRQ_BASE), 1);
        assert_eq!(stats.get_count(100), 0); // Invalid vector
    }

    #[test]
    fn test_error_flags() {
        let flags = ErrorFlags::PRESENT | ErrorFlags::WRITE | ErrorFlags::USER;
        assert!(flags.contains(ErrorFlags::PRESENT));
        assert!(flags.contains(ErrorFlags::WRITE));
        assert!(flags.contains(ErrorFlags::USER));
        assert!(!flags.contains(ErrorFlags::RESERVED));
    }

    #[test]
    fn test_exception_names() {
        assert_eq!(get_exception_name(0), "Division By Zero");
        assert_eq!(get_exception_name(14), "Page Fault");
        assert_eq!(get_exception_name(99), "Unknown");
    }

    #[test]
    fn test_irq_names() {
        assert_eq!(get_irq_name(0), "Timer");
        assert_eq!(get_irq_name(1), "Keyboard");
        assert_eq!(get_irq_name(20), "Unknown");
    }
}
