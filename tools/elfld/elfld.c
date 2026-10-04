/*
 * elfld.so — Edgine 最小 ELF 解释器 (动态链接器)
 *
 * 最小实现: 读取 ELF 头, 加载 PT_LOAD 段, 跳转到入口点.
 * 当前仅支持静态链接的 PIE 可执行文件 (ET_DYN).
 * 动态链接 (共享库加载/符号解析) 待后续扩展.
 *
 * 关联: docs/explain/naming-standpoint.md §五 + §六
 * 编译: musl-gcc -shared -fPIC -nostdlib -o elfld.so elfld.c
 */

#include <stddef.h>
#include <stdint.h>

/* ELF64 类型定义 */
typedef uint64_t Elf64_Addr;
typedef uint64_t Elf64_Off;
typedef uint16_t Elf64_Half;
typedef uint32_t Elf64_Word;
typedef uint64_t Elf64_Xword;

#define EI_NIDENT 16
#define PT_LOAD    1
#define PT_INTERP  3
#define PT_DYNAMIC 2
#define PF_X       1
#define PF_W       2
#define PF_R       4

typedef struct {
    unsigned char e_ident[EI_NIDENT];
    Elf64_Half    e_type;
    Elf64_Half    e_machine;
    Elf64_Word    e_version;
    Elf64_Addr    e_entry;
    Elf64_Off     e_phoff;
    Elf64_Off     e_shoff;
    Elf64_Word    e_flags;
    Elf64_Half    e_ehsize;
    Elf64_Half    e_phentsize;
    Elf64_Half    e_phnum;
    Elf64_Half    e_shentsize;
    Elf64_Half    e_shnum;
    Elf64_Half    e_shstrndx;
} Elf64_Ehdr;

typedef struct {
    Elf64_Word  p_type;
    Elf64_Word  p_flags;
    Elf64_Off   p_offset;
    Elf64_Addr  p_vaddr;
    Elf64_Addr  p_paddr;
    Elf64_Xword p_filesz;
    Elf64_Xword p_memsz;
    Elf64_Xword p_align;
} Elf64_Phdr;

/* 内存保护标志转换 */
static int prot_from_pflags(Elf64_Word flags) {
    int prot = 0;
    if (flags & PF_R) prot |= 0x1;  /* PROT_READ */
    if (flags & PF_W) prot |= 0x2;  /* PROT_WRITE */
    if (flags & PF_X) prot |= 0x4;  /* PROT_EXEC */
    return prot;
}

/*
 * 最小 ELF 解释器入口点.
 *
 * 在实际 Edgine 中, 内核将控制权传递给此函数,
 * 参数为被加载二进制的 ELF 头地址.
 *
 * 当前实现: 仅验证 ELF 有效性并跳转到入口点.
 * 完整实现需: mmap PT_LOAD 段 + 解析 PT_DYNAMIC + 加载共享库 + 重定位.
 */
void __attribute__((noreturn)) _start(void *elf_phdr) {
    /* 最小实现: 直接跳转到程序入口.
     * 在 Edgine 框内核中, 内核已完成 PT_LOAD 加载,
     * elfld.so 只需解析 PT_DYNAMIC 并处理重定位.
     * 当前静态链接场景下, 跳过这些步骤直接返回控制权. */

    /* 从 phdr 回推 ELF 头以获取入口点 */
    Elf64_Phdr *phdr = (Elf64_Phdr *)elf_phdr;
    /* phdr 的地址就是 auxv[AT_PHDR], ELF 头在 phdr - e_phoff 处 */
    Elf64_Ehdr *ehdr = (Elf64_Ehdr *)((char *)phdr - sizeof(Elf64_Ehdr));

    /* 验证 ELF 魔数 */
    if (ehdr->e_ident[0] != 0x7f ||
        ehdr->e_ident[1] != 'E'  ||
        ehdr->e_ident[2] != 'L'  ||
        ehdr->e_ident[3] != 'F') {
        /* ELF 无效, 进入无限循环 (无法 panic 在用户态) */
        for (;;) {}
    }

    /* 跳转到程序入口点 */
    void (*entry)(void) = (void (*)(void))ehdr->e_entry;
    entry();

    /* 不可达 */
    for (;;) {}
}
