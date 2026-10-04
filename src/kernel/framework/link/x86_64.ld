OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
    boot PT_LOAD FLAGS(7);
    kernel PT_LOAD FLAGS(7);
}

SECTIONS
{
    . = 0x100000;
    
    _kernel_start = .;
    
    .stage1_entry : {
        *(.stage1_entry)
    } :boot
    
    .multiboot1 : {
        *(.multiboot1)
    } :boot
    
    .multiboot2 : {
        *(.multiboot2)
    } :boot
    
    . = ALIGN(4096);
    .boot.text : {
        other/build/boot.o(.text)
        __boot_text_end = .;
    } :boot
    
    . = ALIGN(4096);
    .bootbss (NOLOAD) : {
        *(.bootbss)
    } :boot
    
    . = ALIGN(4096);
    _boot_end = .;
    
    _kernel_text_lma = .;
    _kernel_text_vma = 0xFFFF800001000000 + .;
    
    .text : AT(_kernel_text_lma) {
        _kernel_text_start = .;
        *(.kpti_trampoline)
        other/build/isr.o(.text .text.*)
        . = ALIGN(16);
        _kpti_trampoline_end = .;
        *(.text)
        *(.text.*)
        *(.trampoline)
        _kernel_text_end = .;
    } :kernel
    
    . = ALIGN(4096);
    .rodata : AT(_kernel_text_lma + (ADDR(.rodata) - _kernel_text_lma)) {
        *(.rodata)
        *(.rodata.*)
    } :kernel
    
    . = ALIGN(4096);
    .data : AT(_kernel_text_lma + (ADDR(.data) - _kernel_text_lma)) {
        *(.data)
        *(.data.*)
        
        . = ALIGN(8);
        _GLOBAL_OFFSET_TABLE_ = .;
        *(.got)
        *(.got.plt)
    } :kernel
    
    . = ALIGN(4096);
    __bss_start = .;
    .bss (NOLOAD) : AT(_kernel_text_lma + (ADDR(.bss) - _kernel_text_lma)) {
        *(.bss)
        *(.bss.*)
        *(COMMON)
    } :kernel
    
    . = ALIGN(4096);
    .lbss (NOLOAD) : AT(_kernel_text_lma + (ADDR(.lbss) - _kernel_text_lma)) {
        *(.lbss)
        *(.lbss.*)
        *(LARGE_COMMON)
    } :kernel
    
    __bss_end = .;
    
    .rela.dyn : {
        *(.rela.init)
        *(.rela.text .rela.text.*)
        *(.rela.rodata .rela.rodata.*)
        *(.rela.data .rela.data.*)
        *(.rela.got)
        *(.rela.bss .rela.bss.*)
        *(.rela.ifunc)
    } :kernel
    
    .dynsym : {
        *(.dynsym)
    } :kernel
    
    .dynstr : {
        *(.dynstr)
    } :kernel
    
    /DISCARD/ : {
        *(.comment)
        *(.eh_frame*)
    }
    
    . = ALIGN(4096);
    _kernel_end = .;
    /* P2.B + F-13 (DECISION-051): 为汇编端 extern 强制定义 GDT 选择子常量符号.
     * 同时定义 DPL=3 编码的 USER 选择子 (RPL=3), 汇编 `push` 直接引用,
     * 避免 `+3` 表达式被 Rust inline asm 解析器误报 (DECISION-051).
     * 单一来源: gdt.rs pub const SELECTOR_USER_DATA/CODE 值必须与此同步;
     * 未来若 GDT 重排, 需同时修改此处 + gdt.rs. */
    SELECTOR_USER_DATA      = ABSOLUTE(0x18);
    SELECTOR_USER_CODE      = ABSOLUTE(0x20);
    SELECTOR_USER_DATA_RPL3 = ABSOLUTE(0x18 | 3);
    SELECTOR_USER_CODE_RPL3 = ABSOLUTE(0x20 | 3);
    SELECTOR_KERNEL_CODE    = ABSOLUTE(0x08);
    SELECTOR_KERNEL_DATA    = ABSOLUTE(0x10);
    SELECTOR_TSS            = ABSOLUTE(0x28);
    /* P1.D + F-03: Distinguish VMA vs LMA semantics.
     * Historically _kernel_size = VMA_end - VMA_text_start (numerically equal to LMA range
     * because VMA-LMA offset is constant 0xFFFF800001000000 on x86_64).
     * Explicit both flavors; _kernel_size aliases _kernel_size_lma so callers using
     * loader semantics get the LMA value (matches what bootloader copies). */
    _kernel_size_vma = _kernel_end - _kernel_text_vma;
    _kernel_size_lma = _kernel_end - _kernel_text_lma;
    _kernel_size = _kernel_size_lma;
    _kernel_end_phys = _kernel_text_lma + _kernel_size_lma;
}
