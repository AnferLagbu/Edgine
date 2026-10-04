BITS 64

section .text
global _kernel_entry
extern kernel_init

_kernel_entry:
    mov rax, kernel_init
    call rax

.halt:
    sti
    hlt
    jmp .halt