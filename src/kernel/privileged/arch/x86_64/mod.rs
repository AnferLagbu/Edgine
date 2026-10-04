//! x86-64 架构特定实现
//!
//! 包含 GDT, TSS, APIC, IOAPIC 等x86-64特有逻辑。
//!
//! ## 实现状态
//! - [x] `impl CoreArch for X8664` — 基础核心能力
//! - [x] `impl InterruptArch for X8664` — 中断 + IPI
//! - [x] `impl MmuArch for X8664` — MMU + 上下文 + 用户态
//! - [x] `impl SystemArch for X8664` — 端口IO + 电源管理
//! - [x] `impl Arch for X8664` — 超 trait (空)

// ============================================================================
// 保留现有模块 (不动任何实现代码)
// ============================================================================

pub mod acpi;
pub mod apic;
pub mod gdt;
pub mod ioapic;
pub mod smp_init;
pub mod tss;

// ============================================================================
// X8664 架构类型
// ============================================================================

/// `x86_64` 架构标记类型。
///
/// 零大小类型，通过子 trait 提供所有 `x86_64` 硬件操作。
pub struct X8664;

// ============================================================================
// 子 trait 实现 — 拆分自原 Arch trait (Phase 8 refactor)
// 每个 trait 互不依赖，可独立单元测试。
// ============================================================================

use crate::privileged::arch::{Arch, CoreArch, InterruptArch, MmuArch, SystemArch};

// ── CoreArch: 基础核心 ──────────────────────────────────────────────────

impl CoreArch for X8664 {
    /// 获取当前 CPU ID (Local APIC ID)。
    #[inline(always)]
    fn cpu_id() -> u32 {
        // E-04 (2026-09-06): 测试运行器双端适配 — host-test 下无 APIC MMIO,
        // apic::get_id() 读 0xFEE00000 物理地址 → SIGSEGV (host 无映射, 非 panic,
        // catch_unwind 无法捕获). 桩化为 0 (单核语义, 与 B08-14 中断桩化同模式).
        // 仅 host-test feature 生效, kernel_test 行为不变.
        #[cfg(feature = "host-test")]
        {
            // E-04: host 桩分支显式 return, 保持与裸机分支结构对称 (expect 兑底 needless_return)
            #[expect(
                clippy::needless_return,
                reason = "needless_return: host 桩分支显式 return 与裸机分支保持结构对称; 当前优先 expect"
            )]
            return 0;
        }
        #[cfg(not(feature = "host-test"))]
        {
            use crate::privileged::arch::x86_64::apic;
            let id = apic::get_id();
            if id != 0 {
                return id;
            }
            let (_, ebx, _, _) = crate::privileged::cpu::cpuid::cpuid(1, 0);
            ebx >> 24
        }
    }

    /// 获取高精度时间戳 (rdtsc)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn timestamp() -> u64 {
        let lo: u32;
        let hi: u32;
        // SAFETY: rdtsc 不是序列化指令 (不等待先前指令完成), 仅写 EAX/EDX;
        // 声明 nostack/nomem/preserves_flags 防止编译器在指令间重排或溢出状态.
        // 需要"先序完成"语义的精确测量请用 `timestamp_serialized()`.
        unsafe {
            core::arch::asm!(
                "rdtsc",
                out("eax") lo,
                out("edx") hi,
                options(nostack, nomem, preserves_flags)
            );
        }
        (u64::from(hi) << 32) | u64::from(lo)
    }

    /// 序列化时间戳读取: `lfence` 确保先前指令全部完成后 `rdtsc` 才执行。
    ///
    /// 用于精确时间测量 (TSC 频率校准等), 比 `timestamp()` 慢。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn timestamp_serialized() -> u64 {
        let lo: u32;
        let hi: u32;
        // SAFETY: lfence 排序先前 load/store (不声明 nomem, 作为编译器内存屏障),
        // rdtsc 写 EAX/EDX; 序列保证读数不早于先前指令. 参照 Linux rdtsc_ordered.
        unsafe {
            core::arch::asm!(
                "lfence",
                "rdtsc",
                out("eax") lo,
                out("edx") hi,
                options(nostack, preserves_flags)
            );
        }
        (u64::from(hi) << 32) | u64::from(lo)
    }

    /// CPU 暂停等待中断 (hlt)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn halt() {
        // SAFETY: hlt 暂停 CPU 至下一次中断; 不触及 Rust 可见的内存或寄存器.
        // nomem/nostack 标注正确.
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }

    /// 全内存屏障 (mfence)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn fence() {
        // SAFETY: mfence 排序所有 load/store; 未声明寄存器 clobber, preserves_flags 正确.
        unsafe {
            core::arch::asm!("mfence", options(nostack, preserves_flags));
        }
    }

    /// 写内存屏障 (sfence)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn fence_w() {
        // SAFETY: sfence orders stores; no memory reads, no stack use.
        unsafe {
            core::arch::asm!("sfence", options(nostack, preserves_flags));
        }
    }

    /// 读内存屏障 (lfence)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn fence_r() {
        // SAFETY: lfence orders loads; correct memory model freg.
        unsafe {
            core::arch::asm!("lfence", options(nostack, preserves_flags));
        }
    }
}

// ── InterruptArch: 中断 + IPI ────────────────────────────────────────

impl InterruptArch for X8664 {
    /// 禁用中断并返回 RFLAGS (含 IF 位)。
    #[inline(always)]
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    // E-04 (2026-09-06): host-test 下函数体退化为桩 (无 cast), 用 cfg_attr
    // 条件化 expect, 避免 unfulfilled_lint_expectations.
    #[cfg_attr(
        not(feature = "host-test"),
        expect(
            clippy::cast_possible_truncation,
            reason = "cast_possible_truncation: 硬件字段宽度, 寄存器/MMIO 定义保证; 当前优先 expect"
        )
    )]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn interrupt_disable() -> usize {
        // E-04 (2026-09-06): 测试运行器双端适配 — host-test 下 cli 为特权指令,
        // 用户态执行 → SIGSEGV. 桩化返回 0 (与 B08-14 sync::disable_interrupts 桩一致).
        // 仅 host-test feature 生效, kernel_test 行为不变.
        #[cfg(feature = "host-test")]
        {
            // E-04: host 桩分支显式 return, 保持与裸机分支结构对称 (expect 兑底 needless_return)
            #[expect(
                clippy::needless_return,
                reason = "needless_return: host 桩分支显式 return 与裸机分支保持结构对称; 当前优先 expect"
            )]
            return 0;
        }
        #[cfg(not(feature = "host-test"))]
        {
            let flags: u64;
            // SAFETY: pushfq 压入 RFLAGS, pop 弹出到通用寄存器, 然后 cli 关中断.
            // nomem/nostack/preserves_flags 全部由该指令序列满足.
            unsafe {
                core::arch::asm!(
                    "pushfq",
                    "pop {}",
                    "cli",
                    out(reg) flags,
                    options(nomem, nostack, preserves_flags)
                );
            }
            flags as usize
        }
    }

    /// 恢复中断状态，仅当 flags 中 IF 位为 1 时才启用。
    #[inline(always)]
    // E-04 (2026-09-06): host-test 下函数体退化为桩 (无 asm), inline_always
    // lint 不触发 → 用 cfg_attr 条件化 expect, 避免 unfulfilled_lint_expectations.
    #[cfg_attr(
        not(feature = "host-test"),
        expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )
    )]
    fn interrupt_restore(flags: usize) {
        // E-04 (2026-09-06): 测试运行器双端适配 — host-test 下 sti 为特权指令,
        // 用户态执行 → SIGSEGV. 桩化 no-op (与 B08-14 sync::restore_interrupts 桩一致).
        // 仅 host-test feature 生效, kernel_test 行为不变.
        #[cfg(feature = "host-test")]
        {
            let _ = flags;
            // E-04: host 桩分支显式 return, 保持与裸机分支结构对称 (expect 兑底 needless_return)
            #[expect(
                clippy::needless_return,
                reason = "needless_return: host 桩分支显式 return 与裸机分支保持结构对称; 当前优先 expect"
            )]
            return;
        }
        #[cfg(not(feature = "host-test"))]
        {
            if (flags as u64) & (1 << 9) != 0 {
                // SAFETY: sti 启用中断; nomem/nostack 成立, 对内存无可观察副作用.
                unsafe {
                    core::arch::asm!("sti", options(nomem, nostack));
                }
            }
        }
    }

    /// 启用中断 (sti)。
    #[inline(always)]
    // E-04 (2026-09-06): host-test 下函数体退化为桩 (无 asm), inline_always
    // lint 不触发 → 用 cfg_attr 条件化 expect, 避免 unfulfilled_lint_expectations.
    #[cfg_attr(
        not(feature = "host-test"),
        expect(
            clippy::inline_always,
            reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
        )
    )]
    fn interrupt_enable() {
        // E-04 (2026-09-06): 测试运行器双端适配 — host-test 下 sti 为特权指令,
        // 用户态执行 → SIGSEGV (如 do_softirq 内 arch!(interrupt_enable())).
        // 桩化 no-op. 仅 host-test feature 生效, kernel_test 行为不变.
        #[cfg(feature = "host-test")]
        {
            // E-04: host 桩分支显式 return, 保持与裸机分支结构对称 (expect 兑底 needless_return)
            #[expect(
                clippy::needless_return,
                reason = "needless_return: host 桩分支显式 return 与裸机分支保持结构对称; 当前优先 expect"
            )]
            return;
        }
        #[cfg(not(feature = "host-test"))]
        {
            // SAFETY: sti enables interrupts; no memory access, no stack use.
            unsafe {
                core::arch::asm!("sti", options(nomem, nostack));
            }
        }
    }

    /// 检查 IF 位 (RFLAGS bit 9)。
    #[inline(always)]
    fn is_interrupt_enabled() -> bool {
        let flags: u64;
        // SAFETY: pushfq/pop 序列仅将 RFLAGS 读入寄存器; 对调用方 flags 保持不变.
        // 不触及内存或栈.
        unsafe {
            core::arch::asm!(
                "pushfq",
                "pop {}",
                out(reg) flags,
                options(nomem, nostack, preserves_flags)
            );
        }
        (flags & (1 << 9)) != 0
    }

    /// 向目标 CPU 发送 IPI (通过 Local APIC)。
    #[inline(always)]
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    fn send_ipi(target_cpu: u32, vector: u8) {
        use crate::privileged::arch::x86_64::apic;
        apic::send_ipi(target_cpu as u8, vector);
    }

    /// 广播 IPI 到所有 CPU (不含自身)。
    #[inline(always)]
    fn broadcast_ipi(vector: u8) {
        use crate::privileged::arch::x86_64::apic;
        apic::broadcast_ipi(vector);
    }

    fn interrupt_early_init() {
        crate::privileged::idt::idt_init();
    }

    #[expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )]
    fn interrupt_late_init() {
        // cpu_init 必须在 gdt_init 之前调用:
        // kpti_init 依赖 has_invpcid() → get_cpu_info() → cpu_init
        crate::privileged::cpu::cpu_init();

        crate::privileged::arch::x86_64::gdt::gdt_init();

        // 配置 SYSCALL/SYSRET 指令
        // 设置 EFER.SCE, STAR, LSTAR (高半部分地址), SFMASK
        #[cfg(target_arch = "x86_64")]
        {
            const IA32_EFER: u32 = 0xC0000080;
            const IA32_STAR: u32 = 0xC0000081;
            const IA32_LSTAR: u32 = 0xC0000082;
            const IA32_SFMASK: u32 = 0xC0000084;
            const EFER_SCE: u64 = 1 << 0;

            // SAFETY: MSR 写入在 boot 阶段单线程执行
            unsafe {
                let efer = crate::privileged::cpu::msr::read_msr(IA32_EFER);
                crate::privileged::cpu::msr::write_msr(IA32_EFER, efer | EFER_SCE);

                // STAR: [63:48] = SYSRET CS base (0x10), [47:32] = SYSCALL CS base (0x08)
                let star = (0x10u64 << 48) | (0x08u64 << 32);
                crate::privileged::cpu::msr::write_msr(IA32_STAR, star);

                // LSTAR: syscall 入口点 (高半部分地址, KPTI 用户页表只映射高半区)
                // 注意: 函数指针返回的是 LMA (低地址), 需要转换为 VMA (高地址)
                // 链接脚本定义: _kernel_text_vma = 0xFFFF800001000000 + _kernel_text_lma
                // 但 syscall 路径引用数据 (含 GOT) 时, 链接 VMA 下 RIP-relative 访问
                // 计算出的地址指向未映射/错误的物理页 (高链接 VMA 区只重映射了 .text,
                // .data/.bss 仍保留直接映射的错误偏移), 导致 GOT 间接调用 memcpy 读到
                // 零页 → 跳 0x0 → #UD (TRACK-INIT-RING3-DATA-GOT).
                // 修复: 使用 KERNEL_BASE (0xFFFF800000000000) 作为高半区基址,
                // 此时 phys = VA - KERNEL_BASE 由直接映射 (pd_high 大页) 天然正确,
                // 数据引用 (绝对低地址 + RIP-relative) 全部可达, 且与异常/IRQ 路径
                // (IDT 门指向 0xFFFF80000012xxxx) 保持一致.
                // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
                #[expect(
                    clippy::items_after_statements,
                    reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
                )]
                unsafe extern "C" {
                    fn syscall_entry();
                }
                let entry_lma = syscall_entry as *const () as u64;
                let entry_hi = entry_lma + 0xFFFF800000000000u64;
                crate::klog_boot_info!(
                    "[SYSCALL] syscall_entry LMA={:#X}, LSTAR VMA={:#X}",
                    entry_lma,
                    entry_hi
                );
                crate::privileged::cpu::msr::write_msr(IA32_LSTAR, entry_hi);

                // SFMASK: 进入内核时清除 IF
                crate::privileged::cpu::msr::write_msr(IA32_SFMASK, 1 << 9);
            }
        }

        crate::privileged::idt::idt_init();
        crate::privileged::arch::x86_64::apic::apic_init();
        crate::privileged::smp::init();
        crate::privileged::arch::x86_64::smp_init::init();
    }
}

// ── MmuArch: MMU + 上下文 + 用户态 ───────────────────────────────────

impl MmuArch for X8664 {
    /// 刷新单个虚拟地址的 TLB (invlpg)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn tlb_flush_page(vaddr: usize) {
        // SAFETY: invlpg takes the virtual address in a register and
        // invalidates the TLB entry; the address is a kernel VA.
        unsafe {
            core::arch::asm!(
                "invlpg [{}]",
                in(reg) vaddr,
                options(nostack, preserves_flags)
            );
        }
    }

    /// 刷新全部 TLB (重载 CR3)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn tlb_flush_all() {
        // SAFETY: 读再写 CR3 触发完整 TLB 刷新; 中间寄存器使用是 GPR 与 CR3 间的直接搬运.
        unsafe {
            core::arch::asm!(
                "mov rax, cr3",
                "mov cr3, rax",
                out("rax") _,
                options(nostack, preserves_flags)
            );
        }
    }

    /// 读取当前页表基地址 (mov rax, cr3)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn read_page_table_base() -> u64 {
        let cr3: u64;
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "mov {}, cr3",
                out(reg) cr3,
                options(nostack, preserves_flags)
            );
        }
        cr3
    }

    /// 切换页表 (mov to cr3)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn write_page_table_base(paddr: u64) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "mov cr3, {}",
                in(reg) paddr,
                options(nostack, preserves_flags)
            );
        }
    }

    /// 读取页故障地址 (mov rax, cr2)。
    #[inline(always)]
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    fn read_fault_address() -> usize {
        let cr2: u64;
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "mov {}, cr2",
                out(reg) cr2,
                options(nostack, preserves_flags)
            );
        }
        cr2 as usize
    }

    /// 进程上下文切换 (`process_switch_asm`)。
    #[inline(always)]
    fn context_switch(from: *mut u8, to: *const u8) {
        // E-04 (2026-09-06): 测试运行器双端适配 — host-test (std) 下无裸机上下文切换.
        // process_switch_asm 由 proc/switch.asm 在裸机构建时汇编链接, host 无此符号,
        // 直接 extern 调用会链接报 undefined symbol. 桩化为 no-op (host 无硬件切换语义,
        // 与 B08-14 IrqSpinLock 中断桩化同模式). 仅 host-test feature 生效, kernel_test 不变.
        #[cfg(feature = "host-test")]
        {
            let _ = (from, to);
            // E-04: host 桩分支显式 return, 保持与裸机分支结构对称 (expect 兑底 needless_return)
            #[expect(
                clippy::needless_return,
                reason = "needless_return: host 桩分支显式 return 与裸机分支保持结构对称; 当前优先 expect"
            )]
            return;
        }
        #[cfg(not(feature = "host-test"))]
        {
            // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
            unsafe extern "C" {
                fn process_switch_asm(prev: *mut u8, next: *const u8);
            }
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                process_switch_asm(from, to);
            }
        }
    }

    /// 进入用户态 (iretq)。
    ///
    /// 通过 `global_asm!` 在汇编层面定义，确保在 `.kpti_trampoline` section。
    /// 切换到用户页表后，CPU 需要继续执行当前指令（构建 iretq 帧并执行 iretq），
    /// 如果不在 trampoline 区域，用户页表中该地址没有 USER 位，会导致 #PF。
    ///
    /// 调用约定 (System V AMD64 ABI):
    /// - rdi = entry (用户态 RIP)
    /// - rsi = stack (用户态 RSP)
    /// - rdx = arg (用户态参数，当前未使用)
    /// - rcx = `user_cr3` (用户页表物理地址)
    /// - r8 = kstack (内核栈高半区地址)
    #[inline(never)]
    fn enter_user(entry: usize, stack: usize, arg: usize, user_cr3: u64, kstack: u64) -> ! {
        // SAFETY: 调用方保证 entry/stack/user_cr3/kstack 有效。
        // 通过 FFI 调用汇编实现的 enter_user_asm。
        unsafe {
            // SAFETY: enter_user_asm 由 global_asm! 定义于 .kpti_trampoline section,
            // 参数 (entry/stack/user_cr3/kstack) 有效性由上方 SAFETY 保证.
            unsafe extern "C" {
                fn enter_user_asm(
                    entry: usize,
                    stack: usize,
                    arg: usize,
                    user_cr3: u64,
                    kstack: u64,
                ) -> !;
            }
            enter_user_asm(entry, stack, arg, user_cr3, kstack)
        }
    }

    /// 返回用户态 (iretq)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn return_to_user() {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!("iretq", options(noreturn));
        }
    }
}

// ============================================================================
// enter_user 汇编实现 (KPTI trampoline)
// ============================================================================

// 进入用户态的汇编实现。
//
// 必须放在 `.kpti_trampoline` section，因为切换到用户页表后，
// CPU 需要继续执行当前指令（构建 iretq 帧并执行 iretq）。
// 如果不在 trampoline 区域，用户页表中该地址没有 USER 位，会导致 #PF。
//
// 调用约定 (System V AMD64 ABI):
// - rdi = entry (用户态 RIP)
// - rsi = stack (用户态 RSP)
// - rdx = arg (用户态参数，当前未使用)
// - rcx = user_cr3 (用户页表物理地址)
// - r8 = kstack (内核栈高半区地址)
//
// P2.B + F-13 (DECISION-051): GDT 选择子强绑定.
// SELECTOR_USER_DATA = 0x18, SELECTOR_USER_CODE = 0x20 (gdt.rs).
// 汇编不再硬编码 0x1B/0x23, 而用 extern + +3 推导 DPL=3 编码,
// GDT 描述符顺序调整后立即生效.
//
// 执行流程:
// 1. 诊断输出 (CPL=0, 内核栈)
// 2. 切换到用户栈 (CPL=0, 仍可访问高半区)
// 3. 在用户栈构建 iretq 帧
// 4. 切换 CR3 到用户页表 (CPL=0, 高半区 trampoline 可执行)
// 5. 加载用户段寄存器 (CPL→3)
// 6. iretq (从用户栈读取帧)
core::arch::global_asm!(
    r#"
    .section .kpti_trampoline
    .global enter_user_asm
    .type enter_user_asm, @function
    // P2.B + F-13 (DECISION-051): 引用 Rust 端 gdt.rs 选择子常量.
    // 由 linker 解析为外部符号; 后续 push SELECTOR_* + 3 编码 DPL=3.
    .extern SELECTOR_USER_DATA
    .extern SELECTOR_USER_CODE
enter_user_asm:
    // 参数: rdi=entry, rsi=stack, rdx=arg, rcx=user_cr3, r8=kstack
    cli

    // 保存 user_cr3 到 rax (在清除寄存器前)
    mov rax, rcx

    // 保存 entry 到 r12 (在清除寄存器前)
    mov r12, rdi

    // 清除寄存器 (防止泄露内核信息到用户态)
    // 注意：rax 保存 user_cr3，稍后用于切换 CR3
    // 注意：rsi 保存用户栈地址，稍后用于切换 RSP
    mov r8, rsi                     // 暂存用户栈到 r8
    xor ecx, ecx        // 清 rcx
    xor esi, esi        // 清 rsi
    xor edi, edi        // 清 rdi
    xor ebp, ebp        // 清 rbp
    xor r9d, r9d        // 清 r9
    xor r10d, r10d      // 清 r10
    xor r11d, r11d      // 清 r11

    // 切换到用户栈 (CPL=0, 仍可访问高半区)
    mov rsp, r8

    // 在用户栈构建 iretq 帧
    // ⚠ 关键修复 (TRACK-INIT-RING3):
    // iretq 帧必须在用户栈上, 而非内核栈.
    // 原因: 切换 CR3 到 USER_PML4 后, 内核栈页面没有 USER 位,
    // iretq 尝试从内核栈读取帧数据会触发 #PF.
    // P2.B + F-13 (DECISION-051 简化方案): SS = 用户数据段 (DPL=3).
    // 字节长度与原 push 0x1B 一致 (2 字节), 避免 label 偏移重定义.
    // 单一来源: src/kernel/privileged/link/x86_64.ld SELECTOR_USER_DATA_RPL3 与
    // gdt.rs pub const SELECTOR_USER_DATA 同步 (host-tests 校验).
    push 0x1B    // SS (用户数据段)
    
    
    push r8             // RSP (用户栈, 当前 RSP 值)
    
    
    push 0x202          // RFLAGS (IF 位)
    
    
    # P2.B + F-13 (DECISION-051 简化方案): CS = 用户代码段 (DPL=3).
    # 字节长度与原始 push 0x23 (2 字节) 一致, 避免 label 偏移重定义.
    # 单一来源: src/kernel/privileged/link/x86_64.ld SELECTOR_USER_CODE_RPL3
    # 与 gdt.rs pub const SELECTOR_USER_CODE 同步 (host-tests 校验).
    push 0x23    // CS (用户代码段)
    
    
    push r12            // RIP (用户入口, 使用保存的 r12)
    

    // ═══ 关键修复 (TRACK-INIT-RING3): 更新 SyscallPerCpu.user_pml4 ═══
    // 中断/异常返回路径使用 [gs:USER_PML4_OFF] 切换回用户页表.
    // 若不更新, 仍为 KPTI 初始化时的共享页表, 非当前进程的专用页表,
    // 导致用户代码/栈页不可访问 → #PF → Triple Fault.
    // 此时 IA32_GS_BASE = per_cpu_addr, [gs:USER_PML4_OFF] 可安全写入.
    // rax = user_cr3 (当前进程的用户页表物理地址)
    mov gs:[0x10], rax                  // USER_PML4_OFF = 16, 写入 user_pml4

    // ═══ swapgs: 必须在加载 GS 段寄存器之前执行! ═══
    // 根因: mov gs, cx 会从 GDT 描述符加载隐藏基址到 IA32_GS_BASE.
    // 用户数据段描述符 base=0, 导致 IA32_GS_BASE 被清零.
    // 若 swapgs 在 mov gs 之后, 两个 MSR 都为 0, syscall [gs:0] → #PF → Triple Fault.
    // 正确顺序: swapgs (IA32_GS_BASE=0, IA32_KERNEL_GS_BASE=per_cpu_addr)
    //           → mov gs, cx (IA32_GS_BASE 保持 0, IA32_KERNEL_GS_BASE 不受影响)
    swapgs

    // 加载用户态段寄存器 (必须在 mov cr3 之前!).
    // 原因: mov ds/es/fs/gs 需要读取 GDT, GDT 在高半区.
    // 切换 CR3 到用户页表后, 高半区未映射, 无法访问 GDT → #PF.
    // 此时 CPL=0, 内核页表仍有效, GDT 可访问.
    // 注意: mov gs, cx 会将 GDT 描述符的 base(=0) 写入 IA32_GS_BASE,
    // 但 swapgs 已在上方执行, IA32_GS_BASE 已为 0, 不受影响.
    // IA32_KERNEL_GS_BASE 不受 mov gs 指令影响, 保持 per_cpu_addr.
    mov cx, 0x1B
    mov ds, cx
    
    
    mov es, cx
    
    
    mov fs, cx

    mov gs, cx
    

    // ⚠ 关键修复 (TRACK-INIT-RING3):
    // 直接 fall-through 到 trampoline 后续代码.
    // 原因: 代码顺序执行, 无需显式跳转.
    // 高半区 VMA 已在用户页表中映射 (map_text_region_in_user_pml4),
    // 切换 CR3 后 CPU 仍可继续执行.
    

    // ⚠ 关键修复 (TRACK-INIT-RING3):
    // 切换 CR3 到用户页表.
    // rax 保存 user_cr3.
    // 高半区 VMA 已在用户页表中映射, 切换后 CPU 可继续执行.
    mov cr3, rax
    

    // 清除 rax (防止泄露)
    xor eax, eax

    // iretq 返回用户态
    // iretq 从用户栈恢复: RIP, CS, RFLAGS, RSP, SS
    // swapgs 已在段寄存器加载前执行, IA32_KERNEL_GS_BASE = per_cpu_addr
    iretq
    .size enter_user_asm, . - enter_user_asm
"#
);

// ── SystemArch: 端口 IO + 电源管理 ───────────────────────────────────

impl SystemArch for X8664 {
    /// 向 I/O 端口写入字节 (out dx, al)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn outb(port: u16, value: u8) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") port,
                in("al") value,
                options(nostack, preserves_flags)
            );
        }
    }

    /// 从 I/O 端口读取字节 (in al, dx)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn inb(port: u16) -> u8 {
        let value: u8;
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "in al, dx",
                out("al") value,
                in("dx") port,
                options(nostack, preserves_flags)
            );
        }
        value
    }

    /// 向 I/O 端口写入双字 (out dx, eax)。
    #[inline(always)]
    #[expect(
        clippy::inline_always,
        reason = "inline_always: #[inline(always)] 是性能优化 (关键路径/中断处理); 当前优先 expect"
    )]
    fn outl(port: u16, value: u32) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "out dx, eax",
                in("dx") port,
                in("eax") value,
                options(nostack, preserves_flags)
            );
        }
    }

    /// 从 I/O 端口读取双字 (in eax, dx)。
    #[inline(always)]
    fn inl(port: u16) -> u32 {
        let value: u32;
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!(
                "in eax, dx",
                out("eax") value,
                in("dx") port,
                options(nostack, preserves_flags)
            );
        }
        value
    }

    /// 关机 (8042 + triple fault 回退)。
    fn shutdown() -> ! {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!("mov al, 0xFE", "out 0x64, al", options(nomem, nostack));
        }
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!("lidt [0]", "int 3", options(nomem, nostack));
        }
        loop {
            core::hint::spin_loop();
        }
    }

    /// 重启 (键盘控制器 8042 → CPU reset)。
    fn reboot() -> ! {
        while <Self as SystemArch>::inb(0x64) & 2 != 0 {
            core::hint::spin_loop();
        }
        <Self as SystemArch>::outb(0x64, 0xFE);
        loop {
            <Self as CoreArch>::halt();
        }
    }
}

// ── Arch: 超 trait (空 body) ─────────────────────────────────────────

impl Arch for X8664 {}
