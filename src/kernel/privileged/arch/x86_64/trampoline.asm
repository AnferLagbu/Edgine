; ┌──────────────────────────────────────────────────────────────────────┐
; │  AP Trampoline — 从实模式引导到 64-bit 长模式                          │
; │                                                                      │
; │  流程: 实模式(16-bit) → 保护模式(32-bit) → 长模式(64-bit)              │
; │                                                                      │
; │  SIPI vector = 0x08 → AP 从物理地址 0x8000 开始执行                    │
; │                                                                      │
; │  内存布局 (0x8000):                                                   │
; │    0x8000:        jmp trampoline_entry                                │
; │    0x8002-0x8007: padding (6 bytes)                                   │
; │    0x8008-0x8039: ApStartupInfo (BSP 写入，AP 读取，50 bytes packed)  │
; │    0x803A+:       Trampoline 代码                                    │
; │                                                                      │
; │  ApStartupInfo 与 Rust struct 保证字节级兼容 (repr(C, packed)):        │
; │    +0x00: cr3       (u64, 8B)                                        │
; │    +0x08: entry     (u64, 8B)                                        │
; │    +0x10: gdt_limit (u16, 2B)                                        │
; │    +0x12: gdt_base  (u64, 8B)                                        │
; │    +0x1A: stack     (u64, 8B)                                        │
; │    +0x22: lapic_id  (u32, 4B)                                        │
; │    +0x26: ready     (u32, 4B)                                        │
; │    +0x2A: cpu_index (u32, 4B)                                        │
; │    +0x2E: _pad      (u32, 4B)                                        │
; └──────────────────────────────────────────────────────────────────────┘

section .trampoline progbits alloc exec

BITS 16

global ap_trampoline_start
ap_trampoline_start:

global ap_trampoline_16
ap_trampoline_16:
    jmp short trampoline_entry

; ── 对齐填充 ───────────────────────────────────────────────────────────
times 6 db 0

; ── ApStartupInfo (repr(C, packed), 54 bytes) ──────────────────────────
; 对应 sptr (startup info pointer) = 0x8008
; CR3 (offset +0):
times 8 db 0
; entry (offset +8):
times 8 db 0
; gdt_limit (offset +16, 2 bytes):
dw 0
; gdt_base (offset +18, 8 bytes):
times 8 db 0
; stack (offset +26, 8 bytes):
times 8 db 0
; lapic_id (offset +34, 4 bytes):
dd 0
; ready (offset +38, 4 bytes):
dd 0
; cpu_idx (offset +42, 4 bytes):
dd 0
; done (offset +46, 4 bytes): AP 初始化完成后置 1
dd 0
; _pad (offset +50, 4 bytes):
dd 0

; ── ApStartupInfo 基址 (BSP 写入 / AP 读取) ────────────────────────────
SINFO_BASE equ 0x8008

; 结构体成员偏移 (以 SINFO_BASE 为基)
SINFO_CR3       equ SINFO_BASE + 0
SINFO_ENTRY     equ SINFO_BASE + 8
SINFO_GDT_LIMIT equ SINFO_BASE + 16
SINFO_GDT_BASE  equ SINFO_BASE + 18
SINFO_STACK     equ SINFO_BASE + 26
SINFO_LAPIC_ID  equ SINFO_BASE + 34
SINFO_READY     equ SINFO_BASE + 38

; ── 实际入口 (16-bit Real Mode) ────────────────────────────────────────
trampoline_entry:
    cli
    cld

    xor ax, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    mov sp, 0x7000

    lgdt [cs:trampoline_gdt32_ptr - ap_trampoline_16]

    mov eax, cr0
    or al, 1
    mov cr0, eax

    jmp dword 0x08:(trampoline32 - ap_trampoline_16 + 0x8000)

; ── 32-bit GDT + 64-bit 描述符 ──────────────────────────────────────────
; 注意: 32-bit lgdt 只能读取 4 字节基址，不能加载位于 >4GB 的内核 GDT。
; 因此 trampoline 自带 64-bit 描述符用于 32→64 模式跳转，
; 进入长模式后再用 64-bit lgdt 正确加载内核的完整 64-bit GDT。
align 8
trampoline_gdt32:
    dq 0                                   ; 0x00: null
    dq 0x00CF9A000000FFFF                  ; 0x08: 32-bit 代码段 (D=1, L=0)
    dq 0x00CF92000000FFFF                  ; 0x10: 32-bit 数据段
    dq 0x00209A0000000000                  ; 0x18: 64-bit 代码段 (D=0, L=1)
    dq 0x0000920000000000                  ; 0x20: 64-bit 数据段
trampoline_gdt32_end:

trampoline_gdt32_ptr:
    dw trampoline_gdt32_end - trampoline_gdt32 - 1
    dd trampoline_gdt32 - ap_trampoline_16 + 0x8000

; ── 32-bit Protected Mode ──────────────────────────────────────────────
BITS 32
trampoline32:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax

    mov esi, SINFO_CR3
    mov eax, [esi]
    mov cr3, eax

    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8
    or eax, 1 << 11
    wrmsr

    mov eax, cr0
    or eax, 1 << 31
    mov cr0, eax

    jmp dword 0x18:(trampoline64 - ap_trampoline_16 + 0x8000)

; ── 64-bit Long Mode ──────────────────────────────────────────────────
BITS 64
trampoline64:
    lgdt [SINFO_GDT_LIMIT]

    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov rsp, [SINFO_STACK]

    mov edi, [SINFO_LAPIC_ID]

    mov dword [SINFO_READY], 1

    mov rax, [SINFO_ENTRY]
    call rax

.halt:
    hlt
    jmp .halt

global ap_trampoline_end
ap_trampoline_end:

section .note.GNU-stack noalloc noexec nowrite progbits