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

    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[inline(always)]
    pub fn irq(&self) -> u32 {
        self.irq
    }
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[inline(always)]
    pub fn vector(&self) -> u8 {
        self.vector
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "保留 Option/Result<()> 包装便于 API 兼容性 (调用方可能 match 或 .unwrap); 移除包装需同步修改调用点, 风险大"
    )]
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
    ///
    /// # Errors
    /// ISR 注册失败时返回 Err。
    pub fn on_interrupt(&mut self, handler: InterruptHandler) -> Result<(), &'static str> {
        // SAFETY: 启动阶段单线程调用, 无竞争。
        unsafe {
            register_isr(self.vector, handler);
        }
        self.registered = true;
        Ok(())
    }

    /// 启用该中断线 (unmask)
    #[cfg(target_arch = "x86_64")]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn enable(&self) {
        crate::privileged::arch::ioapic::unmask_irq(self.irq as u8);
    }

    #[cfg(target_arch = "aarch64")]
    pub fn enable(&self) {
        let _ = self;
    }

    /// 禁用该中断线 (mask)
    #[cfg(target_arch = "x86_64")]
    // 有意窄化: 显式收窄, 调用方保证值域
    #[expect(clippy::cast_possible_truncation)]
    pub fn disable(&self) {
        crate::privileged::arch::ioapic::mask_irq(self.irq as u8);
    }

    #[cfg(target_arch = "aarch64")]
    pub fn disable(&self) {
        let _ = self;
    }

    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    #[inline(always)]
    pub fn is_registered(&self) -> bool {
        self.registered
    }
}

// ============================================================================
// 内部: ISR 注册表
// ============================================================================

const MAX_ISR_VECTORS: usize = 256;

/// 全局 ISR 函数指针表, 由 idt handlers 分发调用。
/// 使用 `IrqSpinLock` 保护, 中断安全 (`dispatch_irq` 在中断上下文调用).
static ISR_TABLE: crate::privileged::sync::IrqSpinLock<
    [Option<InterruptHandler>; MAX_ISR_VECTORS],
> = crate::privileged::sync::IrqSpinLock::new([None; MAX_ISR_VECTORS]);

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
        let mut table = ISR_TABLE.lock();
        table[idx] = Some(handler);
    }
}

/// 分发中断到已注册的 handler (由 IDT ISR stub 调用)。
pub fn dispatch_irq(vector: u8) -> bool {
    let idx = vector as usize;
    if idx < MAX_ISR_VECTORS {
        let table = ISR_TABLE.lock();
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
