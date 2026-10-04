; B07 MSI-X: vector 0x80-0x9F (irq80-127, MSI 第二段)
; =============================================================================
; isr.asm — x86_64 中断服务程序汇编 stub
;
; 为 IDT 初始化提供 32 ISR + 16 IRQ + syscall + recovery 入口。
; 栈帧布局与 InterruptFrame #[repr(C, packed)] 一致。
;
; ⚠ 教训 (TRACK-INIT-RING3): 绝对禁止在任何入口点 (syscall_entry,
; isr_common, irq_common) 的寄存器保存 (push) 之前插入修改通用寄存器
; 的调试代码 (如 out 0xe9, al)。入口点寄存器承载调用约定约定的值
; (syscall 号、异常码、中断向量), 修改即破坏, 不可恢复。
; 若需调试入口到达, 使用不修改寄存器的机制 (如内存写、LAPIC 调试)。
; =============================================================================

BITS 64

section .text

extern exception_handler
extern irq_handler

; 用户态 CR3 临时保存 (KPTI: 汇编在切换到内核页表前写入, Rust page fault handler 读取)
section .bss
align 8
global USER_CR3_SAVE
USER_CR3_SAVE: resq 1

; 切换回 .text 段, 后续代码必须在 .text 段 (不能在 .bss)
section .text

; ── 通用 ISR stub (无 CPU 错误码) ───────────────────────────────────────
%macro isr_noerr 1
global isr%1
isr%1:
    cli
    push 0
    push %1
    jmp isr_common
%endmacro

; ── ISR stub (CPU 已推入错误码: 8,10-14,17) ────────────────────────────
%macro isr_err 1
global isr%1
isr%1:
    cli
    push %1
    jmp isr_common
%endmacro

; ── 通用 IRQ stub ───────────────────────────────────────────────────────
%macro irq_stub 2
global irq%1
irq%1:
    cli
    push 0
    push %2
    jmp irq_common
%endmacro

; ── 通用入口: 保存寄存器 → exception_handler ────────────────────────────
; 栈布局 (进入 isr_common 时):
;   [rsp+0]  = int_no
;   [rsp+8]  = err_code
;   [rsp+16] = RIP      (CPU 推入)
;   [rsp+24] = CS       (CPU 推入)
;   [rsp+32] = RFLAGS   (CPU 推入)
;   [rsp+40] = RSP      (CPU 推入)
;   [rsp+48] = SS       (CPU 推入)
isr_common:
    ; ── KPTI: 如果来自用户态, 切换到内核页表 ──────────────────────
    ; 检查栈上 CS: 用户代码段 = 0x23, 内核代码段 = 0x08
    ; 来自用户态时 GS 仍为用户 GS, 需要 swapgs 才能读 per-CPU PML4
    ; ⚠ 关键修复 (TRACK-INIT-RING3): 必须在 push rax 之前检查 CS,
    ; 否则栈偏移会被诊断代码破坏.
    cmp word [rsp+24], 0x23
    jne .isr_no_kpti_enter
    swapgs

    ; 保存用户 CR3: 硬件 CR3 此时仍是用户页表
    mov rax, cr3

    mov [USER_CR3_SAVE], rax

    ; 切换到内核页表 (与 syscall_entry 一致: 从 per-CPU 加载内核 PML4)
    ; 教训 (TRACK-INIT-RING3-ISR): 原实现 mov cr3, rax 写回刚保存的用户
    ; CR3, 导致异常处理器在用户页表下访问内核静态数据 → #PF → Triple Fault.
    mov rax, [gs:KERNEL_PML4_OFF]
    mov cr3, rax
    ; 不 swapgs 回来: exception_handler 在内核 GS 下运行 (syscall_entry 模式)
.isr_no_kpti_enter:

    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    call exception_handler

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16

    ; ── KPTI: 如果返回用户态, 切换到用户页表 ──────────────────────
    ; add rsp, 16 后栈布局: [rsp+0]=RIP, [rsp+8]=CS, [rsp+16]=RFLAGS
    ; CS 在 [rsp+8], 不是 [rsp+24] (入口时 CS 在 [rsp+24] 是因为
    ; ISR stub 推入了 int_no+err_code, 但 add rsp,16 已跳过它们)
    cmp word [rsp+8], 0x23
    jne .isr_no_kpti_exit
    ; 此时 GS = 内核 GS (入口已 swapgs 一次), 直接读 per-CPU 用户 PML4
    mov rax, [gs:USER_PML4_OFF]
    mov cr3, rax
    swapgs
.isr_no_kpti_exit:

    iretq

; ── SYSCALL 指令入口 (替代 int 0x80) ─────────────────────────────────────
; 控制流：
;   1. 用户态执行 syscall 指令
;   2. CPU 保存 RIP→RCX, RFLAGS→R11, 加载 CS=STAR[47:32], SS=STAR[47:32]+8
;   3. swapgs → GS 指向 per-CPU SyscallPerCpu 数据
;   4. mov r14, [gs:0] → 加载内核栈顶 (kernel_rsp); 用户 RSP 存入 [gs:USER_RSP_OFF]
;   5. 切 CR3 到内核页表 → 切 RSP 到内核栈
;   6. 构建 InterruptFrame, 调用 syscall_dispatch_from_frame
;   7. 返回: 切用户页表 → iretq 返回用户态 (内核栈为高半区 VA, 无需别名)
;
; SMP 安全: 每个 CPU 有独立的 SyscallPerCpu, 其 kernel_rsp 由
; `cpu::arch::set_kernel_stack` (经 gdt_set_kernel_rsp) 在每次上下文切换时
; 更新为**当前任务内核栈顶 (高半区 VA)** —— 任务因而在 syscall 中让出 CPU
; 后仍保有私有内核栈 (D5 统一内核栈契约). IA32_KERNEL_GS_BASE 在
; gdt_init/gdt_init_ap 中分别设置。

; SyscallPerCpu 字段偏移 (与 gdt.rs SyscallPerCpu 结构体布局一致)
KERNEL_RSP_OFF  equ 0
KERNEL_PML4_OFF equ 8
USER_PML4_OFF   equ 16
USER_RSP_OFF    equ 24
; TRAMPOLINE_TOP_OFF: 本文件不使用, 仅作 SyscallPerCpu 布局锚点 —— 消费方是
; proc/switch.asm 的用户态出口 (KPTI-08 方案 B), 二者偏移必须与 gdt.rs 字段
; 顺序严格一致; 在此列出以免新增字段时遗漏同步.
TRAMPOLINE_TOP_OFF equ 32

global syscall_entry
syscall_entry:
    ; ═══════════════════════════════════════════════════════════════════
    ; 教训 (TRACK-INIT-RING3): 入口处绝对禁止修改任何通用寄存器
    ; 在 push/pop 保存上下文之前. 此前曾在此处插入调试代码
    ;   mov al, 0x53          ; 'S' → 破坏 RAX 低字节
    ;   out 0xe9, al
    ; 导致 RAX 中的 syscall 号被覆盖 (write=1 → 0x53=83=mkdir),
    ; 使 write syscall 被误判为 mkdir, 且因 mkdir 恰好也是 83
    ; 而未被发现. 入口点修改寄存器 = 破坏调用约定 = 不可恢复.
    ; ═══════════════════════════════════════════════════════════════════

    swapgs

    ; ═══════════════════════════════════════════════════════════════════
    ; 教训 (TRACK-INIT-RING3-CR3): CR3 切换必须在栈切换之前.
    ; 此前流程: 先切内核栈 (mov rsp, r14) → push rax → 页错误.
    ; 根因: 内核栈 (syscall_stack) 不在用户页表中, 但此时 CR3 仍指向
    ; 用户页表, push 触发 #PF. 正确顺序: 先切 CR3 (mov cr3, r12)
    ; → 再切 RSP (mov rsp, r14), 确保 push 在内核页表保护下执行.
    ; ═══════════════════════════════════════════════════════════════════

    xor r15d, r15d                  ; R15 = 0 = KERNEL_RSP_OFF
    mov r14, [gs:r15]               ; R14 = kernel_rsp (暂存, CR3 切换后使用)

    ; 使用用户栈暂存 R12 作为 CR3 操作临时寄存器.
    ; push/pop 配对, 用户栈净效果为零, 中断已由 SFMASK 禁用.
    push r12                        ; (a) 保存用户 R12 到用户栈
    mov r12, cr3                    ; R12 = 用户 CR3
    mov [USER_CR3_SAVE], r12        ; 保存用户 CR3 (USER_CR3_SAVE 在用户页表中已映射)
    pop r12                         ; (b) 恢复用户 R12, 用户栈恢复原状

    mov [gs:USER_RSP_OFF], rsp       ; 保存用户 RSP (pop r12 后, 即原始值)
    ; 注: 保存到独立字段 USER_RSP_OFF, 不覆盖 [gs:KERNEL_RSP_OFF] (kernel_rsp),
    ; 否则首次 syscall 后 kernel_rsp 丢失, 后续 syscall 用错内核栈
    ; (TRACK-INIT-RING3-SYSCALL-RET).

    mov r12, [gs:KERNEL_PML4_OFF]   ; R12 = 内核 PML4 物理地址
    mov cr3, r12                    ; ← 切换到内核页表 (此后所有访存走内核页表)

    mov rsp, r14                    ; 切换到内核 RSP (安全: 内核页表已加载)

    ; 构建 InterruptFrame (与 int 0x80 中断帧布局一致)
    push 0x1B                         ; SS = 用户数据段 (0x18|3)

    push qword [gs:USER_RSP_OFF]      ; 用户 RSP (入口时已存入独立字段)

    push r11                          ; RFLAGS

    push 0x23                         ; CS = 用户代码段 (0x20|3)

    push rcx                          ; RIP

    push 0                            ; err_code

    push 0x80                         ; int_no

    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    cld
    call syscall_dispatch_from_frame

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16                       ; 跳过 int_no + err_code

    ; ── 返回路径: 使用 iretq 代替 sysretq ──────────────────────────
    ; 栈上已有完整的 iretq 帧: RIP, CS, RFLAGS, RSP, SS
    ; 不需要 xchg rsp 切换到用户栈 (iretq 从栈上读取 RSP),
    ; 避免在 Ring 0 使用用户栈时被中断导致中断帧推入用户栈.
    ;
    ; 栈布局 (add rsp, 16 后):
    ;   [rsp+0]  = RIP   (用户返回地址)
    ;   [rsp+8]  = CS    (0x23, 用户代码段)
    ;   [rsp+16] = RFLAGS
    ;   [rsp+24] = RSP   (用户栈)
    ;   [rsp+32] = SS    (0x1B, 用户数据段)

    cli                                ; 禁用中断: KPTI 切换 CR3 期间不可中断

    ; ── KPTI: 切换到用户页表 ──────────────────────────────────────
    ; iretq 前 CR3 必须切回 USER_PML4, 否则用户态无法寻址.
    ; 当前在内核栈上, [gs:OFF] 可安全访问.
    ;
    ; 教训: mov cr3, rax 会覆盖 RAX, 入口路径有 push/pop rax 保护,
    ; 但退出路径此前遗漏了该保护, 导致所有 syscall 返回值被用户页表
    ; 物理地址覆盖, 表现为用户态看到随机的 "成功" 返回值.
    ;
    ; 不再做 KERNEL_BASE 别名转换 (原 TRACK-INIT-RING3-SYSCALL-RET 的
    ; `add rsp, KERNEL_BASE` 已删除): 统一内核栈契约 (D5) 规定
    ; [gs:KERNEL_RSP_OFF] 恒为高半区 VA (任务内核栈 phys + KERNEL_BASE,
    ; 见 cpu::arch::set_kernel_stack / gdt_set_kernel_rsp), 本栈在高半区
    ; 直接映射 (共享 pd_high) 中已映射, 切换用户页表后 pop/iretq 仍可读
    ; 同一物理帧. 若再叠加一次别名偏移, RSP 落到 phys + 2*KERNEL_BASE
    ; (未映射) → #PF.
    push rax                           ; 保护 syscall 返回值 (高半区内核栈)
    mov rax, [gs:USER_PML4_OFF]
    mov cr3, rax
    pop rax                            ; 恢复 syscall 返回值 (同一物理帧)

    swapgs                            ; 恢复用户 GS 段
    iretq                             ; iretq 帧从内核栈读取 (用户表已映射)

; ── 通用入口: 保存寄存器 → irq_handler ──────────────────────────────────
; 栈布局同 isr_common
irq_common:
    ; ── KPTI: 如果来自用户态, 切换到内核页表 ──────────────────────
    cmp word [rsp+24], 0x23
    jne .irq_no_kpti_enter
    swapgs

    ; 保存用户 CR3
    mov rax, cr3

    mov [USER_CR3_SAVE], rax

    ; 切换到内核页表
    ; 教训 (TRACK-INIT-RING3-IRQ): 此处必须从 [gs:KERNEL_PML4_OFF] 加载内核
    ; PML4, 而非 mov cr3, rax (rax 是刚保存的用户 CR3). 原实现写回用户 CR3,
    ; 导致用户态中断在用户页表下运行 IRQ 处理器 → 访问内核静态数据 #PF →
    ; 嵌套 #PF → #DF → Triple Fault (init 首个 syscall 前静默崩溃).
    ; 与 syscall_entry 的 KPTI 切换模式保持一致.
    mov rax, [gs:KERNEL_PML4_OFF]
    mov cr3, rax
    ; 不 swapgs 回来: irq_handler 在内核 GS 下运行 (syscall_entry 模式).
    ; 原实现在此再次 swapgs, 导致 handler 在用户 GS 下访问 per-CPU 错乱
    ; (tick 等写入低物理地址, 可能破坏用户页表 → 用户态取指 #PF).
.irq_no_kpti_enter:

    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    call irq_handler

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16

    ; ── KPTI: 如果返回用户态, 切换到用户页表 ──────────────────────
    ; add rsp, 16 后栈布局: [rsp+0]=RIP, [rsp+8]=CS, [rsp+16]=RFLAGS
    ; CS 在 [rsp+8], 不是 [rsp+24] (入口时 CS 在 [rsp+24] 是因为
    ; ISR stub 推入了 int_no+err_code, 但 add rsp,16 已跳过它们)
    cmp word [rsp+8], 0x23
    jne .irq_no_kpti_exit
    ; 此时 GS = 内核 GS (入口已 swapgs 一次), 直接读 per-CPU 用户 PML4
    mov rax, [gs:USER_PML4_OFF]
    mov cr3, rax
    swapgs
.irq_no_kpti_exit:
    iretq

; ── 实例化 ──────────────────────────────────────────────────────────────
isr_noerr 0
isr_noerr 1
isr_noerr 2
isr_noerr 3
isr_noerr 4
isr_noerr 5
isr_noerr 6
isr_noerr 7
isr_err   8
isr_noerr 9
isr_err   10
isr_err   11
isr_err   12
isr_err   13
isr_err   14
isr_noerr 15
isr_noerr 16
isr_err   17
isr_noerr 18
isr_noerr 19
isr_noerr 20
isr_noerr 21
isr_noerr 22
isr_noerr 23
isr_noerr 24
isr_noerr 25
isr_noerr 26
isr_noerr 27
isr_noerr 28
isr_noerr 29
isr_noerr 30
isr_noerr 31

irq_stub 0,  32
irq_stub 1,  33
irq_stub 2,  34
irq_stub 3,  35
irq_stub 4,  36
irq_stub 5,  37
irq_stub 6,  38
irq_stub 7,  39
irq_stub 8,  40
irq_stub 9,  41
irq_stub 10, 42
irq_stub 11, 43
irq_stub 12, 44
irq_stub 13, 45
irq_stub 14, 46
irq_stub 15, 47

; ── MSI 向量 (0x40-0x7F) ─────────────────────────────────────────────
; 64 个 MSI 向量 stub, 使用 irq_common 入口 → irq_handler FFI
irq_stub 16, 64
irq_stub 17, 65
irq_stub 18, 66
irq_stub 19, 67
irq_stub 20, 68
irq_stub 21, 69
irq_stub 22, 70
irq_stub 23, 71
irq_stub 24, 72
irq_stub 25, 73
irq_stub 26, 74
irq_stub 27, 75
irq_stub 28, 76
irq_stub 29, 77
irq_stub 30, 78
irq_stub 31, 79
irq_stub 32, 80
irq_stub 33, 81
irq_stub 34, 82
irq_stub 35, 83
irq_stub 36, 84
irq_stub 37, 85
irq_stub 38, 86
irq_stub 39, 87
irq_stub 40, 88
irq_stub 41, 89
irq_stub 42, 90
irq_stub 43, 91
irq_stub 44, 92
irq_stub 45, 93
irq_stub 46, 94
irq_stub 47, 95
irq_stub 48, 96
irq_stub 49, 97
irq_stub 50, 98
irq_stub 51, 99
irq_stub 52, 100
irq_stub 53, 101
irq_stub 54, 102
irq_stub 55, 103
irq_stub 56, 104
irq_stub 57, 105
irq_stub 58, 106
irq_stub 59, 107
irq_stub 60, 108
irq_stub 61, 109
irq_stub 62, 110
irq_stub 63, 111
irq_stub 64, 112
irq_stub 65, 113
irq_stub 66, 114
irq_stub 67, 115
irq_stub 68, 116
irq_stub 69, 117
irq_stub 70, 118
irq_stub 71, 119
irq_stub 72, 120
irq_stub 73, 121
irq_stub 74, 122
irq_stub 75, 123
irq_stub 76, 124
irq_stub 77, 125
irq_stub 78, 126
irq_stub 79, 127
irq_stub 80, 128
irq_stub 81, 129
irq_stub 82, 130
irq_stub 83, 131
irq_stub 84, 132
irq_stub 85, 133
irq_stub 86, 134
irq_stub 87, 135
irq_stub 88, 136
irq_stub 89, 137
irq_stub 90, 138
irq_stub 91, 139
irq_stub 92, 140
irq_stub 93, 141
irq_stub 94, 142
irq_stub 95, 143
irq_stub 96, 144
irq_stub 97, 145
irq_stub 98, 146
irq_stub 99, 147
irq_stub 100, 148
irq_stub 101, 149
irq_stub 102, 150
irq_stub 103, 151
irq_stub 104, 152
irq_stub 105, 153
irq_stub 106, 154
irq_stub 107, 155
irq_stub 108, 156
irq_stub 109, 157
irq_stub 110, 158
irq_stub 111, 159
irq_stub 112, 160
irq_stub 113, 161
irq_stub 114, 162
irq_stub 115, 163
irq_stub 116, 164
irq_stub 117, 165
irq_stub 118, 166
irq_stub 119, 167
irq_stub 120, 168
irq_stub 121, 169
irq_stub 122, 170
irq_stub 123, 171
irq_stub 124, 172
irq_stub 125, 173
irq_stub 126, 174
irq_stub 127, 175

; ── IPI 向量 (0xFD TLB 失效 / 0xFE reschedule) ─────────────────────────
; 符号名按向量号命名 (irq253/irq254), 避开既有 irq0-irq127;
; 沿用 irq_common 入口, 由 handle_irq 前置分支处理.
irq_stub 253, 0xFD
irq_stub 254, 0xFE

; ── syscall / recovery ─────────────────────────────────────────────────
extern syscall_dispatch_from_frame

global syscall_handler
syscall_handler:
    cli
    push 0
    push 0x80

    ; ── KPTI: 如果来自用户态, 切换到内核页表 ──────────────────────
    cmp word [rsp+24], 0x23
    jne .syscall_handler_no_kpti_enter
    swapgs
    ; 保存用户 CR3
    mov rax, cr3
    mov [USER_CR3_SAVE], rax
    mov rax, [gs:KERNEL_PML4_OFF]
    mov cr3, rax
    swapgs
.syscall_handler_no_kpti_enter:

    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    cld
    call syscall_dispatch_from_frame

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16

    ; ── KPTI: 如果返回用户态, 切换到用户页表 ──────────────────────
    ; add rsp, 16 后栈布局: [rsp+0]=RIP, [rsp+8]=CS, [rsp+16]=RFLAGS
    ; CS 在 [rsp+8], 不是 [rsp+24]
    cmp word [rsp+8], 0x23
    jne .syscall_handler_no_kpti_exit
    swapgs
    mov rax, [gs:USER_PML4_OFF]
    mov cr3, rax
    swapgs
.syscall_handler_no_kpti_exit:

    iretq

global isr0x82
isr0x82:
    cli
    push 0
    push 0x82

    ; ── KPTI: 如果来自用户态, 切换到内核页表 ──────────────────────
    cmp word [rsp+24], 0x23
    jne .isr0x82_no_kpti_enter
    swapgs
    ; 保存用户 CR3
    mov rax, cr3
    mov [USER_CR3_SAVE], rax
    mov rax, [gs:KERNEL_PML4_OFF]
    mov cr3, rax
    swapgs
.isr0x82_no_kpti_enter:

    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    cld
    call exception_handler

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16

    ; ── KPTI: 如果返回用户态, 切换到用户页表 ──────────────────────
    ; add rsp, 16 后栈布局: [rsp+0]=RIP, [rsp+8]=CS, [rsp+16]=RFLAGS
    ; CS 在 [rsp+8], 不是 [rsp+24]
    cmp word [rsp+8], 0x23
    jne .isr0x82_no_kpti_exit
    swapgs
    mov rax, [gs:USER_PML4_OFF]
    mov cr3, rax
    swapgs
.isr0x82_no_kpti_exit:

    iretq

