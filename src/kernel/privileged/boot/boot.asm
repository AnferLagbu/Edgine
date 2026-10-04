BITS 32

; === Stage1 入口跳板 ===
; Stage1 裸加载器跳转到 0x100000，此处的 jmp 重定向到真正的 _start
; Multiboot 头位于偏移 8 处，GRUB 在首 8KB 内搜索 4 字节对齐的魔数，不受影响
section .stage1_entry
    jmp near _start          ; 5 bytes (E9 xx xx xx xx)
    times 3 db 0x90           ; NOP 填充至 8 字节对齐

; === Multiboot1 头 (偏移 8, 4 字节对齐) ===
section .multiboot1
align 4

MULTIBOOT1_MAGIC    equ 0x1BADB002
MULTIBOOT1_FLAGS    equ 0x00010003
MULTIBOOT1_CHECKSUM equ -(MULTIBOOT1_MAGIC + MULTIBOOT1_FLAGS)

multiboot1_header:
    dd MULTIBOOT1_MAGIC
    dd MULTIBOOT1_FLAGS
    dd MULTIBOOT1_CHECKSUM
    dd multiboot1_header
    dd 0x100000
    dd 0
    dd 0
    dd _start

section .multiboot2
align 8

MULTIBOOT2_MAGIC     equ 0xE85250D6
MULTIBOOT2_ARCH_I386 equ 0
HEADER_LENGTH        equ multiboot2_header_end - multiboot2_header_start

multiboot2_header_start:
    dd MULTIBOOT2_MAGIC
    dd MULTIBOOT2_ARCH_I386
    dd HEADER_LENGTH
    dd -(MULTIBOOT2_MAGIC + MULTIBOOT2_ARCH_I386 + HEADER_LENGTH)

    dw 0
    dw 0
    dd 8
multiboot2_header_end:

KERNEL_PHYS_BASE     equ 0x100000
KERNEL_VIRT_BASE     equ 0xFFFF800001000000

section .bootbss
align 4096
pml4:
    resb 4096
pdpt_low:
    resb 4096
pdpt_high:
    resb 4096
pd_low:
    resb 4096
pd_low2:
    resb 4096
pd_low3:
    resb 4096
pd_low4:
    resb 4096
pd_high:
    resb 4096
gdt64:
    resb 30
align 16
global stack_bottom
stack_bottom:
    resb 131072
global stack_top
stack_top:

kernel_info:
    resq 3

saved_multiboot_info:
    resd 1
saved_multiboot_magic:
    resd 1

align 16
trampoline_idt:
    resb 48 * 16
trampoline_idt_end:

trampoline_idtr_limit:
    resw 1
trampoline_idtr_base:
    resq 1

section .text
global _start
extern kernel_init
extern boot_set_multiboot_info
extern stack_top
extern __bss_start
extern _kernel_end

_start:
    cli

    mov al, 0xFF
    out 0x21, al
    out 0xA1, al

    mov esi, ebx
    mov edi, eax

    call .get_eip
.get_eip:
    pop ebx
    sub ebx, .get_eip - _start

    mov [ebx + (saved_multiboot_info - _start)], esi
    mov [ebx + (saved_multiboot_magic - _start)], edi

    lea edi, [ebx + (pml4 - _start)]
    xor eax, eax
    mov ecx, 8192
    rep stosd

    lea edi, [ebx + (pml4 - _start)]
    lea eax, [ebx + (pdpt_low - _start)]
    or eax, 3
    xor edx, edx
    mov [edi], eax
    mov [edi + 4], edx

    lea edi, [ebx + (pdpt_low - _start)]
    lea eax, [ebx + (pd_low - _start)]
    or eax, 3
    mov [edi], eax
    mov [edi + 4], edx

    lea eax, [ebx + (pd_low2 - _start)]
    or eax, 3
    mov [edi + 8], eax
    mov [edi + 12], edx

    lea eax, [ebx + (pd_low3 - _start)]
    or eax, 3
    mov [edi + 16], eax
    mov [edi + 20], edx

    lea eax, [ebx + (pd_low4 - _start)]
    or eax, 3
    mov [edi + 24], eax
    mov [edi + 28], edx

    lea edi, [ebx + (pml4 - _start) + 256 * 8]
    lea eax, [ebx + (pdpt_high - _start)]
    or eax, 3
    mov [edi], eax
    mov [edi + 4], edx

    lea edi, [ebx + (pdpt_high - _start)]
    lea eax, [ebx + (pd_high - _start)]
    or eax, 3
    mov [edi], eax
    mov [edi + 4], edx

    lea edi, [ebx + (pd_low - _start)]
    mov eax, 0x83
    xor edx, edx
    mov ecx, 512
.map_low:
    mov [edi], eax
    mov [edi + 4], edx
    add eax, 0x200000
    adc edx, 0
    add edi, 8
    dec ecx
    jnz .map_low

    lea edi, [ebx + (pd_high - _start)]
    mov eax, 0x83
    xor edx, edx
    mov ecx, 512
.map_high:
    mov [edi], eax
    mov [edi + 4], edx
    add eax, 0x200000
    adc edx, 0
    add edi, 8
    dec ecx
    jnz .map_high

    lea edi, [ebx + (pd_low2 - _start)]
    mov eax, 0x40000083
    xor edx, edx
    mov ecx, 512
.map_low2:
    mov [edi], eax
    mov [edi + 4], edx
    add eax, 0x200000
    adc edx, 0
    add edi, 8
    dec ecx
    jnz .map_low2

    lea edi, [ebx + (pd_low3 - _start)]
    mov eax, 0x80000083
    xor edx, edx
    mov ecx, 512
.map_low3:
    mov [edi], eax
    mov [edi + 4], edx
    add eax, 0x200000
    adc edx, 0
    add edi, 8
    dec ecx
    jnz .map_low3

    lea edi, [ebx + (pd_low4 - _start)]
    mov eax, 0xC0000083
    xor edx, edx
    mov ecx, 512
.map_low4:
    mov [edi], eax
    mov [edi + 4], edx
    add eax, 0x200000
    adc edx, 0
    add edi, 8
    dec ecx
    jnz .map_low4

    lea edi, [ebx + (__bss_start - _start)]
    mov ecx, _kernel_end
    sub ecx, edi
    shr ecx, 2
    xor eax, eax
    rep stosd

    lea edi, [ebx + (gdt64 - _start)]
    mov word [edi], 23
    lea eax, [ebx + (gdt64 - _start + 6)]
    mov dword [edi + 2], eax

    mov dword [edi + 6], 0
    mov dword [edi + 10], 0
    mov dword [edi + 14], 0x0000FFFF
    mov dword [edi + 18], 0x00AF9A00
    mov dword [edi + 22], 0x0000FFFF
    mov dword [edi + 26], 0x00CF9200

    lgdt [edi]

    lea eax, [ebx + (pml4 - _start)]
    mov cr3, eax

    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax

    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8
    or eax, 1 << 11
    wrmsr

    mov eax, cr0
    or eax, 1 << 31
    mov cr0, eax

    lea eax, [ebx + (trampoline64 - _start)]
    push dword 0x08
    push eax
    retf

BITS 64
trampoline64:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    lea rax, [rbx + (trampoline_idt - _start)]
    mov [rbx + (trampoline_idtr_base - _start)], rax
    mov word [rbx + (trampoline_idtr_limit - _start)], 48 * 16 - 1

    mov rdi, rax

    lea rsi, [rbx + (trampoline_exc_stub - _start)]
    mov rdx, rsi
    shr rdx, 32
    mov ecx, 32
.fill_exc:
    mov word [rdi], si
    mov word [rdi + 2], 0x08
    mov byte [rdi + 4], 0
    mov byte [rdi + 5], 0x8E
    mov r8d, esi
    shr r8d, 16
    mov word [rdi + 6], r8w
    mov dword [rdi + 8], edx
    mov dword [rdi + 12], 0
    add rdi, 16
    dec ecx
    jnz .fill_exc

    lea rsi, [rbx + (trampoline_int_stub - _start)]
    mov rdx, rsi
    shr rdx, 32
    mov ecx, 16
.fill_int:
    mov word [rdi], si
    mov word [rdi + 2], 0x08
    mov byte [rdi + 4], 0
    mov byte [rdi + 5], 0x8E
    mov r8d, esi
    shr r8d, 16
    mov word [rdi + 6], r8w
    mov dword [rdi + 8], edx
    mov dword [rdi + 12], 0
    add rdi, 16
    dec ecx
    jnz .fill_int

    lidt [rbx + (trampoline_idtr_limit - _start)]

    jmp trampoline64_high

BITS 64
trampoline64_high:
    lea rsp, [rbx + (stack_top - _start)]

    ; 写入 boot 栈 canary 到 stack_bottom (栈溢出检测)
    ; 若栈溢出至 stack_bottom, canary 被覆盖, Rust 侧 check_boot_stack_canary() 可检测.
    mov rax, 0xDEADBEEFCAFEBABE
    mov qword [rbx + (stack_bottom - _start)], rax

    mov edi, dword [rbx + (saved_multiboot_magic - _start)]
    mov esi, dword [rbx + (saved_multiboot_info - _start)]
    call boot_set_multiboot_info
    call kernel_init

    cli
.halt:
    hlt
    jmp .halt

trampoline_exc_stub:
    push rax
    push rdx
    mov dx, 0x3F8
    mov al, 'X'
    out dx, al
    mov al, '!'
    out dx, al
.halt_exc:
    hlt
    jmp .halt_exc

trampoline_int_stub:
    push rax
    push rdx
    mov al, 0x20
    out 0x20, al
    out 0xA0, al
    pop rdx
    pop rax
    iretq

section .note.GNU-stack noalloc noexec nowrite progbits
