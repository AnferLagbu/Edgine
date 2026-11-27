# ELF 装载读入边界对标：Linux / FreeBSD / xv6 / Redox

> 结论：这四套实现都不把整个 ELF 文件读进内核常驻缓冲。Linux / FreeBSD 只读头部和
> program header table，`PT_LOAD` 直接映射文件页；xv6 按「ELF 头 → 逐个 phdr → 按段按页」
> 顺序读；Redox 更进一步，`execve` 完全放在用户态，内核只提供地址空间和映射原语。
>
> Edgine 的「头部探测 + 可加载前缀」不是异类，而是 xv6 / Redox 的有界读路线在
> 「装载器只接受连续 `&[u8]`」这一约束下的折中。对标同时暴露两个应在 E-2 收紧的边界：
> `p_filesz > p_memsz` 应直接 `ENOEXEC`（当前装载器是 `continue`），以及第二段读回长度
> 必须严格等于计算出的前缀长度（当前装载器对越界段静默 `Ok(0)` 跳过）。

## 1. 范围与证据口径

本轮只取有直接源码证据的四套实现，按源码阅读，不跑运行时实验：

| 对象 | 取用版本 | 主要证据 |
|---|---|---|
| Linux | codebrowser 快照 `v6.19-rc8-185-g2687c848e578`（2026-02-08 生成） | `fs/exec.c`、`fs/binfmt_elf.c`、`include/uapi/linux/binfmts.h` |
| FreeBSD | `main` 分支 2026-11-24 快照（`imgact_elf.c` 3033 行） | `sys/kern/imgact_elf.c`、`sys/kern/kern_exec.c` |
| xv6 | xv6-riscv 教材与源码 | `kernel/exec.c` 的 `exec` / `loadseg`，教材 §3.8 |
| Redox | `redox-os/relibc` master（2026-11-27） | `src/platform/redox/exec.rs`、`redox-rt/src/proc.rs`；`redox-os/kernel` tree 对照 |

行号只用于定位，不承诺跨版本稳定；引用处同时给出函数名，便于按名复核。

## 2. Linux：头部 256 B + phdr 表按需读 + `PT_LOAD` 直接映射

### 2.1 格式嗅探只读前缀

`fs/exec.c` 的 `prepare_binprm()`（L1603–1609）只做一件事：

```c
memset(bprm->buf, 0, BINPRM_BUF_SIZE);
return kernel_read(bprm->file, bprm->buf, BINPRM_BUF_SIZE, &pos);
```

`BINPRM_BUF_SIZE` 在 `include/uapi/linux/binfmts.h` L19 定义为 **256**。也就是说，内核在
识别 ELF / shebang / 其它 binfmt 之前，最多只读文件前 256 字节；这与 Edgine E-3 用
4 字节判 wasm 魔数是同一类做法——**格式嗅探只需要前缀，不需要文件本体**。

### 2.2 program header table 只读表本身

`fs/binfmt_elf.c` 的 `load_elf_phdrs()`（L520–553）先校验
`e_phentsize == sizeof(struct elf_phdr)`，再计算：

```c
size = sizeof(struct elf_phdr) * elf_ex->e_phnum;
if (size == 0 || size > 65536)
    goto out;
elf_phdata = kmalloc(size, GFP_KERNEL);
retval = elf_read(file, elf_phdata, size, elf_ex->e_phoff);
```

它有两条硬边界：phdr 表总大小 **不超过 64 KiB**；`elf_read()` 要求 `kernel_read()` 的
返回值严格等于请求长度，否则返回 `-EIO`（L480–489）。这两点值得 Edgine 直接借用：
**phdr 表是一个有独立上限的小对象，短读必须失败，不能当成空表继续。**

### 2.3 `PT_LOAD` 走文件映射，不拷贝进内核缓冲

主映像的 `PT_LOAD` 循环在 `load_elf_binary()` L1040–1190。每段由 `elf_map()`（L370 起）
/ `elf_load()`（L414 起）按 `p_offset` 和 `p_filesz` 调 `vm_mmap()` 映射文件页；
`p_memsz > p_filesz` 的尾部由 `vm_brk_flags()` / 清零处理，不依赖文件尾。
`elf_load()` 同时检查 `p_filesz > p_memsz`、`p_memsz > TASK_SIZE` 等溢出条件
（L709 附近、L1226 附近），畸形段直接失败。

因此 Linux 的“读入量”实际是：**256 B 头部 + 至多 64 KiB phdr 表 + 解释器路径字符串**；
代码段和数据段由页缓存按需缺页，不进入 `bprm` 的内核缓冲。

### 2.4 `PT_INTERP` 只读路径串，解释器再走一遍同一套

`PT_INTERP` 分支（L885–900）先限制 `p_filesz <= PATH_MAX` 且 `>= 2`，然后：

```c
elf_interpreter = kmalloc(elf_ppnt->p_filesz, GFP_KERNEL);
retval = elf_read(bprm->file, elf_interpreter, elf_ppnt->p_filesz,
                  elf_ppnt->p_offset);
```

只读 `PT_INTERP` 覆盖的那段路径字符串。打开解释器后，`load_elf_interp()`（L645 起）再读
解释器自己的 ELF 头（L926–927）和 phdr 表，并对解释器的 `PT_LOAD` 逐段 `elf_load()`。
**主映像和解释器各自有自己的头部 / phdr / 段读入，不存在“把解释器整文件读进来”这一步。**

## 3. FreeBSD：首部一页 + phdr 按需读 + `vm_object` 映射

### 3.1 只映射第一页作为 `image_header`

`kern_exec.c` 的 `exec_map_first_page()`（L1179 起）只抓 vnode 的第 0 页：

```c
error = vm_page_grab_valid_unlocked(&m, object, 0,
    VM_ALLOC_COUNT(VM_INITIAL_PAGEIN) | VM_ALLOC_NORMAL |
    VM_ALLOC_NOBUSY | VM_ALLOC_WIRED);
imgp->image_header = sf_buf_kva(imgp->firstpage);
```

`imgp->image_header` 因此只覆盖 **一页**，不是整个文件。

### 3.2 phdr 表：能放进首部页就直接指，否则只读表区间

`imgact_elf.c` 的 `exec_elf64_imgact()`（宏展开，L1108 起）先 `check_header()`，再判断：

```c
if (phdr_in_zero_page(hdr) && aligned(imgp->image_header + hdr->e_phoff, Elf_Addr)) {
    phdr = (const Elf_Phdr *)(imgp->image_header + hdr->e_phoff);
} else {
    phdr = m_phdrs = malloc(hdr->e_phnum * sizeof(Elf_Phdr), ...);
    error = vn_rdwr(UIO_READ, imgp->vp, m_phdrs,
        hdr->e_phnum * sizeof(Elf_Phdr), hdr->e_phoff, ...);
}
```

`phdr_in_zero_page()`（L491–494）的判据是
`e_phoff <= PAGE_SIZE && e_phentsize * e_phnum <= PAGE_SIZE - e_phoff`。
放得进首部页就零拷贝；放不进就 `malloc` 一个**刚好等于 phdr 表大小**的缓冲区，
用 `vn_rdwr` 从 `e_phoff` 读这一段。仍然没有整文件读。

### 3.3 `PT_LOAD` 由 `vm_object` 映射，且显式对账真实文件大小

`load_sections()`（L735 起）只遍历 `PT_LOAD`，每个段交给 `load_section()`（L631 起）。
后者先把 `p_offset` / `p_filesz` 映射为 `vm_object` 的文件区间，并在映射前检查：

```c
if ((filsz != 0 && (off_t)filsz + offset > imgp->attr->va_size) ||
    filsz > memsz) {
    uprintf("elf_load_section: truncated ELF file\n");
    return (ENOEXEC);
}
```

这正是 Edgine 当前缺的那条对账：**段的文件区间一旦超过实际文件大小，或
`p_filesz > p_memsz`，FreeBSD 直接 `ENOEXEC`，不靠缺页或“空段”兜底。**
`load_section()` 之后才用 `vm_map_find()` / `vm_imgact_map_page()` 建立文件页映射，
BSS 尾部单独建匿名页并清零。

### 3.4 `PT_INTERP` 同样只读路径，解释器单独装载

`get_interp()`（L1016 起）限制 `p_filesz` 在 `[2, MAXPATHLEN]`；路径在首部页内就直接取指针，
否则 `vn_rdwr` 只读 `p_filesz` 字节。打开解释器后，`load_interp_file()`（L788 起）重新
`exec_map_first_page()`、读解释器自己的 ELF 头和 phdr 表，再 `load_sections()`。
与 Linux 同构：**解释器不是主映像前缀的一部分，而是第二个独立的 ELF 装载对象。**

## 4. xv6：头部 → 逐个 phdr → 按段按页读

xv6 的 `exec()` 是最小实现，没有 `PT_INTERP`，也没有文件映射，但读入边界非常干净：

1. 只读 ELF 头：
   ```c
   readi(ip, (char*)&elf, 0, sizeof(elf))
   ```
2. 以 `elf.phoff` 为起点，每次只读一个 `struct proghdr`：
   ```c
   for(i=0, off=elf.phoff; i<elf.phnum; i++, off+=sizeof(ph)) {
       readi(ip, (char*)&ph, off, sizeof(ph));
       if(ph.type != ELF_PROG_LOAD) continue;
       if(ph.memsz < ph.filesz) goto bad;
       ...
       loaduvm(pgdir, (char*)ph.vaddr, ip, ph.off, ph.filesz);
   }
   ```
3. `loaduvm()` / `loadseg()` 按页把 `ph.filesz` 字节从 `ph.off` 读进新地址空间；
   `ph.memsz` 大于 `ph.filesz` 的剩余部分由 `uvmalloc` 清零。

教材 §3.8 明确说 `loadseg` 用 `walkaddr` 找页、用 `readi` 逐页读；并且专门强调
`p_filesz <= p_memsz` 的检查和地址溢出检查。xv6 没有“整文件读”的概念，甚至没有
“前缀”概念——它把有界读做到了逐 phdr、逐段、逐页。

## 5. Redox：`execve` 在用户态，内核不解析 ELF

Redox 是微内核路线，和上面三套“内核内装载器”不同。`redox-os/kernel` 的源码树里
没有 ELF / exec 装载器；`relibc` 的 `src/platform/redox/exec.rs` L103 直接写明：

> With execve now being implemented in userspace, we need to check ourselves that this
> file is actually executable.

真正的读入在 `redox-rt/src/proc.rs` 的 `fexec_impl()`（L73 起）：

- 只读 ELF 头 `size_of::<Header>()`（L87–89）；
- 限制 **header + phdr 表总大小 <= 1 MiB**（`MAX_PH_SIZE`，L104–114）；
- `pread_all(image_file, e_phoff, phs)` 只读 phdr 表（L132）；
- 遍历 `PT_LOAD`，先 `mmap_anon_remote()` 建匿名目标区间，再对每个段
  `pread_all(image_file, p_offset, &mut dst_memory[voff..voff+filesz])`，
  只读 `p_filesz` 字节（L206–254）；
- `PT_INTERP` 只读 `p_filesz` 字节路径串（L196–204）；
- `pread_all()`（L660–672）在读到 0 字节时返回 `ENOEXEC`，要求读满请求长度。

所以 Redox 的“读入边界”是：**用户态按头部 → phdr 表 → 每个段分别 `pread`**。
内核不持有 ELF 内容，只提供远程 `mmap` 和地址空间切换。它证明“分段有界读”不只适用于
内核内装载器；即使装载器在用户态，也不需要把整个文件读进一个缓冲。

## 6. 对照表

| 实现 | 头部读入 | phdr 读入 | `PT_LOAD` 读入 | `PT_INTERP` | 整文件读？ | 关键失败边界 |
|---|---|---|---|---|---|---|
| Linux | 前 256 B（`BINPRM_BUF_SIZE`） | 只读表，大小 ≤ 64 KiB，`e_phoff` 定位 | `vm_mmap` 映射 `p_offset..p_offset+p_filesz`，不拷贝 | 只读 `p_filesz` 路径；解释器再独立读头/phdr/段 | 否 | phdr 表大小上限；`elf_read` 短读 `-EIO`；`p_filesz > p_memsz` / 超 `TASK_SIZE` 失败 |
| FreeBSD | 只映射第 0 页 | 首部页内直接指，否则 `vn_rdwr` 只读表 | `vm_object` 映射文件区间，BSS 另建匿名页 | 只读 `p_filesz` 路径；解释器独立装载 | 否 | phdr 表能放进页则零拷贝，否则只读表；`filsz+offset > va_size` 或 `filsz > memsz` ⇒ `ENOEXEC` |
| xv6 | 只读 `sizeof(elfhdr)` | 每轮只读一个 `proghdr` | `loaduvm` 按页读 `p_filesz` | 无（静态 ELF） | 否 | `memsz < filesz` ⇒ 失败；每次 `readi` 必须读满 |
| Redox | 只读 `sizeof(Header)` | 表大小含头 ≤ 1 MiB，`pread_all` 精确读 | 每段 `pread_all` 只读 `p_filesz` 到新映射 | 只读 `p_filesz` 路径 | 否（内核不读；用户态分段读） | `MAX_PH_SIZE`；`p_filesz > p_memsz` ⇒ `ENOEXEC`；`pread_all` 读到 0 ⇒ `ENOEXEC` |

共同点很清楚：**没有一家把“文件大小”当成“装载读入量”**；phdr 表、路径串、段区间
各自有独立上限，且短读一律失败。

## 7. 映射到 Edgine

### 7.1 方向成立，但“最小”要加一个限定

Edgine 的方案是“头部探测窗口 → 计算可加载前缀 → 只读前缀”。这在连续缓冲装载器的约束下
是正确的：`load_elf_from_memory_with_interp()` 吃的是 `&[u8]`，不能像 Linux / FreeBSD 那样
把 `PT_LOAD` 直接映射成文件页。

但“可加载前缀”是**连续缓冲前提下的最小前缀**，不是各段字节数之和：如果两个 `PT_LOAD`
之间夹着非装载 section，前缀会把这些字节一起带进来。这个语义应在 E-2 的注释和测试里写明，
避免以后把“前缀”误读成“逐段最小”。

### 7.2 两个应立即收紧的边界

对标暴露出 Edgine 当前装载器的两处“跳过”语义，建议在 E-2 的 `loadable_prefix_len()` 里
在进入装载器之前就拒绝，不修改 `elf_load_segment()` 的签名：

1. **`p_filesz > p_memsz` ⇒ `ENOEXEC`**
   当前 `mod.rs:221` 是 `continue`；Linux（`elf_load` 边界检查）、FreeBSD（`load_section`）、
   Redox（`fexec_impl`）都直接失败。继续装载会把畸形段当空段，和“段为空”无法区分。

2. **第二段读回长度必须严格等于 `loadable_prefix_len()`**
   当前 `elf_load_segment()` 在 `p_offset + p_filesz > elf_size` 时返回 `Ok(0)`，调用方
   `continue`（`mod.rs:284–286`）。如果第二段短读，越界段会被静默跳过，表现成“段为空”。
   E-2 已写“短读即 ENOEXEC”，本轮对标进一步确认这是必须项：Linux `elf_read()` 短读
   `-EIO`、Redox `pread_all()` 读到 0 `ENOEXEC`、xv6 每次 `readi` 必须读满，三家都不允许
   “短读当空段”。

3. **溢出仍按现有 `checked_add` 处理**
   `p_offset + p_filesz`、`p_vaddr + p_memsz` 任一溢出 ⇒ `ENOEXEC`；这条现有设计已覆盖，
   只需在 E-2 的负控制里显式列一条。

### 7.3 `ELF_HEAD_PROBE_MAX = 128 KiB` 是保守取舍，不是唯一解

Linux 只读 256 B 头部，FreeBSD 只映射一页，xv6 只读一个 `elfhdr`；Edgine 的 128 KiB
探测窗口比它们都宽。它足够覆盖 `e_phoff + 128 * sizeof(Elf64Phdr)`（约 7 KiB），
也能容忍 phdr 表不在文件最前面；代价是合法的“phdr 表在 128 KiB 之后”的 ELF 会被拒绝。

本轮不建议为此扩 API：若未来要支持任意 `e_phoff`，可改成三段读
（ELF 头 → `e_phoff` 处的 phdr 表 → 可加载前缀），这正是 Linux / xv6 的形态；
当前先保留 128 KiB 固定窗口，把它写进 §5 边界即可。

### 7.4 wasm 4 B 魔数有直接先例

Linux `prepare_binprm()` 的 256 B 前缀读，就是“先嗅探格式，再决定要不要完整解析”的先例。
Edgine 对 wasm 只读 4 B 魔数、命中后再读 `/bin/wasmrun` 的 ELF 前缀，符合同一原则；
不需要为了判魔数读 wasm 模块本体。

### 7.5 不要因为对标而抬 1 MiB

Linux 的 phdr 表硬上限是 64 KiB，Redox 的 header + phdr 表上限是 1 MiB；有明确上限是常态。
Edgine 把 1 MiB 保留为“可加载前缀预算”，不需要改成 2 MiB / 4 MiB。真正的修法是
“少读非装载段”，不是“把上限抬高”。

## 8. 未纳入与未确认

- **Redox 内核侧**：`redox-os/kernel` tree 中未发现 exec / ELF 装载源文件；`execve` 由
  `relibc` / `redox-rt` 用户态实现，因此本文只把它作为“用户态分段读”的对照，不把它当作
  内核内装载器。
- **MINIX 3 / SerenityOS / GRUB**：本轮未取得可直接引用的 `exec` / ELF 装载源码片段，
  不写入结论，避免二手材料误导。若后续需要扩围，优先补 MINIX 3 的 PM `exec` 与
  SerenityOS 的 `ELF::Image`。
- **运行时行为**：本轮只做源码阅读，未跑各内核的 exec 实验；结论限定在“读入路径与边界”
  这一面。

## 9. 参考

- Linux `fs/exec.c`：<https://codebrowser.dev/linux/linux/fs/exec.c.html>（`prepare_binprm` L1603–1609）
- Linux `fs/binfmt_elf.c`：<https://codebrowser.dev/linux/linux/fs/binfmt_elf.c.html>（`load_elf_phdrs` L520–553、`elf_map` L370 起、`elf_load` L414 起、`PT_INTERP` L885–900、`PT_LOAD` L1040–1190、`load_elf_interp` L645 起）
- Linux `include/uapi/linux/binfmts.h`：<https://codebrowser.dev/linux/linux/include/uapi/linux/binfmts.h.html>（`BINPRM_BUF_SIZE` 256，L19）
- FreeBSD `sys/kern/imgact_elf.c`：<https://cgit.freebsd.org/src/tree/sys/kern/imgact_elf.c>（`phdr_in_zero_page` L491–494、`load_section` L631 起、`load_sections` L735 起、`get_interp` L1016 起、`exec_elf64_imgact` L1108 起）
- FreeBSD `sys/kern/kern_exec.c`：<https://cgit.freebsd.org/src/tree/sys/kern/kern_exec.c>（`exec_map_first_page` L1179 起）
- xv6 `exec.c` 源码页：<http://www.ccs.neu.edu/~kapil/courses/cs3650f15/resources/unix-xv6/HTML/S/56.html>
- xv6-riscv 教材 §3.8：<https://xv6-guide.github.io/xv6-riscv-book/Ch3.S8.html>
- Redox `relibc` `exec.rs`：<https://github.com/redox-os/relibc/blob/master/src/platform/redox/exec.rs>（L103 用户态 execve 注释）
- Redox `redox-rt/src/proc.rs`：<https://github.com/redox-os/relibc/blob/master/redox-rt/src/proc.rs>（`fexec_impl` L73 起、`MAX_PH_SIZE` L104–114、`pread_all` L660–672）
- Redox 内核 tree：<https://github.com/redox-os/kernel>
