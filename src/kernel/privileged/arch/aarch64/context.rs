//! AArch64 上下文切换
//!
//! AAPCS64 callee-saved 寄存器: x19-x30, SP
//! 系统寄存器: TTBR0_EL1, SP_EL0, SPSR_EL1, ELR_EL1
//!
//! ## `ProcessContext` 字段语义 (aarch64 复用 x86_64 字段偏移, 偏移以本文件汇编为权威)
//!
//! | 偏移 | 字段 | aarch64 语义 |
//! |------|------|--------------|
//! | 0..72    | r15..rflags | x19..x28 |
//! | 80       | cr3         | x29 (FP) |
//! | 88       | cs          | x30 (LR) |
//! | 96       | ds          | SP_EL1 (内核栈指针) |
//! | 104      | es          | EL1 侧 TTBR0 (内核可用表: EL1 视图根 / 内核表) |
//! | 112      | fs          | SPSR_EL1 |
//! | 120      | gs          | ELR_EL1 |
//! | 128      | ss          | SP_EL0 (用户栈指针) |
//! | 136      | _fpu_pad    | 用户页表 (EL0 的 TTBR0) |
//! | 144..656 | fpu_state   | V0-V31 |
//! | 656, 664 | fpcr, fpsr  | FPCR / FPSR |
//! | 672..728 | extra_regs  | x0..x7 |
//!
//! 恢复侧按目标 `SPSR_EL1.M[3:0]` 分派两条路径 (见 `context_switch_asm` 注释):
//! 内核续跑 (`M != 0`) 与首次进入 EL0 (`M == 0`).

use core::arch::global_asm;

// ============================================================================
// 上下文切换汇编
// ============================================================================

global_asm!(
    r#"
.section .text.context_switch, "ax"
// 内核构建走 aarch64-unknown-none-softfloat (等效 -neon, 见
// docs/plan/aarch64-kernel-fp-free.md), LLVM 集成汇编器随之禁用 FP/SIMD
// 寄存器; 但本函数必须原样保存/恢复 EL0 的 V0-V31/FPCR/FPSR, 故在此汇编
// 单元内显式重新启用 fp/simd 扩展 —— 仅作用于本单元, 不改变编译器对
// Rust 代码的代码生成策略 (EL1 仍零隐式 FP/SIMD).
.arch_extension fp
.arch_extension simd
.global context_switch_asm

// 汇编函数签名: void context_switch_asm(u64* prev, const u64* next);
// x0 = prev (save), x1 = next (restore)
context_switch_asm:
    // 禁用中断 (DAIF set)
    msr  daifset, #0xF

    // === Save current context to [x0] ===
    //
    // 映射: aarch64 寄存器 → ProcessContext 字段 (x86_64 命名):
    //   x19→r15(0)   x20→r14(8)   x21→r13(16)  x22→r12(24)
    //   x23→rbx(32)  x24→rbp(40)  x25→rax(48)  x26→rip(56)
    //   x27→rsp(64)  x28→rflags(72)  x29→cr3(80)  x30→cs(88)
    //   sp→ds(96)  TTBR0→es(104)  常量0x3C5→fs(112)  x30→gs(120)
    //   SP_EL0→ss(128)  用户页表→_fpu_pad(136)  (系统寄存器映射)

    str  x19, [x0, #0]
    str  x20, [x0, #8]
    str  x21, [x0, #16]
    str  x22, [x0, #24]
    str  x23, [x0, #32]
    str  x24, [x0, #40]
    str  x25, [x0, #48]
    str  x26, [x0, #56]
    str  x27, [x0, #64]
    str  x28, [x0, #72]
    str  x29, [x0, #80]
    str  x30, [x0, #88]
    // 保存 SP (函数调用前的当前栈指针)
    mov  x2, sp
    str  x2, [x0, #96]

    // 读系统寄存器
    // @104: EL1 侧 TTBR0. 本函数运行于 EL1 ⇒ 该值必为内核可用表
    // (EL1 视图根或完整内核表), 恢复侧内核路径据此切回.
    mrs  x2, ttbr0_el1
    str  x2, [x0, #104]
    // @112: SPSR_EL1. **不存 live SPSR**: syscall/异常中途取出的是"被打断的
    // EL0 状态" (0x340 + 用户 PC, I 位清零 ⇒ EL0 可被抢占, 见 ISSUE-RT-005),
    // 不是 EL1 续跑点. 本函数入口已 daifset #0xF
    // ⇒ 续跑点必为 EL1h (M=0b0101) + DAIF 屏蔽, 故写常量 0x3C5; 恢复侧据
    // SPSR.M[3:0] != 0 走"内核续跑"路径.
    movz x2, #0x3C5
    str  x2, [x0, #112]
    // @120: ELR_EL1 = 返回地址 (恢复侧内核路径 eret 回本函数调用点之后)
    str  x30, [x0, #120]
    // @128: SP_EL0 (用户栈指针), 供 EL0 进入路径恢复
    mrs  x2, sp_el0
    str  x2, [x0, #128]
    // @136: 用户页表 (EL0 的 TTBR0). 异常入口汇编已把当前用户页表记录到
    // **本核** KPTI 槽 (TPIDR_EL1 寻址, 偏移 0), 此处快照进 ctx, 供 EL0 进入
    // 路径与 fork 子进程继承使用.
    mrs  x2, tpidr_el1
    ldr  x2, [x2, #0]
    str  x2, [x0, #136]

    // 保存 FPU/SIMD 状态 (V0-V31, FPCR, FPSR)
    // fpu_state 在 offset 144 (18 * 8 = 144 bytes)
    // 需要 16 字节对齐，ProcessContext 已添加 _fpu_pad 保证对齐
    add  x2, x0, #144
    stp  q0, q1, [x2, #0]
    stp  q2, q3, [x2, #32]
    stp  q4, q5, [x2, #64]
    stp  q6, q7, [x2, #96]
    stp  q8, q9, [x2, #128]
    stp  q10, q11, [x2, #160]
    stp  q12, q13, [x2, #192]
    stp  q14, q15, [x2, #224]
    stp  q16, q17, [x2, #256]
    stp  q18, q19, [x2, #288]
    stp  q20, q21, [x2, #320]
    stp  q22, q23, [x2, #352]
    stp  q24, q25, [x2, #384]
    stp  q26, q27, [x2, #416]
    stp  q28, q29, [x2, #448]
    stp  q30, q31, [x2, #480]
    // FPCR / FPSR 落在 ProcessContext 的专用字段 fpcr(@656) / fpsr(@664).
    // 不能写 fpu_state[62]/[63] (= offset 640/648): 那是 q31 的高 16 字节,
    // 会覆盖 V31 并在恢复侧把 q31 残值写进 FPCR/FPSR (双向污染).
    mrs  x2, fpcr
    str  x2, [x0, #656]
    mrs  x2, fpsr
    str  x2, [x0, #664]

    // === 从 [x1] 恢复下一个上下文 ===
    ldr  x19, [x1, #0]
    ldr  x20, [x1, #8]
    ldr  x21, [x1, #16]
    ldr  x22, [x1, #24]
    ldr  x23, [x1, #32]
    ldr  x24, [x1, #40]
    ldr  x25, [x1, #48]
    ldr  x26, [x1, #56]
    ldr  x27, [x1, #64]
    ldr  x28, [x1, #72]
    ldr  x29, [x1, #80]
    ldr  x30, [x1, #88]
    // SP_EL1 (内核栈). 两条恢复路径都需要: 内核路径用它续跑, EL0 路径用它
    // 保证目标进程下次陷入 EL1 时压帧落在自己的内核栈上.
    ldr  x2, [x1, #96]
    mov  sp, x2

    // 恢复 FPU/SIMD 状态 (V0-V31, FPCR, FPSR)
    add  x2, x1, #144
    ldp  q0, q1, [x2, #0]
    ldp  q2, q3, [x2, #32]
    ldp  q4, q5, [x2, #64]
    ldp  q6, q7, [x2, #96]
    ldp  q8, q9, [x2, #128]
    ldp  q10, q11, [x2, #160]
    ldp  q12, q13, [x2, #192]
    ldp  q14, q15, [x2, #224]
    ldp  q16, q17, [x2, #256]
    ldp  q18, q19, [x2, #288]
    ldp  q20, q21, [x2, #320]
    ldp  q22, q23, [x2, #352]
    ldp  q24, q25, [x2, #384]
    ldp  q26, q27, [x2, #416]
    ldp  q28, q29, [x2, #448]
    ldp  q30, q31, [x2, #480]
    // P1.B + F-07: FPCR/FPSR 修改后必须 isb 同步才能生效,
    // 否则 eret 切换 PSTATE 时 FPU 控制位可能延后生效.
    // 落点与保存侧一致: fpcr(@656) / fpsr(@664), 不得读 640/648 (q31 高 16 字节).
    ldr  x2, [x1, #656]
    msr  fpcr, x2
    isb
    ldr  x2, [x1, #664]
    msr  fpsr, x2
    isb

    // === 恢复路径分派 (按目标 SPSR_EL1.M[3:0]) ===
    //
    // 保存侧把"内核续跑点"写成常量 0x3C5 (EL1h), 把"首次进入 EL0"的时间点
    // 由 `proc_save_user_regs_aarch64` 写成用户的 0x340 (EL0t, I 清零). 故此处以
    // SPSR.M 是否为 0 区分两条语义完全不同的恢复路径:
    //
    // - M != 0 (EL1): **内核续跑**. 本任务是"在内核里被换出"的 (schedule 调用点),
    //   恢复 x19-x30/sp 后直接 eret 回调用点之后继续执行. 必须先把 TTBR0 换回
    //   本任务自己的 EL1 页表 (@104 = 保存时的 live TTBR0 = 本进程 EL1 视图根),
    //   否则会用上一个任务的用户半区视图访问本任务的内核栈/镜像.
    // - M == 0 (EL0): **首次进入 EL0** (fork 子进程首次被调度). 目标地址是用户
    //   代码, 必须切到 (用户表 + tramp 表) 后才能 eret, 且切表代码只能在
    //   `.vectors` 高别名上执行 —— 故跳到 trampoline 而非就地 eret.
    ldr  x2, [x1, #112]
    and  x3, x2, #0xF
    cbz  x3, .Lctx_enter_el0

    // ---- 内核续跑路径 ----
    // 恢复 TTBR0 = 本任务 EL1 页表.
    // SIMPLIFIED: 不比较新值与 live 值, 一律 tlbi vmalle1is 冲刷; 影响面 = 每次
    // 同进程线程间切换多一次全表失效 (少量性能损失); 何时需扩展 = 若切换开销成为
    // 瓶颈, 可按 (@104 != 当前 TTBR0) 条件跳过.
    ldr  x2, [x1, #104]
    dsb  ish
    msr  ttbr0_el1, x2
    isb
    tlbi vmalle1is
    dsb  ish
    isb
    // 刷新**本核** KPTI 槽 user_ttbr0 = 本任务的用户页表 (@136).
    // 必需: 被换出期间别的任务会把本核该槽改写成它们自己的用户表, 而本任务
    // 续跑后必经 `el0_return` (它读该槽切 TTBR0 回 EL0) ⇒ 不刷新会 eret 到
    // EL0 时用错页表.
    mrs  x3, tpidr_el1
    ldr  x4, [x1, #136]
    str  x4, [x3, #0]
    // P1.B + F-07: ARM ARM 规定 SPSR/ELR 写入后必须 isb 才能 eret,
    // 否则 CPU 可能用旧值 eret 导致上下文错位. 同步插入 isb.
    ldr  x2, [x1, #112]
    msr  spsr_el1, x2
    isb
    ldr  x2, [x1, #120]
    msr  elr_el1, x2
    isb
    // eret 恢复 SPSR_EL1 → PSTATE (含 DAIF), 无需显式 msr daif.
    eret

    // ---- EL0 首次进入路径 ----
.Lctx_enter_el0:
    // 刷新**本核** KPTI 槽 user_ttbr0 = 本任务的用户页表 (@136); trampoline 读
    // 该槽切 TTBR0.
    mrs  x3, tpidr_el1
    ldr  x4, [x1, #136]
    str  x4, [x3, #0]
    // 用户栈指针 / 用户返回 PC / 用户 PSTATE.
    ldr  x2, [x1, #128]
    msr  sp_el0, x2
    ldr  x2, [x1, #120]
    msr  elr_el1, x2
    ldr  x2, [x1, #112]
    msr  spsr_el1, x2
    isb
    // 用户参数 x0-x7 (fork 子进程 x0 = 0, 由创建方写入 extra_regs[0]).
    // extra_regs 位于偏移 672, 超出 ldp 的 ±504 立即数范围 ⇒ 先用基址寄存器定位.
    add  x12, x1, #672
    ldp  x0, x1, [x12, #0]
    ldp  x2, x3, [x12, #16]
    ldp  x4, x5, [x12, #32]
    ldp  x6, x7, [x12, #48]
    // 跳 trampoline 的**高半区别名**: 该 trampoline 会切 TTBR0 → 用户表,
    // 切换后低半区代码即不可取指, 故必须在高别名上执行.
    // 低半区链接符号 (bit63 == 0) 需加高半区别名基数 (0xFFFF_0000_0000_0000,
    // 即 `mm::KERNEL_BASE` — L1-04 收敛后不再另有别名常量); 已是高地址则直接用.
    adrp x11, {tramp}
    add  x11, x11, #:lo12:{tramp}
    tbnz x11, #63, .Lctx_tramp_hi
    movz x10, #0xFFFF, lsl #48
    add  x11, x11, x10
.Lctx_tramp_hi:
    br   x11
"#,
    tramp = sym crate::privileged::arch::aarch64::exception::kpti_enter_user_trampoline,
);

// ============================================================================
// AArch64 上下文结构 (用于编译时验证布局)
// ============================================================================

/// AArch64 上下文布局 (对应 ProcessContext 偏移)
#[repr(C)]
pub struct Aarch64Context {
    pub x19: u64,        // offset 0 → r15
    pub x20: u64,        // offset 8 → r14
    pub x21: u64,        // offset 16 → r13
    pub x22: u64,        // offset 24 → r12
    pub x23: u64,        // offset 32 → rbx
    pub x24: u64,        // offset 40 → rbp
    pub x25: u64,        // offset 48 → rax
    pub x26: u64,        // offset 56 → rip
    pub x27: u64,        // offset 64 → rsp
    pub x28: u64,        // offset 72 → rflags
    pub x29: u64,        // offset 80 → cr3  (FP)
    pub lr: u64,         // offset 88 → cs   (x30)
    pub sp_el1: u64,     // offset 96 → ds   (内核栈指针)
    pub ttbr0_el1: u64,  // offset 104 → es  (EL1 侧 TTBR0: 视图根 / 内核表)
    pub spsr: u64,       // offset 112 → fs  (恢复路径分派依据)
    pub elr: u64,        // offset 120 → gs
    pub sp_el0: u64,     // offset 128 → ss  (用户栈指针)
    pub user_ttbr0: u64, // offset 136 → _fpu_pad (EL0 的 TTBR0)
}

// SAFETY: C ABI 互操作，函数签名与外部代码约定一致
unsafe extern "C" {
    pub fn context_switch_asm(prev: *const u64, next: *const u64);
}

/// 执行上下文切换。from/to 为原始指针 (实际指向 ProcessContext)。
///
/// # Safety
/// 调用者必须确保两个上下文指针有效且已初始化。
#[inline(always)]
pub unsafe fn switch(from: *mut u8, to: *const u8) {
    unsafe {
        context_switch_asm(from as *const u64, to as *const u64);
    }
}
