//! AArch64 架构实现
//!
//! 子模块:
//! - context:   上下文切换 (context_switch_asm)
//! - mmu:       MMU/页表管理 (identity mapping, TTBR0_EL1)
//! - 寄存器值转换 (DAIF/SPSR/ELR) 多为 u64 ↔ usize 已知安全
//! - exception: 异常向量表 + handler (VBAR_EL1)
//! - gic:       GICv3 中断控制器初始化
//! - psci:      PSCI 电源管理 (关机/重启)
//! - smp_init:  SMP 次核 (AP) 启动 (BSP init + AP ap_main)
//! - timer:     ARM Generic Timer
//! - uart:      PL011 UART 驱动
//!
//! ## 实现状态
//! - [x] `impl CoreArch for Aarch64` — 基础核心能力
//! - [x] `impl InterruptArch for Aarch64` — DAIF + GICv3 SGI 中断
//! - [x] `impl MmuArch for Aarch64` — TTBR0/1 + 上下文切换 + eret
//! - [x] `impl SystemArch for Aarch64` — PSCI + port IO 桩
//! - [x] `impl Arch for Aarch64` — 超 trait (空)
//! - [x] `freg` — FREG恢复 (SGI 7 替代 int 0x82)

pub mod context;
pub mod exception;
pub mod freg;
pub mod gic;
pub mod mmu;
pub mod psci;
pub mod smp_init;
pub mod timer;
pub mod uart;

use core::arch::asm;

/// AArch64 CPU 架构实现 (Aarch64 结构体)
pub struct Aarch64;

use crate::framework::arch::{Arch, CoreArch, InterruptArch, MmuArch, SystemArch};

// ── CoreArch: 基础核心 ──────────────────────────────────────────────────

impl CoreArch for Aarch64 {
    /// 获取当前 CPU ID (MPIDR_EL1 Aff0)。
    #[inline(always)]
    fn cpu_id() -> u32 {
        let mpidr: u64;
        // SAFETY: mrs mpidr_el1 是只读系统寄存器，无副作用。
        unsafe {
            asm!("mrs {}, mpidr_el1", out(reg) mpidr);
        }
        (mpidr & 0xFF) as u32
    }

    /// 获取高精度时间戳 (CNTPCT_EL0)。
    #[inline(always)]
    fn timestamp() -> u64 {
        let cnt: u64;
        // SAFETY: mrs cntpct_el0 是只读系统寄存器。
        unsafe {
            asm!("mrs {}, cntpct_el0", out(reg) cnt, options(nomem, nostack));
        }
        cnt
    }

    /// 序列化时间戳读取: `isb` 上下文同步屏障后读计数器。
    ///
    /// 用于精确时间测量 (TSC 频率校准等), 比 `timestamp()` 慢。
    #[inline(always)]
    fn timestamp_serialized() -> u64 {
        let cnt: u64;
        // SAFETY: isb 是上下文同步屏障 (不声明 nomem, 阻止编译器重排),
        // mrs cntpct_el0 是只读系统寄存器; 序列保证读数不早于先前指令.
        unsafe {
            asm!(
                "isb",
                "mrs {}, cntpct_el0",
                out(reg) cnt,
                options(nostack, preserves_flags)
            );
        }
        cnt
    }

    /// CPU 暂停等待中断 (wfi)。
    #[inline(always)]
    fn halt() {
        // SAFETY: wfi 是标准 CPU 暂停指令，无内存副作用。
        unsafe {
            asm!("wfi", options(nomem, nostack));
        }
    }

    /// 全内存屏障 (dsb sy)。
    #[inline(always)]
    fn fence() {
        // SAFETY: dmb sy 是 aarch64 全系统内存屏障。
        unsafe {
            asm!("dmb sy", options(nomem, nostack));
        }
    }

    /// 写内存屏障 (dmb st)。
    #[inline(always)]
    fn fence_w() {
        // SAFETY: dmb st 是 aarch64 写内存屏障。
        unsafe {
            asm!("dmb st", options(nomem, nostack));
        }
    }

    /// 读内存屏障 (dmb ld)。
    #[inline(always)]
    fn fence_r() {
        // SAFETY: dmb ld 是 aarch64 读内存屏障。
        unsafe {
            asm!("dmb ld", options(nomem, nostack));
        }
    }
}

// ── InterruptArch: 中断 + IPI ────────────────────────────────────────

impl InterruptArch for Aarch64 {
    /// 禁用 IRQ (DAIF bit 1) 并返回 DAIF。
    #[inline(always)]
    fn interrupt_disable() -> usize {
        let daif: u64;
        // SAFETY: mrs daif 读取 + msr daifset #2 禁用 IRQ；都使用立即数，
        // 无内存副作用。必须在关中断前返回原 DAIF。
        unsafe {
            asm!("mrs {}, daif", out(reg) daif);
            asm!("msr daifset, #2");
        }
        daif as usize
    }

    /// 恢复完整 DAIF 屏蔽位 (D/A/I/F).
    ///
    /// P3.C + F-14: 历史版本仅恢复 IRQ (bit 7), 不恢复 D/A/F 位,
    /// 与 x86_64 RFLAGS 完整保存/恢复不对称. 本版本恢复 4 位完整 DAIF,
    /// 使用 4 次 `msr daifclr, #imm` / `msr daifset, #imm` 立即数指令组合
    /// (QEMU 上 `msr daif, Xt` 会挂起, 拆分避免该问题).
    ///
    /// DAIF 位布局 (msr daifclr/daifset 立即数):
    ///   bit 0 = D (Debug mask),  bit 1 = A (SError mask),
    ///   bit 2 = I (IRQ mask),    bit 3 = F (FIQ mask).
    #[inline(always)]
    fn interrupt_restore(flags: usize) {
        // SAFETY: msr daifset/daifclr 是立即数指令, 无内存副作用.
        // flags 由 interrupt_disable 保存的完整 DAIF (低 8 位有效).
        let daif = flags as u64;
        // F (FIQ) — bit 3 in immediate
        if (daif & (1 << 9)) == 0 {
            unsafe {
                asm!("msr daifclr, #8");
            }
        } else {
            unsafe {
                asm!("msr daifset, #8");
            }
        }
        // A (SError) — bit 2 in immediate
        if (daif & (1 << 8)) == 0 {
            unsafe {
                asm!("msr daifclr, #4");
            }
        } else {
            unsafe {
                asm!("msr daifset, #4");
            }
        }
        // I (IRQ) — bit 1 in immediate
        if (daif & (1 << 7)) == 0 {
            unsafe {
                asm!("msr daifclr, #2");
            }
        } else {
            unsafe {
                asm!("msr daifset, #2");
            }
        }
        // D (Debug) — bit 0 in immediate
        if (daif & (1 << 6)) == 0 {
            unsafe {
                asm!("msr daifclr, #1");
            }
        } else {
            unsafe {
                asm!("msr daifset, #1");
            }
        }
    }

    /// 启用 IRQ (msr daifclr)。
    #[inline(always)]
    fn interrupt_enable() {
        // SAFETY: msr daifclr #2 启用 IRQ (清除 I 屏蔽位)。
        unsafe {
            asm!("msr daifclr, #2");
        }
    }

    /// 检查 I (IRQ mask) bit。
    #[inline(always)]
    fn is_interrupt_enabled() -> bool {
        let daif: u64;
        // SAFETY: mrs daif 是只读系统寄存器读取。
        unsafe {
            asm!("mrs {}, daif", out(reg) daif);
        }
        (daif & (1 << 7)) == 0
    }

    #[expect(
        clippy::cast_lossless,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// GICv3 SGI 单播 (ICC_SGI1R_EL1)。
    fn send_ipi(target_cpu: u32, vector: u8) {
        // ICC_SGI1R_EL1 字段: INTID[27:24], Aff1[23:16], TargetList[15:0]。
        // 同簇内按 Aff0 单播: 目标的 Aff0 编码为 **TargetList 的对应位**
        // (Aff1/Aff2/Aff3 均为 0), 即 `1 << aff0`; 误置于 [23:16] (Aff1) 会
        // 寻址到不存在的簇, SGI 永不投递。
        // SIMPLIFIED: 仅按 Aff0 同簇寻址 (接口 `send_ipi` 目标参数窄化为低位);
        //   影响面: 多簇 (>16 核带亲和性) 拓扑下无法寻址跨簇核; 何时需扩展:
        //   引入 cpu_index↔(Aff0,Aff1,Aff2,Aff3) 映射, 按目标 MPIDR 填 Aff1/2/3。
        let sgi: u64 = ((vector & 0xF) as u64) << 24 | (1u64 << (target_cpu & 0xF));
        // SAFETY: msr icc_sgi1r_el1 触发 GICv3 SGI；
        // 目标 CPU 与 vector 已 mask 至合法范围。
        unsafe {
            asm!("msr icc_sgi1r_el1, {}", in(reg) sgi);
        }
    }

    #[expect(
        clippy::cast_lossless,
        reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
    )]
    /// GICv3 SGI 广播 (IRM=1)。
    fn broadcast_ipi(vector: u8) {
        let sgi: u64 = (1u64 << 40) | ((vector & 0xF) as u64) << 24;
        // SAFETY: msr icc_sgi1r_el1 广播 SGI；IRM bit 40 设置为 1 触发广播。
        unsafe {
            asm!("msr icc_sgi1r_el1, {}", in(reg) sgi);
        }
    }

    fn interrupt_early_init() {
        // GICv3 + VBAR_EL1 已由 entry.rs / bootloader 配置
    }

    fn interrupt_late_init() {
        // GICv3 + 异常向量 + 定时器已由 entry.rs 配置; 此处仅完成 SMP 次核启动。
        crate::framework::smp::init();
        smp_init::init();
    }
}

// ── MmuArch: MMU + 上下文 + 用户态 ───────────────────────────────────

impl MmuArch for Aarch64 {
    /// TLBI VA 单页刷新。
    #[inline(always)]
    fn tlb_flush_page(vaddr: usize) {
        // SAFETY: 标准 TLB 单页失效序列；
        // vaddr 是有效内核虚拟地址 (>> 12 取页号)。
        unsafe {
            asm!("dsb ishst", options(nomem, nostack));
            asm!("tlbi vaae1, {}", in(reg) (vaddr as u64 >> 12));
            asm!("dsb ish", options(nomem, nostack));
            asm!("isb", options(nomem, nostack));
        }
    }

    /// TLBI VMALL 全刷新。
    #[inline(always)]
    fn tlb_flush_all() {
        // SAFETY: 标准 TLB 全失效序列 (VMALL E1)。
        unsafe {
            asm!("dsb ishst", options(nomem, nostack));
            asm!("tlbi vmalle1", options(nomem, nostack));
            asm!("dsb ish", options(nomem, nostack));
            asm!("isb", options(nomem, nostack));
        }
    }

    /// 读取 TTBR0_EL1。
    #[inline(always)]
    fn read_page_table_base() -> u64 {
        mmu::read_ttbr0()
    }

    /// 写入 TTBR0_EL1 + ISB。
    #[inline(always)]
    fn write_page_table_base(paddr: u64) {
        // SAFETY: msr ttbr0_el1 写入新页表基址 + isb 同步流水线。
        unsafe {
            asm!("msr ttbr0_el1, {}", in(reg) paddr);
            asm!("isb", options(nomem, nostack));
        }
    }

    /// 读取 FAR_EL1。
    #[inline(always)]
    fn read_fault_address() -> usize {
        mmu::read_far() as usize
    }

    /// AArch64 上下文切换 (x19-x30 + SP + TTBR0 + SPSR + ELR)。
    fn context_switch(from: *mut u8, to: *const u8) {
        // SAFETY: context::switch 是底层上下文切换函数；
        // from/to 由调度器提供保证指向有效 Process。
        unsafe {
            context::switch(from, to);
        }
    }

    /// 进入 EL0 (KPTI 全切换模型).
    ///
    /// 除装载用户态入口寄存器外, 还必须完成三件事:
    /// 1. 记录用户 `TTBR0` 到**本核** KPTI 槽 (异常出口据此切回);
    /// 2. 设置 `SP_EL1 = kstack` —— EL0→EL1 异常入口在切换 `TTBR0` **之前**
    ///    就把 280 字节异常帧压入内核栈, 故内核栈顶页必须提前就位;
    /// 3. 跳转到 `.vectors` 内的高半区 trampoline 完成 `TTBR0/TTBR1` 切换后 eret
    ///    (切换必须在高半区执行, 否则切 `TTBR0` 后低半区代码立即 Prefetch Abort).
    fn enter_user(entry: usize, stack: usize, arg: usize, user_cr3: u64, kstack: u64) -> ! {
        // SPSR_EL1: EL0t (M[3:0]=0000), DAIF 全屏蔽 (F=1,I=1,A=1,D=1).
        // 0x3C0 = (0b1111 << 6) | 0b0000.
        let spsr: u64 = 0x3C0;

        // 内核 MMIO 统一走 TTBR1 高别名 (KPTI 方案 S3: EL1 视图刻意不含 Device 段).
        // 进入 EL0 后 TTBR0 即为用户视图, 内核态 (EL1) 若仍按低半区地址 0x0900_0000
        // 访问 PL011 会触发 L2 翻译故障; 故在用户态初始化前把 PL011_BASE 切到高别名 —
        // EL1 入口汇编已把 TTBR1 切回完整内核表, 其 L1_IDMAP[0] → L2_DEVICE 覆盖 0-1 GiB.
        uart::switch_to_high_half();

        // 记录用户页表到本核 KPTI 槽: 异常出口 (el0_return) 与 trampoline 均从该槽读取
        crate::framework::mm::kpti::kpti_set_user_ttbr0(user_cr3);
        let tramp = exception::kpti_enter_user_trampoline_high();

        // SAFETY: 进入 EL0 的最后一步:
        // - sp_el0/elr_el1/spsr_el1 均为 EL1 可写系统寄存器, 取值由调用方保证合法;
        // - 内核栈经 "SPSel=1 + mov sp" 写入 SP_EL1 (见下);
        // - x0 承载用户态首个参数; `br` 目标为 .vectors 内 trampoline 的高半区别名,
        //   该地址在切换前 (完整内核 TTBR1) 与切换后 (tramp 表) 均可取指;
        // - trampoline 完成切换后 eret 到 EL0, 不会返回.
        // options(noreturn): 本函数不会返回.
        unsafe {
            asm!(
                "msr sp_el0, {sp}",
                "msr elr_el1, {entry}",
                "msr spsr_el1, {spsr}",
                // SP_EL1 不用 `msr sp_el1` 写: 该编码 (S3_4_C4_C1_0) 在 EL1 为
                // UNDEFINED (EL2 已实现时的既有行为, QEMU `max`/`cortex-a72` 实测
                // 均报 Undefined Instruction). 架构等价写法是先置 PSTATE.SP=1,
                // 此时 `mov sp` 即写入 SP_EL1. 该 `mov sp` 会切换当前栈, 故必须
                // 位于本 asm 块最后一条 (其后只剩纯寄存器操作的 `br`).
                "msr spsel, #1",
                "mov sp, {kstack}",
                "br  {tramp}",
                sp = in(reg) stack as u64,
                entry = in(reg) entry as u64,
                spsr = in(reg) spsr,
                kstack = in(reg) kstack,
                in("x0") arg as u64,
                tramp = in(reg) tramp,
                options(noreturn),
            );
        }
    }

    /// 返回 EL0 (eret)。
    fn return_to_user() {
        // TTBR1_EL1 保持 mmu::init 设置不动. KPTI 激活后由异常出口
        // 汇编 (handle_el0_sync/handle_el0_irq 的 eret 前) 负责切换.
        // SAFETY: eret 是 aarch64 标准异常返回指令；
        // options(noreturn) 标识函数不会返回。
        unsafe {
            asm!("eret", options(noreturn));
        }
    }
}

// ── SystemArch: 端口 IO + 电源管理 ───────────────────────────────────

impl SystemArch for Aarch64 {
    fn outb(_port: u16, _value: u8) {}
    fn inb(_port: u16) -> u8 {
        0xFF
    }
    fn outl(_port: u16, _value: u32) {}
    fn inl(_port: u16) -> u32 {
        0xFFFF_FFFF
    }

    /// PSCI SYSTEM_OFF (SMC)。
    fn shutdown() -> ! {
        // SAFETY: smc #0 是 aarch64 安全监控调用；PSCI SYSTEM_OFF
        // 不会返回；loop + wfi 兜底防止固件未实现时的意外返回。
        unsafe {
            let func: u64 = 0x84000008;
            asm!("smc #0", in("x0") func, options(nostack));
        }
        loop {
            unsafe {
                asm!("wfi", options(nomem, nostack));
            }
        }
    }

    /// PSCI SYSTEM_RESET (SMC)。
    fn reboot() -> ! {
        // SAFETY: smc #0 + PSCI SYSTEM_RESET；不会返回。
        unsafe {
            let func: u64 = 0x84000009;
            asm!("smc #0", in("x0") func, options(nostack));
        }
        loop {
            unsafe {
                asm!("wfi", options(nomem, nostack));
            }
        }
    }
}

// ── Arch: 超 trait (空 body) ─────────────────────────────────────────

impl Arch for Aarch64 {}
