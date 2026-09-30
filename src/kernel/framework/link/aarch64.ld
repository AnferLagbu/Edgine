OUTPUT_FORMAT("elf64-littleaarch64")
OUTPUT_ARCH(aarch64)
ENTRY(_start)

PHDRS
{
    boot PT_LOAD FLAGS(7);
    kernel PT_LOAD FLAGS(7);
}

SECTIONS
{
    /* 高半区内核区与低半区引导区的固定偏移 (内核区 VMA = LMA + 本值). */
    _kernel_base = 0xFFFF000000000000;

    /* ── 低半区引导区 (VMA == LMA == PA) ────────────────────────────────
     * 本区在 MMU 启用前由 start.S 以 PC 相对寻址 (adrp/add) 使用, 故三者必须
     * 相等. 镜像文件偏移 0 处为 arm64 Linux Image 头 (`.image_header`, 64 字节),
     * 其中 code0 = `b _start`, 故真实入口 `_start` 位于 0x40080040.
     */
    . = 0x40080000;

    _kernel_start = .;

    /* arm64 Linux Image 头 (64 字节, 内容见 boot/aarch64/start.S):
     * QEMU -kernel 与 U-Boot booti 均以文件偏移 56 处的 "ARM\x64" 幻数识别本
     * 格式, 并据 text_offset 定位装载地址. 本段须位于文件偏移 0 (即 LMA ==
     * _kernel_start), 由 objcopy -O binary 平铺输出保证. */
    .image_header : {
        KEEP(*(.image_header))
    } :boot

    /* 真实入口: Image 头之后 64 字节处. 显式回写使 .text.boot 起始地址确定,
     * 并使 `.image_header` 长度偏离 64 字节时立即以 "cannot move location
     * counter backwards" 报错 (fail-closed). */
    . = 0x40080040;

    .text.boot : {
        *(.text.boot)
    } :boot

    . = ALIGN(4096);
    .bootbss (NOLOAD) : {
        *(.bootbss)
    } :boot

    . = ALIGN(4096);
    _boot_end = .;

    /* Rust 侧 boot 栈 canary 符号 (`proc/process.rs`).
     * 引导栈本体在 `.bootbss` (低半区, VMA == PA), 其符号距高半区内核代码超出
     * `adrp` 的 ±4GB 可达范围, 故此处直接暴露其**高半区别名**
     * (VA = _kernel_base + PA, 运行时经 TTBR1 可达) 供 Rust 侧取址.
     * 与 x86_64 的差异: x86_64 内核低 VMA 链接, 其 `stack_bottom` 是低 VMA 符号,
     * 由 Rust 侧自行加 `KERNEL_BASE` 别名化. */
    stack_bottom = _kernel_base + _boot_stack_bottom;

    /* 设备树 (DTB) 物理地址指针符号 (`boot/aarch64/start.S` 将 x0 存入 `_fdt_addr`).
     * 与 `stack_bottom` 同理: `_fdt_addr` 位于低半区 `.bootbss` (VMA == PA),
     * 其符号距高半区内核代码超出 `adrp` 的 ±4GB 可达范围, 故暴露其**高半区别名**
     * (VA = _kernel_base + PA) 供 Rust 侧经 TTBR1 读取. */
    fdt_addr_ptr = _kernel_base + _fdt_addr;

    /* ── 高半区内核区 (VMA = 高半区, LMA = 物理地址) ────────────────────
     * 内核代码/数据/向量表链接于高半区, 运行时经 TTBR1 取指取数; LMA 决定物理
     * 装载位置, 二者差值恒为 `_kernel_base`.
     */
    _kernel_text_lma = .;
    _kernel_text_vma = _kernel_base + .;
    . = _kernel_text_vma;

    .text : AT(_kernel_text_lma) {
        _kernel_text_start = .;
        *(.text)
        *(.text.*)
        _kernel_text_end = .;
    } :kernel

    . = ALIGN(2048);
    .vectors : AT(ADDR(.vectors) - _kernel_base) {
        _vectors_start = .;
        *(.vectors)
        /* KPTI 以页级粒度映射向量表段, 故段长对齐到 4KB (末页完整计入) */
        . = ALIGN(4096);
        _vectors_end = .;
    } :kernel

    . = ALIGN(4096);
    .rodata : AT(ADDR(.rodata) - _kernel_base) {
        *(.rodata)
        *(.rodata.*)
    } :kernel

    . = ALIGN(4096);
    .data : AT(ADDR(.data) - _kernel_base) {
        *(.data)
        *(.data.*)
    } :kernel

    . = ALIGN(4096);
    __bss_start = .;
    .bss (NOLOAD) : AT(ADDR(.bss) - _kernel_base) {
        *(.bss)
        *(.bss.*)
        *(COMMON)
    } :kernel

    __bss_end = .;

    . = ALIGN(4096);
    _kernel_end = .;
    /* P1.D + F-03: 区分 VMA/LMA 语义 —— 高半区 VMA 与物理 LMA 相差 `_kernel_base`. */
    _kernel_size_vma = _kernel_end - _kernel_text_vma;
    _kernel_size_lma = (_kernel_end - _kernel_base) - _kernel_text_lma;
    _kernel_size = _kernel_size_lma;

    /* arm64 Image 头的 image_size 字段: 自镜像起始 (_kernel_start) 至内核末尾
     * (_kernel_end 对应物理地址) 的有效字节数. 须填真实尺寸 (含 .bss 上界) ——
     * U-Boot booti 对 image_size == 0 的镜像按 16 MiB 假设执行搬移, 有越界覆盖
     * 风险. 由 boot/aarch64/start.S 的 `.image_header` 段引用. */
    _image_size = (_kernel_end - _kernel_base) - _kernel_start;
}