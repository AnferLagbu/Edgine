/*
 * AArch64 启动入口 (汇编)
 *
 * QEMU virt (cortex-a72) / 真机:
 *   arm64 Image 头 (0x40080000, 64B) → _start (0x40080040): 入口 EL3 → EL2 → EL1
 *
 * 职责:
 *   1. 检测当前异常级别
 *   2. EL3→EL2: 配置 SCR_EL3, 返回 EL2
 *   3. EL2→EL1: 配置 HCR_EL2, 返回 EL1
 *   4. EL1: 建立引导页表 + 开启 MMU + 切至高半区内核 VMA, 跳转 Rust entry()
 */

/*
 * arm64 Linux Image 头 (64 字节, 位于镜像文件偏移 0 == 0x40080000)
 *
 * QEMU -kernel 与 U-Boot booti 以文件偏移 56 处的 "ARM\x64" 幻数识别本格式,
 * 并按 text_offset 定位装载地址; U-Boot 另据 image_size 计算搬移长度. 字段
 * 定义见 Linux Documentation/arm64/booting.rst. code0 为 `b _start`, 故真实
 * 入口 `_start` 位于 0x40080040 (由 link/aarch64.ld 显式设定).
 */
.section .image_header, "a"
.balign 8
.global _image_header
_image_header:
    b     _start                // code0: 跳至真实入口 (0x40080040)
    .word 0                     // code1: 保留
    .quad 0x80000               // text_offset: 相对 2MiB 对齐基址的装载偏移
    .quad _image_size           // image_size: 链接期由 aarch64.ld 解析
    .quad 0                     // flags: Bit0=端序(0=LE), Bit1-2=页大小(0=4KiB)
    .quad 0                     // res2: 保留
    .quad 0                     // res3: 保留
    .quad 0                     // res4: 保留
    .word 0x644d5241            // magic: "ARM\x64" (小端存储)
    .word 0                     // res5: 保留

.section .text.boot
.global _start
_start:
    // 保存 x0 (引导程序传入的设备树物理地址). arm64 引导约定: U-Boot booti /
    // QEMU -kernel 均经 x0 传递 DTB 指针. 必须在任何覆盖 x0 的指令之前完成;
    // `_fdt_addr` 位于 .bootbss (VMA==LMA==PA), MMU 未启用时经 PC 相对寻址写入.
    adrp x1, _fdt_addr
    add  x1, x1, #:lo12:_fdt_addr
    str  x0, [x1]

// ============================================================
// 1. 确定当前异常级别 (以 x9 暂存, 不破坏 x0 已保存的 DTB 语义)
// ============================================================
    mrs  x9, CurrentEL
    lsr  x9, x9, #2        // EL = (CurrentEL >> 2) & 0x3
    and  x9, x9, #0x3

    cmp  x9, #3
    beq  el3_entry
    cmp  x9, #2
    beq  el2_entry
    cmp  x9, #1
    beq  el1_entry

    // 未知 EL: 进入死循环
    wfi
    b    .

// ============================================================
// 2. EL3 → EL2
// ============================================================
el3_entry:
    // 关闭 EL3 陷阱 (允许 EL2 访问所有系统寄存器)
    // SCR_EL3: NS=1 (非安全), HCE=1 (HVC 可用), RW=1 (EL2 为 AArch64)
    mov  x0, #(1 << 0)     // NS=1
    orr  x0, x0, #(1 << 8) // HCE=1
    orr  x0, x0, #(1 << 10) // RW=1 (lower EL is AArch64)
    msr  scr_el3, x0

    // 设置 SPSR_EL3: EL=EL2h, DAIF masked
    mov  x0, #0x3c9         // M[3:0]=1001 (EL2h), DAIF=1111 (all masked)
    msr  spsr_el3, x0

    // ELR_EL3 = el2_entry (返回地址)
    adr  x0, el2_entry
    msr  elr_el3, x0

    // P1.C + F-06: ARM ARM 建议 eret 前 isb 同步 SPSR/ELR/SCR 写入,
    // 否则 CPU 可能用旧 SCR 值 eret 导致 EL2 配置未生效.
    isb
    eret

// ============================================================
// 3. EL2 → EL1
// ============================================================
el2_entry:
    // HCR_EL2: RW=1 (EL1 为 AArch64), SWIO=1 (set/way I-cache 可用)
    mov  x0, #(1 << 31)     // RW=1
    orr  x0, x0, #(1 << 1)  // SWIO=1
    msr  hcr_el2, x0

    // CPTR_EL2: 清零 TFP (bit 10) — 否则即使 CPACR_EL1 允许, EL1 的 FP/SIMD 也会 trap 到 EL2
    mrs  x0, cptr_el2
    bic  x0, x0, #(1 << 10) // Clear TFP
    msr  cptr_el2, x0

    // 为 CPACR_EL1 和 CNTHCTL_EL2 设置最大访问权限 (EL1/EL0 可访问 FP/SIMD + Timer)
    mov  x0, #(0x3 << 20)   // FPEN = 0b11 (trap none)
    msr  cpacr_el1, x0

    // CNTHCTL_EL2: EL1PCTEN=1, EL1PCEN=1 (EL1 物理 Counter/Timer 可访问)
    mov  x0, #(1 << 0)      // EL1PCEN
    orr  x0, x0, #(1 << 1)  // EL1PCTEN
    msr  cnthctl_el2, x0

    // P1.C + F-06: 显式清零 EL1 MAIR/TCR 影子寄存器,
    // 防御 reset value 与 arch spec 不一致. EL1 引导页表建立时会立即覆盖.
    msr  mair_el1, xzr
    msr  tcr_el1, xzr

    // P1.C + F-06: 显式清零 VTTBR_EL2 (禁用 stage-2 translation),
    // ARMv8.1+ PE 默认可能在 EL2 用 stage-2. QEMU virt 不开 stage-2,
    // 但实硬件 (某些 SoC) 默认值可能不同, 显式声明意图.
    msr  vttbr_el2, xzr

    // SCTLR_EL1: 清除 (将进行配置)
    msr  sctlr_el1, xzr
    // 同步上述所有 EL1 影子寄存器 + EL2 配置 (ARM ARM 建议 eret 前 isb)
    isb

    // 设置 SPSR_EL2: EL=EL1h, DAIF masked
    mov  x0, #0x3c5         // M[3:0]=0101 (EL1h), DAIF=1111 (all masked)
    msr  spsr_el2, x0

    // ELR_EL2 = el1_entry
    adr  x0, el1_entry
    msr  elr_el2, x0

    // P1.C + F-06: ARM ARM 建议 eret 前 isb 同步 SPSR/ELR/HCR 写入,
    // EL1 配置 (CPACR_EL1/CNTHCTL_EL2/SCTLR_EL1) 必须 eret 前同步.
    isb
    eret

// ============================================================
// 4. EL1 入口 — 建立引导页表 → 开启 MMU → 切高半区内核 VMA
// ============================================================
el1_entry:
    // 4.1 设置栈指针 (boot stack, 位于 .bootbss: VMA == LMA == PA)
    adrp x0, _boot_stack_top
    add  x0, x0, #:lo12:_boot_stack_top
    mov  sp, x0

    // 4.2 (FDT 指针捕获已上移至 _start 首条指令: 此处 x0 已被 _boot_stack_top
    //      覆盖, 不可再作为 DTB 来源)

    // 4.3 建立引导页表: L0 → L1 → L2, 覆盖低 2GB.
    //   - VA 0x0000_0000_0000_0000-0x0000_0000_4000_0000 → 低 1GB Device (2MB 粒度)
    //   - VA 0x0000_0000_4000_0000-0x0000_0000_8000_0000 → DRAM 1GB Normal 块
    //   描述符取值与 mmu.rs::init() 逐条目相同 (PT_TYPE_BLOCK=1, PT_AF=0x400,
    //   PT_ATTR_NORMAL=(0b0100<<2)|(0b0100<<8)=0x410, PT_AP_EL1_RW=0).
    //   高半区别名 VA = PA + 0xFFFF_0000_0000_0000 的 VA[47:39] 恒为 0 ⇒ L0 索引
    //   恒为 0, L1/L2 索引与恒等映射完全一致 ⇒ 同一张表同时服务 TTBR0 与 TTBR1.
    //   表位于 .bootbss (NOLOAD), 内容不保证为 0, 必须先显式清零:
    //   残留 bits[1:0]==0b11 的垃圾项会被投机表遍历当作有效描述符.
    adrp x2, _boot_l0
    add  x2, x2, #:lo12:_boot_l0
    mov  x3, #0
    mov  x4, #(3 * 512)             // 3 张表 × 512 项
9:
    str  x3, [x2], #8
    subs x4, x4, #1
    b.ne 9b

    adrp x2, _boot_l1
    add  x2, x2, #:lo12:_boot_l1
    adrp x3, _boot_l0
    add  x3, x3, #:lo12:_boot_l0
    orr  x5, x2, #0x3               // L0[0] → L1 (表描述符)
    str  x5, [x3]
    adrp x3, _boot_l2
    add  x3, x3, #:lo12:_boot_l2
    orr  x5, x3, #0x3               // L1[0] → L2 (表描述符)
    str  x5, [x2]
    movz x5, #0x4000, lsl #16       // L1[1] → DRAM 1GB 块 @ PA 0x40000000
    movz x6, #0x411                 //   | block | AF(bit10) | AttrIndx=4
    orr  x5, x5, x6                 //   0x411 非位掩码立即数, 需经寄存器拼接
    str  x5, [x2, #8]

    // L2: 512 个 2MB Device 块, 覆盖 0x00000000-0x40000000
    adrp x2, _boot_l2
    add  x2, x2, #:lo12:_boot_l2
    mov  x5, #0x401                 // 首项: PA 0 | block | AF (AttrIndx=0, EL1 RW)
    movz x7, #0x20, lsl #16         // 2MB 步长
    mov  x4, #512
8:
    str  x5, [x2], #8
    add  x5, x5, x7
    subs x4, x4, #1
    b.ne 8b

    // 4.4 MAIR_EL1: Attr0=Device-nGnRnE(0x44), Attr1/Attr4=Normal-WBWA(0xFF).
    //   与 mmu.rs::init() 取值一致. 若不设 (MAIR=0), AttrIndx=4 会被硬件按
    //   Device-nGnRnE 解释 ⇒ DRAM 取指行为不可预期.
    movz x5, #0xFF44
    movk x5, #0x00FF, lsl #32
    msr  mair_el1, x5

    // 4.5 TCR_EL1: T0SZ=T1SZ=16 (48-bit VA), TG0/TG1=4KB, 内部共享 + WBWA.
    //   与 mmu.rs::init() 同值 ⇒ 迁移后 mmu::init() 重写为同值, 无窗口期.
    movz x5, #0x3510
    movk x5, #0xB510, lsl #16
    msr  tcr_el1, x5

    // 4.6 TTBR0/TTBR1 同指引导表 (低半区恒等 + 高半区别名共用一张 L0)
    adrp x5, _boot_l0
    add  x5, x5, #:lo12:_boot_l0
    msr  ttbr0_el1, x5
    msr  ttbr1_el1, x5
    dsb  sy
    isb

    // 4.7 清 TLB 后开启 MMU (M / C / I)
    tlbi vmalle1
    dsb  sy
    isb
    mrs  x5, sctlr_el1
    orr  x5, x5, #1                 // M: MMU
    orr  x5, x5, #4                 // C: D-cache
    orr  x5, x5, #(1 << 12)         // I: I-cache
    msr  sctlr_el1, x5
    isb

    // 4.8 切至高半区内核 VMA. 内核区链接于高半区 (VMA = _kernel_base + LMA),
    //   故 entry 的符号地址远超 `bl` 的 ±128MB 范围, 必须经寄存器间接跳转.
    //   (DTB 指针已由 _start 存入 _fdt_addr, 此处与 x0 无关.)
    ldr  x9, =entry
    br   x9

    // entry() should never return
    wfi
    b    .

// ============================================================
// 5. 次核 (AP) 入口 — PSCI `CPU_ON` 的 entry point
// ============================================================
// 次核以 MMU 关 / 物理地址 / 低半区进入 (PSCI 上电保证). 本 stub 位于
// `.text.boot` (VMA == LMA == PA), 故 `adrp` 取得的是物理地址, 开 MMU 后该
// 地址仍在恒等映射窗口内, x10 持续有效. x0 = context_id (= cpu_index).
.global ap_entry_asm
.type ap_entry_asm, %function
ap_entry_asm:
    adrp    x10, _ap_boot_info
    add     x10, x10, #:lo12:_ap_boot_info
    // 实测 CurrentEL 并落槽 (E7 诊断: 0b01=EL1 / 0b10=EL2 / 0b11=EL3)
    mrs     x9, CurrentEL
    lsr     x9, x9, #2
    str     x9, [x10, #0x48]
    cmp     x9, #1
    b.ne    ap_bad_el
    // 装入 BSP 预写的运行时 MMU 配置 (复用 BSP 页表物理基址, 见 DECISION-082 裁定 3)
    ldr     x11, [x10, #0x00]
    msr     ttbr0_el1, x11
    ldr     x11, [x10, #0x08]
    msr     ttbr1_el1, x11
    ldr     x11, [x10, #0x10]
    msr     mair_el1, x11
    ldr     x11, [x10, #0x18]
    msr     tcr_el1, x11
    tlbi    vmalle1
    dsb     sy
    isb
    // 次核独立栈 (高半区 VA)
    ldr     x12, [x10, #0x28]
    mov     sp, x12
    // 开 MMU (SCTLR_EL1 的 M/C/I 已在槽内预置)
    ldr     x13, [x10, #0x20]
    msr     sctlr_el1, x13
    dsb     sy
    isb
    // cpu_index → ap_main 第 1 参; ap_main 高半区 VA
    ldr     x0, [x10, #0x38]
    ldr     x14, [x10, #0x30]
    br      x14

// 占位分支: 次核未处于 EL1 时自旋 (PSCI 已上电但环境不满足).
// SIMPLIFIED: 仅作 E7 诊断占位, 置 done=1 供 BSP 超时后读取 EL; 若实测为 EL2
//   需在此补 el2→el1 降级序列; 影响面: 该 AP 不上线 (BSP 超时兜底不阻塞单核);
//   何时需扩展: E7 实测确认次核 EL 级后按结论固化本分支.
ap_bad_el:
    mov     x15, #1
    str     x15, [x10, #0x40]
    b       ap_bad_el

// ============================================================
// 引导区数据/页表/栈 (NOLOAD, 低半区 VMA == LMA == PA)
// ============================================================
.section .bootbss, "aw", %nobits

// FDT 地址 (从 QEMU 传来)
.align 3
.global _fdt_addr
_fdt_addr:
    .space 8

// 次核启动信息槽 (BSP 写 / AP 读); 布局见 arch/aarch64/smp_init.rs 的
// `ApBootInfo` (10 × u64 = 80 字节, 编译期 size_of 断言).
.align 3
.global _ap_boot_info
_ap_boot_info:
    .space 80

// 引导页表: L0 / L1 / L2 各 4KB, 连续排布 (4.3 的清零循环依赖连续性)
.align 12
.global _boot_l0
_boot_l0:
    .space 4096
.align 12
.global _boot_l1
_boot_l1:
    .space 4096
.align 12
.global _boot_l2
_boot_l2:
    .space 4096

// Boot 栈
// 注: Rust 侧 process.rs 引用的 `stack_bottom` 由链接脚本 (aarch64.ld) 定义为
// 本符号的**高半区别名** (低半区符号距内核代码超出 adrp ±4GB 可达范围),
// 故 `_boot_stack_bottom` 须为全局符号 (链接脚本表达式只能引用全局符号).
.align 4
.global _boot_stack_bottom
_boot_stack_bottom:
    .space 262144            // 256KB boot stack
_boot_stack_top:
