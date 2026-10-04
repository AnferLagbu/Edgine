//! # IDT 核心管理器
//!
//! 全局中断描述符表管理，提供线程安全的 IDT 操作接口。
//!
//! ## 架构设计
//!
//! ```text
//! IdtManager (全局单例)
//!   ├── inner: IrqSpinLock<IdtState>   (中断安全, 锁内自动 cli)
//!   │     ├── entries`[256]`    (IDT 门描述符)
//!   │     ├── handlers`[256]`   (异常/IRQ 处理函数)
//!   │     └── irq_desc`[16]`    (IRQ 扩展信息)
//!   ├── stats: InterruptStatistics (无锁统计)  ← Phase 1
//!   ├── detailed_stats: DetailedStatistics      ← Phase 3
//!   └── exception_handlers: Trait objects       ← Phase 3
//! ```
//!
//! ## 安全性
//!
//! - `state` 字段为 `IrqSpinLock` (来自 `framework::sync::irq_spinlock`),
//!   锁内自动屏蔽中断, 防止中断处理程序与线程争用同一锁导致死锁.
//! - 中断上下文 (`handle_irq`/`handle_exception`) 仅获取 `IrqSpinLock`,
//!   禁止使用第三方 `spin::Mutex`.

use core::sync::atomic::{AtomicU64, Ordering};

use super::handlers::{RecoveryAction, create_handler};
use super::statistics::DetailedStatistics;
#[cfg(target_arch = "x86_64")]
use super::types::IdtPtr;
use super::types::{
    GDT_KERNEL_CODE, IDT_DPL_USER, IDT_ENTRIES, IDT_TYPE_INTERRUPT, IDT_TYPE_TRAP, IRQ_BASE,
    IRQ_FLAG_SHARED, IdtEntry, InterruptFrame, InterruptStatistics, IrqDescriptor,
};
use crate::framework::sync::IrqSpinLock;

use crate::framework::sync::OnceLock;
use crate::klog_info;
// 内联硬件操作函数 (避免跨模块导入问题)
/// 从端口读字节
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
#[expect(
    clippy::inline_always,
    reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
)]
unsafe fn port_inb(port: u16) -> u8 {
    crate::arch!(inb(port))
}

/// 向端口写字节
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn port_outb(port: u16, value: u8) {
    crate::arch!(outb(port, value));
}

// I-25: legacy 8259A PIC 假性 IRQ 计数 (仅 x86_64, APIC 路径下不递增).
#[cfg(target_arch = "x86_64")]
static SPURIOUS_IRQ_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// 读取 8259A 假性 IRQ 计数 (用于调试/procfs).
#[cfg(target_arch = "x86_64")]
pub fn spurious_irq_count() -> u64 {
    SPURIOUS_IRQ_COUNT.load(core::sync::atomic::Ordering::Relaxed)
}

/// 8259A PIC 假性 IRQ 检测 (I-25).
///
/// 仅对 IRQ7 (master) / IRQ15 (slave) 进行检查; 其他 IRQ 返回 `None` 表示"非假性
/// 候选, 直接进入正常路径". 真实判定通过 `OCW3 = 0x0B` 读 ISR 寄存器, 对应 bit
/// 为 0 即为假性中断.
///
/// 返回 `Some(true)` = 假性 IRQ, 调用方应跳过 handler / softirq;
/// 返回 `Some(false)` = 真实 IRQ, 进入正常路径;
/// 返回 `None` = 非 IRQ7/IRQ15 候选, 跳过检测.
#[cfg(target_arch = "x86_64")]
fn detect_spurious_8259_irq(irq: u8) -> Option<bool> {
    if irq != 7 && irq != 15 {
        return None;
    }
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    let isr = unsafe { read_8259_isr(irq >= 8) };
    let bit = 1u8 << 7; // IRQ7 / IRQ15 都是 bit 7
    Some(isr & bit == 0)
}

/// 读取 8259A 主/从 ISR (In-Service Register).
/// 通过 OCW3 = 0x0B 触发读 ISR (vs IRR); 返回 8-bit 当前在服务中断位图.
// SAFETY: 调用方保证指针/类型有效 (详见上下文) — 仅 I/O 端口读写, 不涉及指针解引用
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn read_8259_isr(slave: bool) -> u8 {
    let cmd_port: u16 = if slave { 0xA0 } else { 0x20 };
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        port_outb(cmd_port, 0x0B);
        port_inb(cmd_port)
    }
}

/// I/O 等待
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn io_wait() {
    unsafe {
        port_outb(0x80, 0);
    }
}

/// 重映射 8259A PIC: IRQ0-7→vec32-39, IRQ8-15→vec40-47
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn remap_pic() {
    unsafe {
        let m = port_inb(0x21);
        let s = port_inb(0xA1);
        port_outb(0x20, 0x11);
        io_wait();
        port_outb(0xA0, 0x11);
        io_wait();
        port_outb(0x21, 0x20);
        io_wait();
        port_outb(0xA1, 0x28);
        io_wait();
        port_outb(0x21, 0x04);
        io_wait();
        port_outb(0xA1, 0x02);
        io_wait();
        port_outb(0x21, 0x01);
        io_wait();
        port_outb(0xA1, 0x01);
        io_wait();
        port_outb(0x21, 0xFF);
        port_outb(0xA1, 0xFF);
        let _ = (m, s);
    }
}

/// 禁用中断
#[inline(always)]
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn cli() {
    let _ = crate::arch!(interrupt_disable());
}

/// Halt 循环 (永不返回)
fn halt_loop() -> ! {
    loop {
        crate::arch!(halt());
    }
}

/// IDT 状态 (受 Mutex 保护)
pub(crate) struct IdtState {
    /// IDT 条目表
    pub entries: [IdtEntry; IDT_ENTRIES],
    /// 异常处理函数指针表
    /// `x86_64` 上 wrapper 函数(asm stub 跳转)用 `extern "C"`,等价于平台 sysv64 ABI
    pub handlers: [Option<extern "C" fn(*mut InterruptFrame)>; IDT_ENTRIES],
    /// IRQ 描述符扩展信息 (B07: 16 → 128, 覆盖 MSI vector 32-127)
    pub irq_descriptors: [IrqDescriptor; 128],
}

impl Default for IdtState {
    fn default() -> Self {
        // 手动初始化 irq_descriptors 数组 (IrqDescriptor 不是 Copy)
        let irq_descs = [const {
            IrqDescriptor {
                handler: None,
                name: "",
                description: "",
                flags: 0,
                call_count: core::sync::atomic::AtomicU64::new(0),
                error_count: core::sync::atomic::AtomicU64::new(0),
            }
        }; 128];

        Self {
            entries: [IdtEntry::default(); IDT_ENTRIES],
            handlers: [None; IDT_ENTRIES],
            irq_descriptors: irq_descs,
        }
    }
}

/// 全局 IDT 管理器
pub struct IdtManager {
    /// 内部状态 (`IrqSpinLock` 保护, 中断安全)
    ///
    /// 锁内自动屏蔽中断, 防止中断处理程序与线程争用同一锁导致死锁.
    /// 所有中断上下文访问 (`handle_irq`/`handle_exception`) 仅使用本字段.
    pub(crate) state: IrqSpinLock<IdtState>,
    /// 基础统计 (Phase 1)
    pub stats: InterruptStatistics,
    /// 详细统计 (Phase 3)
    pub detailed_stats: DetailedStatistics,
    /// 嵌套中断计数
    pub nested_count: AtomicU64,
    /// 当前正在处理的中断向量
    current_vector: AtomicU64,
}

// 全局单例实例
static IDT_MANAGER_INSTANCE: OnceLock<IdtManager> = OnceLock::new();

/// 返回 IDT 条目表的低半区线性地址 (LMA, 与 `lidt` 装载的 `IDTR.BASE` 同源)。
///
/// KPTI 收窄后用户页表须显式映射该页: 用户态触发中断/异常时, CPU 在切换 CR3
/// **之前**就经 `IDTR.BASE` 取门描述符, 故该地址必须恒等可见 (低半区, 无
/// `KERNEL_BASE` 别名, 因为 `load_idt` 装载的就是 `entries` 的链接地址)。
///
/// 可在 `idt_init` 之前调用: 单例存储位于静态区, 地址链接期确定, 本函数只取
/// 地址 (不依赖门已填充, `get_or_init` 此处仅做零值初始化)。
#[cfg(target_arch = "x86_64")]
pub fn idt_entries_base_lma() -> u64 {
    let state = IdtManager::instance().state.lock();
    core::ptr::from_ref(&state.entries).cast::<u8>() as u64
}

impl IdtManager {
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    /// 获取全局 IDT 管理器实例
    pub fn instance() -> &'static Self {
        IDT_MANAGER_INSTANCE.get_or_init(|slot| {
            slot.write(Self {
                state: IrqSpinLock::new(IdtState::default()),
                stats: InterruptStatistics::new(),
                detailed_stats: DetailedStatistics::new(), // Phase 3
                nested_count: AtomicU64::new(0),
                current_vector: AtomicU64::new(0xFFFFFFFFFFFFFFFF),
            });
        })
    }

    /// 初始化 IDT 子系统
    ///
    /// # Arguments
    /// * `isr_table` - ISR handler 地址表 (32 个异常)
    /// * `irq_table` - IRQ handler 地址表 (16 个 IRQ)
    /// * `syscall_handler` - 系统调用门地址
    /// * `isr0x82` - 恢复中断 (int 0x82) 地址
    ///
    /// # Returns
    /// - `Ok(())`: 初始化成功
    #[cfg_attr(
        target_arch = "aarch64",
        expect(
            clippy::unnecessary_wraps,
            reason = "aarch64 IDT init 占位函数, 直接返回 Ok"
        )
    )]
    /// - `Err(msg)`: 初始化失败
    /// # Errors
    /// IDT 初始化失败 (如关键 IST 未配置) 时返回 Err。
    pub fn init(
        &self,
        isr_table: &[u64; 32],
        irq_table: &[u64; 16],
        syscall_handler: u64,
        isr0x82: u64,
    ) -> Result<(), &'static str> {
        // I-24: 启动顺序契约 — TSS init (set_ist[0..4]) 必须在 IDT init 之前完成.
        // 校验关键 IST (0-3) 已填充非零栈顶, 避免 #DF/NMI/#PF/0x82 触发时切换到 0 栈顶.
        // 注释格式: IDT IST=N → TSS ist[N-1]
        //   #DF  (vec 8)  → IDT IST=1 → TSS ist[0]
        //   NMI  (vec 2)  → IDT IST=2 → TSS ist[1]
        //   #PF  (vec 14) → IDT IST=4 → TSS ist[3]
        //   0x82 (恢复)   → IDT IST=3 → TSS ist[2]
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            let tss = unsafe { crate::framework::arch::gdt::get_tss_mut() };
            if !tss.ist_validated() {
                return Err("IDT init: TSS IST[0..3] not initialized (call set_ist first)");
            }
            crate::klog_info!(Kernel, "IDT init: TSS IST validated ok");
        }

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            remap_pic();
        }

        let mut state = self.state.lock();

        // 1. 清空所有条目
        for i in 0..IDT_ENTRIES {
            state.entries[i] = IdtEntry::default();
            state.handlers[i] = None;
        }

        // 2. 设置异常门描述符 (向量 0-31)
        for i in 0..32u8 {
            self.set_gate_internal(
                &mut state,
                i,
                isr_table[i as usize],
                GDT_KERNEL_CODE,
                IDT_TYPE_INTERRUPT,
            );
        }

        // 2a. 为关键异常设置 IST 专用栈 (格式: IDT IST=N → TSS ist[N-1])
        // Double Fault (#DF, 向量 8) → IDT IST=1 → TSS ist[0]
        state.entries[8] = IdtEntry::new_with_ist(
            isr_table[8],
            GDT_KERNEL_CODE,
            IDT_TYPE_INTERRUPT,
            1, // IDT IST=1 → TSS ist[0]
        );
        // NMI (vector 2) → IDT IST=2 → TSS ist[1]
        state.entries[2] = IdtEntry::new_with_ist(
            isr_table[2],
            GDT_KERNEL_CODE,
            IDT_TYPE_INTERRUPT,
            2, // IDT IST=2 → TSS ist[1]
        );
        // Page Fault (#PF, 向量 14) → IDT IST=4 → TSS ist[3]
        // 独立 IST 栈防止 COW/page fault 处理中的递归嵌套导致三重故障
        state.entries[14] = IdtEntry::new_with_ist(
            isr_table[14],
            GDT_KERNEL_CODE,
            IDT_TYPE_INTERRUPT,
            4, // IDT IST=4 → TSS ist[3]
        );

        // 3. 设置 IRQ 门描述符 (向量 32-47)
        for i in 0..16u8 {
            let vector = IRQ_BASE + i;
            self.set_gate_internal(
                &mut state,
                vector,
                irq_table[i as usize],
                GDT_KERNEL_CODE,
                IDT_TYPE_INTERRUPT,
            );
        }

        // 4. 设置系统调用门 (int 0x80, DPL=3 允许 user 调用)
        self.set_gate_internal(
            &mut state,
            0x80,
            syscall_handler,
            GDT_KERNEL_CODE,
            IDT_TYPE_TRAP | IDT_DPL_USER,
        );

        // 5. 设置恢复中断 (int 0x82, freg-stack) → IDT IST=3 → TSS ist[2]
        state.entries[0x82] = IdtEntry::new_with_ist(
            isr0x82,
            GDT_KERNEL_CODE,
            IDT_TYPE_TRAP,
            3, // IDT IST=3 → TSS ist[2]
        );

        drop(state); // 释放锁，准备加载 IDT

        // 6. 加载 IDT 到 CPU
        #[cfg(target_arch = "x86_64")]
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            self.load_idt();
        }

        Ok(())
    }

    /// 编程 MSI 向量 IDT 条目 (向量 0x40-0x9F)
    ///
    /// MSI 向量范围 0x40-0x9F 对应 irq16-irq127 (`MSI_VECTOR_BASE=0x40`),
    /// 共 112 个 stub.
    /// 这些 IDT 条目使用与传统 IRQ 相同的 `irq_common` 入口.
    /// 当前 IDT stub 实际覆盖 0x40-0x9F (irq16-irq127),
    /// 而 `MSI_VECTOR_COUNT=128` 超出此范围 (0xA0-0xBF), 驱动需在 alloc
    /// 后验证落入实际 stub 范围.
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn init_msi_idt(&self, msi_table: &[u64; 112]) {
        let mut state = self.state.lock();
        for (i, &handler_addr) in msi_table.iter().enumerate() {
            let vector = 0x40 + i as u8;
            self.set_gate_internal(
                &mut state,
                vector,
                handler_addr,
                GDT_KERNEL_CODE,
                IDT_TYPE_INTERRUPT,
            );
        }
        crate::klog_info!(Kernel, "IDT: MSI vectors 0x40-0x9F programmed (112 stubs)");
    }

    /// 编程 IPI 向量 IDT 条目 (向量 0xFD TLB 失效 / 0xFE reschedule)
    ///
    /// 两个 IPI 向量使用与传统 IRQ 相同的 `irq_common` 入口, DPL0 且不占用 IST.
    /// 分发由 `handle_irq` 的向量前置分支承担, 不进入 `irq_descriptors` 表.
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "trivially_copy_pass_by_ref: 小类型传引用而非值是既有 API 约定 (改传值会波及调用点); 当前优先 expect"
    )]
    #[expect(clippy::cast_possible_truncation)]
    pub fn init_ipi_idt(&self, ipi_table: &[u64; 2]) {
        let mut state = self.state.lock();
        for (i, &handler_addr) in ipi_table.iter().enumerate() {
            let vector = 0xFD + i as u8;
            self.set_gate_internal(
                &mut state,
                vector,
                handler_addr,
                GDT_KERNEL_CODE,
                IDT_TYPE_INTERRUPT,
            );
        }
        crate::klog_info!(Kernel, "IDT: IPI vectors 0xFD/0xFE programmed (2 stubs)");
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 内部函数: 设置门描述符 (需要 &mut state)
    fn set_gate_internal(
        &self,
        state: &mut IdtState,
        num: u8,
        handler: u64,
        selector: u16,
        type_attr: u8,
    ) {
        let entry = &mut state.entries[num as usize];
        *entry = IdtEntry::new(handler, selector, type_attr);
    }

    /// 加载 IDT 到 CPU (lidt 指令)
    ///
    /// # Safety
    /// 调用方需保证 IDT 表已初始化, 且当前 CPU 的中断已屏蔽.
    #[cfg(target_arch = "x86_64")]
    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    pub(crate) unsafe fn load_idt(&self) {
        unsafe {
            let state = self.state.lock();
            let base_addr = state.entries.as_ptr() as u64;

            let idt_ptr = IdtPtr::new(base_addr);

            core::arch::asm!(
                "lidt [{0}]",
                in(reg) &idt_ptr,
                options(nostack, preserves_flags)
            );
        }
    }

    /// 注册异常处理函数
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn set_exception_handler(&self, vector: u8, handler: extern "C" fn(*mut InterruptFrame)) {
        if vector < IDT_ENTRIES as u8 {
            let mut state = self.state.lock();
            state.handlers[vector as usize] = Some(handler);
        }
    }

    /// 注册 IRQ 处理函数
    /// # Errors
    /// IRQ 编号大于等于 128 时返回 Err。
    ///
    /// B07 MSI-X: 范围扩展到 128 项 (irq 0-127). 传统 PIC IRQ 0-15 (vector 32-47)
    /// 与 MSI vector 32-127 (vector 64-175, 即 MSI 0x40-0x9F) 共用同一数组.
    /// MSI-X 用户应使用 `register_msi_irq` (绕过 8259 spurious 检测).
    pub fn register_irq(
        &self,
        irq: u8,
        handler: extern "C" fn(*mut InterruptFrame),
        name: &'static str,
        flags: u32,
    ) -> Result<(), &'static str> {
        if irq >= 128 {
            return Err("Invalid IRQ number");
        }

        let mut state = self.state.lock();
        let vector = (IRQ_BASE + irq) as usize;

        // 检查是否已有 handler (非共享模式下警告)
        if (flags & IRQ_FLAG_SHARED) == 0 && state.handlers[vector].is_some() {
            // Log warning (Phase 3 实现)
        }

        // 更新 descriptor
        state.irq_descriptors[irq as usize] = IrqDescriptor {
            handler: Some(handler),
            name,
            description: "",
            flags,
            call_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
        };

        // 注册 handler
        state.handlers[vector] = Some(handler);

        Ok(())
    }

    /// 注册 MSI-X IRQ 处理函数
    ///
    /// 与 `register_irq` 区别:
    /// - irq 范围扩展到 32-127 (MSI vector 0x40-0x9F)
    /// - 不进行 8259 spurious 检测 (MSI 不走 8259)
    /// - EOI 走 LAPIC (APIC 已初始化时), 兜底走 8259 EOI
    /// # Errors
    /// IRQ 编号大于等于 128 或小于 32 时返回 Err。
    pub fn register_msi_irq(
        &self,
        irq: u8,
        handler: extern "C" fn(*mut InterruptFrame),
        name: &'static str,
    ) -> Result<(), &'static str> {
        if irq >= 128 {
            return Err("Invalid MSI IRQ number");
        }
        if irq < 32 {
            return Err("MSI irq must be >= 32 (vector 0x40+); use register_irq for PIC IRQ");
        }
        self.register_irq(irq, handler, name, 0)
    }

    /// 注销 IRQ 处理函数
    /// # Errors
    /// IRQ 编号大于等于 128 时返回 Err。
    pub fn unregister_irq(
        &self,
        irq: u8,
        handler: extern "C" fn(*mut InterruptFrame),
    ) -> Result<(), &'static str> {
        if irq >= 128 {
            return Err("Invalid IRQ number");
        }

        let mut state = self.state.lock();
        let vector = (IRQ_BASE + irq) as usize;

        // 匹配 handler 后注销
        if let Some(registered) = state.handlers[vector] {
            let registered_ptr = registered as *const ();
            let input_ptr = handler as *const ();

            if registered_ptr == input_ptr {
                state.handlers[vector] = None;
                state.irq_descriptors[irq as usize] = IrqDescriptor::empty();
                return Ok(());
            }
        }

        Err("Handler not found")
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 启用指定 IRQ
    pub fn enable_irq(&self, irq: u8) {
        // 使用 GSI 路由, 不再限制 irq < 16
        #[cfg(target_arch = "x86_64")]
        {
            if crate::framework::arch::ioapic::is_initialized() {
                crate::framework::arch::ioapic::unmask_irq(irq);
                return;
            }
        }
        // 兜底: 传统 PIC (8259A), 仅 irq < 16
        if irq >= 16 {
            return;
        }
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            if irq < 8 {
                let mask = port_inb(0x21) & !(1 << irq);
                port_outb(0x21, mask);
            } else {
                let slave_mask = port_inb(0xA1) & !(1 << (irq - 8));
                port_outb(0xA1, slave_mask);

                let master_mask = port_inb(0x21) & !(1 << 2);
                port_outb(0x21, master_mask);
            }
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 禁用指定 IRQ
    pub fn disable_irq(&self, irq: u8) {
        // 使用 GSI 路由, 不再限制 irq < 16
        #[cfg(target_arch = "x86_64")]
        {
            if crate::framework::arch::ioapic::is_initialized() {
                crate::framework::arch::ioapic::mask_irq(irq);
                return;
            }
        }
        // 兜底: 传统 PIC (8259A), 仅 irq < 16
        if irq >= 16 {
            return;
        }
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            if irq < 8 {
                let mask = port_inb(0x21) | (1 << irq);
                port_outb(0x21, mask);
            } else {
                let mask = port_inb(0xA1) | (1 << (irq - 8));
                port_outb(0xA1, mask);
            }
        }
    }

    /// 处理异常 (从 `exception_handler` FFI 调用)
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    pub fn handle_exception(&self, frame: *mut InterruptFrame) {
        if frame.is_null() {
            return;
        }

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let vector = (*frame).int_no as u8;

            let nesting = self.nested_count.fetch_add(1, Ordering::SeqCst);
            self.current_vector
                .store(u64::from(vector), Ordering::SeqCst);

            self.stats.record_exception(vector);
            self.detailed_stats.record_exception(vector, &*frame);
            self.detailed_stats.record_nested(nesting + 1);

            if vector < 32 {
                let exception_handler = create_handler(vector);

                let action = exception_handler.handle(frame);

                // 埋点 (见 docs/plan/tlb-shootdown-epoch.md S-10): 用户态异常现场.
                // `handlers.rs` 对用户态 #DE/#GPF 一律 `TerminateProcess(1)`, 对用户态
                // #PF 用 `TerminateProcess(pid)` —— 故 "exit code=1" 与 "exit code=pid"
                // 指向完全不同的异常源. 异常处理器自身不打印现场, 无本行就无法把
                // "进程以 code=1 终止" 归因到具体异常向量与 RIP. 内核态异常另有 Panic
                // 路径 (不在此重复).
                //
                // 仅在**未被恢复**时打印: 用户态缺页含大量正常路径 (fork 后 COW 写
                // 共享页 / 栈扩展 / demand paging), 处理器返回 `Recovered` 后原指令
                // 重试即成功, 属正常执行而非异常; 一律按 ERR 打印会把正常缺页误判为故障.
                if (*frame).is_user_mode() && !matches!(action, RecoveryAction::Recovered) {
                    // 帧为 packed 结构, 字段须先取出到局部变量再按值传参 (E0793).
                    let err = (*frame).err_code;
                    let rip = (*frame).rip;
                    let rsp = (*frame).rsp;
                    let cr2 = (*frame).fault_address();
                    crate::klog_err!(
                        Kernel,
                        "[IDT] user exception: vec={} err={:#X} rip={:#X} rsp={:#X} cr2={:#X}",
                        vector,
                        err,
                        rip,
                        rsp,
                        cr2
                    );
                }

                self.detailed_stats.record_recovery_action(&action);

                self.execute_recovery_action(&action, &*frame);

                if let Some(c_handler) = self.state.lock().handlers[vector as usize] {
                    c_handler(frame); // C handler 可能会执行额外的副作用
                }
            } else if (vector as usize) >= IRQ_BASE as usize
                && (vector as usize) < IRQ_BASE as usize + 16
            {
                // IRQ 处理
                self.handle_irq(frame, vector);
            } else {
                // 其他向量 (syscall, recovery 等)
                match vector {
                    0x80 => { /* System call - handled by syscall handler */ }
                    0x82 => { /* Recovery interrupt - handled by freg-stack */ }
                    _ => {} // 忽略未知向量
                }
            }

            // 恢复嵌套计数
            self.nested_count.fetch_sub(1, Ordering::SeqCst);
            self.current_vector
                .store(0xFFFFFFFFFFFFFFFF, Ordering::SeqCst);
        }
    }

    /// 执行恢复动作 (Phase 3 结构化错误处理)
    fn execute_recovery_action(&self, action: &RecoveryAction, _frame: &InterruptFrame) {
        match action {
            RecoveryAction::Recovered => {
                // 成功恢复，无需额外操作
            }

            RecoveryAction::TerminateProcess(exit_code) => {
                // User-mode 异常：终止进程
                // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
                unsafe extern "C" {
                    fn process_exit(code: u32);
                }
                // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
                unsafe extern "C" {
                    fn scheduler_yield();
                }

                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    process_exit(*exit_code);
                    scheduler_yield();
                }
            }

            RecoveryAction::DomainRecovery => {
                // 尝试域级恢复 (freg-stack)
                // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
                unsafe extern "C" {
                    fn recovery_try_recover_from_idt() -> i32;
                }

                // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                unsafe {
                    let result = recovery_try_recover_from_idt();
                    match result {
                        0 => {} // Recovery 成功
                        -2 => {
                            // 已尝试过，拒绝循环 → panic
                            self.kernel_panic("Recovery already attempted");
                        }
                        _ => {
                            // Recovery 失败 → panic
                            self.kernel_panic("Domain recovery failed");
                        }
                    }
                }
            }

            RecoveryAction::Panic(info) => {
                // 无法恢复 → kernel panic
                self.kernel_panic(info.reason);
            }
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// Kernel panic (停止系统)
    fn kernel_panic(&self, message: &str) {
        let _ = message;

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            cli();
            halt_loop();
        }
    }

    /// 处理 IRQ
    pub fn handle_irq(&self, frame: *mut InterruptFrame, vector: u8) {
        // IPI 分支: 必须前置于 `vector - IRQ_BASE` 之前.
        // IRQ_BASE=32, 对 0xFD/0xFE 得 221/222, 越界直索
        // irq_descriptors[128] ⇒ 中断上下文 panic.
        // 0xFD (TLB 失效): 收敛入口 `smp::tlb_catch_up_local` 完成
        // "先读当前代 → 全量刷新本核 TLB → 声明本核已追平该代" (次序不可颠倒,
        // 否则"假追平"); 随后登记运行期探针观测 (未装备时为空操作).
        // 不进入 do_softirq/信号投递路径.
        if vector == 0xFD {
            crate::framework::smp::tlb_catch_up_local();
            crate::framework::smp::tlb_probe_report();
            self.send_eoi(0);
            return;
        }
        // 0xFE (reschedule): 接线既有 resched_ipi_handler 登记本核挂起重调度;
        // reschedule 天然 fire-and-forget, 不等 ack. EOI 之后经统一调度点
        // 执行切换 (本路径不进 do_softirq/信号投递).
        if vector == 0xFE {
            crate::framework::proc::cpu_queue::resched_ipi_handler();
            self.send_eoi(0);
            crate::framework::proc::cpu_queue::run_pending_resched();
            return;
        }

        let irq = vector - IRQ_BASE;

        // B07 MSI-X: irq >= 16 走 MSI 路径 (irq 16-31 为预留, 32-63 为 MSI vector).
        // MSI 不走 8259 PIC, 不需 spurious 检测, EOI 走 LAPIC.
        if irq >= 16 {
            // MSI 分支: vector 0x40-0x7F (irq 32-63) 由 LAPIC 直接投递,
            // 不经过 8259A. 跳过 spurious 检测, 直接查 handler 表.
            self.stats.record_irq(irq);

            let handler_opt = {
                let state = self.state.lock();
                let handler = state.irq_descriptors[irq as usize].handler;
                if let Some(desc) = state.irq_descriptors.get(irq as usize) {
                    desc.call_count.fetch_add(1, Ordering::Relaxed);
                }
                handler
            };

            if let Some(handler) = handler_opt {
                handler(frame);
            }

            // MSI EOI: LAPIC 路径 (send_eoi 内已检查 APIC init)
            self.send_eoi(irq);

            crate::framework::irq::do_softirq();

            // 信号投递 (同传统 IRQ 路径)
            // SAFETY: frame 有效
            unsafe {
                let f = &*frame;
                if f.cs & 0x3 == 0x3 {
                    crate::framework::proc::do_signal_deliver(frame);
                }
            }

            // 中断退出前执行挂起重调度 (须在 do_softirq 返回之后)
            crate::framework::proc::cpu_queue::run_pending_resched();
            return;
        }

        if irq < 16 {
            // I-25: legacy 8259A PIC 假性 IRQ 检测.
            // IRQ7 (master) 与 IRQ15 (slave) 在级联时可能产生假性中断;
            // 8259A 通过 OCW3=0x0B 读 ISR 寄存器确认: 若对应 bit 为 0 即为假性.
            // 假性 IRQ 不应调用 handler, 也不计入 irq_counts 有效统计.
            // - master 假性 (IRQ7): 无需 EOI (不发送 0x20, 否则会误清 pending IRQ)
            // - slave 假性 (IRQ15): 仅向 master 发送 EOI (0x20), 不向 slave 发送 (0xA0)
            #[cfg(target_arch = "x86_64")]
            {
                if let Some(spurious) = detect_spurious_8259_irq(irq) {
                    if spurious {
                        SPURIOUS_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
                        if irq >= 8 {
                            // slave 假性: 仅 EOI master, 避免误清 slave 上未决的
                            // 真实 IRQ
                            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
                            unsafe {
                                port_outb(0x20, 0x20);
                            }
                        }
                        // master 假性: 不发送 EOI; 两者均跳过 handler / softirq
                        return;
                    }
                }
            }

            self.stats.record_irq(irq);

            let handler_opt = {
                let state = self.state.lock();
                let handler = state.irq_descriptors[irq as usize].handler;
                if let Some(desc) = state.irq_descriptors.get(irq as usize) {
                    desc.call_count.fetch_add(1, Ordering::Relaxed);
                }
                handler
            };

            if let Some(handler) = handler_opt {
                handler(frame);
            }

            self.send_eoi(irq);

            crate::framework::irq::do_softirq();

            // 返回用户态前检查待投递信号
            // SAFETY: frame 有效, IRQ 处理完成, 即将 iretq
            unsafe {
                let f = &*frame;
                // 仅在返回用户态时检查 (CS 低2位=3 表示用户态)
                if f.cs & 0x3 == 0x3 {
                    crate::framework::proc::do_signal_deliver(frame);
                }
            }

            // 中断退出前执行挂起重调度 (须在 do_softirq 返回之后)
            crate::framework::proc::cpu_queue::run_pending_resched();
        } else {
            // MSI 向量 (0x40-0x7F → irq 0x10-0x3F): 通过 ISR_TABLE 分发
            crate::framework::irqline::dispatch_irq(vector);
            self.send_eoi(irq);
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 发送 EOI (End of Interrupt)
    fn send_eoi(&self, irq: u8) {
        // Use Local APIC EOI if available (modern systems); 仅 x86_64 支持 APIC.
        let apic_handled = {
            #[cfg(target_arch = "x86_64")]
            {
                if crate::framework::arch::apic::is_initialized() {
                    crate::framework::arch::apic::eoi();
                    true
                } else {
                    false
                }
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };
        if !apic_handled {
            // 兜底: 对老式系统回退到传统 PIC EOI
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                if irq >= 8 {
                    port_outb(0xA0, 0x20);
                }
                port_outb(0x20, 0x20);
            }
        }
    }

    /// 导出状态信息 (用于调试)
    pub fn dump_state(&self) {
        let state = self.state.lock();

        // 打印嵌套中断计数
        let nesting = self.nested_count.load(Ordering::Relaxed);
        let current_vec = self.current_vector.load(Ordering::Relaxed);

        // TD-12: klog 替代原 let _ = ...
        klog_info!(
            Kernel,
            "IDT dump: nesting={} current_vec={} descriptors={}",
            nesting,
            current_vec,
            state.irq_descriptors.len()
        );
    }

    /// 获取中断计数
    pub fn get_interrupt_count(&self, vector: u8) -> u64 {
        self.stats.get_count(vector)
    }

    /// 打印统计信息
    pub fn print_statistics(&self) {
        // TD-12: klog 格式化输出统计 (替换原 let _ = ...)
        klog_info!(Kernel, "IDT statistics:");
        for v in 0..32u8 {
            let count = self.stats.exception_counts[v as usize].load(Ordering::Relaxed);
            if count > 0 {
                klog_info!(Kernel, "  exception[{}] = {}", v, count);
            }
        }
        for i in 0..16u8 {
            let count = self.stats.irq_counts[i as usize].load(Ordering::Relaxed);
            if count > 0 {
                klog_info!(Kernel, "  irq[{}] = {}", i, count);
            }
        }
        let nested = self.stats.nested_interrupts.load(Ordering::Relaxed);
        if nested > 0 {
            klog_info!(Kernel, "  nested_interrupts = {}", nested);
        }
    }
}
