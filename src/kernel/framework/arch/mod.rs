//! 架构相关模块
//!
//! ## 架构抽象层 (Architecture Abstraction Layer)
//!
//! 采用 **多子 trait + 超 trait** 模式：
//! - `CoreArch` — 基础核心能力 (halt, `cpu_id`, timestamp, 内存屏障)
//! - `InterruptArch` — 中断 + IPI
//! - `MmuArch` — 内存管理 + 上下文切换 + 用户态
//! - `SystemArch` — 端口IO + 电源管理
//! - `Arch` — 超 trait，要求全部子 trait
//!
//! 通过 `CurrentArch` 类型别名和 `arch!` 宏实现编译期架构分发。
//!
//! ```text
//! arch/
//!  ├── mod.rs         # CoreArch/InterruptArch/MmuArch/SystemArch 子 trait
//!  │                  # + Arch 超 trait + CurrentArch + arch! 宏
//!  ├── x86_64/
//!  │   └── mod.rs     # impl CoreArch/InterruptArch/MmuArch/SystemArch/Arch for X8664
//!  └── aarch64/
//!      └── mod.rs     # impl CoreArch/InterruptArch/MmuArch/SystemArch/Arch for Aarch64
//! ```
//!
//! ## 设计动机
//!
//! 单 `Arch` trait 在双架构下够用，但随着架构增多 (riscv64, loongarch64) 会膨胀。
//! 拆分子 trait 后：
//! - 新架构可按优先级分阶段实现 (先 `CoreArch` 跑起来, 再加 `InterruptArch`)
//! - 各子 trait 可独立单元测试
//! - 调用方可按需导入 (如 MMU 代码只需 `use MmuArch`)
//!
//! ## 安全说明
//!
//! 所有 trait 方法标记为 `unsafe` 因为它们直接操作硬件:
//! - MMIO/PMIO 操作 (端口读写)
//! - 特权指令 (cli/sti, 写 CR3, invlpg)
//! - 跨地址空间操作 (`context_switch`)
//!
//! 调用方必须确保:
//! - 中断保存/恢复成对调用
//! - 页表基地址是有效的物理地址
//! - `context_switch` 在正确的上下文中调用

// ============================================================================
// 模块声明
// ============================================================================

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

/// GICv3 初始化判据 — 架构中立纯逻辑 (无 asm / MMIO)。
///
/// `aarch64/gic.rs` 读寄存器后交本模块判定; 门控含 `host-test` 以便 host 侧
/// 逐支路单元测试失败/自校正判据 (ISSUE-RT-002)。
#[cfg(any(target_arch = "aarch64", feature = "host-test"))]
pub mod gic_logic;

/// D7: Shadow Stack (CET) + 控制流完整性
pub mod shadow_stack;

// ============================================================================
// 公共 API 导出 (便捷访问) — 避免跨子系统直接访问 arch 内部子模块
// ============================================================================

#[cfg(target_arch = "x86_64")]
pub use x86_64::X8664;
#[cfg(target_arch = "x86_64")]
pub use x86_64::acpi;
#[cfg(target_arch = "x86_64")]
pub use x86_64::apic;
#[cfg(target_arch = "x86_64")]
pub use x86_64::gdt;
#[cfg(target_arch = "x86_64")]
pub use x86_64::ioapic;
#[cfg(target_arch = "x86_64")]
pub use x86_64::tss;

#[cfg(target_arch = "aarch64")]
pub use aarch64::Aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::exception;
#[cfg(target_arch = "aarch64")]
pub use aarch64::gic;
#[cfg(target_arch = "aarch64")]
pub use aarch64::mmu;
#[cfg(target_arch = "aarch64")]
pub use aarch64::timer;
#[cfg(target_arch = "aarch64")]
pub use aarch64::uart;

// shadow_stack 公共接口 re-export — 避免跨子系统直接访问 arch::shadow_stack 内部
pub use shadow_stack::*;

// ============================================================================
// Trait 定义 — 多子 trait + 超 trait (Phase 8: refactored from monolithic Arch)
// ============================================================================

// ── CoreArch: 基础核心能力 ──────────────────────────────────────────────

/// 基础架构能力 — 任何新架构必须首先实现此 trait。
///
/// 方法:
/// - `cpu_id()` — CPU 唯一标识
/// - `timestamp()` — 高精度时间戳/计数器
/// - `halt()` — CPU 暂停直到中断
/// - `fence()` / `fence_w()` / `fence_r()` — 内存屏障
pub trait CoreArch {
    /// 获取当前 CPU ID (APIC ID / `MPIDR_EL1`)。
    fn cpu_id() -> u32;
    /// 获取高精度时间戳 (rdtsc / mrs `cntpct_el0`)。
    fn timestamp() -> u64;
    /// 序列化时间戳读取 — 确保先前指令全部完成后才读数。
    ///
    /// 用于精确时间测量 (如 TSC 频率校准), 比 `timestamp()` 慢。
    /// 默认实现与 `timestamp()` 相同; 架构在有专用序列化手段时覆写:
    /// - x86_64: `lfence` + `rdtsc`
    /// - aarch64: `isb` + `mrs cntpct_el0`
    fn timestamp_serialized() -> u64 {
        Self::timestamp()
    }
    /// 暂停 CPU 直到下一次中断 (hlt / wfi)。
    fn halt();
    /// 全内存屏障 (mfence / dsb sy)。
    fn fence();
    /// 写内存屏障 (sfence / dmb st)。
    fn fence_w();
    /// 读内存屏障 (lfence / dmb ld)。
    fn fence_r();
}

// ── InterruptArch: 中断 + 核间中断 ──────────────────────────────────────

/// 中断控制能力。
///
/// 方法:
/// - `interrupt_disable/enable/restore/is_enabled` — 中断屏蔽
/// - `interrupt_early_init` / `interrupt_late_init` — 中断子系统初始化
/// - `send_ipi` / `broadcast_ipi` — 核间中断
pub trait InterruptArch {
    /// 禁用中断并返回之前的中断状态标志 (cli / msr daifset)。
    fn interrupt_disable() -> usize;
    /// 恢复之前保存的中断状态 (写 RFLAGS / msr daif)。
    fn interrupt_restore(flags: usize);
    /// 启用中断 (sti / msr daifclr)。
    fn interrupt_enable();
    /// 检查中断是否已启用。
    fn is_interrupt_enabled() -> bool;
    /// 最小中断初始化 — 仅设置中断向量表/描述符表。
    ///
    /// `x86_64`: IDT 初始化 (`idt_init`)
    /// aarch64: `GICv3` + `VBAR_EL1` 已由 bootloader 配置, 此处为空操作
    fn interrupt_early_init();
    /// 完整中断初始化 — 包括中断控制器、IPI、定时器等。
    ///
    /// `x86_64`: GDT + IDT + APIC + SMP AP boot
    /// aarch64: `GICv3` + Exception vectors + timer (已由 entry.rs 完成, 此处为空操作)
    fn interrupt_late_init();
    /// 向目标 CPU 发送核间中断。
    fn send_ipi(target_cpu: u32, vector: u8);
    /// 向所有 CPU (不含自身) 广播 IPI。
    fn broadcast_ipi(vector: u8);
}

// ── MmuArch: 内存管理 + 上下文切换 + 用户态 ───────────────────────────

/// 内存管理与上下文切换能力。
///
/// 方法:
/// - `tlb_flush_page/all` — TLB 管理
/// - `read/write_page_table_base` — 页表切换
/// - `read_fault_address` — 页错误诊断
/// - `context_switch` — 进程上下文切换
/// - `enter_user` / `return_to_user` — 用户态入口/出口
pub trait MmuArch {
    /// 刷新单个虚拟地址的 TLB 条目 (invlpg / tlbi vaae1)。
    fn tlb_flush_page(vaddr: usize);
    /// 刷新整个 TLB (写 CR3 / tlbi vmalle1)。
    fn tlb_flush_all();
    /// 读取当前页表基地址 (mov cr3 / mrs `TTBR0_EL1`)。
    fn read_page_table_base() -> u64;
    /// 写入页表基地址 (mov to cr3 / msr `TTBR0_EL1`)。
    fn write_page_table_base(paddr: u64);
    /// 读取触发页错误的地址 (mov cr2 / mrs `FAR_EL1`)。
    fn read_fault_address() -> usize;
    /// 保存当前上下文到 `from`，从 `to` 恢复上下文。
    ///
    /// # Safety
    /// `from` 和 `to` 必须指向有效的 `ProcessContext` 内存。
    fn context_switch(from: *mut u8, to: *const u8);
    /// 进入用户态执行 (sysret / eret)，此函数不会返回。
    /// `user_cr3` 为用户页表物理地址, 在 iretq 前切换 CR3.
    /// `kstack` 为进程内核栈顶 (高半部分地址), CR3 切换前先切到该栈.
    fn enter_user(entry: usize, stack: usize, arg: usize, user_cr3: u64, kstack: u64) -> !;
    /// 从内核态返回到用户态 (iretq / eret)。
    fn return_to_user();
}

// ── SystemArch: 端口 IO + 电源管理 ────────────────────────────────────

/// 系统控制与 IO 能力。
///
/// 方法:
/// - `outb/inb/outl/inl` — 端口 IO (x86 特有，ARM 提供 stub)
/// - `shutdown` / `reboot` — 电源管理
pub trait SystemArch {
    /// 向 I/O 端口写入字节 (out dx, al)。
    fn outb(port: u16, value: u8);
    /// 从 I/O 端口读取字节 (in al, dx)。
    fn inb(port: u16) -> u8;
    /// 向 I/O 端口写入双字 (out dx, eax)。
    fn outl(port: u16, value: u32);
    /// 从 I/O 端口读取双字 (in eax, dx)。
    fn inl(port: u16) -> u32;
    /// 关机 (ACPI / PSCI `SYSTEM_OFF)，永不返回`。
    fn shutdown() -> !;
    /// 重启 (8042 / PSCI `SYSTEM_RESET)，永不返回`。
    fn reboot() -> !;
}

// ── Arch: 超 trait (委托模式) ─────────────────────────────────────────

/// 完整架构能力 — 要求所有子 trait。
/// 每个方法有默认实现，委托到对应的子 trait:
/// - `CoreArch` → CPU 标识/时间戳/停机/内存栅栏
/// - `InterruptArch` → 中断禁用/使能/恢复/查询, 发送/广播 IPI
/// - `MmuArch` → 单页/全表 TLB 刷新, 读写页表基址, 读故障地址,
///                上下文切换, 进出用户态
/// - `SystemArch` → 端口 IO 字节/双字, 关机, 重启
///
/// 新架构移植时，实现子 trait 后加 `impl Arch for MyArch {}` 即可获得完整接口。
pub trait Arch: CoreArch + InterruptArch + MmuArch + SystemArch {
    // ── 委托到 CoreArch ─────────────────────────────────
    fn cpu_id() -> u32 {
        <Self as CoreArch>::cpu_id()
    }
    fn timestamp() -> u64 {
        <Self as CoreArch>::timestamp()
    }
    fn timestamp_serialized() -> u64 {
        <Self as CoreArch>::timestamp_serialized()
    }
    fn halt() {
        <Self as CoreArch>::halt();
    }
    fn fence() {
        <Self as CoreArch>::fence();
    }
    fn fence_w() {
        <Self as CoreArch>::fence_w();
    }
    fn fence_r() {
        <Self as CoreArch>::fence_r();
    }

    // ── 委托到 InterruptArch ────────────────────────────
    fn interrupt_disable() -> usize {
        <Self as InterruptArch>::interrupt_disable()
    }
    fn interrupt_restore(flags: usize) {
        <Self as InterruptArch>::interrupt_restore(flags);
    }
    fn interrupt_enable() {
        <Self as InterruptArch>::interrupt_enable();
    }
    fn is_interrupt_enabled() -> bool {
        <Self as InterruptArch>::is_interrupt_enabled()
    }
    fn interrupt_early_init() {
        <Self as InterruptArch>::interrupt_early_init();
    }
    fn interrupt_late_init() {
        <Self as InterruptArch>::interrupt_late_init();
    }
    fn send_ipi(target_cpu: u32, vector: u8) {
        <Self as InterruptArch>::send_ipi(target_cpu, vector);
    }
    fn broadcast_ipi(vector: u8) {
        <Self as InterruptArch>::broadcast_ipi(vector);
    }

    // ── 委托到 MmuArch ─────────────────────────────────
    fn tlb_flush_page(vaddr: usize) {
        <Self as MmuArch>::tlb_flush_page(vaddr);
    }
    fn tlb_flush_all() {
        <Self as MmuArch>::tlb_flush_all();
    }
    fn read_page_table_base() -> u64 {
        <Self as MmuArch>::read_page_table_base()
    }
    fn write_page_table_base(paddr: u64) {
        <Self as MmuArch>::write_page_table_base(paddr);
    }
    fn read_fault_address() -> usize {
        <Self as MmuArch>::read_fault_address()
    }
    fn context_switch(from: *mut u8, to: *const u8) {
        <Self as MmuArch>::context_switch(from, to);
    }
    fn enter_user(entry: usize, stack: usize, arg: usize, user_cr3: u64, kstack: u64) -> ! {
        <Self as MmuArch>::enter_user(entry, stack, arg, user_cr3, kstack)
    }
    fn return_to_user() {
        <Self as MmuArch>::return_to_user();
    }

    // ── 委托到 SystemArch ──────────────────────────────
    fn outb(port: u16, value: u8) {
        <Self as SystemArch>::outb(port, value);
    }
    fn inb(port: u16) -> u8 {
        <Self as SystemArch>::inb(port)
    }
    fn outl(port: u16, value: u32) {
        <Self as SystemArch>::outl(port, value);
    }
    fn inl(port: u16) -> u32 {
        <Self as SystemArch>::inl(port)
    }
    fn shutdown() -> ! {
        <Self as SystemArch>::shutdown()
    }
    fn reboot() -> ! {
        <Self as SystemArch>::reboot()
    }
}

// ============================================================================
// CurrentArch 类型别名 (编译期架构选择)
// ============================================================================

#[cfg(target_arch = "x86_64")]
/// 当前编译目标的架构类型 — `x86_64`。
pub type CurrentArch = x86_64::X8664;

#[cfg(target_arch = "aarch64")]
/// 当前编译目标的架构类型 — AArch64。
pub type CurrentArch = aarch64::Aarch64;

// ============================================================================
// arch! 宏 — 编译期零开销架构分发
// ============================================================================

/// 编译期架构分发宏。
///
/// 展开为对 `<CurrentArch as Arch>` 的 trait 方法调用，零运行时开销。
///
/// # 用法
///
/// ```ignore
/// // 无参数调用
/// arch!(tlb_flush_all());
///
/// // 带参数调用
/// arch!(tlb_flush_page(addr));
/// arch!(outb(0x3F8, b'H'));
///
/// // 带返回值
/// let ts = arch!(timestamp());
/// ```
///
/// # 展开示例
///
/// `arch!(tlb_flush_all())` 展开为对当前架构的 `tlb_flush_all` 方法调用,
/// 形如 `<CurrentArch as Arch>::tlb_flush_all()`.
#[macro_export]
macro_rules! arch {
    ($method:ident ( $($arg:expr_2021),* $(,)? )) => {
        <$crate::framework::arch::CurrentArch as $crate::framework::arch::Arch>::$method($($arg),*)
    };
    ($method:ident ()) => {
        <$crate::framework::arch::CurrentArch as $crate::framework::arch::Arch>::$method()
    };
}
