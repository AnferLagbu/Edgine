//! `IrqLine` — 中断线安全句柄 (TCB)
//!
//! 设备驱动通过此句柄注册 ISR, 框架负责 IDT/APIC/GIC 编排。
//! 隐藏中断向量号、中断控制器等硬件细节。
//!
//! ## 与 Asterinas OSTD `IrqLine` 的关系
//!
//! 等价于 OSTD 的 `IrqLine`。
//!
//! ## SAFETY 不变量
//!
//! - 一个 `IrqLine` 最多注册一个 ISR (可通过重新注册覆盖)。
//! - ISR 在中断上下文中调用: 不可 sleep / 不可持 Mutex / 不可阻塞。
//! - 中断向量 ≤ 255 (`x86_64`) 或 ≤ 1023 (aarch64 GIC)。

#![deny(missing_docs)]

/// 中断处理函数签名。
///
/// # 约束
/// - ISR 上下文调用 (栈深度受限, 不可睡眠)。
/// - 返回 true 表示本 handler 处理了此中断。
pub type InterruptHandler = fn() -> bool;

/// 中断线句柄。
///
/// 每个设备通过此句柄注册/注销 ISR。
pub struct IrqLine {
    vector: u8,
    irq: u32,
    registered: bool,
}

impl IrqLine {
    /// 创建中断线句柄。
    ///
    /// # SAFETY
    /// - irq 必须是有效的中断请求号。
    /// - vector 是 IDT 中断向量号。
    pub unsafe fn new(irq: u32, vector: u8) -> Self {
        Self {
            vector,
            irq,
            registered: false,
        }
    }

    #[inline]
    /// 返回该中断线对应的硬件中断请求号 (IRQ)。
    pub fn irq(&self) -> u32 {
        self.irq
    }
    #[inline]
    /// 返回该中断线对应的 IDT 中断向量号。
    pub fn vector(&self) -> u8 {
        self.vector
    }

    /// 注册 ISR 到全局中断表。
    ///
    /// # 安全约束
    /// - handler 必须在中断上下文安全 (无 sleep, 无 Mutex, 快速返回)。
    ///
    /// # B03-23 严禁持锁同步原语名单 (含 IrqSpinLock/Mutex/Semaphore)
    /// 中断上下文持锁 = 同 CPU 中断重入死锁。`on_interrupt` 内部不含编译
    /// 期强制, 完全靠以下文档约束:
    /// 1. 不调任何 Mutex/Semaphore (无论是 std::sync::Mutex 还是 kernel::sync::IrqSpinLock/Mutex)
    /// 2. 不调任何同步原语的 `lock()`
    /// 3. 不调 `schedule()` / `yield()` (可能持 sched lock)
    /// 4. 不分配内存 (kmalloc/GFP_KERNEL)
    /// 5. 不睡眠 (sleep/yield)
    ///
    /// 若需延迟处理, 通过 `raise_softirq()` 提交到底半部执行。
    pub fn on_interrupt(&mut self, handler: InterruptHandler) {
        // SAFETY: 启动阶段单线程调用, 无竞争。
        unsafe {
            register_isr(self.vector, handler);
        }
        self.registered = true;
    }

    /// 启用该中断线 (unmask)
    ///
    /// 委托 `IdtManager` 统一路由, 保证投递侧 (IOAPIC vs 8259A) 与 EOI 侧
    /// 判据一致 (见 `idt::legacy_irq_via_ioapic`), 避免 8259A 投递却仅发
    /// LAPIC EOI 导致该类 IRQ 永久停投.
    pub fn enable(&self) {
        crate::privileged::idt::IdtManager::instance()
            .enable_irq(crate::privileged::lib::low_u8_from_u32(self.irq));
    }

    /// 禁用该中断线 (mask)
    ///
    /// 委托 `IdtManager` 统一路由 (理由同 `enable`).
    pub fn disable(&self) {
        crate::privileged::idt::IdtManager::instance()
            .disable_irq(crate::privileged::lib::low_u8_from_u32(self.irq));
    }

    #[inline]
    /// 返回该中断线是否已注册 ISR。
    pub fn is_registered(&self) -> bool {
        self.registered
    }
}

// ============================================================================
// 内部: ISR 注册表
// ============================================================================

const MAX_ISR_VECTORS: usize = 256;

/// 全局 ISR 函数指针表, 由 idt handlers 分发调用。
///
/// 启动期单线程注册 (见 `on_interrupt` / `register_isr` 约束), 运行期只读;
/// 故用 `RacyCell` 无锁访问, **严禁在中断上下文持锁** (对齐 `SOFTIRQ_HANDLERS`
/// 与设备 IRQ 表的既有惯例). P-30 前误用 `IrqSpinLock` 并在 `dispatch_irq`
/// (中断上下文) 持 guard, 违反 `IrqSpinLock` 契约与本文件 B03-23 禁锁名单,
/// 是单核 #GP 经对核 `raw_lock` 自旋放大成整机冻结的根因.
static ISR_TABLE: crate::privileged::racy_cell::RacyCell<
    [Option<InterruptHandler>; MAX_ISR_VECTORS],
> = crate::privileged::racy_cell::RacyCell::new([None; MAX_ISR_VECTORS]);

/// 注册中断向量对应的 ISR 处理器。
///
/// # SAFETY
///
/// 1. 仅在启动单线程阶段 (无并发中断) 调用, 写 `ISR_TABLE` 安全
/// 2. `vector` 必须小于 `MAX_ISR_VECTORS` (内部会检查)
/// 3. `handler` 必须是 `'static` 生命周期的合法函数指针, 可被中断上下文调用
///    (不持有任何 Rust 锁, 不分配, 不睡眠)
unsafe fn register_isr(vector: u8, handler: InterruptHandler) {
    let idx = vector as usize;
    if idx < MAX_ISR_VECTORS {
        // 启动期单线程调用 (无并发), 无锁写入; 运行期本表只读.
        ISR_TABLE.get_mut()[idx] = Some(handler);
    }
}

/// 分发中断到已注册的 handler (由 IDT ISR stub 调用).
pub fn dispatch_irq(vector: u8) -> bool {
    let idx = vector as usize;
    if idx < MAX_ISR_VECTORS {
        // P-30 根治: 中断上下文绝不持锁 (本文件 B03-23 禁锁名单 + IrqSpinLock
        // "不可在中断上下文使用"). ISR_TABLE 启动期注册、运行期只读, 故无锁读
        // (对齐 SOFTIRQ_HANDLERS 惯例): 先拷出 fn 指针再调用, 不在 handler()
        // 期间持有任何借用/ guard.
        // SAFETY: ISR_TABLE 启动期单线程注册、运行期只读, 此处仅无并发读.
        let table = unsafe { ISR_TABLE.get() };
        if let Some(handler) = table[idx] {
            return handler();
        }
    }
    false
}

// SAFETY: IrqLine 句柄在设备驱动中独占, 启动阶段单线程注册, 运行时只读。
unsafe impl Send for IrqLine {}
// SAFETY: ISR_TABLE 初始化后运行时只读, 句柄字段 (vector/irq/registered) 在驱动上下文独占访问。
unsafe impl Sync for IrqLine {}
