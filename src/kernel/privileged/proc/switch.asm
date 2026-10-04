BITS 64

section .text
global process_switch_asm
global user_entry_trampoline
extern user_entry_target
extern user_entry_cr3
; P2.B + F-13 (DECISION-051): GDT 选择子强绑定.
; O-04 (proc/switch.asm:113 `mov ax, 0x23` 硬编码) 同期处理.
extern SELECTOR_USER_CODE

; KPTI-08: SyscallPerCpu.trampoline_top 的偏移 (须与 gdt.rs SyscallPerCpu 字段顺序
; 及 isr.asm TRAMPOLINE_TOP_OFF 保持一致).
; 该字段由 gdt_init/gdt_init_ap 写入本 CPU KPTI 出口栈顶 VA (高半区), 其栈顶页已在
; 每个用户页表中显式映射 (kpti::map_kernel_pages_in_user_pml4), 供本文件用户态出口
; 在切 CR3 前后构建/读取 iretq 帧.
TRAMPOLINE_TOP_OFF equ 32

; void process_switch_asm(ProcessContext* prev, const ProcessContext* next);
; RDI = *mut ProcessContext (save current register state here)
; RSI = *const ProcessContext (load next register state from here)
;
; ProcessContext layout (each field 8 bytes):
;   +0:   r15
;   +8:   r14
;  +16:   r13
;  +24:   r12
;  +32:   rbx
;  +40:   rbp
;  +48:   rax
;  +56:   rip
;  +64:   rsp
;  +72:   rflags
;  +80:   cr3
;  +88:   cs
;  +96:   ds
; +104:   es
; +112:   fs
; +120:   gs
; +128:   ss
; +136:   _fpu_pad (8 bytes padding for 16-byte alignment)
; +144:   fpu_state[64] (512 bytes, Phase 2: fxsave/fxrstor)
; +656:   fpcr  (aarch64)
; +664:   fpsr  (aarch64)
; +672:   extra_regs[8] (B05-55: rdi/rsi/rdx/rcx/r8/r9/r10/r11, 首次进入用户态
;                      的进程 (fork 子进程) 由 iretq 前恢复, 见 types.rs)

process_switch_asm:
    cli

    ; Save current context to [RDI] (prev)
    mov [rdi + 0], r15
    mov [rdi + 8], r14
    mov [rdi + 16], r13
    mov [rdi + 24], r12
    mov [rdi + 32], rbx
    mov [rdi + 40], rbp
    mov [rdi + 48], rax

    ; Save rip, rsp, rflags from stack
    mov rax, [rsp]
    mov [rdi + 56], rax        ; rip (return address)
    lea rax, [rsp + 8]
    mov [rdi + 64], rax        ; rsp
    pushfq
    pop rax
    mov [rdi + 72], rax        ; rflags

    ; Save cr3
    mov rax, cr3
    mov [rdi + 80], rax

    ; Save segment registers
    mov [rdi + 88], cs
    mov [rdi + 96], ds
    mov [rdi + 104], es
    mov [rdi + 112], fs
    mov [rdi + 120], gs
    mov [rdi + 128], ss

    ; Save FPU/SSE state (fxsave requires 16-byte aligned memory)
    ; fpu_state is at offset 144 (17 fields + 1 padding = 18 * 8 = 144 bytes)
    lea rax, [rdi + 144]
    fxsave [rax]

    ; Restore next context from [RSI] (next)
    mov r15, [rsi + 0]
    mov r14, [rsi + 8]
    mov r13, [rsi + 16]
    mov r12, [rsi + 24]
    mov rbx, [rsi + 32]
    mov rbp, [rsi + 40]

    ; KPTI-08: CR3 切换位置下移 (原先在此处立即切到 next 的页表).
    ; 收窄后 next 用户页表只含"入口依赖面"内核页 (出口 trampoline 段/入口 text/
    ; GDT/IDT/TSS/栈顶页), 而下方仍需访问 next 的 ProcessContext ([rsi + ...],
    ; 位于内核堆高半区别名) 并装载段寄存器 (读 GDT 描述符). 若提前切到 next
    ; 用户页表, 这些访存将 #PF. 故改为: 两条分支各自在**最后一次**访问 [rsi]
    ; 之后才切 CR3 —— 内核线程分支切后仅使用已取到的栈/rip; 用户分支由出口
    ; stub 在 `.kpti_trampoline` 段内完成切换.

    ; Restore segment registers (ds, es, fs, gs)
    ; cs and ss are restored via iretq frame
    ; B05-55 修复: 调度器上下文 (syscall/中断入口 swapgs 后) GS base=per_cpu,
    ; KERNEL_GS_BASE=0. 切到用户进程前先 swapgs, 使 KERNEL_GS_BASE=per_cpu —
    ; 否则用户态异常/中断入口 isr_common/irq_common 的 swapgs 会把 KERNEL_GS_BASE
    ; (0) 换入 GS base → [gs:KERNEL_PML4_OFF] 访问地址 8 → #PF → 死循环.
    ; GS base 随后由 mov gs (用户数据段 base=0) 恢复为 0 (用户 GS).
    ; 仅 next 为用户态 (cs=0x23) 时 swapgs; 内核态/内核线程切换 (cs=0x08) 不 swapgs.
    ;
    ; GS 段寄存器装载**仅限用户态路径**: `mov gs, sel` 以描述符基址 (数据段基址恒 0)
    ; 写入 IA32_GS_BASE (见 arch/x86_64/mod.rs enter_user_asm 处同一结论). 用户态路径
    ; 正需 base=0, 且此时 KERNEL_GS_BASE 已在 swapgs 后为 per-CPU 地址; 而内核态恢复
    ; 路径 (任务阻塞在 syscall 内被换回) 若照搬装载, 会把 per-CPU 基址清零, 使随后
    ; 内核态 [gs:...] 访问 (KPTI 出口读 USER_PML4) 落到物理低地址 → 读垃圾 CR3 挂起.
    ;
    ; KPTI-08: 先读本 CPU KPTI 出口栈顶 (借用 rdi; rdi 在保存路径后即空闲, 用户态
    ; 路径末尾由 extra_regs 重载). 必须在 swapgs **之前**读: swapgs 后 GS base 换为
    ; 0 (用户 GS), `[gs:TRAMPOLINE_TOP_OFF]` 会落到线性地址 32. 取到的是高半区 VA,
    ; 其栈顶页已在每个用户页表中显式映射, 供出口 stub 在切 CR3 后压/弹 iretq 帧.
    mov rdi, [gs:TRAMPOLINE_TOP_OFF]
    cmp word [rsi + 88], 0x23
    jne .no_swapgs_next
    swapgs
    mov ax, [rsi + 120]
    mov gs, ax
.no_swapgs_next:
    mov ax, [rsi + 96]
    mov ds, ax
    mov ax, [rsi + 104]
    mov es, ax
    mov ax, [rsi + 112]
    mov fs, ax

    ; Restore FPU/SSE state (fxrstor requires 16-byte aligned memory)
    ; fpu_state is at offset 144 (17 fields + 1 padding = 18 * 8 = 144 bytes)
    lea rax, [rsi + 144]
    fxrstor [rax]

    ; ── 路径分派: 内核线程 (cs=0x08) vs 用户态 (cs=0x23) ─────────────
    ; 本判断唯一作用是**选择栈切换方式** (KPTI-08 前在 caller-saved 恢复处还有
    ; 第二个 0x23 判断, 因内核线程分支已在此提前跳离而恒为真, 已作为死分支删除).
    ;
    ; iretq 在同特权级下只弹 RIP/CS/RFLAGS, 不加载 RSP/SS (x86 SDM),
    ; 因此不能靠 iretq 完成内核线程的栈切换.
    ; 改用 mov rsp + jmp 实现: 保存侧把 rip 存为 context_switch 的返回地址、
    ; rsp 存为该返回地址之上的位置 (见本文件上方 mov rax,[rsp] / lea rax,[rsp+8]),
    ; 故 "mov rsp, [rsi+64]; jmp qword [rsi+56]" 与 ret 完全等价, 与保存侧对称.
    ; 此时 rsi 仍指向 next 的 ProcessContext (mov rsp 不改 rsi), 该结构位于
    ; 内核内存, 换栈后依旧可读.
    cmp word [rsi + 88], 0x23
    je .switch_user_path
    ; 内核线程: next 的 CR3 即内核 PML4 (全内核映射, 无收窄), 切换后仍可继续
    ; 读 [rsi] 取 rax/rsp/rip, 故在此切 CR3 无映射面风险.
    mov rax, [rsi + 80]         ; cr3
    mov cr3, rax
    mov rax, [rsi + 48]         ; 内核线程仅需恢复 rax
    mov rsp, [rsi + 64]         ; 切到 next 的栈
    jmp qword [rsi + 56]        ; 跳到 next 的 rip (首次为 idle_entry)
.switch_user_path:

    ; ── KPTI-08 方案 B: 用户态出口改用 per-CPU trampoline 栈 ──────────
    ; 收窄后 next 用户页表不再复制内核高半区别名, prev 的内核栈 (本函数当前
    ; 运行栈) 在 next 页表中不可达, 故**不能**再在原栈上 push/iretq —— 此前
    ; 依赖的"高半区共享直接映射 (pd_high) 天然可达"这一 D5 前提已随 KPTI-08
    ; 失效. 改为切到本 CPU 出口栈 (rdi = gdt.rs 写入的 trampoline_top, 高半区
    ; VA): 此刻仍在内核页表下, 该栈经高半区直接映射可写; 切 CR3 后其**栈顶页**
    ; 由 kpti::map_kernel_pages_in_user_pml4 显式映射进每份用户页表, 出口 stub
    ; 方能 pop/iretq.
    mov rsp, rdi                ; rsp = trampoline_top

    ; Build iretq frame (低→高: RIP, CS, RFLAGS, RSP, SS, CR3).
    ; CR3 **先**压入, 使其落在 SS 之上: 出口 stub 在自身 `push rax` 后经
    ; [rsp+48] 取回该值 (此时 rsp = RIP 槽 - 8). 帧共 6 槽 (48 字节), 全部落在
    ; trampoline 栈顶页内 (页对齐栈顶, 见 gdt.rs AlignedStack).
    push qword [rsi + 80]       ; cr3
    push qword [rsi + 128]      ; ss
    push qword [rsi + 64]       ; rsp
    push qword [rsi + 72]       ; rflags
    push qword [rsi + 88]       ; cs
    push qword [rsi + 56]       ; rip

    ; B05-55 修复: 恢复 caller-saved 寄存器 (rdi/rsi/rdx/rcx/r8-r11).
    ; ProcessContext 布局: fpu_state[64] @ 144 (512B), fpcr @ 656, fpsr @ 664,
    ; extra_regs[8] @ 672 (B05-55 新增, 见 functions/proc/types.rs).
    ; 已运行过的进程返回用户态时, 寄存器由 syscall/中断栈的 InterruptFrame
    ; (schedule 后 iretq) 覆盖恢复, 此处不影响. 首次被调度的进程 (fork 子进程)
    ; 用这些继承的寄存器值 (fork 时父进程的 rdi 等) 进入用户态.
    ; ⚠ 必须放在所有 [rsi] (ProcessContext) 访问之后、切 CR3 之前, 且恢复 rax
    ; 之后 (rax 是 fork 返回值, 不能被覆盖).
    ; 内核线程分支已在 fxrstor 后 jmp 走, 到达本处必为用户态, 无需再判 cs.
    mov rax, rsi                ; rax 暂存 ctx 指针 (rsi 即将被覆盖)
    mov rdi, [rax + 672]        ; rdi (覆盖此前暂存的 trampoline_top, 已用完)
    mov rsi, [rax + 680]        ; rsi (从 [rax+...] 读, rax 保持 ctx)
    mov rdx, [rax + 688]
    mov rcx, [rax + 696]
    mov r8,  [rax + 704]
    mov r9,  [rax + 712]
    mov r10, [rax + 720]
    mov r11, [rax + 728]
    mov rax, [rax + 48]         ; rax = fork 返回值 (最后恢复)

    ; KPTI-08: 帧已在 trampoline 栈上建好, 且此后不再访问 [rsi] (内核堆) 与
    ; prev 的内核栈. 转出口 stub —— 由它在 `.kpti_trampoline` 段 (用户页表恒
    ; 映射) 内切 CR3 并 iretq. 此处尚未切 CR3, 跳入 stub 时取指仍在内核页表内.
    jmp kpti_exit_trampoline

; ── KPTI-08 用户态出口 stub ────────────────────────────────────────────
; 位于 `.kpti_trampoline` 段: 链接脚本 (link/x86_64.ld) 把 `*(.kpti_trampoline)`
; 与 `other/build/isr.o(.text)` 排在 `_kernel_text_start ~ _kpti_trampoline_end`,
; 该区段被 kpti::map_kernel_pages_in_user_pml4 映射进**每份**用户页表, 故切
; CR3 之后本段仍可取指.
;
; 入参: rsp = per-CPU trampoline 栈上的 iretq 帧, 低→高:
;   [rsp+0]=RIP [rsp+8]=CS [rsp+16]=RFLAGS [rsp+24]=RSP [rsp+32]=SS [rsp+40]=CR3
; 此时除 rax (fork 返回值) 外全部通用寄存器已是用户值, 不可再借作中间量,
; 故 CR3 由帧内取回.
section .kpti_trampoline progbits alloc exec nowrite
kpti_exit_trampoline:
    push rax                    ; 暂存入口 rax (rsp -= 8, CR3 落到 [rsp+48])
    mov rax, [rsp + 48]         ; rax = 帧内 CR3 槽 (SS 之上那一格)
    mov cr3, rax                ; 切到 next 用户页表 (本段/栈顶页均已映射)
    pop rax                     ; 恢复入口 rax, rsp 回到 RIP 槽
    iretq                       ; 用户页表下弹出 RIP/CS/RFLAGS/RSP/SS

section .text
user_entry_trampoline:
    ; P2.B + F-13 (DECISION-051 简化方案): CS = 用户代码段 (DPL=3).
    ; 字节长度与原 mov ax, 0x23 一致 (4 字节), 避免 label 偏移重定义.
    ; 单一来源: src/kernel/privileged/link/x86_64.ld SELECTOR_USER_CODE_RPL3 与
    ; gdt.rs pub const SELECTOR_USER_CODE 同步 (host-tests 校验).
    mov ax, 0x23
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    mov rax, [rel user_entry_cr3]
    mov cr3, rax

    jmp [rel user_entry_target]

section .note.GNU-stack noalloc noexec nowrite progbits