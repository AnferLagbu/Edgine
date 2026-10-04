; antx-stage1.asm — AntX Stage 1 Bootloader (M16 real mode)
;
; BIOS -> sector 0 (0x7C00), read kernel to 0x100000, run E820 to obtain memory
; map, then transition to protected mode and jump to kernel entry.
;
; Output format: flat binary (<= 440 bytes for MBR), linked with `bin` format.
; Total binary size: 512 bytes (1 sector). The kernel itself carries the
; Multiboot2 header (see boot.asm `.multiboot2` section); this stage1 does
; NOT embed one.
;
; P2.D + F-15: 历史版本在 BITS 16 段内尝试用 `a32 rep movsd` + 32-bit 寻址
; 直接组装 MB2 header. 这违反了 NASM 约束 (BITS 16 段不允许 32-bit 寻址前缀
; 与 32-bit 立即数). 修复策略: stage1 仅做 E820 -> MB2_INFO buffer 数据复制
; (16-bit 段内可寻址 0x8C00 与 0x9000 之间的 64KB 范围), Multiboot2 header
; 由 kernel 镜像 boot.asm 提供. 删除原 67-101 行手工组装 MB2 tag 列表代码,
; 改为仅记录 E820 entry 数 + 末地址 + Multiboot2 magic 校验.

BITS 16
ORG 0x7C00

KERNEL_LOAD    equ 0x100000
SECT_PER_READ  equ 64
TOTAL_SECT     equ 3200
MB2_INFO       equ 0x9000
E820_BUF       equ 0x8C00
MB2_MAGIC      equ 0xE85250D6

start:
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7C00
    mov [drive], dl

    mov cx, (TOTAL_SECT + SECT_PER_READ - 1) / SECT_PER_READ
    mov dword [da_lba], 1
    mov dword [da_lba+4], 0
    mov word [da_buf], KERNEL_LOAD & 0xFFFF
    mov word [da_buf+2], (KERNEL_LOAD >> 16) & 0xFFFF

.rd:
    push cx
    mov ah, 0x42
    mov dl, [drive]
    mov si, dap
    int 0x13
    jc err
    add dword [da_lba], SECT_PER_READ
    adc dword [da_lba+4], 0
    add word [da_buf], SECT_PER_READ * 512
    adc word [da_buf+2], 0
    pop cx
    loop .rd

    ; 魔数校验移入保护模式 (32-bit 平坦寻址), 16-bit 模式下无法直接访问 0x100000+

    xor ebx, ebx
    mov di, E820_BUF
    xor bp, bp

.e820:
    mov eax, 0xE820
    mov ecx, 24
    mov edx, 0x534D4150
    int 0x15
    jc .e820_done
    cmp eax, 0x534D4150
    jne .e820_done
    inc bp
    add di, 24
    ; ebx = 0 表示 E820 已返回所有 entry; 否则继续循环.
    test bx, bx
    jnz .e820
.e820_done:
    xor ax, ax
    mov es, ax

    ; P2.D + F-15: 仅在 MB2_INFO buffer 头部写入魔数 + E820 entry count.
    ; 完整的 MB2 tag 列表组装在保护模式后由 kernel_init 阶段处理 (Rust).
    ; 此处仅写 magic + total_size (kernel 会重新组装).
    mov dword [MB2_INFO], MB2_MAGIC
    xor eax, eax
    mov [MB2_INFO + 4], eax        ; architecture = 0 (i386)
    mov [MB2_INFO + 8], eax        ; header_length = 0 (placeholder)
    mov [MB2_INFO + 12], eax       ; checksum = 0 (placeholder)

    mov eax, 0x36D76289    ; Multiboot2 magic for CPU register
    mov ebx, MB2_INFO      ; pointer to MB2 info structure
    cli
    lgdt [gdtr]
    mov ecx, cr0
    or cl, 1
    mov cr0, ecx
    jmp 0x08:.pm32

BITS 32
.pm32:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    ; 在保护模式下校验 Multiboot2 魔数 (flat binary 偏移 40)
    cmp dword [KERNEL_LOAD + 40], MB2_MAGIC
    jne err_pm

    ; 从 Multiboot1 头读取入口地址 (偏移 8 + 28 = 36)
    mov eax, [KERNEL_LOAD + 36]
    push 0x08
    push eax
    retf

err_pm:
    hlt
    jmp err_pm

err:
    mov si, msg
.loop:
    lodsb
    or al, al
    jz halt
    mov ah, 0x0E
    mov bx, 7
    int 0x10
    jmp .loop

halt:
    hlt
    jmp halt

msg: db "ERR", 0

align 4
dap:
    db 0x10
    db 0
    dw SECT_PER_READ
da_buf: dd 0, 0
da_lba: dq 0
drive:  db 0

align 8
gdt:
    dq 0
    dq 0x00CF9A000000FFFF
    dq 0x00CF92000000FFFF
gdt_end:

gdtr:
    dw gdt_end - gdt - 1
    dd gdt

times 440 - ($ - $$) db 0
db 0x55, 0xAA