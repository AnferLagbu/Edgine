//! AArch64 异常向量表 (Exception Vector Table)
//!
//! ARMv8-A 异常级别 EL1 (内核) 异常向量表。
//! 每个异常类型有 4 个入口: 同异常级别使用 SP_EL0/SP_ELx, 不同异常级别使用 SP_EL0/SP_ELx。
//! 本文件 cast 多为异常返回码/中断号转换, 硬件已知安全.
//!
//! 向量表布局 (VBAR_EL1):
//! | 偏移 | 类型 | 级别 | 说明 |
//! |------|------|------|------|
//! | +0x000 | Synchronous | EL1t | current EL, SP_EL0 |
//! | +0x080 | IRQ | EL1t | — |
//! | +0x100 | FIQ | EL1t | — |
//! | +0x180 | SError | EL1t | — |
//! | +0x200 | Synchronous | EL1h | current EL, SP_ELx |
//! | +0x280 | IRQ | EL1h | — |
//! | +0x300 | FIQ | EL1h | — |
//! | +0x380 | SError | EL1h | — |
//! | +0x400 | Synchronous | EL0 in AArch64 | — |
//! | +0x480 | IRQ | EL0 in AArch64 | — |
//! | +0x500 | FIQ | EL0 in AArch64 | — |
//! | +0x580 | SError | EL0 in AArch64 | — |
//! | +0x600 | Synchronous | EL0 in AArch32 | — |
//! | +0x680 | IRQ | EL0 in AArch32 | — |
//! | +0x700 | FIQ | EL0 in AArch32 | — |
//! | +0x780 | SError | EL0 in AArch32 | — |

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// 定时器中断间隔 (ticks), 在 boot 时由 timer::init() 设置
pub static TIMER_INTERVAL_TICKS: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// 异常向量表 (global_asm)
// ============================================================================

global_asm!(
    r#"
// ============================================================
// AArch64 异常向量表 (VBAR_EL1, 每个入口 32 条指令 = 128 bytes)
// ARM 要求向量表按 2KB (2048 bytes) 对齐
//
// 关键设计: 每个 128-byte 槽位只放一条 b 指令跳转到外部 handler,
// 确保所有 16 个入口精确处于 VBAR + idx*128 偏移处, 不会溢出错位。
// ============================================================
.section .vectors, "ax"
.balign 2048
.global exception_vector_table
exception_vector_table:

// -------- EL1t: 当前 EL, 使用 SP_EL0 --------
.balign 128
    b   unexpected_exception       // curr_el_sp0_sync
.balign 128
    b   unexpected_exception       // curr_el_sp0_irq
.balign 128
    b   unexpected_exception       // curr_el_sp0_fiq
.balign 128
    b   unexpected_exception       // curr_el_sp0_serror

// -------- EL1h: current EL with SP_ELx (标准内核路径) --------
.balign 128
    b   handle_el1h_sync           // curr_el_spx_sync
.balign 128
    b   handle_el1h_irq            // curr_el_spx_irq
.balign 128
    b   unexpected_exception       // curr_el_spx_fiq
.balign 128
    b   unexpected_exception       // curr_el_spx_serror

// -------- EL0 in AArch64 --------
.balign 128
    b   handle_el0_sync            // lower_el_aarch64_sync
.balign 128
    b   handle_el0_irq             // lower_el_aarch64_irq
.balign 128
    b   unexpected_exception       // lower_el_aarch64_fiq
.balign 128
    b   unexpected_exception       // lower_el_aarch64_serror

// -------- EL0 in AArch32 (未使用) --------
.balign 128
    b   unexpected_exception       // lower_el_aarch32_sync
.balign 128
    b   unexpected_exception       // lower_el_aarch32_irq
.balign 128
    b   unexpected_exception       // lower_el_aarch32_fiq
.balign 128
    b   unexpected_exception       // lower_el_aarch32_serror

// ============================================================
// Handler code (位于向量表外部, 不受 128-byte 槽位限制)
// ============================================================

// -------- EL1h 同步异常处理 --------
handle_el1h_sync:
    sub  sp, sp, #(8 * 35)
    stp  x0, x1, [sp, #(8 * 0)]
    stp  x2, x3, [sp, #(8 * 2)]
    stp  x4, x5, [sp, #(8 * 4)]
    stp  x6, x7, [sp, #(8 * 6)]
    stp  x8, x9, [sp, #(8 * 8)]
    stp  x10, x11, [sp, #(8 * 10)]
    stp  x12, x13, [sp, #(8 * 12)]
    stp  x14, x15, [sp, #(8 * 14)]
    stp  x16, x17, [sp, #(8 * 16)]
    stp  x18, x19, [sp, #(8 * 18)]
    stp  x20, x21, [sp, #(8 * 20)]
    stp  x22, x23, [sp, #(8 * 22)]
    stp  x24, x25, [sp, #(8 * 24)]
    stp  x26, x27, [sp, #(8 * 26)]
    stp  x28, x29, [sp, #(8 * 28)]
    str  x30, [sp, #(8 * 30)]

    mrs  x0, elr_el1
    mrs  x1, spsr_el1
    stp  x0, x1, [sp, #(8 * 31)]
    mov  x1, sp
    add  x1, x1, #(8 * 35)
    str  x1, [sp, #(8 * 33)]

    mov  x0, sp
    bl   sync_exception_handler

    ldr  x30, [sp, #(8 * 30)]
    ldp  x0, x1, [sp, #(8 * 31)]
    msr  elr_el1, x0
    msr  spsr_el1, x1
    ldp  x0, x1, [sp, #(8 * 0)]
    ldp  x2, x3, [sp, #(8 * 2)]
    ldp  x4, x5, [sp, #(8 * 4)]
    ldp  x6, x7, [sp, #(8 * 6)]
    ldp  x8, x9, [sp, #(8 * 8)]
    ldp  x10, x11, [sp, #(8 * 10)]
    ldp  x12, x13, [sp, #(8 * 12)]
    ldp  x14, x15, [sp, #(8 * 14)]
    ldp  x16, x17, [sp, #(8 * 16)]
    ldp  x18, x19, [sp, #(8 * 18)]
    ldp  x20, x21, [sp, #(8 * 20)]
    ldp  x22, x23, [sp, #(8 * 22)]
    ldp  x24, x25, [sp, #(8 * 24)]
    ldp  x26, x27, [sp, #(8 * 26)]
    ldp  x28, x29, [sp, #(8 * 28)]
    add  sp, sp, #(8 * 35)
    eret

// -------- EL1h IRQ handler --------
handle_el1h_irq:
    sub  sp, sp, #(8 * 35)
    stp  x0, x1, [sp, #(8 * 0)]
    stp  x2, x3, [sp, #(8 * 2)]
    stp  x4, x5, [sp, #(8 * 4)]
    stp  x6, x7, [sp, #(8 * 6)]
    stp  x8, x9, [sp, #(8 * 8)]
    stp  x10, x11, [sp, #(8 * 10)]
    stp  x12, x13, [sp, #(8 * 12)]
    stp  x14, x15, [sp, #(8 * 14)]
    stp  x16, x17, [sp, #(8 * 16)]
    stp  x18, x19, [sp, #(8 * 18)]
    stp  x20, x21, [sp, #(8 * 20)]
    stp  x22, x23, [sp, #(8 * 22)]
    stp  x24, x25, [sp, #(8 * 24)]
    stp  x26, x27, [sp, #(8 * 26)]
    stp  x28, x29, [sp, #(8 * 28)]
    str  x30, [sp, #(8 * 30)]

    mrs  x0, elr_el1
    mrs  x1, spsr_el1
    stp  x0, x1, [sp, #(8 * 31)]
    mov  x1, sp
    add  x1, x1, #(8 * 35)
    str  x1, [sp, #(8 * 33)]

    mov  x0, sp
    bl   irq_handler

    ldr  x30, [sp, #(8 * 30)]
    ldp  x0, x1, [sp, #(8 * 31)]
    msr  elr_el1, x0
    msr  spsr_el1, x1
    ldp  x0, x1, [sp, #(8 * 0)]
    ldp  x2, x3, [sp, #(8 * 2)]
    ldp  x4, x5, [sp, #(8 * 4)]
    ldp  x6, x7, [sp, #(8 * 6)]
    ldp  x8, x9, [sp, #(8 * 8)]
    ldp  x10, x11, [sp, #(8 * 10)]
    ldp  x12, x13, [sp, #(8 * 12)]
    ldp  x14, x15, [sp, #(8 * 14)]
    ldp  x16, x17, [sp, #(8 * 16)]
    ldp  x18, x19, [sp, #(8 * 18)]
    ldp  x20, x21, [sp, #(8 * 20)]
    ldp  x22, x23, [sp, #(8 * 22)]
    ldp  x24, x25, [sp, #(8 * 24)]
    ldp  x26, x27, [sp, #(8 * 26)]
    ldp  x28, x29, [sp, #(8 * 28)]
    add  sp, sp, #(8 * 35)
    eret

// -------- EL0 sync handler (SVC / 数据异常) --------
// 入口时刻 (TTBR0 = 用户表, TTBR1 = tramp 表) 内核栈**不可达** —— 其 VA 在高半区,
// 故必须**先切两条 TTBR 再压帧**. 本段位于 .vectors (高半区) ⇒ 切表后取指不受影响.
// 切表序列需 2 个 scratch 寄存器 (x3/x4), 而此刻 x0-x30 全是用户态活跃值 (帧尚未
// 落栈), 故先借 TPIDRRO_EL0 (EL1 可写 / EL0 只读, 内核无用途; `msr` 不消耗 GPR)
// 中转用户 x3. 本核 KPTI 槽基址由 TPIDR_EL1 提供 (上电路径按核绑定, 见
// `mm/kpti_aarch64.rs::kpti_bind_cpu`), 槽页在 trampoline 表内恒可达.
handle_el0_sync:
    msr  tpidrro_el0, x3                // 中转保住用户 x3
    mrs  x3, tpidr_el1                  // x3 ← 本核 KPTI 槽基址 (用户 x3 已在 TPIDRRO)
    str  x4, [x3, #8]                   // tramp_save0 ← 用户 x4
    mrs  x4, tpidrro_el0
    str  x4, [x3, #16]                  // tramp_save1 ← 用户 x3
    msr  tpidrro_el0, xzr               // 清中转 (防 EL0 经 TPIDRRO_EL0 读内核残留)

    // 第一步: 切完整内核表 (TTBR0) + 完整内核 TTBR1. 这是**读取保留槽的前提** ——
    // 此刻 TTBR0 仍指向用户表 (不含内核 DRAM 的恒等映射); TTBR1 仍是 tramp 表,
    // 只映射 .vectors / 全局量页 / 每核槽页 ⇒ 高半区别名同样取不到用户表所在 PA.
    // 只有先切内核表, 内核映像/栈/页表所在物理页才可达.
    mrs  x4, ttbr0_el1                  // 旧 TTBR0 = 用户页表
    str  x4, [x3, #0]                   // 本核 user_ttbr0
    // kernel_ttbr0 / kernel_ttbr1 属 boot 期只读单实例全局量 (x4 兼作基址与取值).
    adrp x4, {kpti_globals}
    add  x4, x4, #:lo12:{kpti_globals}
    ldr  x4, [x4, #16]                  // kernel_ttbr0
    cbz  x4, 1f
    dsb  ish
    msr  ttbr0_el1, x4
    isb
1:
    adrp x4, {kpti_globals}
    add  x4, x4, #:lo12:{kpti_globals}
    ldr  x4, [x4, #8]                   // kernel_ttbr1
    cbz  x4, 2f
    dsb  ish
    msr  ttbr1_el1, x4
    isb
2:
    tlbi vmalle1is
    dsb  ish
    isb
    ldr  x4, [x3, #8]                   // 取回用户 x4
    ldr  x3, [x3, #16]                  // 取回用户 x3

    // 第二步: 落异常帧 (280 字节). 此刻内核栈已可达 (见上), 帧内为真实用户值.
    sub  sp, sp, #(8 * 35)
    stp  x0, x1, [sp, #(8 * 0)]
    stp  x2, x3, [sp, #(8 * 2)]
    stp  x4, x5, [sp, #(8 * 4)]
    stp  x6, x7, [sp, #(8 * 6)]
    stp  x8, x9, [sp, #(8 * 8)]
    stp  x10, x11, [sp, #(8 * 10)]
    stp  x12, x13, [sp, #(8 * 12)]
    stp  x14, x15, [sp, #(8 * 14)]
    stp  x16, x17, [sp, #(8 * 16)]
    stp  x18, x19, [sp, #(8 * 18)]
    stp  x20, x21, [sp, #(8 * 20)]
    stp  x22, x23, [sp, #(8 * 22)]
    stp  x24, x25, [sp, #(8 * 24)]
    stp  x26, x27, [sp, #(8 * 26)]
    stp  x28, x29, [sp, #(8 * 28)]
    str  x30, [sp, #(8 * 30)]

    mrs  x0, elr_el1
    mrs  x1, spsr_el1
    stp  x0, x1, [sp, #(8 * 31)]
    mrs  x1, sp_el0
    str  x1, [sp, #(8 * 33)]

    // 第三步: TTBR0 精化为**本进程 EL1 视图** (方案 S3). 用户表**保留槽 index 1**
    // 存有视图根物理地址, 只写地址不置 bits[1:0] ⇒ 硬件视为无效描述符 (该 VA 段
    // 保持未映射), 软件却可直接读出. 视图 = 用户半区 (进程页可达), 供 copy_from_user
    // 等直接解引用用户裸指针; 内核镜像/栈经 TTBR1 高半区可达, 不在本视图内 (L1-05).
    // 保留槽为 0 (视图未建) 时保持完整内核表 —— 退化为旧行为.
    // 注: TTBR0 仅承载 BADDR (本内核恒以 ASID=0 切表), 故 x2 可直接作地址基.
    mrs  x3, tpidr_el1                  // x3 ← 本核 KPTI 槽基址
    ldr  x2, [x3, #0]                   // 本核 user_ttbr0 (物理地址)
    // 用户表所在物理页经**高半区别名**读取 (此刻 TTBR1 已是完整内核表):
    // 表链接于高半区后, TTBR0 恒等面不再保证覆盖该 PA, 别名面则恒可达.
    movz x5, #0xFFFF, lsl #48
    add  x2, x2, x5                     // x2 = 别名地址 (仅用于读, 值仍为 PA)
    ldr  x4, [x2, #8]
    cbz  x4, 3f
    dsb  ish
    msr  ttbr0_el1, x4
    isb
    tlbi vmalle1is
    dsb  ish
    isb
3:

    // 检查 ESR_EL1.EC 判断异常类型
    mrs  x0, esr_el1
    lsr  x0, x0, #26        // EC = ESR[31:26]
    cmp  x0, #0x15          // SVC from AArch64
    beq  handle_svc

    // 其他 EL0 同步异常 → sync_exception_handler
    mov  x0, sp
    bl   sync_exception_handler
    b    el0_return

handle_svc:
    // SVC #0: x0 = 系统调用号, x1-x5 = 参数
    mov  x0, sp             // x0 = ExceptionFrame*
    bl   svc_handler

    // 把返回值存入帧内 x0
    str  x0, [sp, #(8 * 0)]
    // fallthrough → el0_return

// -------- EL0 统一出口: 切回 (用户表 + tramp 表) 后 eret --------
// 必须在高半区 (.vectors) 内切 TTBR0 —— 切换后低半区代码即不可取指.
// 切表后内核栈即不可达 (其 VA 在高半区), 故**先**把帧读回; 唯独 x3/x4 (切表序列
// 的 scratch) 先暂存进全局量槽, 切表后再从 (tramp 表映射的) 全局量页取回.
el0_return:
    mrs  x3, tpidr_el1                  // x3 ← 本核 KPTI 槽基址
    ldr  x4, [sp, #(8 * 4)]
    str  x4, [x3, #8]                   // tramp_save0 ← 帧内用户 x4
    ldr  x4, [sp, #(8 * 3)]
    str  x4, [x3, #16]                  // tramp_save1 ← 帧内用户 x3

    // 恢复除 x3/x4 外的全部寄存器 (此刻内核栈仍可达).
    ldr  x1, [sp, #(8 * 33)]
    msr  sp_el0, x1
    ldr  x30, [sp, #(8 * 30)]
    ldp  x0, x1, [sp, #(8 * 31)]
    msr  elr_el1, x0
    msr  spsr_el1, x1
    ldp  x0, x1, [sp, #(8 * 0)]
    ldr  x2, [sp, #(8 * 2)]
    ldr  x5, [sp, #(8 * 5)]
    ldp  x6, x7, [sp, #(8 * 6)]
    ldp  x8, x9, [sp, #(8 * 8)]
    ldp  x10, x11, [sp, #(8 * 10)]
    ldp  x12, x13, [sp, #(8 * 12)]
    ldp  x14, x15, [sp, #(8 * 14)]
    ldp  x16, x17, [sp, #(8 * 16)]
    ldp  x18, x19, [sp, #(8 * 18)]
    ldp  x20, x21, [sp, #(8 * 20)]
    ldp  x22, x23, [sp, #(8 * 22)]
    ldp  x24, x25, [sp, #(8 * 24)]
    ldp  x26, x27, [sp, #(8 * 26)]
    ldp  x28, x29, [sp, #(8 * 28)]
    add  sp, sp, #(8 * 35)

    // 切回 (用户表 + tramp 表). tramp_ttbr1 属 boot 期只读单实例全局量;
    // user_ttbr0 取自本核槽 (TPIDR_EL1 寻址). 本段位于 .vectors (高半区)
    // ⇒ 切换后取指不受影响; 仅用 x4 作 scratch (用户 x3/x4 已入暂存槽).
    adrp x4, {kpti_globals}
    add  x4, x4, #:lo12:{kpti_globals}
    ldr  x4, [x4, #0]                   // tramp_ttbr1 (切 TTBR1 前必须读出)
    cbz  x4, 3f
    dsb  ish
    msr  ttbr1_el1, x4
    isb
3:
    ldr  x4, [x3, #0]                   // 本核 user_ttbr0
    cbz  x4, 4f
    dsb  ish
    msr  ttbr0_el1, x4
    isb
4:
    tlbi vmalle1is
    dsb  ish
    isb

    // 取回用户 x3/x4: 本核槽页经 tramp 表仍可达 (x3 尚为本核槽基址).
    ldr  x4, [x3, #8]
    ldr  x3, [x3, #16]
    eret

// -------- EL0 IRQ handler --------
handle_el0_irq:
    // KPTI 入口: 与 handle_el0_sync 同构 (先切两条 TTBR 再压帧; x3/x4 经
    // TPIDRRO_EL0 中转存入本核槽暂存槽; 本核槽基址由 TPIDR_EL1 提供).
    msr  tpidrro_el0, x3                // 中转保住用户 x3
    mrs  x3, tpidr_el1                  // x3 ← 本核 KPTI 槽基址
    str  x4, [x3, #8]                   // tramp_save0 ← 用户 x4
    mrs  x4, tpidrro_el0
    str  x4, [x3, #16]                  // tramp_save1 ← 用户 x3
    msr  tpidrro_el0, xzr               // 清中转

    mrs  x4, ttbr0_el1                  // 旧 TTBR0 = 用户页表
    str  x4, [x3, #0]                   // 本核 user_ttbr0
    adrp x4, {kpti_globals}
    add  x4, x4, #:lo12:{kpti_globals}
    ldr  x4, [x4, #16]                  // kernel_ttbr0 (完整内核表)
    cbz  x4, 5f
    dsb  ish
    msr  ttbr0_el1, x4
    isb
5:
    adrp x4, {kpti_globals}
    add  x4, x4, #:lo12:{kpti_globals}
    ldr  x4, [x4, #8]                   // kernel_ttbr1
    cbz  x4, 6f
    dsb  ish
    msr  ttbr1_el1, x4
    isb
6:
    tlbi vmalle1is
    dsb  ish
    isb
    ldr  x4, [x3, #8]                   // 取回用户 x4
    ldr  x3, [x3, #16]                  // 取回用户 x3

    sub  sp, sp, #(8 * 35)
    stp  x0, x1, [sp, #(8 * 0)]
    stp  x2, x3, [sp, #(8 * 2)]
    stp  x4, x5, [sp, #(8 * 4)]
    stp  x6, x7, [sp, #(8 * 6)]
    stp  x8, x9, [sp, #(8 * 8)]
    stp  x10, x11, [sp, #(8 * 10)]
    stp  x12, x13, [sp, #(8 * 12)]
    stp  x14, x15, [sp, #(8 * 14)]
    stp  x16, x17, [sp, #(8 * 16)]
    stp  x18, x19, [sp, #(8 * 18)]
    stp  x20, x21, [sp, #(8 * 20)]
    stp  x22, x23, [sp, #(8 * 22)]
    stp  x24, x25, [sp, #(8 * 24)]
    stp  x26, x27, [sp, #(8 * 26)]
    stp  x28, x29, [sp, #(8 * 28)]
    str  x30, [sp, #(8 * 30)]

    mrs  x0, elr_el1
    mrs  x1, spsr_el1
    stp  x0, x1, [sp, #(8 * 31)]
    mrs  x1, sp_el0
    str  x1, [sp, #(8 * 33)]

    // 第二步: TTBR0 精化为本进程 EL1 视图 (保留槽 index 1)
    mrs  x3, tpidr_el1
    ldr  x2, [x3, #0]                   // 本核 user_ttbr0 (物理地址)
    // 同 handle_el0_sync: 用户表页经高半区别名读取 (TTBR0 恒等面不再保证覆盖)
    movz x5, #0xFFFF, lsl #48
    add  x2, x2, x5
    ldr  x4, [x2, #8]                   // 保留槽 = 本进程 EL1 视图根
    cbz  x4, 7f
    dsb  ish
    msr  ttbr0_el1, x4
    isb
    tlbi vmalle1is
    dsb  ish
    isb
7:

    mov  x0, sp
    bl   irq_handler_el0
    b    el0_return

// ============================================================
// KPTI 进入 EL0 trampoline (必须位于 .vectors: 由 TTBR1 高半区取指)
// 入参: sp_el0 / elr_el1 / spsr_el1 / sp_el1 已由 Rust 侧写好, x0 = 用户参数
// 出口: 切 TTBR0 → 用户页表, TTBR1 → tramp 表后 eret
// ============================================================
.global kpti_enter_user_trampoline
kpti_enter_user_trampoline:
    mrs  x9, tpidr_el1                  // x9 ← 本核 KPTI 槽基址
    ldr  x11, [x9, #0]                  // 本核 user_ttbr0
    adrp x9, {kpti_globals}
    add  x9, x9, #:lo12:{kpti_globals}
    ldr  x10, [x9, #0]                  // tramp_ttbr1 (切 TTBR1 前必须读出)
    cbz  x11, 7f
    dsb  ish
    msr  ttbr0_el1, x11
    isb
7:
    cbz  x10, 8f
    dsb  ish
    msr  ttbr1_el1, x10
    isb
8:
    tlbi vmalle1is
    dsb  ish
    isb
    eret

// -------- 未预期异常处理 --------
unexpected_exception:
    mrs  x0, esr_el1
    mrs  x1, elr_el1
    mrs  x2, far_el1
    wfi
    b    unexpected_exception
"#,
    kpti_globals = sym crate::framework::mm::kpti::KPTI_GLOBALS,
);

// ============================================================================
// 异常上下文 (保存/恢复)
// ============================================================================

/// 异常帧 (保存在 SPSR_EL1 + ELR_EL1)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ExceptionFrame {
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,
    pub x29: u64, // FP
    pub x30: u64, // LR
    pub elr: u64,
    pub spsr: u64,
    pub sp: u64,
}

// ============================================================================
// 异常向量表导出 (供 boot 入口设置 VBAR_EL1)
// ============================================================================

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    /// 异常向量表起始地址 (定义在 asm)
    pub static exception_vector_table: u8;
}

// SAFETY: C ABI 互操作，符号由本文件 global_asm 定义 (.vectors 段)
unsafe extern "C" {
    /// KPTI 进入 EL0 的 trampoline 入口 (位于 `.vectors` 段, 高半区链接地址)
    pub(crate) static kpti_enter_user_trampoline: u8;
}

/// 返回 KPTI 进入 EL0 trampoline 的**高半区**地址.
///
/// 该 trampoline 位于 `.vectors` 段 (链接于高半区), 进入 EL0 前 `TTBR0` 仍指向
/// 内核恒等表、`TTBR1` 指向 tramp 表, 故该高半区地址经 `TTBR1` 可达, 保证切换
/// `TTBR0` 后当前指令流不被 Prefetch Abort.
pub fn kpti_enter_user_trampoline_high() -> u64 {
    &raw const kpti_enter_user_trampoline as u64
}

// ============================================================================
// 异常处理函数
// ============================================================================

/// SVC 系统调用处理。
///
/// EL0 SVC 系统调用处理器。
/// 从 EL0 通过 `svc #0` 进入。
/// QueenX aarch64 系统调用约定: x0=syscall_num, x1-x4=args, 返回 x0。
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn svc_handler(frame: &mut ExceptionFrame) -> u64 {
    // APS-05: 双核并发 EL0 观测 (有界诊断, 每核前若干次) —— EL0 syscall 入口.
    crate::framework::syscall::observe_el0_syscall();

    let syscall_num = frame.x0;

    // KPTI-17 D1: 把 EL0 用户寄存器快照写入当前进程 Process.context.
    // aarch64 的 syscall 路径不经 `syscall_dispatch_from_frame` (x86_64 B05-55
    // 捕获点), 故在此补齐: fork/clone 复制 context 时才能得到真实用户状态,
    // 子进程首次被调度时从正确的用户返回点继续.
    // 必须在 rt_sigreturn 处理前捕获 (sigreturn 会恢复 signal 帧).
    let cur_pid = crate::framework::proc::process_get_current_pid();
    if cur_pid != 0 {
        crate::framework::proc::proc_save_user_regs_aarch64(cur_pid, frame);
    }

    // rt_sigreturn 特殊处理: 需要直接修改 frame, 不走正常 dispatch.
    // aarch64: SYS_rt_sigreturn = 139
    // aarch64 信号投递 (do_signal_deliver) 尚未实现, 此拦截为预留.
    // 当信号投递实现后, 从用户栈读取 Aarch64SignalFrame 并恢复 x0-x30/elr/spsr/sp.
    if syscall_num == 139 {
        // 清除 SS_ONSTACK 标记
        if let Some(pid) =
            Some(crate::framework::proc::process_get_current_pid()).filter(|&p| p != 0)
        {
            crate::framework::proc::process_with_mut(pid, |proc| {
                use core::sync::atomic::Ordering;
                let flags = proc.sigaltstack_flags.load(Ordering::Acquire);
                proc.sigaltstack_flags.store(
                    flags & !crate::framework::proc::SS_ONSTACK,
                    Ordering::Release,
                );
            });
        }
        // sigreturn 不返回值, 保持 x0 原值
        return frame.x0;
    }

    let arg0 = frame.x1;
    let arg1 = frame.x2;
    let arg2 = frame.x3;
    let arg3 = frame.x4;
    let arg4 = frame.x5;
    let arg5 = frame.x6;

    // 调用通用 syscall 分发器 (syscall 模块已全局化)
    let result =
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe { crate::framework::syscall::syscall_dispatch(syscall_num, arg0, arg1, arg2, arg3, arg4, arg5) };

    // 返回值写入 x0
    result as u64
}

/// EL0 IRQ 处理器
#[unsafe(no_mangle)]
#[expect(
    clippy::items_after_statements,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
pub extern "C" fn irq_handler_el0(_frame: &ExceptionFrame) {
    // GIC ACK + handle + EOI
    let intid = super::gic::acknowledge();
    if intid >= 1020 {
        // 伪中断, 无需 EOI
        return;
    }

    // Timer interrupt (PPI 30 = non-secure physical timer)
    if intid == 30 {
        // 重新装载定时器 (ARM Generic Timer 是一次性的)
        super::timer::reload(TIMER_INTERVAL_TICKS.load(Ordering::Relaxed));

        static TIMER_COUNT_EL0: AtomicU64 = AtomicU64::new(0);
        let el0count = TIMER_COUNT_EL0.fetch_add(1, Ordering::Relaxed) + 1;
        if el0count <= 5 {
            crate::klog_info!(
                Boot,
                "TIMER IRQ (EL0) count={} ready={}",
                el0count,
                crate::framework::net::NET_READY.load(core::sync::atomic::Ordering::Acquire)
            );
        }

        crate::framework::timer::on_timer_interrupt();

        // I-50: hrtimer_run_queues 已在 on_timer_interrupt 内统一触发, 此处不再显式调用.

        // smoltcp: 始终轮询（DHCP 需要在 poll 中完成握手）
        #[cfg(not(feature = "kernel_test"))]
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            crate::framework::net::poll_network();
        }

        // 仅当 scheduler 已初始化时触发调度
        if crate::framework::proc::SCHEDULER_READY.load(Ordering::Acquire) {
            // SAFETY: scheduler_tick 由框架调度器提供, 中断路径触发调度
            unsafe extern "C" {
                fn scheduler_tick();
            }
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                scheduler_tick();
            }
        }
    }

    // 设备 SPI 分发 (须在 EOI 前: 电平触发中断需先完成设备侧 ack)
    let _ = super::gic::dispatch_device_spi(intid);

    super::gic::end_of_interrupt(intid);

    crate::framework::irq::do_softirq();
}

/// 默认同步异常处理 (EL1h / EL0)
///
/// 两条调用路径共用本函数: `handle_el1h_sync` (内核态同 EL 异常) 与
/// `handle_el0_sync` (EL0 非 SVC 同步异常)。行为按异常来源分派:
///
/// - **EL0** (`frame.spsr` 的 `M[3:0] == 0`)：用户态越权访问 (翻译/权限失败)、
///   未定义指令等 —— 一律**终止当前进程**并调度离去。不得返回 EL0: `ELR` 仍指向
///   触发异常的那条指令, 返回即再次触发同一异常 (死循环)。
/// - **EL1** (内核态)：保持"打印现场 + 停机"。内核态同步异常是内核缺陷, 必须
///   暴露而不能被当作"某个进程的故障"掩盖 (对照 KPTI-17 的 `ESR=0x96000061`
///   即 EC=0x25 同 EL 数据异常)。
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::no_effect_underscore_binding,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
pub extern "C" fn sync_exception_handler(frame: &ExceptionFrame) {
    let esr: u64;
    let far: u64;
    let elr: u64;
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        core::arch::asm!("mrs {}, esr_el1", out(reg) esr);
        core::arch::asm!("mrs {}, far_el1", out(reg) far);
        core::arch::asm!("mrs {}, elr_el1", out(reg) elr);
    }
    let _ec = (esr >> 26) & 0x3F;

    // 直接 UART 输出以确保能观察同步异常
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        super::uart::putc(b'S');
        super::uart::putc(b'Y');
        super::uart::putc(b'N');
        super::uart::putc(b'C');
        super::uart::putc(b'!');
        super::uart::putc(b' ');
        super::uart::putc(b'E');
        super::uart::putc(b'S');
        super::uart::putc(b'R');
        super::uart::putc(b'=');
        exc_puthex(esr);
        super::uart::putc(b' ');
        super::uart::putc(b'F');
        super::uart::putc(b'A');
        super::uart::putc(b'R');
        super::uart::putc(b'=');
        exc_puthex(far);
        super::uart::putc(b' ');
        super::uart::putc(b'E');
        super::uart::putc(b'L');
        super::uart::putc(b'R');
        super::uart::putc(b'=');
        exc_puthex(elr);
        super::uart::putc(b'\r');
        super::uart::putc(b'\n');
    }

    // ── EL0 同步异常 → 终止当前进程 (KPTI-09) ──────────────────────────────
    // `SPSR_EL1.M[3:0] == 0` (EL0t) 即异常取自用户态。此时内核入口已完成
    // TTBR0/TTBR1 切换到本进程 EL1 视图 (见 `handle_el0_sync` 前置段), 故可
    // 直接复用 syscall 退出路径的终止原语 —— 与 x86_64 `idt::execute_recovery_action`
    // 的 `TerminateProcess` 口径一致 (退出码 = pid)。
    //
    // `process_exit` 内部经 `SCHEDULER.exit` 末尾的 `schedule()` 切离本栈, 本函数
    // 因此不会返回; 其后的 `scheduler_yield` 与停机循环为防御性兜底。
    //
    // SIMPLIFIED: 不按 ESR.EC/DFSC 细分故障语义 (翻译/权限/未定义指令一律同等对待),
    // 也不向用户态投递具体信号; 影响: 用户态无法区分 SIGSEGV/SIGILL/SIGBUS (与
    // x86_64 侧"终止而不投递具体信号"现状一致); 何时需扩展: 需要按信号语义投递
    // (信号帧构建 + sigreturn 恢复) 时。
    #[expect(
        clippy::verbose_bit_mask,
        reason = "verbose_bit_mask: `spsr & 0xF == 0` 即 SPSR_EL1.M[3:0] == 0 的位域判据, \
                  与上方注释同形; 改 trailing_zeros >= 4 反而偏离架构文档, 当前优先 expect"
    )]
    if frame.spsr & 0xF == 0 {
        let pid = crate::framework::proc::process_get_current_pid();
        crate::klog_err!(
            Boot,
            "EL0 sync fault: pid={} ESR={:#X} FAR={:#X} ELR={:#X} -> terminate",
            pid,
            esr,
            far,
            elr
        );
        if pid != 0 {
            crate::framework::proc::process_exit(pid);
            crate::framework::proc::scheduler_yield();
        }
    }

    loop {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

/// 辅助函数: 通过 UART 以十六进制输出 u64
// SAFETY: 调用方保证指针/类型有效 (详见上下文)
unsafe fn exc_puthex(val: u64) {
    unsafe {
        for shift in (0..16).rev() {
            let nibble = ((val >> (shift * 4)) & 0xF) as u8;
            let c = if nibble < 10 {
                b'0' + nibble
            } else {
                b'A' + nibble - 10
            };
            super::uart::putc(c);
        }
    }
}

/// 跨核 TLB 失效 SGI 编号 (aarch64 等价于 x86_64 向量 0xFD)
///
/// 发送侧 `send_ipi(target, 0xFD)` 把 `vector & 0xF` 编码进 `ICC_SGI1R_EL1[27:24]`
/// (见 arch/aarch64/mod.rs 的 `send_ipi`), GIC 交付的 INTID 即低 4 位,
/// 故接收侧 intid = 0xFD & 0xF = 13, 与栏栈 SGI 7 及 timer PPI 30 均不冲突.
pub const TLB_SHOOTDOWN_SGI: u32 = 0xFD & 0xF;

/// 跨核重新调度 SGI 编号 (aarch64 等价于 x86_64 向量 0xFE)
///
/// 发送侧 `send_ipi(target, 0xFE)` 编码后接收侧 intid = 0xFE & 0xF = 14,
/// 与 TLB 失效 SGI 13 及栏栈 SGI 7 均不冲突.
pub const RESCHEDULE_SGI: u32 = 0xFE & 0xF;

/// 默认 IRQ 处理 (EL1h)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
#[expect(
    clippy::items_after_statements,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
pub extern "C" fn irq_handler(_frame: &ExceptionFrame) {
    // GIC ACK
    let intid = super::gic::acknowledge();

    // 诊断: 记录所有 IRQ 以追踪崩溃点
    {
        static IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
        let count = IRQ_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if count <= 10 {
            crate::klog_info!(Boot, "IRQ: intid={} count={}", intid, count);
        }
    }

    if intid >= 1020 {
        return;
    }

    // ── 栏栈恢复 SGI 7 (aarch64 等价于 x86_64 int 0x82) ─────────────────
    if intid == super::barrier::BARRIER_RECOVERY_SGI as u32 {
        let result = super::barrier::barrier_sgi_handler();
        if result < 0 {
            crate::klog_info!(Boot, "Barrier recovery SGI failed: {}", result);
        }
        super::gic::end_of_interrupt(intid);
        return;
    }

    // ── 跨核 TLB 失效 SGI 13 (aarch64 等价于 x86_64 向量 0xFD) ──────────
    if intid == TLB_SHOOTDOWN_SGI {
        // 收敛入口 `smp::tlb_catch_up_local` 完成
        // "先读当前代 → 全量刷新本核 TLB → 声明本核已追平该代", 次序不可颠倒
        // (先 flush 后读代会读到 flush 之后新发布的代, 把本次 flush 未覆盖的批次
        // 误判为已追平); 随后登记运行期探针观测 (未装备时为空操作).
        crate::framework::smp::tlb_catch_up_local();
        crate::framework::smp::tlb_probe_report();
        super::gic::end_of_interrupt(intid);
        return;
    }

    // ── 跨核重新调度 SGI 14 (aarch64 等价于 x86_64 向量 0xFE) ───────────
    if intid == RESCHEDULE_SGI {
        // 复用既有 IPI 入口, 内部登记 Sched softirq (fire-and-forget, 不等确认)
        crate::framework::proc::cpu_queue::resched_ipi_handler();
        super::gic::end_of_interrupt(intid);
        return;
    }

    // Timer interrupt (PPI 30 = non-secure physical timer)
    if intid == 30 {
        // 重新装载定时器 (ARM Generic Timer 是一次性的)
        super::timer::reload(TIMER_INTERVAL_TICKS.load(Ordering::Relaxed));

        static TIMER_COUNT: AtomicU64 = AtomicU64::new(0);
        let tcount = TIMER_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if tcount <= 5 {
            crate::klog_info!(
                Boot,
                "TIMER IRQ count={} ready={}",
                tcount,
                crate::framework::net::NET_READY.load(core::sync::atomic::Ordering::Acquire)
            );
        }

        crate::framework::timer::on_timer_interrupt();

        // 网络轮询
        #[cfg(not(feature = "kernel_test"))]
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            crate::framework::net::poll_network();
        }

        // 仅当 scheduler 已初始化时触发调度
        if crate::framework::proc::SCHEDULER_READY.load(core::sync::atomic::Ordering::Acquire) {
            // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
            unsafe extern "C" {
                fn scheduler_tick();
            }
            // SAFETY: 调用方保证指针/类型有效 (详见上下文)
            unsafe {
                scheduler_tick();
            }
        }
    }

    // 设备 SPI 分发 (须在 EOI 前: 电平触发中断需先完成设备侧 ack)
    let _ = super::gic::dispatch_device_spi(intid);

    super::gic::end_of_interrupt(intid);

    // 与 x86_64 IRQ 路径及 EL0 路径对齐: 中断退出前执行软中断底半部
    crate::framework::irq::do_softirq();
}

/// 默认 FIQ 处理 (EL1h)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn fiq_handler(_frame: &ExceptionFrame) {}

/// 默认 SError 处理 (EL1h)
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn serror_handler(_frame: &ExceptionFrame) {
    loop {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 设置 VBAR_EL1 指向异常向量表 (仅设寄存器, 不开中断)
///
/// # Safety
///
/// 仅在启动阶段调用，调用前需确保向量表已链接到内核镜像中。
///
/// VBAR_EL1 必须使用 TTBR1 高地址 (0xFFFF_0000_...), 因为进入 EL0 后
/// TTBR0_EL1 指向用户页表, 低地址无法通过 TTBR0 访问。
/// 向量表链接于高半区 (VMA = PA + KERNEL_BASE), 符号地址本身即高地址。
///
/// VBAR_EL1 是 **per-CPU** 寄存器: BSP 与每个 AP 均须各自调用本函数.
/// AP 侧刻意不在此时开中断 (延后至 per-CPU 状态就绪, 见 `smp_init::ap_main`).
pub unsafe fn init_vectors() {
    unsafe {
        let vbar = &exception_vector_table as *const u8 as u64;
        core::arch::asm!("msr vbar_el1, {}", in(reg) vbar);

        // ISB 确保写 VBAR 在取指前完成
        core::arch::asm!("isb");
    }
}

#[expect(
    clippy::borrow_as_ptr,
    reason = "DECISION-043 pedantic 兜底: aarch64 编译目标特有 lint, 当前批量 expect 兑底"
)]
/// 初始化异常: 设置 VBAR_EL1 指向向量表, 清除 DAIF (开中断)
///
/// # Safety
///
/// 仅在启动阶段调用，调用前需确保向量表已链接到内核镜像中。
pub unsafe fn init() {
    unsafe {
        init_vectors();
        // 清除 DAIF (Debug/SError/IRQ/FIQ 掩码), 使能中断
        core::arch::asm!("msr daifclr, #0xF");
    }
}
